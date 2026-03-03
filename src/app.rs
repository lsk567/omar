#![allow(dead_code)]

use anyhow::Result;
use std::collections::HashMap;

use crate::config::Config;
use crate::manager::MANAGER_SESSION;
use crate::memory;
use crate::projects::{self, Project};
use crate::scheduler::{ScheduledEvent, TickerBuffer};
use crate::tmux::{HealthChecker, HealthInfo, HealthState, Session, TmuxClient};
use crate::DASHBOARD_SESSION;

// Re-export for API handlers
pub use crate::manager::MANAGER_SESSION as MANAGER_SESSION_NAME;

/// Shared app state for API access
pub type SharedApp = App;

/// Information about an agent for display
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub session: Session,
    pub health: HealthState,
    pub health_info: HealthInfo,
}

/// A node in the chain-of-command tree
#[derive(Debug, Clone)]
pub struct CommandTreeNode {
    /// Display name (e.g. "Executive Assistant", "Project Manager: rest-api")
    pub name: String,
    /// Full tmux session name (empty for EA root)
    pub session_name: String,
    /// Health state of this agent
    pub health: HealthState,
    /// Depth in the tree (0 = EA, 1 = PM, 2 = worker)
    pub depth: usize,
    /// Whether this is the last sibling at its depth
    pub is_last_sibling: bool,
    /// For each ancestor depth, whether that ancestor was the last sibling.
    /// Used to decide whether to draw "│" or " " for vertical continuation lines.
    pub ancestor_is_last: Vec<bool>,
}

/// A group of agents: a PM and its workers, or unassigned workers
pub struct AgentGroup<'a> {
    /// The PM agent, if any (None for unassigned group)
    pub pm: Option<&'a AgentInfo>,
    /// Workers under this PM (or unassigned)
    pub workers: Vec<&'a AgentInfo>,
}

/// Application state
pub struct App {
    pub agents: Vec<AgentInfo>,
    pub manager: Option<AgentInfo>,
    pub command_tree: Vec<CommandTreeNode>,
    pub selected: usize,
    pub manager_selected: bool,
    pub interactive_mode: bool,
    pub should_quit: bool,
    pub show_help: bool,
    pub show_confirm_kill: bool,
    pub filter: String,
    pub status_message: Option<String>,
    pub projects: Vec<Project>,
    pub project_input_mode: bool,
    pub project_input: String,
    pub show_events: bool,
    pub scheduled_events: Vec<ScheduledEvent>,
    pub ticker: TickerBuffer,
    pub ticker_offset: usize,
    pub show_debug_console: bool,
    /// Session name of the agent shown in the bottom panel (default: MANAGER_SESSION)
    pub focus_parent: String,
    /// Stack for Esc navigation (drill-up restores previous parent)
    focus_stack: Vec<String>,
    /// Indices into self.agents for the current focus_parent's direct children
    pub focus_child_indices: Vec<usize>,
    agent_parents: HashMap<String, String>,
    worker_tasks: HashMap<String, String>,
    client: TmuxClient,
    health_checker: HealthChecker,
    default_command: String,
    default_workdir: String,
    session_prefix: String,
}

