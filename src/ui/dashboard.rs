use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use regex::Regex;

use crate::app::{AgentInfo, App};
use crate::memory;
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

    // Split focus parent area: if command tree has content, show it alongside
    if app.command_tree.len() > 1 {
        let ea_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(chunks[2]);
        render_focus_parent(frame, app, ea_chunks[0]);
        render_command_tree(frame, app, ea_chunks[1]);
    } else {
        render_focus_parent(frame, app, chunks[2]);
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

    if app.show_events {
        render_events_popup(frame, app);
    }

    if app.show_debug_console {
        render_debug_console(frame, app);
    }
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let (running, idle) = app.health_counts();
    let total = app.total_agents();
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    let next_ea_event = app
        .scheduled_events
        .iter()
        .filter(|e| e.receiver == "ea")
        .min_by_key(|e| e.timestamp);
    let next_event = app.scheduled_events.iter().min_by_key(|e| e.timestamp);

    let mut status_text = vec![
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
        Span::raw(" | Events: "),
        Span::styled(
            format!("{}", app.scheduled_events.len()),
            Style::default().fg(Color::Cyan),
        ),
    ];

    if let Some(event) = next_ea_event {
        status_text.push(Span::raw(" | EA Wake: "));
        status_text.push(Span::styled(
            format_countdown_ns(event.timestamp, now_ns),
            Style::default().fg(Color::Magenta),
        ));
    } else if let Some(event) = next_event {
        status_text.push(Span::raw(" | Next Event: "));
        status_text.push(Span::styled(
            format!(
                "{} ({})",
                truncate_str(&event.receiver, 10),
                format_countdown_ns(event.timestamp, now_ns)
            ),
            Style::default().fg(Color::Magenta),
        ));
    }

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
    let children = app.focus_children();

    if children.is_empty() {
        let empty_msg = Paragraph::new("No child agents.\n\nPress 'n' to spawn a new agent.")
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

    // Simple 2-column grid layout
    let cols = 2.min(children.len()).max(1);
    let total_rows = children.len().div_ceil(cols);

    let row_height = (area.height / total_rows as u16).max(6);
    let row_constraints: Vec<Constraint> = (0..total_rows)
        .map(|_| Constraint::Length(row_height))
        .collect();

    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for (i, child) in children.iter().enumerate() {
        let row = i / cols;
        let col = i % cols;

        if row >= row_chunks.len() {
            break;
        }

        let col_constraints: Vec<Constraint> = (0..cols)
            .map(|_| Constraint::Ratio(1, cols as u32))
            .collect();

        let col_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(row_chunks[row]);

        if col < col_chunks.len() {
            let is_selected = !app.manager_selected && i == app.selected;
            render_summary_card(frame, app, child, col_chunks[col], is_selected);
        }
    }
}

fn render_focus_parent(frame: &mut Frame, app: &App, area: Rect) {
    let parent_info = app.focus_parent_info();

    if let Some(info) = parent_info {
        let is_selected = app.manager_selected;

        // Build display title based on focus parent type
        let parent_name = &app.focus_parent;
        let display_title = if *parent_name == crate::manager::MANAGER_SESSION {
            "Executive Assistant".to_string()
        } else {
            let short = parent_name
                .strip_prefix(app.client().prefix())
                .unwrap_or(parent_name);
            if let Some(rest) = short.strip_prefix("pm-") {
                format!("Project Manager: {}", rest)
            } else {
                short.to_string()
            }
        };

        // Health status dot
        let (health_color, status_icon) = match info.health {
            HealthState::Running => (Color::Green, "●"),
            HealthState::Idle => (Color::Yellow, "○"),
        };

        let (border_color, title_line) = if is_selected {
            (
                Color::Magenta,
                Line::from(vec![
                    Span::styled(" [", Style::default().fg(Color::Magenta)),
                    Span::styled(status_icon, Style::default().fg(health_color)),
                    Span::styled("] ", Style::default().fg(Color::Magenta)),
                    Span::styled(&display_title, Style::default().fg(health_color)),
                    Span::styled(" - Enter to open ", Style::default().fg(Color::Magenta)),
                ]),
            )
        } else {
            (
                Color::Blue,
                Line::from(vec![
                    Span::styled(" ", Style::default().fg(Color::Blue)),
                    Span::styled(status_icon, Style::default().fg(health_color)),
                    Span::styled(" ", Style::default().fg(Color::Blue)),
                    Span::styled(&display_title, Style::default().fg(health_color)),
                    Span::styled(" ", Style::default().fg(Color::Blue)),
                ]),
            )
        };

        let border_style = Style::default()
            .fg(border_color)
            .add_modifier(if is_selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });

        let block = Block::default()
            .title(title_line)
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(border_style);

        // Get focus parent output - more lines to fill the panel
        let available_lines = area.height.saturating_sub(2) as i32;
        let output = app
            .get_focus_parent_output(available_lines.max(20))
            .unwrap_or_default();

        // Parse ANSI codes and convert to ratatui text
        let content = match ansi_to_tui::IntoText::into_text(&output) {
            Ok(text) => text,
            Err(_) => {
                let plain = strip_ansi(&output);
                ratatui::text::Text::raw(plain)
            }
        };

        // Calculate scroll to show bottom of content
        let content_height = content.lines.len() as u16;
        let visible_height = area.height.saturating_sub(2);
        let scroll = content_height.saturating_sub(visible_height);

        let paragraph = Paragraph::new(content)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        frame.render_widget(paragraph, area);
    } else {
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

        // Check if this node is the current focus parent
        let is_focus = node.session_name == app.focus_parent;

        let mut spans: Vec<Span> = Vec::new();

        if node.depth == 0 {
            // Root (EA): no connector, just name + icon
            let name_style = if is_focus {
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            };
            if is_focus {
                spans.push(Span::styled("►", Style::default().fg(Color::Magenta)));
            }
            spans.push(Span::styled(format!(" {} ", node.name), name_style));
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

            let name_style = if is_focus {
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            if is_focus {
                spans.push(Span::styled("►", Style::default().fg(Color::Magenta)));
            }
            spans.push(Span::styled(format!("{} ", node.name), name_style));
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

fn render_summary_card(
    frame: &mut Frame,
    app: &App,
    agent: &AgentInfo,
    area: Rect,
    selected: bool,
) {
    let (health_color, status_icon) = match agent.health {
        HealthState::Running => (Color::Green, "●"),
        HealthState::Idle => (Color::Yellow, "○"),
    };

    let border_color = if selected {
        Color::Magenta
    } else {
        Color::Gray
    };

    let border_style = Style::default().fg(border_color).add_modifier(if selected {
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

    // Title with status indicator
    let title_line = if selected {
        Line::from(vec![
            Span::styled(" [", Style::default().fg(border_color)),
            Span::styled(status_icon, Style::default().fg(health_color)),
            Span::styled("] ", Style::default().fg(border_color)),
            Span::styled(&display, Style::default().fg(health_color)),
            Span::styled(" ", Style::default().fg(border_color)),
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

    // Available width for text content (minus borders and label prefix)
    let content_width = area.width.saturating_sub(2) as usize; // -2 for borders

    let mut lines: Vec<Line> = Vec::new();

    // Sub-agent count (first)
    let child_count = app.child_count(&agent.session.name);
    if child_count > 0 {
        lines.push(Line::from(vec![
            Span::styled("Sub-agents: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", child_count),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    // Status line (from status file, fallback to last_output)
    let status_text = memory::load_agent_status(&agent.session.name).unwrap_or_else(|| {
        if agent.health_info.last_output.is_empty() {
            "waiting for the agent to report status".to_string()
        } else {
            agent.health_info.last_output.clone()
        }
    });
    lines.push(Line::from(vec![
        Span::styled("Status: ", Style::default().fg(Color::Yellow)),
        Span::styled(status_text, Style::default().fg(Color::White)),
    ]));

    // Task (multi-line word wrap to fill available card space)
    let task = app
        .worker_tasks()
        .get(&agent.session.name)
        .cloned()
        .unwrap_or_else(|| "(no task assigned)".to_string());
    let label = "Task: ";
    let inner_height = area.height.saturating_sub(2) as usize; // minus top/bottom border
    let task_lines_budget = inner_height.saturating_sub(lines.len());

    if task_lines_budget > 0 {
        // Word-wrap the task text into lines
        let first_width = content_width.saturating_sub(label.len());
        let cont_width = content_width;

        let mut wrapped: Vec<String> = Vec::new();
        let mut remaining = task.as_str();

        // First line has reduced width due to "Task: " label
        let widths = std::iter::once(first_width).chain(std::iter::repeat(cont_width));
        for w in widths {
            if remaining.is_empty() || w == 0 {
                break;
            }
            if remaining.len() <= w {
                wrapped.push(remaining.to_string());
                remaining = "";
            } else {
                // Try to break at a word boundary
                let break_at = remaining[..w]
                    .rfind(' ')
                    .map(|i| i + 1) // include the space on the current line
                    .unwrap_or(w);
                wrapped.push(remaining[..break_at].trim_end().to_string());
                remaining = &remaining[break_at..];
            }
        }

        // Fit into budget, truncating last visible line if needed
        if wrapped.len() <= task_lines_budget {
            // Everything fits
            for (i, line_text) in wrapped.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(label, Style::default().fg(Color::Cyan)),
                        Span::styled(line_text.clone(), Style::default().fg(Color::White)),
                    ]));
                } else {
                    lines.push(Line::from(Span::styled(
                        line_text.clone(),
                        Style::default().fg(Color::White),
                    )));
                }
            }
        } else {
            // Truncate: show budget-1 full lines + last line with "..."
            let last = task_lines_budget - 1;
            for (i, text) in wrapped.iter().enumerate().take(task_lines_budget) {
                let is_first = i == 0;
                if i < last {
                    if is_first {
                        lines.push(Line::from(vec![
                            Span::styled(label, Style::default().fg(Color::Cyan)),
                            Span::styled(text.clone(), Style::default().fg(Color::White)),
                        ]));
                    } else {
                        lines.push(Line::from(Span::styled(
                            text.clone(),
                            Style::default().fg(Color::White),
                        )));
                    }
                } else {
                    // Last line: truncate with ellipsis
                    let w = if is_first { first_width } else { cont_width };
                    let truncated = if text.len() > w.saturating_sub(3) && w > 3 {
                        format!("{}...", &text[..w - 3])
                    } else {
                        format!("{}...", text)
                    };
                    if is_first {
                        lines.push(Line::from(vec![
                            Span::styled(label, Style::default().fg(Color::Cyan)),
                            Span::styled(truncated, Style::default().fg(Color::White)),
                        ]));
                    } else {
                        lines.push(Line::from(Span::styled(
                            truncated,
                            Style::default().fg(Color::White),
                        )));
                    }
                }
            }
        }
    }

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(paragraph, area);
}

fn render_help_bar(frame: &mut Frame, app: &App, area: Rect) {
    let at_root = app.focus_parent == crate::manager::MANAGER_SESSION;

    let mut help_text = vec![
        Span::styled("↑↓", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Nav "),
        Span::styled("→", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Drill-in "),
        Span::styled("←", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Back "),
        Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Popup "),
        Span::styled("n", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":New "),
        Span::styled("d", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Kill "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Quit "),
    ];
    if !at_root {
        help_text.push(Span::styled(
            "Esc",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        help_text.push(Span::raw(":Back "));
    }
    help_text.push(Span::styled(
        "?",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    help_text.push(Span::raw(":Help"));

    let ticker_content = app.ticker.render(std::time::Duration::from_secs(5));

    if ticker_content.is_empty() {
        // No ticker content — full-width help text
        let paragraph =
            Paragraph::new(Line::from(help_text)).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(paragraph, area);
    } else {
        // Compute help text width (sum of span widths)
        let help_width: u16 = help_text.iter().map(|s| s.width() as u16).sum();
        let help_col_width = help_width.saturating_add(1).min(area.width);

        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(help_col_width), Constraint::Min(1)])
            .split(area);

        // Left: help text
        let help_paragraph =
            Paragraph::new(Line::from(help_text)).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(help_paragraph, h_chunks[0]);

        // Right: ticker (static if fits, scrolling if too long)
        let ticker_area_width = h_chunks[1].width as usize;
        if ticker_area_width > 0 {
            let content_len = ticker_content.chars().count();
            let visible = if content_len <= ticker_area_width {
                // Fits — display statically, right-aligned
                format!("{:>width$}", ticker_content, width = ticker_area_width)
            } else {
                // Too long — scroll
                let padded: String = std::iter::repeat_n(' ', ticker_area_width)
                    .chain(ticker_content.chars())
                    .collect();
                let total_len = padded.chars().count();
                let offset = app.ticker_offset % total_len;
                padded
                    .chars()
                    .cycle()
                    .skip(offset)
                    .take(ticker_area_width)
                    .collect()
            };

            let ticker_paragraph = Paragraph::new(Line::from(Span::styled(
                visible,
                Style::default().fg(Color::Yellow),
            )));
            frame.render_widget(ticker_paragraph, h_chunks[1]);
        }
    }
}

fn render_help_popup(frame: &mut Frame) {
    let area = centered_rect(60, 50, frame.area());

    let help_content = vec![
        Line::from(Span::styled(
            "Keyboard Shortcuts",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  q           Quit"),
        Line::from("  Esc, ←      Back (drill up)"),
        Line::from("  Tab, →      Drill into selected agent"),
        Line::from("  ↑/↓, j/k   Move selection up/down"),
        Line::from("  Enter       Attach to selected agent"),
        Line::from("  n           Spawn new agent"),
        Line::from("  d           Kill selected agent"),
        Line::from("  p           Add a project"),
        Line::from("  e           Show scheduled events"),
        Line::from("  D           Debug console"),
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

fn render_events_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(70, 60, frame.area());

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Scheduled Events",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if app.scheduled_events.is_empty() {
        lines.push(Line::from(Span::styled(
            "No events in queue",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Header
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<14} {:<14} {:<24} ", "Sender", "Receiver", "Timestamp"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Payload",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "─".repeat(70),
            Style::default().fg(Color::DarkGray),
        )));

        for event in &app.scheduled_events {
            // Format timestamp as human-readable relative time
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            let time_str = if event.timestamp <= now_ns {
                "overdue".to_string()
            } else {
                let diff_ms = (event.timestamp - now_ns) / 1_000_000;
                if diff_ms < 1000 {
                    format!("in {}ms", diff_ms)
                } else if diff_ms < 60_000 {
                    format!("in {:.1}s", diff_ms as f64 / 1000.0)
                } else if diff_ms < 3_600_000 {
                    format!("in {:.1}m", diff_ms as f64 / 60_000.0)
                } else {
                    format!("in {:.1}h", diff_ms as f64 / 3_600_000.0)
                }
            };

            // Truncate payload to fit
            let payload = if event.payload.len() > 30 {
                format!("{}...", &event.payload[..27])
            } else {
                event.payload.clone()
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<14}", truncate_str(&event.sender, 13)),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("{:<14}", truncate_str(&event.receiver, 13)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{:<24}", time_str),
                    Style::default().fg(Color::White),
                ),
                Span::raw(payload),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press Esc or 'e' to close",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .title(" Events Queue ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_debug_console(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 40, frame.area());

    let messages = app.ticker.latest(10);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Debug Console",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "No messages yet",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, msg) in messages.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>2}. ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(msg.clone(), Style::default().fg(Color::Yellow)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press Esc or 'D' to close",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .title(" Debug Console ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}…", &s[..max_len - 1])
    } else {
        s.to_string()
    }
}

fn format_countdown_ns(target_ns: u64, now_ns: u64) -> String {
    if target_ns <= now_ns {
        return "due now".to_string();
    }

    let total_secs = (target_ns - now_ns) / 1_000_000_000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if hours > 0 {
        format!("in {}:{:02}:{:02}", hours, mins, secs)
    } else {
        format!("in {:02}:{:02}", mins, secs)
    }
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
