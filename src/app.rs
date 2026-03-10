#![allow(dead_code)]

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::Config;
use crate::ea::{self, EaId, EaInfo};
use crate::memory;
use crate::projects::{self, Project};
use crate::scheduler::{ScheduledEvent, TickerBuffer};
use crate::settings::DashboardSettings;
use crate::tmux::{HealthChecker, HealthInfo, HealthState, Session, TmuxClient};
use crate::DASHBOARD_SESSION;

/// Shared app state for API access
pub type SharedApp = App;

/// What kind of confirmation the user is being prompted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Kill the selected agent
    Kill,
    /// Quit omar (kills EA)
    Quit,
    /// Delete the currently active EA (not allowed for EA 0)
    DeleteEa,
}

/// Which left-sidebar panel is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarPanel {
    Projects,
    Events,
    ChainOfCommand,
}

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
    // EA fields
    pub active_ea: EaId,
    pub registered_eas: Vec<EaInfo>,
    pub base_prefix: String,
    pub omar_dir: PathBuf,

    // Existing fields (scoped to active_ea)
    pub agents: Vec<AgentInfo>,
    pub manager: Option<AgentInfo>,
    pub command_tree: Vec<CommandTreeNode>,
    pub selected: usize,
    pub manager_selected: bool,
    pub should_quit: bool,
    pub show_help: bool,
    pub pending_confirm: Option<ConfirmAction>,
    pub filter: String,
    pub status_message: Option<String>,
    /// Warning that persists across tick clears (e.g., tmux misconfiguration)
    pub persistent_warning: Option<String>,
    pub projects: Vec<Project>,
    pub project_input_mode: bool,
    pub project_input: String,
    pub ea_input_mode: bool,
    pub ea_input: String,
    pub show_events: bool,
    /// Enlarged sidebar popup (None = hidden)
    pub sidebar_popup: Option<SidebarPanel>,
    pub scheduled_events: Vec<ScheduledEvent>,
    pub ticker: TickerBuffer,
    pub ticker_offset: usize,
    pub quote_index: usize,
    pub quote_order: Vec<usize>,
    pub show_debug_console: bool,
    pub show_settings: bool,
    pub settings_selected: usize,
    pub settings: DashboardSettings,
    /// Session name of the agent shown in the bottom panel (the EA's manager session)
    pub focus_parent: String,
    /// Stack for Esc navigation (drill-up restores previous parent)
    focus_stack: Vec<String>,
    /// Indices into self.agents for the current focus_parent's direct children
    pub focus_child_indices: Vec<usize>,
    agent_parents: HashMap<String, String>,
    worker_tasks: HashMap<String, String>,
    agent_statuses: HashMap<String, String>,
    /// Whether the left sidebar is focused (vs the right agent panels)
    pub sidebar_focused: bool,
    /// Which sidebar panel is active
    pub sidebar_panel: SidebarPanel,
    /// Selected index within the active sidebar panel
    pub sidebar_selected: usize,
    client: TmuxClient,
    health_checker: HealthChecker,
    health_threshold: i64,
    default_command: String,
    default_workdir: String,
    session_prefix: String,
}

impl App {
    pub fn new(config: &Config, ticker: TickerBuffer) -> Self {
        let base_prefix = config.dashboard.session_prefix.clone();
        let omar_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".omar");

        // Run legacy migration (files + tmux sessions)
        ea::migrate_legacy_state(&omar_dir);
        ea::migrate_legacy_sessions(&base_prefix);

        let active_ea: EaId = 0;
        let registered_eas = ea::load_registry(&omar_dir);

        // EA-scoped prefix and manager session
        let session_prefix = ea::ea_prefix(active_ea, &base_prefix);
        let manager_session = ea::ea_manager_session(active_ea, &base_prefix);

        let client = TmuxClient::new(&session_prefix);
        let health_checker = HealthChecker::new(client.clone(), config.health.idle_warning);

        let state_dir = ea::ea_state_dir(active_ea, &omar_dir);
        std::fs::create_dir_all(state_dir.join("status")).ok();