impl App {
    pub fn new(config: &Config, ticker: TickerBuffer) -> Self {
        let client = TmuxClient::new(&config.dashboard.session_prefix);
        let health_checker = HealthChecker::new(client.clone(), config.health.idle_warning);

        Self {
            agents: Vec::new(),
            manager: None,
            command_tree: Vec::new(),
            selected: 0,
            manager_selected: true,
            interactive_mode: false,
            should_quit: false,
            show_help: false,
            show_confirm_kill: false,
            filter: String::new(),
            status_message: None,
            projects: projects::load_projects(),
            project_input_mode: false,
            project_input: String::new(),
            show_events: false,
            scheduled_events: Vec::new(),
            ticker,
            ticker_offset: 0,
            show_debug_console: false,
            focus_parent: MANAGER_SESSION.to_string(),
            focus_stack: Vec::new(),
            focus_child_indices: Vec::new(),
            agent_parents: HashMap::new(),
            worker_tasks: HashMap::new(),
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
        // Ensure manager exists
        self.ensure_manager()?;

        // Get all sessions
        let sessions = self.client.list_all_sessions()?;

        // Separate manager from other agents, filtering out non-omar sessions
        let mut manager_session = None;
        let mut other_sessions = Vec::new();

        for session in sessions {
            if session.name == MANAGER_SESSION {
                manager_session = Some(session);
            } else if session.name == DASHBOARD_SESSION {
                // Skip the dashboard's own tmux session
                continue;
            } else if !self.session_prefix.is_empty()
                && !session.name.starts_with(&self.session_prefix)
            {
                // Skip sessions that don't match the configured prefix
                continue;
            } else {
                other_sessions.push(session);
            }
        }

        // Update manager info
        self.manager = manager_session.map(|session| {
            let health_info = self.health_checker.check_detailed(&session.name);
            let health = health_info.state;
            AgentInfo {
                session,
                health,
                health_info,
            }
        });

        // Update agents list (excluding attached sessions)
        // Attached sessions are likely the user's main terminal, not agents
        let filtered: Vec<Session> = other_sessions
            .into_iter()
            .filter(|session| !session.attached)
            .collect();

        self.agents = filtered
            .into_iter()
            .map(|session| {
                let health_info = self.health_checker.check_detailed(&session.name);
                let health = health_info.state;
                AgentInfo {
                    session,
                    health,
                    health_info,
                }
            })
            .collect();

        // Clean up stale frame data for sessions that no longer exist
        let active: Vec<String> = self
            .agents
            .iter()
            .map(|a| a.session.name.clone())
            .chain(self.manager.iter().map(|m| m.session.name.clone()))
            .collect();
        self.health_checker.retain_sessions(&active);

        // Apply filter if set
        if !self.filter.is_empty() {
            let filter = self.filter.to_lowercase();
            self.agents
                .retain(|a| a.session.name.to_lowercase().contains(&filter));
        }

        // Reload projects from file (picks up API-side changes)
        self.projects = projects::load_projects();

        // Load parent mappings, worker tasks, and build the chain-of-command tree
        self.agent_parents = memory::load_agent_parents();
        self.worker_tasks = memory::load_worker_tasks();
        self.command_tree = build_tree(
            &self.agents,
            self.manager.as_ref(),
            &self.agent_parents,
            &self.session_prefix,
        );

        // Recompute focus children indices
        self.focus_child_indices = self.compute_focus_child_indices();

        // Keep selection in bounds relative to focus children
        if !self.manager_selected
            && !self.focus_child_indices.is_empty()
            && self.selected >= self.focus_child_indices.len()
        {
            self.selected = self.focus_child_indices.len() - 1;
        }

        Ok(())
    }

    /// Ensure manager session exists, start if not
    fn ensure_manager(&self) -> Result<()> {
        if self.client.has_session(MANAGER_SESSION)? {
            return Ok(());
        }

        // Build command with EA system prompt + memory baked in
        let cmd = crate::manager::build_ea_command(&self.default_command);

        // Start manager session — system prompt set at process start
        let workdir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        self.client
            .new_session(MANAGER_SESSION, &cmd, Some(&workdir))?;

        // Write memory after creating manager
        memory::write_memory(&self.agents, None, &self.client);

        Ok(())
    }

    /// Get filtered agents
    pub fn visible_agents(&self) -> &[AgentInfo] {
        &self.agents
    }

    /// Get all agents (for API)
    pub fn agents(&self) -> &[AgentInfo] {
        &self.agents
    }

    /// Get manager info (for API)
    pub fn manager(&self) -> Option<&AgentInfo> {
        self.manager.as_ref()
    }

    /// Group agents into PM → worker hierarchies for grid display
    pub fn agent_groups(&self) -> Vec<AgentGroup<'_>> {
        build_agent_groups(&self.agents, &self.agent_parents, &self.session_prefix)
    }

    /// Get default command
    pub fn default_command(&self) -> &str {
        &self.default_command
    }

    /// Get the agent_parents map (for API/display)
    pub fn agent_parents(&self) -> &HashMap<String, String> {
        &self.agent_parents
    }

    /// Get the worker_tasks map (for display)
    pub fn worker_tasks(&self) -> &HashMap<String, String> {
        &self.worker_tasks
    }

