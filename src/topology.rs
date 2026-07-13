use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::tmux::TmuxClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytecode {
    pub version: u32,
    pub team: String,
    pub instructions: Vec<Instruction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Instruction {
    BeginPlan {
        team: String,
    },
    SpawnAgent {
        name: String,
        backend: String,
    },
    DefinePort {
        name: String,
        kind: PortKind,
        #[serde(rename = "type")]
        ty: String,
    },
    InstallReaction {
        id: String,
        agent: String,
        dependencies: Vec<String>,
        productions: Vec<String>,
        contract: String,
        prompt: String,
    },
    CreateChannel {
        id: String,
        source: String,
        target: String,
    },
    CommitPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    Input,
    Output,
    Action,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortState {
    pub kind: PortKind,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionState {
    pub agent: String,
    pub dependencies: Vec<String>,
    pub productions: Vec<String>,
    pub contract: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelState {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmState {
    pub version: u32,
    pub team: String,
    pub agents: BTreeMap<String, AgentState>,
    pub ports: BTreeMap<String, PortState>,
    pub reactions: BTreeMap<String, ReactionState>,
    pub channels: BTreeMap<String, ChannelState>,
    #[serde(skip)]
    pub executed_instructions: usize,
}

pub trait AgentRuntime {
    fn spawn_agent(&mut self, name: &str, backend: &str) -> Result<()>;
}

pub struct TmuxAgentRuntime {
    client: TmuxClient,
    workdir: String,
}

impl TmuxAgentRuntime {
    pub fn new(client: TmuxClient, workdir: String) -> Self {
        Self { client, workdir }
    }
}

impl AgentRuntime for TmuxAgentRuntime {
    fn spawn_agent(&mut self, name: &str, backend: &str) -> Result<()> {
        let session_name = format!("{}{}", self.client.prefix(), name);
        if self.client.has_session(&session_name)? {
            bail!("agent '{name}' already exists");
        }

        let backend = canonical_backend(backend);
        let command = config::resolve_backend(backend)
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("unsupported backend '{backend}' for agent '{name}'"))?;
        self.client
            .new_session(&session_name, &command, Some(&self.workdir))?;
        println!("Spawned topology agent: {name} ({backend})");
        Ok(())
    }
}

#[derive(Default)]
pub struct DryRunAgentRuntime {
    pub spawns: Vec<(String, String)>,
}

impl AgentRuntime for DryRunAgentRuntime {
    fn spawn_agent(&mut self, name: &str, backend: &str) -> Result<()> {
        println!("Would spawn topology agent: {name} ({backend})");
        self.spawns.push((name.to_string(), backend.to_string()));
        Ok(())
    }
}

pub fn load_bytecode(path: &std::path::Path) -> Result<Bytecode> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read bytecode {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid bytecode JSON in {}", path.display()))
}

pub fn execute<R: AgentRuntime>(bytecode: &Bytecode, runtime: &mut R) -> Result<VmState> {
    let mut state = verify(bytecode)?;
    for instruction in &bytecode.instructions {
        match instruction {
            Instruction::SpawnAgent { name, backend } => {
                runtime.spawn_agent(name, backend)?;
            }
            Instruction::BeginPlan { .. }
            | Instruction::DefinePort { .. }
            | Instruction::InstallReaction { .. }
            | Instruction::CreateChannel { .. }
            | Instruction::CommitPlan => {}
        }
        state.executed_instructions += 1;
    }
    Ok(state)
}

pub fn verify(bytecode: &Bytecode) -> Result<VmState> {
    if bytecode.version != 1 {
        bail!("unsupported bytecode version {}", bytecode.version);
    }
    if !valid_identifier(&bytecode.team) {
        bail!("invalid team name '{}'", bytecode.team);
    }
    if bytecode.instructions.len() < 2 {
        bail!("bytecode plan is incomplete");
    }
    match bytecode.instructions.first() {
        Some(Instruction::BeginPlan { team }) if team == &bytecode.team => {}
        Some(Instruction::BeginPlan { team }) => {
            bail!("plan team '{team}' does not match '{}';", bytecode.team)
        }
        _ => bail!("bytecode must begin with begin_plan"),
    }
    if !matches!(bytecode.instructions.last(), Some(Instruction::CommitPlan)) {
        bail!("bytecode must end with commit_plan");
    }

    let mut state = VmState {
        version: bytecode.version,
        team: bytecode.team.clone(),
        agents: BTreeMap::new(),
        ports: BTreeMap::new(),
        reactions: BTreeMap::new(),
        channels: BTreeMap::new(),
        executed_instructions: 0,
    };
    let mut committed = false;

    for (index, instruction) in bytecode.instructions.iter().enumerate() {
        if committed {
            bail!("instruction {index} appears after commit_plan");
        }
        match instruction {
            Instruction::BeginPlan { .. } if index == 0 => {}
            Instruction::BeginPlan { .. } => bail!("duplicate begin_plan at instruction {index}"),
            Instruction::SpawnAgent { name, backend } => {
                require_identifier("agent", name)?;
                if backend.trim().is_empty() {
                    bail!("agent '{name}' has an empty backend");
                }
                if state
                    .agents
                    .insert(
                        name.clone(),
                        AgentState {
                            backend: backend.clone(),
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate agent '{name}'");
                }
            }
            Instruction::DefinePort { name, kind, ty } => {
                require_identifier("port", name)?;
                if ty.trim().is_empty() {
                    bail!("port '{name}' has an empty type");
                }
                if state
                    .ports
                    .insert(
                        name.clone(),
                        PortState {
                            kind: *kind,
                            ty: ty.clone(),
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate port '{name}'");
                }
            }
            Instruction::InstallReaction {
                id,
                agent,
                dependencies,
                productions,
                contract,
                prompt,
            } => {
                if !state.agents.contains_key(agent) {
                    bail!("reaction '{id}' references unknown agent '{agent}'");
                }
                for dependency in dependencies {
                    let port = state.ports.get(dependency).with_context(|| {
                        format!("reaction '{id}' has unknown dependency '{dependency}'")
                    })?;
                    if port.kind == PortKind::Output {
                        bail!("reaction '{id}' cannot depend on output '{dependency}'");
                    }
                }
                for production in productions {
                    let port = state.ports.get(production).with_context(|| {
                        format!("reaction '{id}' has unknown production '{production}'")
                    })?;
                    if port.kind == PortKind::Input {
                        bail!("reaction '{id}' cannot produce input '{production}'");
                    }
                }
                if state
                    .reactions
                    .insert(
                        id.clone(),
                        ReactionState {
                            agent: agent.clone(),
                            dependencies: dependencies.clone(),
                            productions: productions.clone(),
                            contract: contract.clone(),
                            prompt: prompt.clone(),
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate reaction '{id}'");
                }
            }
            Instruction::CreateChannel { id, source, target } => {
                let source_exists =
                    state.ports.contains_key(source) || state.reactions.contains_key(source);
                let target_exists =
                    state.ports.contains_key(target) || state.reactions.contains_key(target);
                if !source_exists || !target_exists {
                    bail!("channel '{id}' has undefined endpoint '{source}' -> '{target}'");
                }
                if state
                    .channels
                    .insert(
                        id.clone(),
                        ChannelState {
                            source: source.clone(),
                            target: target.clone(),
                        },
                    )
                    .is_some()
                {
                    bail!("duplicate channel '{id}'");
                }
            }
            Instruction::CommitPlan => committed = true,
        }
    }

    verify_channels_match_reactions(&state)?;
    Ok(state)
}

fn verify_channels_match_reactions(state: &VmState) -> Result<()> {
    let edges: BTreeSet<(&str, &str)> = state
        .channels
        .values()
        .map(|channel| (channel.source.as_str(), channel.target.as_str()))
        .collect();
    for (id, reaction) in &state.reactions {
        for dependency in &reaction.dependencies {
            if !edges.contains(&(dependency.as_str(), id.as_str())) {
                bail!("reaction '{id}' is missing dependency channel from '{dependency}'");
            }
        }
        for production in &reaction.productions {
            if !edges.contains(&(id.as_str(), production.as_str())) {
                bail!("reaction '{id}' is missing production channel to '{production}'");
            }
        }
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn require_identifier(kind: &str, value: &str) -> Result<()> {
    if valid_identifier(value) {
        Ok(())
    } else {
        bail!("invalid {kind} name '{value}'")
    }
}

fn canonical_backend(backend: &str) -> &str {
    match backend.to_ascii_lowercase().as_str() {
        "claude" | "claudecode" => "claude",
        "codex" => "codex",
        "opencode" => "opencode",
        "cursor" => "cursor",
        "agy" => "agy",
        _ => backend,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program() -> Bytecode {
        serde_json::from_str(
            r#"{
              "version": 1,
              "team": "Demo",
              "instructions": [
                {"op":"begin_plan","team":"Demo"},
                {"op":"spawn_agent","name":"worker","backend":"Codex"},
                {"op":"define_port","name":"request","kind":"input","type":"string"},
                {"op":"define_port","name":"done","kind":"output","type":"bool"},
                {"op":"install_reaction","id":"reaction.0","agent":"worker","dependencies":["request"],"productions":["done"],"contract":"done","prompt":"Work"},
                {"op":"create_channel","id":"in","source":"request","target":"reaction.0"},
                {"op":"create_channel","id":"out","source":"reaction.0","target":"done"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn executes_verified_initial_topology() {
        let mut runtime = DryRunAgentRuntime::default();
        let state = execute(&program(), &mut runtime).unwrap();
        assert_eq!(runtime.spawns, vec![("worker".into(), "Codex".into())]);
        assert_eq!(state.agents.len(), 1);
        assert_eq!(state.ports.len(), 2);
        assert_eq!(state.reactions.len(), 1);
        assert_eq!(state.channels.len(), 2);
        assert_eq!(state.executed_instructions, program().instructions.len());
    }

    #[test]
    fn verifies_before_spawning() {
        let mut program = program();
        program.instructions.pop();
        let mut runtime = DryRunAgentRuntime::default();
        assert!(execute(&program, &mut runtime).is_err());
        assert!(runtime.spawns.is_empty());
    }
}
