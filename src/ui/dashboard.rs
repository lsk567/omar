use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::app::{AgentInfo, App};
use crate::tmux::HealthState;

/// Render the entire dashboard
pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Status bar
            Constraint::Min(0),    // Agent grid
            Constraint::Length(1), // Help bar
        ])
        .split(frame.area());

    render_status_bar(frame, app, chunks[0]);
    render_agent_grid(frame, app, chunks[1]);
    render_help_bar(frame, chunks[2]);

    // Render overlays
    if app.show_help {
        render_help_popup(frame);
    }

    if app.show_confirm_kill {
        render_confirm_kill(frame, app);
    }
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let (ok, idle, stuck) = app.health_counts();
    let total = app.agents.len();

    let status_text = vec![
        Span::styled("OMA ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("| Agents: "),
        Span::styled(format!("{}", total), Style::default().fg(Color::White)),
        Span::raw(" | "),
        Span::styled(format!("{} OK", ok), Style::default().fg(Color::Green)),
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

    let cols = 3.min(app.agents.len()).max(1);
    let rows = app.agents.len().div_ceil(cols);

    // Create row constraints
    let row_constraints: Vec<Constraint> = (0..rows).map(|_| Constraint::Length(7)).collect();

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
            let is_selected = i == app.selected;
            render_agent_card(frame, agent, col_chunks[col], is_selected);
        }
    }
}

fn render_agent_card(frame: &mut Frame, agent: &AgentInfo, area: Rect, selected: bool) {
    let (border_color, status_icon) = match agent.health {
        HealthState::Ok => (Color::Green, "●"),
        HealthState::Idle => (Color::Yellow, "○"),
        HealthState::Stuck => (Color::Red, "✖"),
    };

    let border_style = if selected {
        Style::default()
            .fg(border_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(border_color)
    };

    // Extract short name (remove prefix)
    let short_name = agent
        .session
        .name
        .split('-')
        .next_back()
        .unwrap_or(&agent.session.name);

    let title = if selected {
        format!(" [{}] ", short_name)
    } else {
        format!(" {} ", short_name)
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let idle_display = agent.health_info.idle_display();
    let status_color = match agent.health {
        HealthState::Ok => Color::Green,
        HealthState::Idle => Color::Yellow,
        HealthState::Stuck => Color::Red,
    };

    let content = vec![
        Line::from(vec![
            Span::styled(status_icon, Style::default().fg(status_color)),
            Span::raw(" "),
            Span::styled(
                agent.health.as_str().to_uppercase(),
                Style::default().fg(status_color),
            ),
        ]),
        Line::from(vec![
            Span::raw("Idle: "),
            Span::styled(idle_display, Style::default().fg(Color::White)),
        ]),
        Line::from(Span::styled(
            truncate(
                &agent.health_info.last_output,
                area.width.saturating_sub(4) as usize,
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn render_help_bar(frame: &mut Frame, area: Rect) {
    let help_text = vec![
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
    ];

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

/// Truncate a string to fit within max_len (char-aware for UTF-8)
fn truncate(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else if max_len > 3 {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    } else {
        s.chars().take(max_len).collect()
    }
}
