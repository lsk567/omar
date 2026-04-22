//! MCP stdio client for talking to the OMAR MCP server.
//!
//! The bridge spawns `omar mcp-server` (no `--context-file` — the server
//! builds a default context from the user's config and active EA) and
//! exchanges JSON-RPC messages over the child's stdio. Line-delimited
//! framing is used; the omar server accepts both that and Content-Length,
//! line-delimited is simpler here.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, warn};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    /// Spawn `omar mcp-server` and complete the initialize handshake.
    /// `omar_binary` is the path to the omar executable (resolved by the
    /// caller — typically next to the bridge binary, falling back to
    /// `omar` on PATH).
    pub async fn start(omar_binary: &PathBuf) -> Result<Self> {
        let mut child = Command::new(omar_binary)
            .arg("mcp-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherit stderr so the MCP server's log lines (config errors,
            // EA resolution failures) surface in the bridge's log.
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to spawn {:?} mcp-server", omar_binary))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("mcp-server child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("mcp-server child has no stdout"))?;
        let stdout = BufReader::new(stdout);

        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "omar-slack-bridge",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .await?;
        debug!(
            "MCP initialize ok: server={}",
            result
                .get("serverInfo")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>")
        );
        Ok(())
    }

    /// Send a JSON-RPC request and read the matching response.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_vec(&req)?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .context("Failed to write MCP request")?;
        self.stdin
            .flush()
            .await
            .context("Failed to flush MCP stdin")?;

        // omar's server replies with a single line in line-delimited mode.
        let mut buf = String::new();
        let n = self
            .stdout
            .read_line(&mut buf)
            .await
            .context("Failed to read MCP response")?;
        if n == 0 {
            return Err(anyhow!("MCP server closed stdout unexpectedly"));
        }
        let resp: Value = serde_json::from_str(buf.trim())
            .with_context(|| format!("Invalid JSON in MCP response: {}", buf.trim()))?;

        if resp.get("id").and_then(|v| v.as_u64()) != Some(id) {
            return Err(anyhow!(
                "MCP response id mismatch (wanted {}, got {})",
                id,
                resp.get("id").cloned().unwrap_or(Value::Null)
            ));
        }
        if let Some(err) = resp.get("error") {
            return Err(anyhow!(
                "MCP server error: {}",
                err.get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
            ));
        }
        resp.get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP response missing 'result'"))
    }

    /// Call a tool and return the `structuredContent` on success.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let result = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
            let msg = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown tool error");
            return Err(anyhow!("tool {} failed: {}", name, msg));
        }
        Ok(result
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Best-effort cleanup of the child process. Tokio's Child does not
        // kill on drop by default; an orphaned `omar mcp-server` would
        // leak if the bridge panics.
        let _ = self.child.start_kill();
    }
}

/// A lazy, self-reconnecting MCP client. Every call to `post_slack_event`
/// attempts to use the existing child; if the child has died (stdout EOF,
/// write failure), the next call transparently respawns. The bridge keeps
/// running across transient MCP-server crashes without losing inbound
/// Slack messages to a permanently-broken connection.
pub struct OmarMcp {
    omar_binary: PathBuf,
    client: Option<McpClient>,
}

impl OmarMcp {
    pub fn new(omar_binary: PathBuf) -> Self {
        Self {
            omar_binary,
            client: None,
        }
    }

    async fn client_mut(&mut self) -> Result<&mut McpClient> {
        if self.client.is_none() {
            let client = McpClient::start(&self.omar_binary).await?;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().unwrap())
    }

    /// Post an inbound Slack message to the EA's event queue.
    pub async fn post_slack_event(&mut self, payload: &str) -> Result<()> {
        let args = json!({
            "sender": "slack-bridge",
            "receiver": "ea",
            "payload": payload,
        });
        if let Err(e) = self.try_call("omar_wake_later", args.clone()).await {
            warn!(
                "MCP omar_wake_later failed ({}); restarting MCP server and retrying",
                e
            );
            self.client = None;
            self.try_call("omar_wake_later", args).await?;
        }
        Ok(())
    }

    async fn try_call(&mut self, name: &str, args: Value) -> Result<Value> {
        self.client_mut().await?.call_tool(name, args).await
    }

    /// Best-effort startup probe so the bridge logs whether the MCP server
    /// is reachable before any Slack traffic arrives.
    pub async fn health_check(&mut self) -> Result<()> {
        self.client_mut().await?;
        Ok(())
    }
}
