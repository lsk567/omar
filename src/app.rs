#![allow(dead_code)]

use anyhow::Result;

use crate::config::Config;
use crate::tmux::{HealthChecker, HealthInfo, HealthState, Session, TmuxClient};

/// Information about an agent for display
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub session: Session,
    pub health: HealthState,
    pub health_info: HealthInfo,
}

/// Application state
pub struct App {
    pub agents: Vec<AgentInfo>,
    pub selected: usize,
    pub should_quit: bool,
    pub show_help: bool,
    pub show_confirm_kill: bool,
    pub filter: String,
    pub status_message: Option<String>,
    client: TmuxClient,
    health_checker: HealthChecker,
    default_command: String,
    default_workdir: String,
    session_prefix: String,
}

impl App {
    pub fn new(config: &Config) -> Self {
        let client = TmuxClient::new(&config.dashboard.session_prefix);
        let health_checker = HealthChecker::new(
            client.clone(),
            config.health.idle_warning,
            config.health.idle_critical,
            &config.health.error_patterns,
        );

        Self {
            agents: Vec::new(),
            selected: 0,
            should_quit: false,
            show_help: false,
            show_confirm_kill: false,
            filter: String::new(),
            status_message: None,
            client,
            health_checker,
            default_command: config.agent.default_command.clone(),
            default_workdir: config.agent.default_workdir.clone(),
            session_prefix: config.dashboard.session_prefix.clone(),
        }
    }

    pub fn client(&self) -> &TmuxClient {
        &self.client
    }

    /// Refresh the list of agents
    pub fn refresh(&mut self) -> Result<()> {
        let sessions = self.client.list_sessions()?;

        self.agents = sessions
            .into_iter()
            .map(|session| {
                let health_info = self.health_checker.check_detailed(&session);
                let health = health_info.state;
                AgentInfo {
                    session,
                    health,
                    health_info,
                }
            })
            .collect();

        // Apply filter if set
        if !self.filter.is_empty() {
            let filter = self.filter.to_lowercase();
            self.agents
                .retain(|a| a.session.name.to_lowercase().contains(&filter));
        }

        // Keep selection in bounds
        if !self.agents.is_empty() && self.selected >= self.agents.len() {
            self.selected = self.agents.len() - 1;
        }

        Ok(())
    }

    /// Get filtered agents
    pub fn visible_agents(&self) -> &[AgentInfo] {
        &self.agents
    }

    /// Move selection down
    pub fn next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
        }
    }

    /// Move selection up
    pub fn previous(&mut self) {
        if !self.agents.is_empty() {
            self.selected = self
                .selected
                .checked_sub(1)
                .unwrap_or(self.agents.len().saturating_sub(1));
        }
    }

    /// Get currently selected agent
    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        self.agents.get(self.selected)
    }

    /// Attach to the selected agent via popup
    pub fn attach_selected(&self) -> Result<()> {
        if let Some(agent) = self.selected_agent() {
            self.client
                .attach_popup(&agent.session.name, "80%", "80%")?;
        }
        Ok(())
    }

    /// Kill the selected agent
    pub fn kill_selected(&mut self) -> Result<()> {
        if let Some(agent) = self.selected_agent() {
            let name = agent.session.name.clone();
            self.client.kill_session(&name)?;
            self.status_message = Some(format!("Killed agent: {}", name));
            self.refresh()?;
        }
        self.show_confirm_kill = false;
        Ok(())
    }

    /// Generate a unique agent name
    fn generate_agent_name(&self) -> String {
        let existing: std::collections::HashSet<_> = self
            .agents
            .iter()
            .map(|a| a.session.name.as_str())
            .collect();

        for i in 1..1000 {
            let name = format!("{}{}", self.session_prefix, i);
            if !existing.contains(name.as_str()) {
                return name;
            }
        }
        format!(
            "{}{}",
            self.session_prefix,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        )
    }

    /// Spawn a new agent with default settings
    pub fn spawn_agent(&mut self) -> Result<()> {
        // Refresh first to get current state
        self.refresh()?;

        let name = self.generate_agent_name();
        let workdir = if self.default_workdir == "." {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        } else {
            self.default_workdir.clone()
        };

        self.client
            .new_session(&name, &self.default_command, Some(&workdir))?;

        let short_name = name.strip_prefix(&self.session_prefix).unwrap_or(&name);
        self.status_message = Some(format!("Spawned agent: {}", short_name));
        self.refresh()?;

        // Select the new agent
        if let Some(pos) = self.agents.iter().position(|a| a.session.name == name) {
            self.selected = pos;
        }

        Ok(())
    }

    /// Set status message
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }

    /// Clear status message
    pub fn clear_status(&mut self) {
        self.status_message = None;
    }

    /// Get counts by health state: (working, waiting, idle, stuck)
    pub fn health_counts(&self) -> (usize, usize, usize, usize) {
        let mut working = 0;
        let mut waiting = 0;
        let mut idle = 0;
        let mut stuck = 0;

        for agent in &self.agents {
            match agent.health {
                HealthState::Working => working += 1,
                HealthState::WaitingForInput => waiting += 1,
                HealthState::Idle => idle += 1,
                HealthState::Stuck => stuck += 1,
            }
        }

        (working, waiting, idle, stuck)
    }
}
