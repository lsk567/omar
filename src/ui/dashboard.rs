use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use regex::Regex;

use crate::app::{AgentInfo, App};
use crate::tmux::HealthState;

/// Render the entire dashboard
pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),      // Status bar
            Constraint::Percentage(55), // Agent grid + projects sidebar
            Constraint::Min(8),         // Manager panel (~33%)
            Constraint::Length(1),      // Help bar
        ])
        .split(frame.area());

    render_status_bar(frame, app, chunks[0]);

    // Split agent grid area into projects sidebar + agent grid
    if !app.projects.is_empty() {
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(25), Constraint::Min(0)])
            .split(chunks[1]);
        render_projects_panel(frame, app, h_chunks[0]);
        render_agent_grid(frame, app, h_chunks[1]);
    } else {
        render_agent_grid(frame, app, chunks[1]);
    }

    // Split EA area: if command tree has content, show it alongside the EA panel
    if app.command_tree.len() > 1 {
        let ea_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(chunks[2]);
        render_manager_panel(frame, app, ea_chunks[0]);
        render_command_tree(frame, app, ea_chunks[1]);
    } else {
        render_manager_panel(frame, app, chunks[2]);
    }

    render_help_bar(frame, app, chunks[3]);

    // Render overlays
    if app.show_help {
        render_help_popup(frame);
    }

    if app.show_confirm_kill {
        render_confirm_kill(frame, app);
    }

    if app.project_input_mode {
        render_project_input(frame, app);
    }
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let (running, idle) = app.health_counts();
    let total = app.total_agents();

    let status_text = vec![
        Span::styled(
            "One-Man Army ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("| Agents: "),
        Span::styled(format!("{}", total), Style::default().fg(Color::White)),
        Span::raw(" | "),
        Span::styled(
            format!("{} Running", running),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" "),
        Span::styled(format!("{} Idle", idle), Style::default().fg(Color::Yellow)),
    ];

    let status_line = Line::from(status_text);

    // Add status message if present
    let content = if let Some(ref msg) = app.status_message {
        vec![
            status_line,
            Line::from(Span::styled(msg.clone(), Style::default().fg(Color::Cyan))),
        ]
    } else {
        vec![status_line]
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(content).block(block);
    frame.render_widget(paragraph, area);
}

fn render_projects_panel(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Projects ")
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::DarkGray));

    let lines: Vec<Line> = app
        .projects
        .iter()
        .map(|p| {
            Line::from(Span::styled(
                format!("{}. {}", p.id, p.name),
                Style::default().fg(Color::White),
            ))
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn render_agent_grid(frame: &mut Frame, app: &App, area: Rect) {
    if app.agents.is_empty() {
        let empty_msg =
            Paragraph::new("No agent sessions found.\n\nPress 'n' to spawn a new agent.")
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Thick)
                        .title(" Agents ")
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
        frame.render_widget(empty_msg, area);
        return;
    }

    let groups = app.agent_groups();

    // Calculate total visual rows needed
    let mut total_rows: usize = 0;
    for group in &groups {
        if group.pm.is_some() {
            total_rows += 1; // PM card (full width)
        }
        if !group.workers.is_empty() {
            let cols = 2.min(group.workers.len()).max(1);
            total_rows += group.workers.len().div_ceil(cols);
        }
    }

    if total_rows == 0 {
        return;
    }

    let row_height = (area.height / total_rows as u16).max(6);
    let row_constraints: Vec<Constraint> = (0..total_rows)
        .map(|_| Constraint::Length(row_height))
        .collect();

    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    let mut row_idx = 0;

    for group in &groups {
        // Render PM card (full width)
        if let Some(pm) = group.pm {
            if row_idx < row_chunks.len() {
                let is_selected = app
                    .agents
                    .iter()
                    .position(|a| a.session.name == pm.session.name)
                    .is_some_and(|idx| !app.manager_selected && idx == app.selected);
                let is_interactive = app.interactive_mode && is_selected;
                render_agent_card(
                    frame,
                    app,
                    pm,
                    row_chunks[row_idx],
                    is_selected,
                    is_interactive,
                );
                row_idx += 1;
            }
        }

        // Render workers in 2-column sub-grid below their PM
        if !group.workers.is_empty() {
            let cols = 2.min(group.workers.len()).max(1);
            for (i, worker) in group.workers.iter().enumerate() {
                let sub_row = i / cols;
                let sub_col = i % cols;
                let current_row = row_idx + sub_row;

                if current_row >= row_chunks.len() {
                    break;
                }

                let col_constraints: Vec<Constraint> = (0..cols)
                    .map(|_| Constraint::Ratio(1, cols as u32))
                    .collect();

                let col_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(col_constraints)
                    .split(row_chunks[current_row]);

                if sub_col < col_chunks.len() {
                    let is_selected = app
                        .agents
                        .iter()
                        .position(|a| a.session.name == worker.session.name)
                        .is_some_and(|idx| !app.manager_selected && idx == app.selected);
                    let is_interactive = app.interactive_mode && is_selected;
                    render_agent_card(
                        frame,
                        app,
                        worker,
                        col_chunks[sub_col],
                        is_selected,
                        is_interactive,
                    );
                }
            }
            row_idx += group.workers.len().div_ceil(cols);
        }
    }
}

fn render_manager_panel(frame: &mut Frame, app: &App, area: Rect) {
    if app.manager.is_some() {
        let is_selected = app.manager_selected;
        let is_interactive = app.interactive_mode;

        // Different styles for different states
        let (border_color, title) = if is_interactive {
            (
                Color::Cyan,
                " Executive Assistant [INTERACTIVE - Esc to exit] ",
            )
        } else if is_selected {
            (
                Color::Magenta,
                " [Executive Assistant] - Press Enter to open ",
            )
        } else {
            (Color::Blue, " Executive Assistant ")
        };

        let border_style =
            Style::default()
                .fg(border_color)
                .add_modifier(if is_selected || is_interactive {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(border_style);

        // Get manager output - more lines to fill the panel
        let available_lines = area.height.saturating_sub(2) as i32; // -2 for borders
        let output = app
            .get_manager_output(available_lines.max(20))
            .unwrap_or_default();

        // Parse ANSI codes and convert to ratatui text
        let content = match ansi_to_tui::IntoText::into_text(&output) {
            Ok(text) => text,
            Err(_) => {
                // Fallback: strip ANSI and show plain text
                let plain = strip_ansi(&output);
                ratatui::text::Text::raw(plain)
            }
        };

        // Calculate scroll to show bottom of content
        let content_height = content.lines.len() as u16;
        let visible_height = area.height.saturating_sub(2); // -2 for borders
        let scroll = content_height.saturating_sub(visible_height);

        let paragraph = Paragraph::new(content)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        frame.render_widget(paragraph, area);
    } else {
        // Manager not available (shouldn't happen normally)
        let block = Block::default()
            .title(" Executive Assistant ")
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::DarkGray));

        let paragraph = Paragraph::new("Starting Executive Assistant...")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);

        frame.render_widget(paragraph, area);
    }
}

