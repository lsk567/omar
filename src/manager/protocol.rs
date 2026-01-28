//! JSON protocol for manager ↔ OMAR communication

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// A proposed agent in a plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedAgent {
    pub name: String,
    pub role: String,
    pub task: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// A plan proposed by the manager
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub description: String,
    pub agents: Vec<ProposedAgent>,
}

/// Messages from manager to OMAR
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ManagerMessage {
    #[serde(rename = "plan")]
    Plan {
        description: String,
        agents: Vec<ProposedAgent>,
    },
    #[serde(rename = "send")]
    Send { target: String, message: String },
    #[serde(rename = "query")]
    Query { target: String },
    #[serde(rename = "complete")]
    Complete { summary: String },
}

/// Messages from OMAR to manager
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OmarMessage {
    #[serde(rename = "plan_approved")]
    PlanApproved { modifications: Vec<String> },
    #[serde(rename = "plan_rejected")]
    PlanRejected { reason: String },
    #[serde(rename = "status")]
    Status {
        agent: String,
        state: String,
        last_output: String,
    },
    #[serde(rename = "agent_complete")]
    AgentComplete { agent: String, summary: String },
    #[serde(rename = "agent_blocked")]
    AgentBlocked { agent: String, reason: String },
}

/// Try to parse a manager message from text
/// Looks for JSON blocks in the output
pub fn parse_manager_message(text: &str) -> Option<ManagerMessage> {
    // Look for JSON block between ```json and ```
    if let Some(start) = text.find("```json") {
        let json_start = start + 7;
        if let Some(end) = text[json_start..].find("```") {
            let json_str = &text[json_start..json_start + end].trim();
            if let Ok(msg) = serde_json::from_str(json_str) {
                return Some(msg);
            }
        }
    }

    // Try to find raw JSON object
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            let json_str = &text[start..=end];
            if let Ok(msg) = serde_json::from_str(json_str) {
                return Some(msg);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plan() {
        let text = r#"
Here's my proposed plan:

```json
{
  "type": "plan",
  "description": "Build REST API",
  "agents": [
    {"name": "api", "role": "API Developer", "task": "Create endpoints", "depends_on": []},
    {"name": "db", "role": "DB Developer", "task": "Setup schema", "depends_on": []}
  ]
}
```

Let me know if this looks good!
"#;

        let msg = parse_manager_message(text).unwrap();
        match msg {
            ManagerMessage::Plan {
                description,
                agents,
            } => {
                assert_eq!(description, "Build REST API");
                assert_eq!(agents.len(), 2);
                assert_eq!(agents[0].name, "api");
            }
            _ => panic!("Expected Plan message"),
        }
    }

    #[test]
    fn test_parse_send() {
        let text = r#"{"type": "send", "target": "api", "message": "Add /users endpoint"}"#;

        let msg = parse_manager_message(text).unwrap();
        match msg {
            ManagerMessage::Send { target, message } => {
                assert_eq!(target, "api");
                assert_eq!(message, "Add /users endpoint");
            }
            _ => panic!("Expected Send message"),
        }
    }
}
