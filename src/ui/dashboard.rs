use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
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
            Constraint::Percentage(40), // Agent grid
            Constraint::Min(10),        // Manager panel (takes remaining ~50%)
            Constraint::Length(1),      // Help bar
        ])
        .split(frame.area());

    render_status_bar(frame, app, chunks[0]);
    render_agent_grid(frame, app, chunks[1]);
    render_manager_panel(frame, app, chunks[2]);
    render_help_bar(frame, app, chunks[3]);

    // Render overlays
    if app.show_help {
        render_help_popup(frame);
    }

    if app.show_confirm_kill {
        render_confirm_kill(frame, app);
    }
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let (working, waiting, idle, stuck) = app.health_counts();
    let total = app.total_agents();

    let status_text = vec![
        Span::styled("One-Man Army ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("| Agents: "),
        Span::styled(format!("{}", total), Style::default().fg(Color::White)),
        Span::raw(" | "),
        Span::styled(
            format!("{} Working", working),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{} Waiting", waiting),
            Style::default().fg(Color::Blue),
        ),
        Span::raw(" "),
        Span::styled(format!("{} Idle", idle), Style::default().fg(Color::Yellow)),
        Span::raw(" "),
        Span::styled(format!("{} Stuck", stuck), Style::default().fg(Color::Red)),
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
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(content).block(block);
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
                        .title(" Agents ")
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
        frame.render_widget(empty_msg, area);
        return;
    }

    // Max 2 columns for better readability
    let cols = 2.min(app.agents.len()).max(1);
    let rows = app.agents.len().div_ceil(cols);

    // Calculate row height to fill available space
    let row_height = area.height / rows as u16;
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Length(row_height.max(8)))
        .collect();

    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for (i, agent) in app.agents.iter().enumerate() {
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
            let is_interactive = app.interactive_mode && is_selected;
            render_agent_card(
                frame,
                app,
                agent,
                col_chunks[col],
                is_selected,
                is_interactive,
            );
        }
    }
}

fn render_manager_panel(frame: &mut Frame, app: &App, area: Rect) {
    if app.manager.is_some() {
        let is_selected = app.manager_selected;
        let is_interactive = app.interactive_mode;

        // Different styles for different states
        let (border_color, title) = if is_interactive {
            (Color::Cyan, " MANAGER [INTERACTIVE - Esc to exit] ")
        } else if is_selected {
            (Color::Magenta, " [MANAGER] - Press 'i' to type ")
        } else {
            (Color::Blue, " MANAGER ")
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
            .title(" MANAGER ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let paragraph = Paragraph::new("Starting manager...")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);

        frame.render_widget(paragraph, area);
    }
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
        HealthState::Working => (Color::Green, "●"),
        HealthState::WaitingForInput => (Color::Blue, "◆"),
        HealthState::Idle => (Color::Yellow, "○"),
        HealthState::Stuck => (Color::Red, "✖"),
    };

    // Border color based on state
    let border_color = if interactive {
        Color::Cyan
    } else if selected {
        Color::Magenta
    } else {
        health_color
    };

    let border_style = Style::default()
        .fg(border_color)
        .add_modifier(if selected || interactive {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });

    // Title with status indicator
    let title = if interactive {
        format!(" {} [INTERACTIVE - Esc] ", &agent.session.name)
    } else if selected {
        format!(" [{}] {} - 'i' to type ", status_icon, &agent.session.name)
    } else {
        format!(" {} {} ", status_icon, &agent.session.name)
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
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
            "manager"
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