fn render_command_tree(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Chain of Command ")
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Blue));

    let mut lines: Vec<Line> = Vec::new();

    for node in &app.command_tree {
        let (health_color, icon) = match node.health {
            HealthState::Running => (Color::Green, "●"),
            HealthState::Idle => (Color::Yellow, "○"),
        };

        let mut spans: Vec<Span> = Vec::new();

        if node.depth == 0 {
            // Root (EA): no connector, just name + icon
            spans.push(Span::styled(
                format!(" {} ", node.name),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(icon, Style::default().fg(health_color)));
        } else {
            // Build prefix from ancestor continuation lines
            let mut prefix = String::from(" ");
            for i in 0..node.ancestor_is_last.len() {
                if i == 0 {
                    continue; // skip EA level (always root)
                }
                if node.ancestor_is_last[i] {
                    prefix.push_str("    ");
                } else {
                    prefix.push_str(" │  ");
                }
            }

            // Add connector for this node
            if node.is_last_sibling {
                prefix.push_str(" └── ");
            } else {
                prefix.push_str(" ├── ");
            }

            spans.push(Span::styled(prefix, Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                format!("{} ", node.name),
                Style::default().fg(Color::White),
            ));
            spans.push(Span::styled(icon, Style::default().fg(health_color)));
        }

        lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// Strip ANSI escape codes from a string (fallback)
fn strip_ansi(s: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap());
    re.replace_all(s, "").to_string()
}

fn render_agent_card(
    frame: &mut Frame,
    app: &App,
    agent: &AgentInfo,
    area: Rect,
    selected: bool,
    interactive: bool,
) {
    let (health_color, status_icon) = match agent.health {
        HealthState::Running => (Color::Green, "●"),
        HealthState::Idle => (Color::Yellow, "○"),
    };

    // Border color based on state
    let border_color = if interactive {
        Color::Cyan
    } else if selected {
        Color::Magenta
    } else {
        Color::Gray
    };

    let border_style = Style::default()
        .fg(border_color)
        .add_modifier(if selected || interactive {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });

    // Display name: strip prefix, label PMs as "Project Manager"
    let short_name = agent
        .session
        .name
        .strip_prefix(app.client().prefix())
        .unwrap_or(&agent.session.name);
    let display = if let Some(rest) = short_name.strip_prefix("pm-") {
        format!("Project Manager: {}", rest)
    } else {
        short_name.to_string()
    };

    // Title with status indicator — dot icon and name use health_color,
    // surrounding decoration uses border_color.
    let title_line = if interactive {
        Line::from(vec![
            Span::styled(" ", Style::default().fg(border_color)),
            Span::styled(&display, Style::default().fg(health_color)),
            Span::styled(" [INTERACTIVE - Esc] ", Style::default().fg(border_color)),
        ])
    } else if selected {
        Line::from(vec![
            Span::styled(" [", Style::default().fg(border_color)),
            Span::styled(status_icon, Style::default().fg(health_color)),
            Span::styled("] ", Style::default().fg(border_color)),
            Span::styled(&display, Style::default().fg(health_color)),
            Span::styled(" - Enter to open ", Style::default().fg(border_color)),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ", Style::default().fg(border_color)),
            Span::styled(status_icon, Style::default().fg(health_color)),
            Span::styled(" ", Style::default().fg(border_color)),
            Span::styled(&display, Style::default().fg(health_color)),
            Span::styled(" ", Style::default().fg(border_color)),
        ])
    };

    let block = Block::default()
        .title(title_line)
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(border_style);

    // Get live output from the agent's pane
    let available_lines = area.height.saturating_sub(2) as i32;
    let output = app
        .get_agent_output(&agent.session.name, available_lines.max(10))
        .unwrap_or_default();

    // Parse ANSI codes
    let content = match ansi_to_tui::IntoText::into_text(&output) {
        Ok(text) => text,
        Err(_) => {
            let plain = strip_ansi(&output);
            ratatui::text::Text::raw(plain)
        }
    };

    // Scroll to bottom
    let content_height = content.lines.len() as u16;
    let visible_height = area.height.saturating_sub(2);
    let scroll = content_height.saturating_sub(visible_height);

    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

fn render_help_bar(frame: &mut Frame, app: &App, area: Rect) {
    let help_text = if app.interactive_mode {
        // Interactive mode help
        let target = if app.manager_selected {
            "Executive Assistant"
        } else {
            "agent"
        };
        vec![
            Span::styled(
                "Esc",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            ),
            Span::raw(":Exit interactive "),
            Span::styled("Type", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" to send input to {}", target)),
        ]
    } else if app.manager_selected || app.selected_agent().is_some() {
        // Any agent selected help (including manager)
        vec![
            Span::styled(
                "i",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            ),
            Span::raw(":Interactive "),
            Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Popup "),
            Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Nav "),
            Span::styled("p", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Project "),
            Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Quit "),
            Span::styled("?", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Help"),
        ]
    } else {
        // Normal help
        vec![
            Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Quit "),
            Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Nav "),
            Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Attach "),
            Span::styled("n", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":New "),
            Span::styled("d", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Kill "),
            Span::styled("p", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Project "),
            Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Refresh "),
            Span::styled("?", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(":Help"),
        ]
    };

    let paragraph =
        Paragraph::new(Line::from(help_text)).style(Style::default().fg(Color::DarkGray));

    frame.render_widget(paragraph, area);
}

fn render_help_popup(frame: &mut Frame) {
    let area = centered_rect(60, 50, frame.area());

    let help_content = vec![
        Line::from(Span::styled(
            "Keyboard Shortcuts",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  q, Esc      Quit"),
        Line::from("  j, Down     Move selection down"),
        Line::from("  k, Up       Move selection up"),
        Line::from("  Enter       Attach to selected agent"),
        Line::from("  n           Spawn new agent"),
        Line::from("  d           Kill selected agent"),
        Line::from("  p           Add a project"),
        Line::from("  r           Refresh agent list"),
        Line::from("  ?           Toggle this help"),
        Line::from(""),
        Line::from(Span::styled(
            "Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(help_content).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_confirm_kill(frame: &mut Frame, app: &App) {
    let area = centered_rect(40, 20, frame.area());

    let agent_name = app
        .selected_agent()
        .map(|a| a.session.name.clone())
        .unwrap_or_else(|| "?".to_string());

    let content = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Kill this agent?",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(agent_name, Style::default().fg(Color::Yellow))),
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::styled("y", Style::default().fg(Color::Green)),
            Span::raw(": Yes  "),
            Span::styled("n", Style::default().fg(Color::Red)),
            Span::raw(": No"),
        ]),
    ];

    let block = Block::default()
        .title(" Confirm ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let paragraph = Paragraph::new(content)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_project_input(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 20, frame.area());

    let content = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Add Project",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("> {}_", app.project_input),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Enter to confirm, Esc to cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Block::default()
        .title(" New Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(content)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

/// Create a centered rectangle
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