    /// Compute indices into self.agents for focus_parent's direct children
    fn compute_focus_child_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        if self.focus_parent == MANAGER_SESSION {
            // Root view: show PMs + orphan workers (direct children of EA)
            for (i, agent) in self.agents.iter().enumerate() {
                let short = agent
                    .session
                    .name
                    .strip_prefix(&self.session_prefix)
                    .unwrap_or(&agent.session.name);
                let is_pm = short.starts_with("pm-");
                if is_pm {
                    // PMs are always direct children of EA
                    indices.push(i);
                } else {
                    // Non-PM: include if it has no parent, or its parent is not a live PM
                    let parent = self.agent_parents.get(&agent.session.name);
                    let has_live_pm_parent = parent
                        .map(|p| {
                            self.agents.iter().any(|a| {
                                a.session.name == *p
                                    && a.session
                                        .name
                                        .strip_prefix(&self.session_prefix)
                                        .unwrap_or(&a.session.name)
                                        .starts_with("pm-")
                            })
                        })
                        .unwrap_or(false);
                    if !has_live_pm_parent {
                        indices.push(i);
                    }
                }
            }
        } else {
            // Non-root: show agents whose parent matches focus_parent
            for (i, agent) in self.agents.iter().enumerate() {
                if let Some(parent) = self.agent_parents.get(&agent.session.name) {
                    if *parent == self.focus_parent {
                        indices.push(i);
                    }
                }
            }
        }
        indices
    }

    /// Get the direct children of the current focus parent
    pub fn focus_children(&self) -> Vec<&AgentInfo> {
        self.focus_child_indices
            .iter()
            .filter_map(|&i| self.agents.get(i))
            .collect()
    }

    /// Get AgentInfo for the focus parent
    pub fn focus_parent_info(&self) -> Option<&AgentInfo> {
        if self.focus_parent == MANAGER_SESSION {
            self.manager.as_ref()
        } else {
            self.agents
                .iter()
                .find(|a| a.session.name == self.focus_parent)
        }
    }

    /// Check if an agent has children (is a PM or EA)
    fn agent_has_children(&self, session_name: &str) -> bool {
        if session_name == MANAGER_SESSION {
            return true;
        }
        self.agent_parents.values().any(|p| p == session_name)
    }

    /// Count children for a given agent
    pub fn child_count(&self, session_name: &str) -> usize {
        if session_name == MANAGER_SESSION {
            // Count PMs + orphans
            return self.compute_focus_child_indices().len();
        }
        self.agent_parents
            .values()
            .filter(|p| *p == session_name)
            .count()
    }

    /// Build breadcrumb path from root to current focus parent
    pub fn breadcrumb(&self) -> Vec<String> {
        let mut crumbs: Vec<String> = vec!["EA".to_string()];
        for session in &self.focus_stack {
            if *session == MANAGER_SESSION {
                continue; // Already added EA
            }
            let short = session
                .strip_prefix(&self.session_prefix)
                .unwrap_or(session);
            if let Some(rest) = short.strip_prefix("pm-") {
                crumbs.push(format!("PM: {}", rest));
            } else {
                crumbs.push(short.to_string());
            }
        }
        // Add current focus parent if not EA
        if self.focus_parent != MANAGER_SESSION {
            let short = self
                .focus_parent
                .strip_prefix(&self.session_prefix)
                .unwrap_or(&self.focus_parent);
            if let Some(rest) = short.strip_prefix("pm-") {
                crumbs.push(format!("PM: {}", rest));
            } else {
                crumbs.push(short.to_string());
            }
        }
        crumbs
    }

    /// Drill down into the selected agent (Tab). Only works if the agent has children.
    pub fn drill_down(&mut self) {
        let session_name = if self.manager_selected {
            // Already viewing EA's children; drill down into EA is a no-op
            return;
        } else {
            // Get the session name of the selected focus child
            if let Some(&idx) = self.focus_child_indices.get(self.selected) {
                if let Some(agent) = self.agents.get(idx) {
                    agent.session.name.clone()
                } else {
                    return;
                }
            } else {
                return;
            }
        };

        // Only drill down if the selected agent has children
        if !self.agent_has_children(&session_name) {
            return;
        }

        self.focus_stack.push(self.focus_parent.clone());
        self.focus_parent = session_name.clone();
        self.selected = 0;
        self.manager_selected = false;
        self.focus_child_indices = self.compute_focus_child_indices();

        let short = session_name
            .strip_prefix(&self.session_prefix)
            .unwrap_or(&session_name);
        self.status_message = Some(format!("Viewing: {}", short));
    }

    /// Drill up to the parent view (Esc). Returns true if drilled up, false if at root.
    pub fn drill_up(&mut self) -> bool {
        if self.focus_stack.is_empty() {
            return false;
        }
        self.focus_parent = self.focus_stack.pop().unwrap();
        self.selected = 0;
        self.manager_selected = true;
        self.focus_child_indices = self.compute_focus_child_indices();
        true
    }

    /// Move selection down (within focus children + focus parent)
    pub fn next(&mut self) {
        let child_count = self.focus_child_indices.len();
        if self.manager_selected {
            // From focus parent, go to first child (if any)
            if child_count > 0 {
                self.manager_selected = false;
                self.selected = 0;
            }
        } else if child_count > 0 {
            if self.selected + 1 >= child_count {
                // From last child, go to focus parent
                self.manager_selected = true;
            } else {
                self.selected += 1;
            }
        } else {
            self.manager_selected = true;
        }
    }

    /// Move selection up (within focus children + focus parent)
    pub fn previous(&mut self) {
        let child_count = self.focus_child_indices.len();
        if self.manager_selected {
            // From focus parent, go to last child (if any)
            if child_count > 0 {
                self.manager_selected = false;
                self.selected = child_count - 1;
            }
        } else if child_count > 0 {
            if self.selected == 0 {
                self.manager_selected = true;
            } else {
                self.selected -= 1;
            }
        } else {
            self.manager_selected = true;
        }
    }

    /// Get currently selected agent (could be focus parent or a focus child)
    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        if self.manager_selected {
            self.focus_parent_info()
        } else {
            self.focus_child_indices
                .get(self.selected)
                .and_then(|&idx| self.agents.get(idx))
        }
    }

    /// Attach to the selected agent via popup
    pub fn attach_selected(&self) -> Result<()> {
        if let Some(agent) = self.selected_agent() {
            self.client
                .attach_popup(&agent.session.name, "90%", "90%")?;
        }
        Ok(())
    }

    /// Kill the selected agent
    pub fn kill_selected(&mut self) -> Result<()> {
        if let Some(agent) = self.selected_agent() {
            // Safety: don't kill attached sessions (user's terminal)
            if agent.session.attached {
                self.status_message = Some("Cannot kill attached session".to_string());
                self.show_confirm_kill = false;
                return Ok(());
            }

            // Safety: don't kill manager from 'd' key (use separate mechanism)
            if agent.session.name == MANAGER_SESSION {
                self.status_message = Some("Cannot kill manager with 'd'".to_string());
                self.show_confirm_kill = false;
                return Ok(());
            }

            let name = agent.session.name.clone();
            self.client.kill_session(&name)?;
            memory::remove_agent_parent(&name);
            self.status_message = Some(format!("Killed agent: {}", name));
            self.refresh()?;
            memory::write_memory(&self.agents, self.manager.as_ref(), &self.client);
        }
        self.show_confirm_kill = false;
        Ok(())
    }

    /// Generate a unique agent name
    pub fn generate_agent_name(&self) -> String {
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

        memory::write_memory(&self.agents, self.manager.as_ref(), &self.client);

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

    /// Get counts by health state: (running, idle)
    /// Includes manager in the count
    pub fn health_counts(&self) -> (usize, usize) {
        let mut running = 0;
        let mut idle = 0;

        for agent in &self.agents {
            match agent.health {
                HealthState::Running => running += 1,
                HealthState::Idle => idle += 1,
            }
        }

        if let Some(ref manager) = self.manager {
            match manager.health {
                HealthState::Running => running += 1,
                HealthState::Idle => idle += 1,
            }
        }

        (running, idle)
    }

    /// Get total agent count (including manager)
    pub fn total_agents(&self) -> usize {
        self.agents.len() + if self.manager.is_some() { 1 } else { 0 }
    }

    /// Enter interactive mode (for selected agent or manager)
    pub fn enter_interactive(&mut self) {
        if self.selected_agent().is_some() {
            self.interactive_mode = true;
        }
    }

    /// Exit interactive mode
    pub fn exit_interactive(&mut self) {
        self.interactive_mode = false;
    }

    /// Get the session name of the currently selected agent
    fn selected_session(&self) -> Option<&str> {
        self.selected_agent().map(|a| a.session.name.as_str())
    }

    /// Send a key to the selected agent (for interactive mode)
    pub fn send_key_to_selected(&self, key: &str) -> Result<()> {
        if let Some(session) = self.selected_session() {
            self.client.send_keys(session, key)
        } else {
            Ok(())
        }
    }

    /// Send literal text to the selected agent (for interactive mode)
    pub fn send_text_to_selected(&self, text: &str) -> Result<()> {
        if let Some(session) = self.selected_session() {
            self.client.send_keys_literal(session, text)
        } else {
            Ok(())
        }
    }

    /// Get focus parent pane output (more lines for display)
    pub fn get_focus_parent_output(&self, lines: i32) -> Result<String> {
        self.client.capture_pane(&self.focus_parent, lines)
    }

    /// Get agent pane output by session name
    pub fn get_agent_output(&self, session: &str, lines: i32) -> Result<String> {
        self.client.capture_pane(session, lines)
    }

    /// Add a project and update memory
    pub fn add_project(&mut self, name: &str) {
        let _ = projects::add_project(name);
        self.projects = projects::load_projects();
        memory::write_memory(&self.agents, self.manager.as_ref(), &self.client);
    }

    /// Complete (remove) a project by id and update memory
    pub fn complete_project(&mut self, id: usize) {
        let _ = projects::remove_project(id);
        self.projects = projects::load_projects();
        memory::write_memory(&self.agents, self.manager.as_ref(), &self.client);
    }
}

