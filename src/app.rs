#![allow(dead_code)]

use anyhow::Result;
use std::collections::HashMap;

use crate::config::Config;
use crate::manager::EaContext;
use crate::memory;

/// Resolve the current working directory as a String, falling back to ".".
pub fn current_workdir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string())
}
use crate::projects::{self, Project};
use crate::scheduler::{ScheduledEvent, TickerBuffer};
use crate::tmux::{HealthChecker, HealthInfo, HealthState, Session, TmuxClient};
use crate::DASHBOARD_SESSION;

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
    pub command_tree: Vec<CommandTreeNode>,
    pub selected: usize,
    pub manager_selected: bool,
    pub should_quit: bool,
    pub show_help: bool,
    pub show_confirm_kill: bool,
    pub show_confirm_kill_ea: bool,
    pub filter: String,
    pub status_message: Option<String>,
    pub projects: Vec<Project>,
    pub project_input_mode: bool,
    pub project_input: String,
    pub ea_input_mode: bool,
    pub ea_input: String,
    pub show_events: bool,
    pub scheduled_events: Vec<ScheduledEvent>,
    pub ticker: TickerBuffer,
    pub ticker_offset: usize,
    pub show_debug_console: bool,
    /// Session name of the agent shown in the bottom panel
    pub focus_parent: String,
    /// Stack for Esc navigation (drill-up restores previous parent)
    focus_stack: Vec<String>,
    /// Indices into self.agents for the current focus_parent's direct children
    pub focus_child_indices: Vec<usize>,
    agent_parents: HashMap<String, String>,
    worker_tasks: HashMap<String, String>,
    /// All EA contexts
    pub eas: Vec<EaContext>,
    /// Index of the active (focused) EA
    pub active_ea: usize,
    /// Manager sessions for each EA (parallel to `eas`)
    pub managers: Vec<Option<AgentInfo>>,
    /// Cached summaries for non-active EAs: (agent_count, running_count)
    ea_summaries: Vec<(usize, usize)>,
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

        // Load persisted EA list, falling back to the default EA
        let ea_ids = memory::load_ea_ids();
        let eas: Vec<EaContext> = if ea_ids.is_empty() {
            vec![EaContext::default()]
        } else {
            ea_ids.iter().map(|id| EaContext::new(id)).collect()
        };
        let managers = vec![None; eas.len()];
        let ea_summaries = vec![(0, 0); eas.len()];
        let active_ea = 0;

        Self {
            agents: Vec::new(),
            command_tree: Vec::new(),
            selected: 0,
            manager_selected: true,
            should_quit: false,
            show_help: false,
            show_confirm_kill: false,
            show_confirm_kill_ea: false,
            filter: String::new(),
            status_message: None,
            projects: projects::load_projects(&eas[active_ea].state_dir),
            project_input_mode: false,
            project_input: String::new(),
            ea_input_mode: false,
            ea_input: String::new(),
            show_events: false,
            scheduled_events: Vec::new(),
            ticker,
            ticker_offset: 0,
            show_debug_console: false,
            focus_parent: eas[active_ea].session_name.clone(),
            focus_stack: Vec::new(),
            focus_child_indices: Vec::new(),
            agent_parents: HashMap::new(),
            worker_tasks: HashMap::new(),
            eas,
            active_ea,
            managers,
            ea_summaries,
            client,
            health_checker,
            default_command: config.agent.default_command.clone(),
            default_workdir: config.agent.default_workdir.clone(),
            session_prefix: config.dashboard.session_prefix.clone(),
        }
    }

    /// Active EA context (shorthand)
    pub fn ea(&self) -> &EaContext {
        &self.eas[self.active_ea]
    }

    /// True when any popup or input overlay is active.
    pub fn has_popup(&self) -> bool {
        self.show_help
            || self.show_confirm_kill
            || self.show_confirm_kill_ea
            || self.project_input_mode
            || self.ea_input_mode
            || self.show_events
            || self.show_debug_console
    }

    pub fn client(&self) -> &TmuxClient {
        &self.client
    }

    /// Refresh the list of agents
    pub fn refresh(&mut self) -> Result<()> {
        // Get all sessions
        let sessions = self.client.list_all_sessions()?;

        // Separate managers from other agents, filtering out non-omar sessions
        let mut manager_map: HashMap<String, Session> = HashMap::new();
        let mut other_sessions = Vec::new();

        for session in sessions {
            if self.eas.iter().any(|e| e.session_name == session.name) {
                manager_map.insert(session.name.clone(), session);
            } else if session.name == DASHBOARD_SESSION
                || (!self.session_prefix.is_empty()
                    && !session.name.starts_with(&self.session_prefix))
            {
                continue;
            } else {
                other_sessions.push(session);
            }
        }

        // Start any EA managers not yet running (using the session list we already have)
        self.ensure_managers(&manager_map)?;

        // Update managers for all EAs
        self.managers = self
            .eas
            .iter()
            .map(|ea| {
                manager_map.remove(&ea.session_name).map(|session| {
                    let health_info = self.health_checker.check_detailed(&session.name);
                    let health = health_info.state;
                    AgentInfo {
                        session,
                        health,
                        health_info,
                    }
                })
            })
            .collect();

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
            .chain(self.manager().iter().map(|m| m.session.name.clone()))
            .collect();
        self.health_checker.retain_sessions(&active);

        // Apply filter if set
        if !self.filter.is_empty() {
            let filter = self.filter.to_lowercase();
            self.agents
                .retain(|a| a.session.name.to_lowercase().contains(&filter));
        }

        // Reload active EA's projects, parent mappings, and worker tasks
        self.reload_ea_state();

        // Pre-compute per-EA summaries for non-active EAs (avoids disk I/O in tree builder)
        self.ea_summaries = self
            .eas
            .iter()
            .enumerate()
            .map(|(i, ea)| {
                if i == self.active_ea {
                    (0, 0) // Not used for active EA
                } else {
                    let parents = memory::load_agent_parents(&ea.state_dir);
                    let count = parents.len();
                    let running = self
                        .agents
                        .iter()
                        .filter(|a| parents.contains_key(&a.session.name))
                        .filter(|a| a.health == HealthState::Running)
                        .count();
                    (count, running)
                }
            })
            .collect();

        self.command_tree = build_multi_tree(
            &self.agents,
            &self.managers,
            &self.agent_parents,
            &self.session_prefix,
            &self.eas,
            self.active_ea,
            &self.ea_summaries,
        );

        // Keep selection in bounds relative to focus children
        if !self.manager_selected
            && !self.focus_child_indices.is_empty()
            && self.selected >= self.focus_child_indices.len()
        {
            self.selected = self.focus_child_indices.len() - 1;
        }

        Ok(())
    }

    /// Start manager sessions for EAs that aren't already running.
    /// Uses the pre-fetched session map to avoid extra tmux subprocess calls.
    fn ensure_managers(&self, existing: &HashMap<String, Session>) -> Result<()> {
        let workdir = current_workdir();

        for ea in &self.eas {
            if existing.contains_key(&ea.session_name) {
                continue;
            }

            // Build command with EA system prompt + memory baked in
            let cmd = crate::manager::build_ea_command(&self.default_command, ea);

            self.client
                .new_session(&ea.session_name, &cmd, Some(&workdir))?;

            // Write memory after creating manager
            memory::write_memory(
                &ea.state_dir,
                &ea.session_name,
                &self.agents,
                None,
                &self.client,
            );
        }

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

    /// Get active manager info (for API)
    pub fn manager(&self) -> Option<&AgentInfo> {
        self.managers[self.active_ea].as_ref()
    }

    /// Write memory state for the active EA
    fn flush_memory(&self) {
        memory::write_memory(
            &self.ea().state_dir,
            &self.ea().session_name,
            &self.agents,
            self.manager(),
            &self.client,
        );
    }

    /// Reset focus navigation to the active EA root
    fn reset_focus(&mut self) {
        self.focus_parent = self.ea().session_name.clone();
        self.focus_stack.clear();
        self.manager_selected = true;
        self.selected = 0;
    }

    /// Reload projects, agent parents, worker tasks, and focus indices for the active EA
    fn reload_ea_state(&mut self) {
        self.projects = projects::load_projects(&self.ea().state_dir);
        self.agent_parents = memory::load_agent_parents(&self.ea().state_dir);
        self.worker_tasks = memory::load_worker_tasks(&self.ea().state_dir);
        self.focus_child_indices = self.compute_focus_child_indices();
    }

    /// Persist the current EA ID list to disk
    fn persist_ea_ids(&self) {
        let ids: Vec<String> = self.eas.iter().map(|e| e.id.clone()).collect();
        memory::save_ea_ids(&ids);
    }

    /// Add a new EA, persist, and ensure its manager starts
    pub fn add_ea(&mut self, id: &str) -> Result<()> {
        // Sanitize: only allow alphanumeric, dash, underscore
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            anyhow::bail!("EA ID must be alphanumeric (dashes/underscores allowed)");
        }

        // Check for duplicate
        if self.eas.iter().any(|e| e.id == id) {
            anyhow::bail!("EA '{}' already exists", id);
        }

        // Transitioning from single to multi-EA: adopt untracked agents
        // into the default EA so they don't vanish from the dashboard.
        if self.eas.len() == 1 {
            let default_ea = &self.eas[0];
            let mut parents = memory::load_agent_parents(&default_ea.state_dir);
            let mut adopted = false;
            for agent in &self.agents {
                if !parents.contains_key(&agent.session.name) {
                    parents.insert(agent.session.name.clone(), default_ea.session_name.clone());
                    adopted = true;
                }
            }
            if adopted {
                memory::write_agent_parents(&default_ea.state_dir, &parents);
            }
        }

        let ea = EaContext::new(id);
        std::fs::create_dir_all(&ea.state_dir).ok();
        self.eas.push(ea);
        self.managers.push(None);
        self.ea_summaries.push((0, 0));
        self.persist_ea_ids();

        // Switch to the new EA
        self.active_ea = self.eas.len() - 1;
        self.reset_focus();
        self.reload_ea_state();

        Ok(())
    }

    /// Remove an EA and kill all its sessions
    pub fn remove_ea(&mut self, index: usize) -> Result<()> {
        if self.eas.len() <= 1 {
            anyhow::bail!("Cannot remove the last EA");
        }
        if index >= self.eas.len() {
            anyhow::bail!("EA index out of range");
        }

        let ea = &self.eas[index];

        // Kill the manager session
        let _ = self.client.kill_session(&ea.session_name);

        // Kill all agents that belong to this EA (workers/PMs)
        let parents = memory::load_agent_parents(&ea.state_dir);
        for child in parents.keys() {
            let _ = self.client.kill_session(child);
        }

        self.eas.remove(index);
        self.managers.remove(index);
        self.ea_summaries.remove(index);
        self.persist_ea_ids();

        // Fix active_ea
        if self.active_ea >= self.eas.len() {
            self.active_ea = self.eas.len() - 1;
        }
        self.reset_focus();

        Ok(())
    }

    /// Switch active EA by index
    pub fn switch_ea(&mut self, index: usize) {
        if index < self.eas.len() && index != self.active_ea {
            self.active_ea = index;
            self.reset_focus();
            self.reload_ea_state();
        }
    }

    /// Cycle to next EA
    pub fn next_ea(&mut self) {
        let next = (self.active_ea + 1) % self.eas.len();
        self.switch_ea(next);
    }

    /// Cycle to previous EA
    pub fn prev_ea(&mut self) {
        let prev = if self.active_ea == 0 {
            self.eas.len() - 1
        } else {
            self.active_ea - 1
        };
        self.switch_ea(prev);
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
        if self.focus_parent == self.ea().session_name {
            // Root view: show agents that are direct children of EA
            for (i, agent) in self.agents.iter().enumerate() {
                if let Some(p) = self.agent_parents.get(&agent.session.name) {
                    if *p == self.ea().session_name {
                        // Explicit child of EA
                        indices.push(i);
                    } else if !self.agents.iter().any(|a| a.session.name == *p) {
                        // Parent is dead → show as orphan at root
                        indices.push(i);
                    }
                    // else: has a live parent deeper in the tree
                } else if self.eas.len() <= 1 {
                    // Single EA: untracked agents show as orphans (backward compat)
                    indices.push(i);
                }
                // Multi-EA: agents not in agent_parents belong to a different EA; skip.
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
        if self.focus_parent == self.ea().session_name {
            self.manager()
        } else {
            self.agents
                .iter()
                .find(|a| a.session.name == self.focus_parent)
        }
    }

    /// Check if an agent has children (is a PM or EA)
    fn agent_has_children(&self, session_name: &str) -> bool {
        if session_name == self.ea().session_name {
            return true;
        }
        self.agent_parents.values().any(|p| p == session_name)
    }

    /// Count children for a given agent
    pub fn child_count(&self, session_name: &str) -> usize {
        if session_name == self.ea().session_name {
            return self.focus_child_indices.len();
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
            if *session == self.ea().session_name {
                continue; // Already added EA
            }
            let short = session
                .strip_prefix(&self.session_prefix)
                .unwrap_or(session);
            crumbs.push(short.to_string());
        }
        // Add current focus parent if not EA
        if self.focus_parent != self.ea().session_name {
            let short = self
                .focus_parent
                .strip_prefix(&self.session_prefix)
                .unwrap_or(&self.focus_parent);
            crumbs.push(short.to_string());
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

    /// Get the short name (receiver name) of the selected agent.
    pub fn selected_agent_short_name(&self) -> Option<String> {
        self.selected_agent().map(|a| {
            a.session
                .name
                .strip_prefix(self.client.prefix())
                .unwrap_or(&a.session.name)
                .to_string()
        })
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
            if agent.session.name == self.ea().session_name {
                self.status_message = Some("Cannot kill manager with 'd'".to_string());
                self.show_confirm_kill = false;
                return Ok(());
            }

            let name = agent.session.name.clone();
            self.client.kill_session(&name)?;
            memory::remove_agent_parent(&self.ea().state_dir, &name);
            self.status_message = Some(format!("Killed agent: {}", name));
            self.refresh()?;
            self.flush_memory();
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
            current_workdir()
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

        self.flush_memory();

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

        if let Some(manager) = self.manager() {
            match manager.health {
                HealthState::Running => running += 1,
                HealthState::Idle => idle += 1,
            }
        }

        (running, idle)
    }

    /// Get total agent count (including manager)
    pub fn total_agents(&self) -> usize {
        self.agents.len() + if self.manager().is_some() { 1 } else { 0 }
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
        let _ = projects::add_project(&self.ea().state_dir, name);
        self.projects = projects::load_projects(&self.ea().state_dir);
        self.flush_memory();
    }

    /// Complete (remove) a project by id and update memory
    pub fn complete_project(&mut self, id: usize) {
        let _ = projects::remove_project(&self.ea().state_dir, id);
        self.projects = projects::load_projects(&self.ea().state_dir);
        self.flush_memory();
    }
}

/// Group agents into parent → children hierarchies for grid display.
///
/// Parents are agents that have children via the agent_parents map.
/// Agents without a live parent go into an orphan group (pm: None).
pub fn build_agent_groups<'a>(
    agents: &'a [AgentInfo],
    agent_parents: &HashMap<String, String>,
    _session_prefix: &str,
) -> Vec<AgentGroup<'a>> {
    // Find agents that are parents (have children pointing to them)
    let mut parent_agents: Vec<&AgentInfo> = Vec::new();
    let mut leaf_agents: Vec<&AgentInfo> = Vec::new();

    for agent in agents {
        let has_children = agent_parents
            .values()
            .any(|parent| *parent == agent.session.name);
        if has_children {
            parent_agents.push(agent);
        } else {
            leaf_agents.push(agent);
        }
    }

    let mut parent_children: HashMap<String, Vec<&AgentInfo>> = HashMap::new();
    let mut orphans: Vec<&AgentInfo> = Vec::new();

    for agent in leaf_agents {
        if let Some(parent_session) = agent_parents.get(&agent.session.name) {
            if parent_agents
                .iter()
                .any(|p| p.session.name == *parent_session)
            {
                parent_children
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

    for parent in &parent_agents {
        let workers = parent_children
            .remove(&parent.session.name)
            .unwrap_or_default();
        groups.push(AgentGroup {
            pm: Some(parent),
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

/// Build a multi-root command tree — one subtree per EA.
///
/// The active EA is fully expanded. Non-active EAs show a single collapsed
/// root node with a summary (agent count, running count).
pub fn build_multi_tree(
    agents: &[AgentInfo],
    managers: &[Option<AgentInfo>],
    agent_parents: &HashMap<String, String>,
    session_prefix: &str,
    eas: &[EaContext],
    active_ea: usize,
    ea_summaries: &[(usize, usize)],
) -> Vec<CommandTreeNode> {
    let mut nodes = Vec::new();
    for (i, ea) in eas.iter().enumerate() {
        let manager = managers.get(i).and_then(|m| m.as_ref());
        let is_last_root = i == eas.len() - 1;

        if i == active_ea {
            // Active EA: full subtree
            let subtree = build_tree(
                agents,
                manager,
                agent_parents,
                session_prefix,
                ea,
                eas.len() > 1,
            );
            for (j, mut node) in subtree.into_iter().enumerate() {
                if j == 0 && eas.len() > 1 {
                    node.is_last_sibling = is_last_root;
                }
                nodes.push(node);
            }
        } else {
            // Non-active EA: collapsed root with pre-computed summary
            let ea_health = manager.map(|m| m.health).unwrap_or(HealthState::Idle);
            let (child_count, running) = ea_summaries.get(i).copied().unwrap_or((0, 0));

            let summary = if child_count == 0 {
                format!("{} (no agents)", ea.display_name)
            } else {
                format!(
                    "{} ({} agent{}, {} running)",
                    ea.display_name,
                    child_count,
                    if child_count == 1 { "" } else { "s" },
                    running
                )
            };

            nodes.push(CommandTreeNode {
                name: summary,
                session_name: ea.session_name.clone(),
                health: ea_health,
                depth: 0,
                is_last_sibling: is_last_root,
                ancestor_is_last: vec![],
            });
        }
    }
    nodes
}

/// Build the chain-of-command tree from current agents and parent mappings.
///
/// Tree structure (recursive, arbitrary depth):
///   EA (root, depth 0)
///   ├── Agent with children (depth 1)
///   │   └── Sub-agent (depth 2)
///   │       └── Sub-sub-agent (depth 3) ...
///   └── Orphan agents with no parent (depth 1, under EA)
pub fn build_tree(
    agents: &[AgentInfo],
    manager: Option<&AgentInfo>,
    agent_parents: &HashMap<String, String>,
    session_prefix: &str,
    ea: &EaContext,
    multi_ea: bool,
) -> Vec<CommandTreeNode> {
    let mut nodes = Vec::new();

    // Root: EA
    let ea_health = manager.map(|m| m.health).unwrap_or(HealthState::Idle);
    nodes.push(CommandTreeNode {
        name: ea.display_name.clone(),
        session_name: ea.session_name.clone(),
        health: ea_health,
        depth: 0,
        is_last_sibling: true,
        ancestor_is_last: vec![],
    });

    // Build a children map: parent_session -> vec of child agents
    let mut children_map: HashMap<String, Vec<&AgentInfo>> = HashMap::new();
    let mut orphans: Vec<&AgentInfo> = Vec::new();

    for agent in agents {
        if let Some(parent_session) = agent_parents.get(&agent.session.name) {
            // Check that the parent actually exists (either as agent or EA)
            let parent_exists = *parent_session == ea.session_name
                || agents.iter().any(|a| a.session.name == *parent_session);
            if parent_exists {
                children_map
                    .entry(parent_session.clone())
                    .or_default()
                    .push(agent);
            } else {
                // Parent listed but no longer running — show as orphan under this EA
                orphans.push(agent);
            }
        } else if !multi_ea {
            // Single EA: untracked agents show as orphans (backward compat)
            orphans.push(agent);
        }
        // Multi-EA: agents not in agent_parents belong to a different EA; skip.
    }

    // Recursively add nodes starting from EA's children
    let ea_children = children_map.get(&ea.session_name as &str);
    let total_root_children = ea_children.map(|c| c.len()).unwrap_or(0) + orphans.len();

    #[allow(clippy::too_many_arguments)]
    fn add_children(
        nodes: &mut Vec<CommandTreeNode>,
        children_map: &HashMap<String, Vec<&AgentInfo>>,
        parent_session: &str,
        depth: usize,
        ancestor_is_last: &[bool],
        session_prefix: &str,
        total_siblings: usize,
        start_idx: usize,
    ) {
        if let Some(children) = children_map.get(parent_session) {
            for (idx, child) in children.iter().enumerate() {
                let sibling_idx = start_idx + idx;
                let is_last = sibling_idx == total_siblings - 1;
                let short = child
                    .session
                    .name
                    .strip_prefix(session_prefix)
                    .unwrap_or(&child.session.name);

                nodes.push(CommandTreeNode {
                    name: short.to_string(),
                    session_name: child.session.name.clone(),
                    health: child.health,
                    depth,
                    is_last_sibling: is_last,
                    ancestor_is_last: ancestor_is_last.to_vec(),
                });

                // Recurse into this child's children
                let grandchildren_count = children_map
                    .get(&child.session.name)
                    .map(|c| c.len())
                    .unwrap_or(0);
                if grandchildren_count > 0 {
                    let mut next_ancestors = ancestor_is_last.to_vec();
                    next_ancestors.push(is_last);
                    add_children(
                        nodes,
                        children_map,
                        &child.session.name,
                        depth + 1,
                        &next_ancestors,
                        session_prefix,
                        grandchildren_count,
                        0,
                    );
                }
            }
        }
    }

    // Add EA's direct children
    let ea_direct_count = ea_children.map(|c| c.len()).unwrap_or(0);
    add_children(
        &mut nodes,
        &children_map,
        &ea.session_name,
        1,
        &[true], // EA is always last at depth 0
        session_prefix,
        total_root_children,
        0,
    );

    // Add orphan agents (no parent or dead parent) under EA
    if !orphans.is_empty() {
        for (orphan_idx, orphan) in orphans.iter().enumerate() {
            let short = orphan
                .session
                .name
                .strip_prefix(session_prefix)
                .unwrap_or(&orphan.session.name);
            let sibling_idx = ea_direct_count + orphan_idx;
            nodes.push(CommandTreeNode {
                name: short.to_string(),
                session_name: orphan.session.name.clone(),
                health: orphan.health,
                depth: 1,
                is_last_sibling: sibling_idx == total_root_children - 1,
                ancestor_is_last: vec![true],
            });

            // Orphans can also have children
            let child_count = children_map
                .get(&orphan.session.name)
                .map(|c| c.len())
                .unwrap_or(0);
            if child_count > 0 {
                add_children(
                    &mut nodes,
                    &children_map,
                    &orphan.session.name,
                    2,
                    &[true, sibling_idx == total_root_children - 1],
                    session_prefix,
                    child_count,
                    0,
                );
            }
        }
    }

    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::MANAGER_SESSION;
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
    fn test_groups_parent_with_children() {
        let agents = vec![
            make_agent("omar-agent-rest-api", HealthState::Running),
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-api".to_string(),
            "omar-agent-rest-api".to_string(),
        );
        parents.insert(
            "omar-agent-auth".to_string(),
            "omar-agent-rest-api".to_string(),
        );

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // One parent group, no orphans
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pm.unwrap().session.name, "omar-agent-rest-api");
        assert_eq!(groups[0].workers.len(), 2);
    }

    #[test]
    fn test_groups_all_orphans() {
        let agents = vec![
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
        ];
        let parents = HashMap::new();

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // One orphan group
        assert_eq!(groups.len(), 1);
        assert!(groups[0].pm.is_none());
        assert_eq!(groups[0].workers.len(), 2);
    }

    #[test]
    fn test_groups_parent_without_children() {
        // An agent with no children and no parent → orphan (not a "parent" group)
        let agents = vec![make_agent("omar-agent-rest-api", HealthState::Running)];
        let parents = HashMap::new();

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // No children means it's not detected as a parent → orphan
        assert_eq!(groups.len(), 1);
        assert!(groups[0].pm.is_none());
        assert_eq!(groups[0].workers.len(), 1);
    }

    #[test]
    fn test_groups_mixed_parent_and_orphans() {
        let agents = vec![
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-worker1", HealthState::Running),
            make_agent("omar-agent-orphan1", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-worker1".to_string(),
            "omar-agent-api".to_string(),
        );

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        // Parent group + orphan group
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].pm.unwrap().session.name, "omar-agent-api");
        assert_eq!(groups[0].workers.len(), 1);
        assert!(groups[1].pm.is_none());
        assert_eq!(groups[1].workers.len(), 1);
    }

    #[test]
    fn test_groups_stale_parent_becomes_orphan() {
        let agents = vec![make_agent("omar-agent-worker1", HealthState::Running)];
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-worker1".to_string(),
            "omar-agent-gone".to_string(),
        );

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        assert_eq!(groups.len(), 1);
        assert!(groups[0].pm.is_none());
        assert_eq!(groups[0].workers.len(), 1);
    }

    #[test]
    fn test_groups_two_parents_each_with_children() {
        let agents = vec![
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-frontend", HealthState::Running),
            make_agent("omar-agent-api-worker", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Running),
            make_agent("omar-agent-ui", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-api-worker".to_string(),
            "omar-agent-api".to_string(),
        );
        parents.insert("omar-agent-auth".to_string(), "omar-agent-api".to_string());
        parents.insert(
            "omar-agent-ui".to_string(),
            "omar-agent-frontend".to_string(),
        );

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].pm.unwrap().session.name, "omar-agent-api");
        assert_eq!(groups[0].workers.len(), 2);
        assert_eq!(groups[1].pm.unwrap().session.name, "omar-agent-frontend");
        assert_eq!(groups[1].workers.len(), 1);
    }

    #[test]
    fn test_groups_empty_agents() {
        let agents = vec![];
        let parents = HashMap::new();

        let groups = build_agent_groups(&agents, &parents, "omar-agent-");

        assert!(groups.is_empty());
    }

    // ── build_tree tests ──

    #[test]
    fn test_build_tree_ea_only() {
        let agents = vec![];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let parents = HashMap::new();
        let tree = build_tree(
            &agents,
            Some(&ea),
            &parents,
            "omar-agent-",
            &EaContext::default(),
            false,
        );

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].name, "Executive Assistant");
        assert_eq!(tree[0].depth, 0);
    }

    #[test]
    fn test_build_tree_with_parent_and_children() {
        let agents = vec![
            make_agent("omar-agent-rest-api", HealthState::Running),
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
        ];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-rest-api".to_string(),
            MANAGER_SESSION.to_string(),
        );
        parents.insert(
            "omar-agent-api".to_string(),
            "omar-agent-rest-api".to_string(),
        );
        parents.insert(
            "omar-agent-auth".to_string(),
            "omar-agent-rest-api".to_string(),
        );

        let tree = build_tree(
            &agents,
            Some(&ea),
            &parents,
            "omar-agent-",
            &EaContext::default(),
            false,
        );

        // EA + rest-api + 2 children = 4 nodes
        assert_eq!(tree.len(), 4);
        assert_eq!(tree[0].name, "Executive Assistant");
        assert_eq!(tree[0].depth, 0);
        assert_eq!(tree[1].name, "rest-api");
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
    fn test_build_tree_orphan_agents() {
        // Agents tracked by this EA (in agent_parents) but whose parent
        // session doesn't exist should show as orphans under the EA.
        let agents = vec![
            make_agent("omar-agent-debug", HealthState::Running),
            make_agent("omar-agent-test", HealthState::Idle),
        ];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let mut parents = HashMap::new();
        // Both agents point to a dead parent → orphan under EA
        parents.insert(
            "omar-agent-debug".to_string(),
            "omar-agent-dead-pm".to_string(),
        );
        parents.insert(
            "omar-agent-test".to_string(),
            "omar-agent-dead-pm".to_string(),
        );

        let tree = build_tree(
            &agents,
            Some(&ea),
            &parents,
            "omar-agent-",
            &EaContext::default(),
            false,
        );

        // EA + 2 orphans directly under EA = 3 nodes
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[1].name, "debug");
        assert_eq!(tree[1].depth, 1);
        assert!(!tree[1].is_last_sibling);
        assert_eq!(tree[2].name, "test");
        assert_eq!(tree[2].depth, 1);
        assert!(tree[2].is_last_sibling);
    }

    #[test]
    fn test_build_tree_deep_hierarchy() {
        // EA -> agent-a -> agent-b -> agent-c (3 levels deep)
        let agents = vec![
            make_agent("omar-agent-a", HealthState::Running),
            make_agent("omar-agent-b", HealthState::Running),
            make_agent("omar-agent-c", HealthState::Idle),
        ];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let mut parents = HashMap::new();
        parents.insert("omar-agent-a".to_string(), MANAGER_SESSION.to_string());
        parents.insert("omar-agent-b".to_string(), "omar-agent-a".to_string());
        parents.insert("omar-agent-c".to_string(), "omar-agent-b".to_string());

        let tree = build_tree(
            &agents,
            Some(&ea),
            &parents,
            "omar-agent-",
            &EaContext::default(),
            false,
        );

        assert_eq!(tree.len(), 4);
        assert_eq!(tree[0].name, "Executive Assistant");
        assert_eq!(tree[0].depth, 0);
        assert_eq!(tree[1].name, "a");
        assert_eq!(tree[1].depth, 1);
        assert_eq!(tree[2].name, "b");
        assert_eq!(tree[2].depth, 2);
        assert_eq!(tree[3].name, "c");
        assert_eq!(tree[3].depth, 3);
    }

    #[test]
    fn test_build_tree_stale_parent_treated_as_orphan() {
        let agents = vec![make_agent("omar-agent-worker1", HealthState::Running)];
        let ea = make_agent("omar-agent-ea", HealthState::Running);
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-worker1".to_string(),
            "omar-agent-gone".to_string(),
        );

        let tree = build_tree(
            &agents,
            Some(&ea),
            &parents,
            "omar-agent-",
            &EaContext::default(),
            false,
        );

        // worker1 should be orphan under EA since parent doesn't exist
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[1].name, "worker1");
        assert_eq!(tree[1].depth, 1);
    }

    // ── focus navigation tests ──

    #[test]
    fn test_focus_children_root_shows_direct_ea_children_and_orphans() {
        // EA -> api (with children: api-worker, auth), orphan (dead parent)
        let agents = [
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-api-worker", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
            make_agent("omar-agent-orphan", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert("omar-agent-api".to_string(), MANAGER_SESSION.to_string());
        parents.insert(
            "omar-agent-api-worker".to_string(),
            "omar-agent-api".to_string(),
        );
        parents.insert("omar-agent-auth".to_string(), "omar-agent-api".to_string());
        // Orphan's parent is dead — tracked by this EA but parent session gone
        parents.insert(
            "omar-agent-orphan".to_string(),
            "omar-agent-dead-pm".to_string(),
        );

        // Compute focus children at root (only agents in agent_parents)
        let mut indices = Vec::new();
        for (i, agent) in agents.iter().enumerate() {
            if let Some(p) = parents.get(&agent.session.name) {
                if *p == MANAGER_SESSION || !agents.iter().any(|a| a.session.name == *p) {
                    indices.push(i);
                }
            }
        }

        // Root should show: api (EA child) + orphan (dead parent)
        assert_eq!(indices.len(), 2);
        assert_eq!(agents[indices[0]].session.name, "omar-agent-api");
        assert_eq!(agents[indices[1]].session.name, "omar-agent-orphan");

        // Drill into api: should show its children
        let focus_parent = "omar-agent-api";
        let mut child_indices = Vec::new();
        for (i, agent) in agents.iter().enumerate() {
            if let Some(parent) = parents.get(&agent.session.name) {
                if *parent == focus_parent {
                    child_indices.push(i);
                }
            }
        }
        assert_eq!(child_indices.len(), 2);
        assert_eq!(
            agents[child_indices[0]].session.name,
            "omar-agent-api-worker"
        );
        assert_eq!(agents[child_indices[1]].session.name, "omar-agent-auth");
    }

    #[test]
    fn test_child_count() {
        let parents: HashMap<String, String> = [
            ("omar-agent-api", "omar-agent-rest-api"),
            ("omar-agent-auth", "omar-agent-rest-api"),
            ("omar-agent-ui", "omar-agent-frontend"),
        ]
        .iter()
        .map(|(c, p)| (c.to_string(), p.to_string()))
        .collect();

        let count = parents
            .values()
            .filter(|p| *p == "omar-agent-rest-api")
            .count();
        assert_eq!(count, 2);

        let count = parents
            .values()
            .filter(|p| *p == "omar-agent-frontend")
            .count();
        assert_eq!(count, 1);

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
            crumbs.push(short.to_string());
        }
        if focus_parent != MANAGER_SESSION {
            let short = focus_parent
                .strip_prefix(session_prefix)
                .unwrap_or(&focus_parent);
            crumbs.push(short.to_string());
        }
        assert_eq!(crumbs, vec!["EA"]);

        // Drilled into agent: ["EA", "rest-api"]
        let focus_stack = vec![MANAGER_SESSION.to_string()];
        let focus_parent = "omar-agent-rest-api".to_string();
        let mut crumbs = vec!["EA".to_string()];
        for session in &focus_stack {
            if *session == MANAGER_SESSION {
                continue;
            }
            let short = session.strip_prefix(session_prefix).unwrap_or(session);
            crumbs.push(short.to_string());
        }
        if focus_parent != MANAGER_SESSION {
            let short = focus_parent
                .strip_prefix(session_prefix)
                .unwrap_or(&focus_parent);
            crumbs.push(short.to_string());
        }
        assert_eq!(crumbs, vec!["EA", "rest-api"]);
    }

    #[test]
    fn test_build_multi_tree_active_expanded_others_collapsed() {
        let agents = vec![
            make_agent("omar-agent-pm-rest-api", HealthState::Running),
            make_agent("omar-agent-worker-1", HealthState::Idle),
        ];
        let ea_default = EaContext::default();
        let ea_backend = EaContext::new("backend");
        let eas = vec![ea_default.clone(), ea_backend.clone()];

        let managers = vec![
            Some(make_agent(MANAGER_SESSION, HealthState::Running)),
            Some(make_agent(&ea_backend.session_name, HealthState::Running)),
        ];

        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-pm-rest-api".to_string(),
            MANAGER_SESSION.to_string(),
        );
        parents.insert(
            "omar-agent-worker-1".to_string(),
            "omar-agent-pm-rest-api".to_string(),
        );

        // Pre-computed summaries: active EA (unused), backend EA (0 agents)
        let ea_summaries = vec![(0, 0), (0, 0)];

        // Active EA = 0 (default): first EA expanded, second collapsed
        let tree = build_multi_tree(
            &agents,
            &managers,
            &parents,
            "omar-agent-",
            &eas,
            0,
            &ea_summaries,
        );

        // First node: active EA root (expanded)
        assert_eq!(tree[0].depth, 0);
        assert_eq!(tree[0].session_name, MANAGER_SESSION);
        assert!(!tree[0].is_last_sibling); // not last — there's a second EA

        // There should be child nodes (PM + worker) for the active EA
        let active_children: Vec<_> = tree.iter().skip(1).take_while(|n| n.depth > 0).collect();
        assert!(
            !active_children.is_empty(),
            "active EA should have expanded children"
        );

        // Last node: collapsed EA root
        let last = tree.last().unwrap();
        assert_eq!(last.depth, 0);
        assert_eq!(last.session_name, ea_backend.session_name);
        assert!(last.is_last_sibling);
        // Collapsed EA name includes summary
        assert!(
            last.name.contains("EA: backend"),
            "collapsed name should contain EA display name: {}",
            last.name
        );
    }
}