        Self {
            active_ea,
            registered_eas,
            base_prefix,
            omar_dir,
            agents: Vec::new(),
            manager: None,
            command_tree: Vec::new(),
            selected: 0,
            manager_selected: true,
            should_quit: false,
            show_help: false,
            pending_confirm: None,
            filter: String::new(),
            status_message: None,
            persistent_warning: None,
            projects: projects::load_projects_from(&state_dir),
            project_input_mode: false,
            project_input: String::new(),
            ea_input_mode: false,
            ea_input: String::new(),
            show_events: false,
            sidebar_popup: None,
            scheduled_events: Vec::new(),
            ticker,
            ticker_offset: 0,
            quote_index: 0,
            quote_order: {
                // Shuffle quote indices using time-seeded LCG
                let n = crate::ui::QUOTE_COUNT;
                let mut order: Vec<usize> = (0..n).collect();
                let mut seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64;
                for i in (1..n).rev() {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let j = (seed >> 33) as usize % (i + 1);
                    order.swap(i, j);
                }
                order
            },
            show_debug_console: false,
            show_settings: false,
            settings_selected: 0,
            settings: DashboardSettings::load(),
            focus_parent: manager_session,
            focus_stack: Vec::new(),
            focus_child_indices: Vec::new(),
            agent_parents: HashMap::new(),
            worker_tasks: HashMap::new(),
            agent_statuses: HashMap::new(),
            sidebar_focused: false,
            sidebar_panel: SidebarPanel::Projects,
            sidebar_selected: 0,
            client,
            health_checker,
            health_threshold: config.health.idle_warning,
            default_command: config.agent.default_command.clone(),
            default_workdir: config.agent.default_workdir.clone(),
            session_prefix,
        }
    }

    /// EA state directory for the active EA
    pub fn state_dir(&self) -> PathBuf {
        ea::ea_state_dir(self.active_ea, &self.omar_dir)
    }

    /// Manager session name for the active EA
    pub fn manager_session_name(&self) -> String {
        ea::ea_manager_session(self.active_ea, &self.base_prefix)
    }

    /// True when any popup or input overlay is active.
    pub fn has_popup(&self) -> bool {
        self.show_help
            || self.pending_confirm.is_some()
            || self.project_input_mode
            || self.ea_input_mode
            || self.show_events
            || self.show_debug_console
            || self.show_settings
            || self.sidebar_popup.is_some()
    }

    pub fn client(&self) -> &TmuxClient {
        &self.client
    }

    /// Refresh the list of agents (scoped to active EA)
    pub fn refresh(&mut self) -> Result<()> {
        let manager_session = self.manager_session_name();

        // Ensure manager exists
        self.ensure_manager()?;

        // Get all sessions
        let sessions = self.client.list_all_sessions()?;

        // Separate manager from other agents, filtering out non-EA sessions
        let mut manager_session_found = None;
        let mut other_sessions = Vec::new();

        for session in sessions {
            if session.name == manager_session {
                manager_session_found = Some(session);
            } else if session.name == DASHBOARD_SESSION {
                // Skip the dashboard's own tmux session
                continue;
            } else if !self.session_prefix.is_empty()
                && !session.name.starts_with(&self.session_prefix)
            {
                // Skip sessions that don't match the active EA's prefix
                continue;
            } else {
                other_sessions.push(session);
            }
        }

        // Update manager info
        self.manager = manager_session_found.map(|session| {
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

        // Reload projects from EA-scoped file (picks up API-side changes)
        let state_dir = self.state_dir();
        self.projects = projects::load_projects_from(&state_dir);

        // Load parent mappings, worker tasks, and build the chain-of-command tree
        self.agent_parents = memory::load_agent_parents_from(&state_dir);
        self.worker_tasks = memory::load_worker_tasks_from(&state_dir);
        self.command_tree = build_tree(
            &self.agents,
            self.manager.as_ref(),
            &self.agent_parents,
            &self.session_prefix,
            &manager_session,
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
        let manager_session = self.manager_session_name();
        if self.client.has_session(&manager_session)? {
            return Ok(());
        }

        // Get EA name for prompt
        let ea_name = self
            .registered_eas
            .iter()
            .find(|ea| ea.id == self.active_ea)
            .map(|ea| ea.name.as_str())
            .unwrap_or("Default");

        // Build command with EA system prompt + memory baked in
        let cmd = crate::manager::build_ea_command(
            &self.default_command,
            self.active_ea,
            ea_name,
            &self.omar_dir,
        );

        // Start manager session — system prompt set at process start
        let workdir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        self.client
            .new_session(&manager_session, &cmd, Some(&workdir))?;

        // Write memory after creating manager
        let state_dir = self.state_dir();
        memory::write_memory_to(
            &state_dir,
            &self.agents,
            None,
            &manager_session,
            &self.client,
        );

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

    /// Get an agent's self-reported status
    pub fn agent_status(&self, session: &str) -> Option<&String> {
        self.agent_statuses.get(session)
    }

    /// Update an agent's self-reported status
    pub fn set_agent_status(&mut self, session: String, status: String) {
        self.agent_statuses.insert(session, status);
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
        let manager_session = self.manager_session_name();
        let mut indices = Vec::new();
        if self.focus_parent == manager_session {
            // Root view: show agents that are direct children of EA, plus orphans
            for (i, agent) in self.agents.iter().enumerate() {
                let parent = self.agent_parents.get(&agent.session.name);
                match parent {
                    // Explicit child of EA (manager session)
                    Some(p) if *p == manager_session => indices.push(i),
                    // Has a live parent that is NOT the EA → belongs deeper in the tree
                    Some(p) if self.agents.iter().any(|a| a.session.name == *p) => {}
                    // Parent is dead or missing → show as orphan at root
                    _ => indices.push(i),
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
        let manager_session = self.manager_session_name();
        if self.focus_parent == manager_session {
            self.manager.as_ref()
        } else {
            self.agents
                .iter()
                .find(|a| a.session.name == self.focus_parent)
        }
    }

    /// Check if an agent has children (is a PM or EA)
    fn agent_has_children(&self, session_name: &str) -> bool {
        if session_name == self.manager_session_name() {
            return true;
        }
        self.agent_parents.values().any(|p| p == session_name)
    }

    /// Count children for a given agent
    pub fn child_count(&self, session_name: &str) -> usize {
        if session_name == self.manager_session_name() {
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
        let manager_session = self.manager_session_name();
        let mut crumbs: Vec<String> = vec!["EA".to_string()];
        for session in &self.focus_stack {
            if *session == manager_session {
                continue; // Already added EA
            }
            let short = session
                .strip_prefix(&self.session_prefix)
                .unwrap_or(session);
            crumbs.push(short.to_string());
        }
        // Add current focus parent if not EA
        if self.focus_parent != manager_session {
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

    /// Move sidebar focus to the next panel, skipping Events when hidden.
    pub fn sidebar_next(&mut self) {
        self.sidebar_panel = match self.sidebar_panel {
            SidebarPanel::Projects => {
                if self.settings.show_event_queue {
                    SidebarPanel::Events
                } else {
                    SidebarPanel::ChainOfCommand
                }
            }
            SidebarPanel::Events => SidebarPanel::ChainOfCommand,
            SidebarPanel::ChainOfCommand => SidebarPanel::Projects,
        };
    }

    /// Move sidebar focus to the previous panel, skipping Events when hidden.
    pub fn sidebar_previous(&mut self) {
        self.sidebar_panel = match self.sidebar_panel {
            SidebarPanel::Projects => SidebarPanel::ChainOfCommand,
            SidebarPanel::Events => SidebarPanel::Projects,
            SidebarPanel::ChainOfCommand => {
                if self.settings.show_event_queue {
                    SidebarPanel::Events
                } else {
                    SidebarPanel::Projects
                }
            }
        };
    }

    /// Grid column count (matches render_agent_grid logic).
    fn grid_cols(&self) -> usize {
        2.min(self.focus_child_indices.len()).max(1)
    }

    /// Try to move selection left within the agent grid.
    /// Returns true if the move happened, false if already at the left edge.
    pub fn grid_left(&mut self) -> bool {
        if self.sidebar_focused || self.manager_selected {
            return false;
        }
        let cols = self.grid_cols();
        if cols <= 1 {
            return false;
        }
        let col = self.selected % cols;
        if col > 0 {
            self.selected -= 1;
            true
        } else {
            false
        }
    }

    /// Try to move selection right within the agent grid.
    /// Returns true if the move happened, false if already at the right edge.
    pub fn grid_right(&mut self) -> bool {
        if self.sidebar_focused || self.manager_selected {
            return false;
        }
        let cols = self.grid_cols();
        if cols <= 1 {
            return false;
        }
        let col = self.selected % cols;
        let child_count = self.focus_child_indices.len();
        if col + 1 < cols && self.selected + 1 < child_count {
            self.selected += 1;
            true
        } else {
            false
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
        let manager_session = self.manager_session_name();
        if let Some(agent) = self.selected_agent() {
            // Safety: don't kill attached sessions (user's terminal)
            if agent.session.attached {
                self.status_message = Some("Cannot kill attached session".to_string());
                self.pending_confirm = None;
                return Ok(());
            }

            // Safety: don't kill manager from 'd' key (use separate mechanism)
            if agent.session.name == manager_session {
                self.status_message = Some("Cannot kill manager with 'd'".to_string());
                self.pending_confirm = None;
                return Ok(());
            }

            let name = agent.session.name.clone();
            self.client.kill_session(&name)?;
            let state_dir = self.state_dir();
            memory::remove_agent_parent_in(&state_dir, &name);
            self.status_message = Some(format!("Killed agent: {}", name));
            self.refresh()?;
            memory::write_memory_to(
                &state_dir,
                &self.agents,
                self.manager.as_ref(),
                &manager_session,
                &self.client,
            );
        }
        self.pending_confirm = None;
        Ok(())
    }

    /// Generate a unique agent name (within the active EA's namespace)
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

        let state_dir = self.state_dir();
        let manager_session = self.manager_session_name();
        memory::write_memory_to(
            &state_dir,
            &self.agents,
            self.manager.as_ref(),
            &manager_session,
            &self.client,
        );

        Ok(())
    }

    /// Set status message
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }

    /// Set a persistent warning that survives clear_status() calls
    pub fn set_persistent_warning(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.persistent_warning = Some(msg.clone());
        self.status_message = Some(msg);
    }

    /// Clear status message (persistent warnings are restored)
    pub fn clear_status(&mut self) {
        self.status_message = self.persistent_warning.clone();
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

    /// Get focus parent pane output (more lines for display)
    pub fn get_focus_parent_output(&self, lines: i32) -> Result<String> {
        self.client.capture_pane(&self.focus_parent, lines)
    }

    /// Get agent pane output by session name
    pub fn get_agent_output(&self, session: &str, lines: i32) -> Result<String> {
        self.client.capture_pane(session, lines)
    }

    /// Add a project and update memory (EA-scoped)
    pub fn add_project(&mut self, name: &str) {
        let state_dir = self.state_dir();
        let _ = projects::add_project_in(&state_dir, name);
        self.projects = projects::load_projects_from(&state_dir);
        let manager_session = self.manager_session_name();
        memory::write_memory_to(
            &state_dir,
            &self.agents,
            self.manager.as_ref(),
            &manager_session,
            &self.client,
        );
    }

    /// Complete (remove) a project by id and update memory (EA-scoped)
    pub fn complete_project(&mut self, id: usize) {
        let state_dir = self.state_dir();
        let _ = projects::remove_project_in(&state_dir, id);
        self.projects = projects::load_projects_from(&state_dir);
        let manager_session = self.manager_session_name();
        memory::write_memory_to(
            &state_dir,
            &self.agents,
            self.manager.as_ref(),
            &manager_session,
            &self.client,
        );
    }

    // ── Multi-EA methods ──

    /// Switch the dashboard to a different EA. Full reload of all state.
    pub fn switch_ea(&mut self, ea_id: EaId) -> Result<()> {
        self.active_ea = ea_id;

        // Reconstruct tmux client with new EA's prefix
        let new_prefix = ea::ea_prefix(ea_id, &self.base_prefix);
        self.session_prefix = new_prefix.clone();
        self.client = TmuxClient::new(&new_prefix);
        self.health_checker = HealthChecker::new(self.client.clone(), self.health_threshold);

        // Reset all view state
        self.focus_parent = ea::ea_manager_session(ea_id, &self.base_prefix);
        self.focus_stack.clear();
        self.selected = 0;
        self.manager_selected = true;

        // Reload all state for the new EA
        let state_dir = ea::ea_state_dir(ea_id, &self.omar_dir);
        std::fs::create_dir_all(&state_dir).ok();
        self.projects = projects::load_projects_from(&state_dir);
        self.agent_parents = memory::load_agent_parents_from(&state_dir);
        self.worker_tasks = memory::load_worker_tasks_from(&state_dir);

        // Refresh discovers agents via the new prefix
        self.refresh()?;
        self.set_status(format!("Switched to EA {}", ea_id));
        Ok(())
    }

    /// Cycle to the next registered EA
    pub fn cycle_next_ea(&mut self) {
        if self.registered_eas.len() <= 1 {
            return;
        }
        let current_idx = self
            .registered_eas
            .iter()
            .position(|ea| ea.id == self.active_ea)
            .unwrap_or(0);
        let next_idx = (current_idx + 1) % self.registered_eas.len();
        let next_ea = self.registered_eas[next_idx].id;
        if let Err(e) = self.switch_ea(next_ea) {
            self.set_status(format!("Error switching EA: {}", e));
        }
    }

    /// Cycle to the previous registered EA
    pub fn cycle_previous_ea(&mut self) {
        if self.registered_eas.len() <= 1 {
            return;
        }
        let current_idx = self
            .registered_eas
            .iter()
            .position(|ea| ea.id == self.active_ea)
            .unwrap_or(0);
        let prev_idx = if current_idx == 0 {
            self.registered_eas.len() - 1
        } else {
            current_idx - 1
        };
        let prev_ea = self.registered_eas[prev_idx].id;
        if let Err(e) = self.switch_ea(prev_ea) {
            self.set_status(format!("Error switching EA: {}", e));
        }
    }

    /// Create a new EA and add it to the registry
    pub fn create_ea(&mut self, name: String, desc: Option<String>) -> EaId {
        match ea::register_ea(&self.omar_dir, &name, desc.as_deref()) {
            Ok(ea_id) => {
                self.registered_eas = ea::load_registry(&self.omar_dir);
                self.set_status(format!("Created EA {}: {}", ea_id, name));
                ea_id
            }
            Err(e) => {
                self.set_status(format!("Error creating EA: {}", e));
                self.active_ea // Return current EA on error
            }
        }
    }

    /// Delete the specified EA: kill all its tmux sessions, remove state, unregister.
    /// EA 0 cannot be deleted. Switches to EA 0 if the active EA is deleted.
    pub fn delete_ea(&mut self, ea_id: EaId) -> Result<()> {
        if ea_id == 0 {
            self.set_status("Cannot delete EA 0");
            self.pending_confirm = None;
            return Ok(());
        }

        // Kill all tmux sessions for this EA
        let ea_prefix = ea::ea_prefix(ea_id, &self.base_prefix);
        let manager_session = ea::ea_manager_session(ea_id, &self.base_prefix);
        let ea_client = TmuxClient::new(&ea_prefix);

        // Kill worker sessions
        if let Ok(sessions) = ea_client.list_sessions() {
            for session in sessions {
                if session.name != manager_session {
                    let _ = ea_client.kill_session(&session.name);
                }
            }
        }

        // Kill manager session
        if ea_client.has_session(&manager_session).unwrap_or(false) {
            let _ = ea_client.kill_session(&manager_session);
        }

        // Remove state directory
        let state_dir = ea::ea_state_dir(ea_id, &self.omar_dir);
        if state_dir.exists() {
            let _ = std::fs::remove_dir_all(&state_dir);
        }

        // Unregister EA
        ea::unregister_ea(&self.omar_dir, ea_id)?;
        self.registered_eas = ea::load_registry(&self.omar_dir);

        // Switch to EA 0 if we just deleted the active EA
        if self.active_ea == ea_id {
            self.switch_ea(0)?;
        }

        self.set_status(format!("Deleted EA {}", ea_id));
        self.pending_confirm = None;
        Ok(())
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
    manager_session: &str,
) -> Vec<CommandTreeNode> {
    let mut nodes = Vec::new();

    // Root: EA
    let ea_health = manager.map(|m| m.health).unwrap_or(HealthState::Idle);
    nodes.push(CommandTreeNode {
        name: "Executive Assistant".to_string(),
        session_name: manager_session.to_string(),
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
            let parent_exists = *parent_session == manager_session
                || agents.iter().any(|a| a.session.name == *parent_session);
            if parent_exists {
                children_map
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

    // Recursively add nodes starting from EA's children
    let ea_children = children_map.get(manager_session);
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
        manager_session,
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
    use crate::tmux::{HealthInfo, HealthState, Session};

    /// Manager session name used in tests (EA 0 with "omar-agent-" prefix)
    const TEST_MANAGER: &str = "omar-agent-ea-0";

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
        let ea = make_agent(TEST_MANAGER, HealthState::Running);
        let parents = HashMap::new();
        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-", TEST_MANAGER);

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
        let ea = make_agent(TEST_MANAGER, HealthState::Running);
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-rest-api".to_string(),
            TEST_MANAGER.to_string(),
        );
        parents.insert(
            "omar-agent-api".to_string(),
            "omar-agent-rest-api".to_string(),
        );
        parents.insert(
            "omar-agent-auth".to_string(),
            "omar-agent-rest-api".to_string(),
        );

        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-", TEST_MANAGER);

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
        let agents = vec![
            make_agent("omar-agent-debug", HealthState::Running),
            make_agent("omar-agent-test", HealthState::Idle),
        ];
        let ea = make_agent(TEST_MANAGER, HealthState::Running);
        let parents = HashMap::new();

        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-", TEST_MANAGER);

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
        let ea = make_agent(TEST_MANAGER, HealthState::Running);
        let mut parents = HashMap::new();
        parents.insert("omar-agent-a".to_string(), TEST_MANAGER.to_string());
        parents.insert("omar-agent-b".to_string(), "omar-agent-a".to_string());
        parents.insert("omar-agent-c".to_string(), "omar-agent-b".to_string());

        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-", TEST_MANAGER);

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
        let ea = make_agent(TEST_MANAGER, HealthState::Running);
        let mut parents = HashMap::new();
        parents.insert(
            "omar-agent-worker1".to_string(),
            "omar-agent-gone".to_string(),
        );

        let tree = build_tree(&agents, Some(&ea), &parents, "omar-agent-", TEST_MANAGER);

        // worker1 should be orphan under EA since parent doesn't exist
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[1].name, "worker1");
        assert_eq!(tree[1].depth, 1);
    }

    // ── focus navigation tests ──

    #[test]
    fn test_focus_children_root_shows_direct_ea_children_and_orphans() {
        // EA -> api (with children: api-worker, auth), orphan
        let agents = [
            make_agent("omar-agent-api", HealthState::Running),
            make_agent("omar-agent-api-worker", HealthState::Running),
            make_agent("omar-agent-auth", HealthState::Idle),
            make_agent("omar-agent-orphan", HealthState::Idle),
        ];
        let mut parents = HashMap::new();
        parents.insert("omar-agent-api".to_string(), TEST_MANAGER.to_string());
        parents.insert(
            "omar-agent-api-worker".to_string(),
            "omar-agent-api".to_string(),
        );
        parents.insert("omar-agent-auth".to_string(), "omar-agent-api".to_string());

        // Compute focus children at root using the new logic
        let mut indices = Vec::new();
        for (i, agent) in agents.iter().enumerate() {
            let parent = parents.get(&agent.session.name);
            match parent {
                Some(p) if *p == TEST_MANAGER => indices.push(i),
                Some(p) if agents.iter().any(|a| a.session.name == *p) => {}
                _ => indices.push(i),
            }
        }

        // Root should show: api (EA child) + orphan (no parent)
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
        let focus_parent = TEST_MANAGER.to_string();
        let mut crumbs = vec!["EA".to_string()];
        for session in &focus_stack {
            if *session == TEST_MANAGER {
                continue;
            }
            let short = session.strip_prefix(session_prefix).unwrap_or(session);
            crumbs.push(short.to_string());
        }
        if focus_parent != TEST_MANAGER {
            let short = focus_parent
                .strip_prefix(session_prefix)
                .unwrap_or(&focus_parent);
            crumbs.push(short.to_string());
        }
        assert_eq!(crumbs, vec!["EA"]);

        // Drilled into agent: ["EA", "rest-api"]
        let focus_stack = vec![TEST_MANAGER.to_string()];
        let focus_parent = "omar-agent-rest-api".to_string();
        let mut crumbs = vec!["EA".to_string()];
        for session in &focus_stack {
            if *session == TEST_MANAGER {
                continue;
            }
            let short = session.strip_prefix(session_prefix).unwrap_or(session);
            crumbs.push(short.to_string());
        }
        if focus_parent != TEST_MANAGER {
            let short = focus_parent
                .strip_prefix(session_prefix)
                .unwrap_or(&focus_parent);
            crumbs.push(short.to_string());
        }
        assert_eq!(crumbs, vec!["EA", "rest-api"]);
    }
}