/// Group agents into PM → worker hierarchies for grid display.
///
/// PMs are identified by the "pm-" prefix after stripping the session prefix.
/// Workers are assigned to PMs via the agent_parents map (child → parent).
/// Workers without a valid PM parent go into an orphan group (pm: None).
pub fn build_agent_groups<'a>(
    agents: &'a [AgentInfo],
    agent_parents: &HashMap<String, String>,
    session_prefix: &str,
) -> Vec<AgentGroup<'a>> {
    let mut pms: Vec<&AgentInfo> = Vec::new();
    let mut non_pms: Vec<&AgentInfo> = Vec::new();
    for agent in agents {
        let short = agent
            .session
            .name
            .strip_prefix(session_prefix)
            .unwrap_or(&agent.session.name);
        if short.starts_with("pm-") {
            pms.push(agent);
        } else {
            non_pms.push(agent);
        }
    }

    let mut pm_children: HashMap<String, Vec<&AgentInfo>> = HashMap::new();
    let mut orphans: Vec<&AgentInfo> = Vec::new();

    for agent in non_pms {
        if let Some(parent_session) = agent_parents.get(&agent.session.name) {
            if pms.iter().any(|pm| pm.session.name == *parent_session) {
                pm_children
                    .entry(parent_session.clone())
                    .or_default()
                    .push(agent);
            } else {
                orphans.push(agent);
            }
        } else {
            orphans.push(agent);
        }
    }

    let mut groups = Vec::new();

    for pm in &pms {
        let workers = pm_children.remove(&pm.session.name).unwrap_or_default();
        groups.push(AgentGroup {
            pm: Some(pm),
            workers,
        });
    }

    if !orphans.is_empty() {
        groups.push(AgentGroup {
            pm: None,
            workers: orphans,
        });
    }

    groups
}

/// Build the chain-of-command tree from current agents and parent mappings.
///
/// Tree structure:
///   EA (root, depth 0)
///   ├── PM agents (depth 1, identified by "pm-" prefix)
///   │   └── Workers with that PM as parent (depth 2)
///   └── Orphan workers with no parent (depth 1, under EA)
pub fn build_tree(
    agents: &[AgentInfo],
    manager: Option<&AgentInfo>,
    agent_parents: &HashMap<String, String>,
    session_prefix: &str,
) -> Vec<CommandTreeNode> {
    let mut nodes = Vec::new();

    // Root: EA
    let ea_health = manager.map(|m| m.health).unwrap_or(HealthState::Idle);
    nodes.push(CommandTreeNode {
        name: "Executive Assistant".to_string(),
        session_name: MANAGER_SESSION.to_string(),
        health: ea_health,
        depth: 0,
        is_last_sibling: true,
        ancestor_is_last: vec![],
    });

    // Partition agents into PMs and non-PMs
    let mut pms: Vec<&AgentInfo> = Vec::new();
    let mut non_pms: Vec<&AgentInfo> = Vec::new();
    for agent in agents {
        let short = agent
            .session
            .name
            .strip_prefix(session_prefix)
            .unwrap_or(&agent.session.name);
        if short.starts_with("pm-") {
            pms.push(agent);
        } else {
            non_pms.push(agent);
        }
    }

    // For each non-PM, figure out if it has a PM parent
    let mut pm_children: HashMap<String, Vec<&AgentInfo>> = HashMap::new();
    let mut orphans: Vec<&AgentInfo> = Vec::new();

    for agent in &non_pms {
        if let Some(parent_session) = agent_parents.get(&agent.session.name) {
            // Check that the parent PM actually exists
            if pms.iter().any(|pm| pm.session.name == *parent_session) {
                pm_children
                    .entry(parent_session.clone())
                    .or_default()
                    .push(agent);
            } else {
                orphans.push(agent);
            }
        } else {
            orphans.push(agent);
        }
    }

    // Add PM nodes (depth 1) and their worker children (depth 2)
    for (pm_idx, pm) in pms.iter().enumerate() {
        // PM is last among EA children only if it's the last PM AND there are no orphans
        let is_last = pm_idx == pms.len() - 1 && orphans.is_empty();

        let short = pm
            .session
            .name
            .strip_prefix(session_prefix)
            .unwrap_or(&pm.session.name);
        let pm_display = if let Some(rest) = short.strip_prefix("pm-") {
            format!("Project Manager: {}", rest)
        } else {
            short.to_string()
        };

        nodes.push(CommandTreeNode {
            name: pm_display,
            session_name: pm.session.name.clone(),
            health: pm.health,
            depth: 1,
            is_last_sibling: is_last,
            ancestor_is_last: vec![true], // EA is always last at depth 0
        });

        // Add workers under this PM (depth 2)
        let children = pm_children.get(&pm.session.name);
        if let Some(kids) = children {
            for (kid_idx, kid) in kids.iter().enumerate() {
                let kid_short = kid
                    .session
                    .name
                    .strip_prefix(session_prefix)
                    .unwrap_or(&kid.session.name);
                let kid_is_last = kid_idx == kids.len() - 1;
                nodes.push(CommandTreeNode {
                    name: kid_short.to_string(),
                    session_name: kid.session.name.clone(),
                    health: kid.health,
                    depth: 2,
                    is_last_sibling: kid_is_last,
                    ancestor_is_last: vec![true, is_last],
                });
            }
        }
    }

    // Add orphan workers under a synthetic "Unassigned" group (depth 1)
    if !orphans.is_empty() {
        nodes.push(CommandTreeNode {
            name: "Unassigned".to_string(),
            session_name: String::new(),
            health: HealthState::Idle,
            depth: 1,
            is_last_sibling: true, // always last child of EA
            ancestor_is_last: vec![true],
        });

        for (orphan_idx, orphan) in orphans.iter().enumerate() {
            let short = orphan
                .session
                .name
                .strip_prefix(session_prefix)
                .unwrap_or(&orphan.session.name);
            let is_last = orphan_idx == orphans.len() - 1;
            nodes.push(CommandTreeNode {
                name: short.to_string(),
                session_name: orphan.session.name.clone(),
                health: orphan.health,
                depth: 2,
                is_last_sibling: is_last,
                ancestor_is_last: vec![true, true], // EA is last, Unassigned is last
            });
        }
    }

    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{HealthInfo, HealthState, Session};

    fn make_agent(name: &str, health: HealthState) -> AgentInfo {
        AgentInfo {
            session: Session {
                name: name.to_string(),
                activity: 0,
                attached: false,
                pane_pid: 0,
            },
            health,
            health_info: HealthInfo {
                state: health,
                last_output: String::new(),
            },
        }
    }

    // ── build_agent_groups tests ──

    #[test]
    fn test_groups_pm_with_workers() {
        let agents = vec![
            make_agent("omar-agent-pm-rest-api", HealthState::Running),
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-api".to_string(),
            "omar-agent-pm-rest-api".to_string(),
        );
        parents.insert(
            "omar-agent-auth".to_string(),
            "omar-agent-pm-rest-api".to_string(),
        );

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // One PM group, no orphans
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pm.unwrap().session.name, "omar-agent-pm-rest-api");
        assert_eq!(groups[0].workers.len(), 2);
        assert_eq!(groups[0].workers[0].session.name, "omar-agent-api");
        assert_eq!(groups[0].workers[1].session.name, "omar-agent-auth");
    }

    #[test]
    fn test_groups_all_orphans_no_pm() {
        let agents = vec![
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
        ];
        let parents = HashMap::new();

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // One orphan group, no PM groups
        assert_eq!(groups.len(), 1);
        assert!(groups[0].pm.is_none());
        assert_eq!(groups[0].workers.len(), 2);
    }

    #[test]
    fn test_groups_pm_without_workers() {
        let agents = vec![make_agent("omar-agent-pm-rest-api", HealthState::Running)];
        let parents = HashMap::new();

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // One PM group with zero workers
        assert_eq!(groups.len(), 1);
        assert!(groups[0].pm.is_some());
        assert!(groups[0].workers.is_empty());
    }

    #[test]
    fn test_groups_mixed_pm_and_orphans() {
        let agents = vec![
            make_agent("omar-agent-pm-api", HealthState::Running),
            make_agent("omar-agent-worker1", HealthState::Running),
            make_agent("omar-agent-orphan1", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-worker1".to_string(),
            "omar-agent-pm-api".to_string(),
        );

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // PM group + orphan group
        assert_eq!(groups.len(), 2);

        // First group: PM with worker1
        assert_eq!(groups[0].pm.unwrap().session.name, "omar-agent-pm-api");
        assert_eq!(groups[0].workers.len(), 1);
        assert_eq!(groups[0].workers[0].session.name, "omar-agent-worker1");

        // Second group: orphan
        assert!(groups[1].pm.is_none());
        assert_eq!(groups[1].workers.len(), 1);
        assert_eq!(groups[1].workers[0].session.name, "omar-agent-orphan1");
    }

    #[test]
    fn test_groups_stale_parent_becomes_orphan() {
        let agents = vec![make_agent("omar-agent-worker1", HealthState::Running)];
        // Parent PM doesn't exist in agents list
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-worker1".to_string(),
            "omar-agent-pm-gone".to_string(),
        );

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // Worker should be orphan since parent PM doesn't exist
        assert_eq!(groups.len(), 1);
        assert!(groups[0].pm.is_none());
        assert_eq!(groups[0].workers.len(), 1);
        assert_eq!(groups[0].workers[0].session.name, "omar-agent-worker1");
    }

    #[test]
    fn test_groups_two_pms_each_with_workers() {
        let agents = vec![
            make_agent("omar-agent-pm-api", HealthState::Running),
            make_agent("omar-agent-pm-frontend", HealthState::Running),
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Running),
            make_agent("omar-agent-ui", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-api".to_string(),
            "omar-agent-pm-api".to_string(),
        );
        parents.insert(
            "omar-agent-auth".to_string(),
            "omar-agent-pm-api".to_string(),
        );
        parents.insert(
            "omar-agent-ui".to_string(),
            "omar-agent-pm-frontend".to_string(),
        );

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // Two PM groups, no orphans
        assert_eq!(groups.len(), 2);

        assert_eq!(groups[0].pm.unwrap().session.name, "omar-agent-pm-api");
        assert_eq!(groups[0].workers.len(), 2);

        assert_eq!(groups[1].pm.unwrap().session.name, "omar-agent-pm-frontend");
        assert_eq!(groups[1].workers.len(), 1);
        assert_eq!(groups[1].workers[0].session.name, "omar-agent-ui");
    }

    #[test]
    fn test_groups_empty_agents() {
        let agents = vec![];
        let parents = HashMap::new();

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        assert!(groups.is_empty());
    }

    #[test]
    fn test_groups_empty_parents_with_pm() {
        // PM exists but no parent mappings at all — workers become orphans
        let agents = vec![
            make_agent("omar-agent-pm-api", HealthState::Running),
            make_agent("omar-agent-worker1", HealthState::Running),
        ];
        let parents = HashMap::new();

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // PM group (no workers) + orphan group (worker1)
        assert_eq!(groups.len(), 2);
        assert!(groups[0].pm.is_some());
        assert!(groups[0].workers.is_empty());
        assert!(groups[1].pm.is_none());
        assert_eq!(groups[1].workers.len(), 1);
    }

    // ── build_tree tests ──

    #[test]
    fn test_build_tree_ea_only() {
        let agents = vec![];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let parents = HashMap::new();
        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-");

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].name, "Executive Assistant");
        assert_eq!(tree[0].depth, 0);
    }

    #[test]
    fn test_build_tree_with_pm_and_workers() {
        let agents = vec![
            make_agent("omar-agent-pm-rest-api", HealthState::Running),
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
        ];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-api".to_string(),
            "omar-agent-pm-rest-api".to_string(),
        );
        parents.insert(
            "omar-agent-auth".to_string(),
            "omar-agent-pm-rest-api".to_string(),
        );

        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-");

        // EA + PM + 2 workers = 4 nodes
        assert_eq!(tree.len(), 4);
        assert_eq!(tree[0].name, "Executive Assistant");
        assert_eq!(tree[0].depth, 0);
        assert_eq!(tree[1].name, "Project Manager: rest-api");
        assert_eq!(tree[1].depth, 1);
        assert!(tree[1].is_last_sibling);
        assert_eq!(tree[2].name, "api");
        assert_eq!(tree[2].depth, 2);
        assert!(!tree[2].is_last_sibling);
        assert_eq!(tree[3].name, "auth");
        assert_eq!(tree[3].depth, 2);
        assert!(tree[3].is_last_sibling);
    }

    #[test]
    fn test_build_tree_orphan_workers() {
        let agents = vec![
            make_agent("omar-agent-debug", HealthState::Running),
            make_agent("omar-agent-test", HealthState::Idle),
        ];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let parents = HashMap::new();

        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-");

        // EA + Unassigned group + 2 orphans = 4 nodes
        assert_eq!(tree.len(), 4);
        assert_eq!(tree[1].name, "Unassigned");
        assert_eq!(tree[1].depth, 1);
        assert!(tree[1].is_last_sibling);
        assert_eq!(tree[2].name, "debug");
        assert_eq!(tree[2].depth, 2);
        assert!(!tree[2].is_last_sibling);
        assert_eq!(tree[3].name, "test");
        assert_eq!(tree[3].depth, 2);
        assert!(tree[3].is_last_sibling);
    }

    #[test]
    fn test_build_tree_mixed_pm_and_orphans() {
        let agents = vec![
            make_agent("omar-agent-pm-api", HealthState::Running),
            make_agent("omar-agent-worker1", HealthState::Running),
            make_agent("omar-agent-orphan1", HealthState::Idle),
        ];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-worker1".to_string(),
            "omar-agent-pm-api".to_string(),
        );

        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-");

        // EA + PM + worker1 + Unassigned + orphan1 = 5
        assert_eq!(tree.len(), 5);
        assert_eq!(tree[0].depth, 0); // EA
        assert_eq!(tree[1].name, "Project Manager: api");
        assert_eq!(tree[1].depth, 1);
        assert!(!tree[1].is_last_sibling); // not last because Unassigned follows
        assert_eq!(tree[2].name, "worker1");
        assert_eq!(tree[2].depth, 2);
        assert_eq!(tree[3].name, "Unassigned");
        assert_eq!(tree[3].depth, 1);
        assert!(tree[3].is_last_sibling);
        assert_eq!(tree[4].name, "orphan1");
        assert_eq!(tree[4].depth, 2);
        assert!(tree[4].is_last_sibling);
    }

    #[test]
    fn test_build_tree_stale_parent_treated_as_orphan() {
        let agents = vec![make_agent("omar-agent-worker1", HealthState::Running)];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        // Parent PM doesn't exist in agents
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-worker1".to_string(),
            "omar-agent-pm-gone".to_string(),
        );

        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-");

        // worker1 should be under Unassigned since its PM doesn't exist
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[1].name, "Unassigned");
        assert_eq!(tree[1].depth, 1);
        assert_eq!(tree[2].name, "worker1");
        assert_eq!(tree[2].depth, 2);
    }

    // ── focus navigation tests ──

    /// Helper to build a minimal App-like struct for focus testing.
    /// We can't construct a full App (needs TmuxClient), so we test
    /// compute_focus_child_indices, child_count, and breadcrumb via
    /// the underlying logic directly.

    #[test]
    fn test_focus_children_root_shows_pms_and_orphans() {
        // Simulates: EA has 1 PM with 2 workers, 1 orphan
        let agents = vec![
            make_agent("omar-agent-pm-api", HealthState::Running),
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
            make_agent("omar-agent-orphan", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-api".to_string(),
            "omar-agent-pm-api".to_string(),
        );
        parents.insert(
            "omar-agent-auth".to_string(),
            "omar-agent-pm-api".to_string(),
        );

        // Manually compute focus children at root (MANAGER_SESSION)
        let session_prefix = "omar-agent-";
        let _focus_parent = MANAGER_SESSION;
        let mut indices = Vec::new();
        for (i, agent) in agents.iter().enumerate() {
            let short = agent
                .session
                .name
                .strip_prefix(session_prefix)
                .unwrap_or(&agent.session.name);
            let is_pm = short.starts_with("pm-");
            if is_pm {
                indices.push(i);
            } else {
                let parent = parents.get(&agent.session.name);
                let has_live_pm_parent = parent
                    .map(|p| {
                        agents.iter().any(|a| {
                            a.session.name == *p
                                && a.session
                                    .name
                                    .strip_prefix(session_prefix)
                                    .unwrap_or(&a.session.name)
                                    .starts_with("pm-")
                        })
                    })
                    .unwrap_or(false);
                if !has_live_pm_parent {
                    indices.push(i);
                }
            }
        }

        // Root should show PM + orphan (not the PM's workers)
        assert_eq!(indices.len(), 2);
        assert_eq!(agents[indices[0]].session.name, "omar-agent-pm-api");
        assert_eq!(agents[indices[1]].session.name, "omar-agent-orphan");

        // Drill into PM: should show its workers
        let focus_parent = "omar-agent-pm-api";
        let mut pm_indices = Vec::new();
        for (i, agent) in agents.iter().enumerate() {
            if let Some(parent) = parents.get(&agent.session.name) {
                if *parent == focus_parent {
                    pm_indices.push(i);
                }
            }
        }
        assert_eq!(pm_indices.len(), 2);
        assert_eq!(agents[pm_indices[0]].session.name, "omar-agent-api");
        assert_eq!(agents[pm_indices[1]].session.name, "omar-agent-auth");

        let _ = focus_parent; // suppress unused warning
    }

    #[test]
    fn test_child_count() {
        let parents: HashMap<String, String> = [
            ("omar-agent-api", "omar-agent-pm-api"),
            ("omar-agent-auth", "omar-agent-pm-api"),
            ("omar-agent-ui", "omar-agent-pm-frontend"),
        ]
        .iter()
        .map(|(c, p)| (c.to_string(), p.to_string()))
        .collect();

        // PM-api has 2 children
        let count = parents
            .values()
            .filter(|p| *p == "omar-agent-pm-api")
            .count();
        assert_eq!(count, 2);

        // PM-frontend has 1 child
        let count = parents
            .values()
            .filter(|p| *p == "omar-agent-pm-frontend")
            .count();
        assert_eq!(count, 1);

        // Worker has 0 children
        let count = parents.values().filter(|p| *p == "omar-agent-api").count();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_breadcrumb_building() {
        let session_prefix = "omar-agent-";

        // At root: just ["EA"]
        let focus_stack: Vec<String> = vec![];
        let focus_parent = MANAGER_SESSION.to_string();
        let mut crumbs = vec!["EA".to_string()];
        for session in &focus_stack {
            if *session == MANAGER_SESSION {
                continue;
            }
            let short = session.strip_prefix(session_prefix).unwrap_or(session);
            if let Some(rest) = short.strip_prefix("pm-") {
                crumbs.push(format!("PM: {}", rest));
            } else {
                crumbs.push(short.to_string());
            }
        }
        if focus_parent != MANAGER_SESSION {
            let short = focus_parent
                .strip_prefix(session_prefix)
                .unwrap_or(&focus_parent);
            if let Some(rest) = short.strip_prefix("pm-") {
                crumbs.push(format!("PM: {}", rest));
            } else {
                crumbs.push(short.to_string());
            }
        }
        assert_eq!(crumbs, vec!["EA"]);

        // Drilled into PM: ["EA", "PM: rest-api"]
        let focus_stack = vec![MANAGER_SESSION.to_string()];
        let focus_parent = "omar-agent-pm-rest-api".to_string();
        let mut crumbs = vec!["EA".to_string()];
        for session in &focus_stack {
            if *session == MANAGER_SESSION {
                continue;
            }
            let short = session.strip_prefix(session_prefix).unwrap_or(session);
            if let Some(rest) = short.strip_prefix("pm-") {
                crumbs.push(format!("PM: {}", rest));
            } else {
                crumbs.push(short.to_string());
            }
        }
        if focus_parent != MANAGER_SESSION {
            let short = focus_parent
                .strip_prefix(session_prefix)
                .unwrap_or(&focus_parent);
            if let Some(rest) = short.strip_prefix("pm-") {
                crumbs.push(format!("PM: {}", rest));
            } else {
                crumbs.push(short.to_string());
            }
        }
        assert_eq!(crumbs, vec!["EA", "PM: rest-api"]);
    }
}
