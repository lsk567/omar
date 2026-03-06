use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::App;
use crate::tmux::HealthState;

/// Render the dataflow (LF-style reactor) view.
///
/// Layout:
///   ┌─ Status bar ──────────────────────────────┐
///   │ One-Man Army | Agents: 5 | ...            │
///   ├───────────────────────────────────────────┤
///   │                                           │
///   │  ┌─ EA ●──────┐                           │
///   │  │ timer: 2m  │                           │
///   │  └──┬────┬────┘                           │
///   │     │    │                                │
///   │  ┌──▼──┐ ┌──▼──────┐                      │
///   │  │pm-a │ │pm-b     │                      │
///   │  │  ●  │ │  ○      │                      │
///   │  └──┬──┘ └─────────┘                      │
///   │     │                                     │
///   │  ┌──▼──┐                                  │
///   │  │w-1  │                                  │
///   │  │  ●  │                                  │
///   │  └─────┘                                  │
///   │                                           │
///   ├───────────────────────────────────────────┤
///   │ ↑↓:Nav Tab:Flat view ...                  │
///   └───────────────────────────────────────────┘
pub fn render(frame: &mut Frame, app: &App) {
    let status_height = if app.status_message.is_some() { 4 } else { 3 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_height),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(frame.area());

    // Reuse the existing status bar renderer
    super::dashboard::render_status_bar_pub(frame, app, chunks[0]);

    render_dataflow_canvas(frame, app, chunks[1]);

    render_dataflow_help_bar(frame, app, chunks[2]);
}

/// Render the main dataflow canvas with reactor boxes and connections
fn render_dataflow_canvas(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Dataflow View ")
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Blue));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 10 || inner.height < 5 {
        return;
    }

    // Build the tree structure for layout
    let tree = &app.command_tree;
    if tree.is_empty() {
        let msg = Paragraph::new("No agents running.").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(msg, inner);
        return;
    }

    // Group nodes by depth for horizontal layering
    let max_depth = tree.iter().map(|n| n.depth).max().unwrap_or(0);

    // Collect nodes per depth level
    let mut levels: Vec<Vec<usize>> = vec![Vec::new(); max_depth + 1];
    for (i, node) in tree.iter().enumerate() {
        levels[node.depth].push(i);
    }

    // Calculate vertical space per level
    let total_levels = levels.len() as u16;
    // Each level needs: reactor box (3 rows) + connection line (1 row) = 4 rows
    // Except last level needs no connection line
    let rows_per_level = 4u16;
    let available_rows = inner.height;

    // If not enough space, compress
    let actual_rows = rows_per_level.min(available_rows / total_levels.max(1));

    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    for (depth, node_indices) in levels.iter().enumerate() {
        if node_indices.is_empty() {
            continue;
        }

        let y_offset = depth as u16 * actual_rows;
        if y_offset >= available_rows {
            break;
        }

        let box_height = (actual_rows - 1).max(3).min(available_rows - y_offset);
        let nodes_count = node_indices.len() as u16;

        // Calculate width per reactor box
        let box_width = (inner.width / nodes_count.max(1)).clamp(12, 30);
        let total_width = box_width * nodes_count;
        let left_pad = (inner.width.saturating_sub(total_width)) / 2;

        for (col, &node_idx) in node_indices.iter().enumerate() {
            let node = &tree[node_idx];

            let box_x = inner.x + left_pad + (col as u16) * box_width;
            let box_y = inner.y + y_offset;

            if box_x + box_width > inner.x + inner.width {
                break;
            }

            let box_area = Rect::new(
                box_x,
                box_y,
                box_width.min(inner.x + inner.width - box_x),
                box_height.min(3),
            );

            render_reactor_box(frame, app, node_idx, box_area, now_ns);

            // Draw connection line down to children (if any and space permits)
            if depth < max_depth && box_y + box_height < inner.y + available_rows {
                let connector_y = box_y + box_height.min(3);
                if connector_y < inner.y + available_rows {
                    // Find children of this node
                    let has_children = tree.iter().any(|n| {
                        n.depth == depth + 1
                            && is_child_of(app, &n.session_name, &node.session_name)
                    });
                    if has_children {
                        let mid_x = box_x + box_width / 2;
                        let connector_area = Rect::new(mid_x, connector_y, 1, 1);
                        let connector =
                            Paragraph::new("│").style(Style::default().fg(Color::DarkGray));
                        frame.render_widget(connector, connector_area);
                    }
                }
            }
        }
    }
}

/// Check if child_session is a direct child of parent_session
fn is_child_of(app: &App, child_session: &str, parent_session: &str) -> bool {
    use crate::manager::MANAGER_SESSION;
    if parent_session == MANAGER_SESSION {
        // EA's children: agents whose parent is MANAGER_SESSION or orphans
        app.agent_parents()
            .get(child_session)
            .map(|p| p == MANAGER_SESSION)
            .unwrap_or(true) // orphans are also EA children
    } else {
        app.agent_parents()
            .get(child_session)
            .map(|p| p == parent_session)
            .unwrap_or(false)
    }
}

/// Render a single reactor box
fn render_reactor_box(frame: &mut Frame, app: &App, node_idx: usize, area: Rect, now_ns: u64) {
    let node = &app.command_tree[node_idx];

    let (health_color, icon) = match node.health {
        HealthState::Running => (Color::Green, "●"),
        HealthState::Idle => (Color::Yellow, "○"),
    };

    // Check for scheduled events targeting this agent
    let next_event = app
        .scheduled_events
        .iter()
        .filter(|e| {
            e.receiver == node.session_name || {
                // Also match short name
                node.session_name
                    .strip_prefix(app.client().prefix())
                    .map(|short| e.receiver == short)
                    .unwrap_or(false)
            }
        })
        .min_by_key(|e| e.timestamp);

    let timer_str = next_event.map(|e| {
        if e.timestamp <= now_ns {
            "due".to_string()
        } else {
            let secs = (e.timestamp - now_ns) / 1_000_000_000;
            if secs < 60 {
                format!("{}s", secs)
            } else {
                format!("{}m", secs / 60)
            }
        }
    });

    // Build title
    let title_line = Line::from(vec![
        Span::styled(format!(" {} ", icon), Style::default().fg(health_color)),
        Span::styled(
            &node.name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);

    let border_color = match node.health {
        HealthState::Running => Color::Green,
        HealthState::Idle => Color::DarkGray,
    };

    let block = Block::default()
        .title(title_line)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));

    // Inner content: timer + child count
    let mut content_spans: Vec<Span> = Vec::new();

    if let Some(ref timer) = timer_str {
        content_spans.push(Span::styled("⏱ ", Style::default().fg(Color::Cyan)));
        content_spans.push(Span::styled(
            timer.clone(),
            Style::default().fg(Color::Magenta),
        ));
    }

    let child_count = app.child_count(&node.session_name);
    if child_count > 0 {
        if !content_spans.is_empty() {
            content_spans.push(Span::raw(" "));
        }
        content_spans.push(Span::styled(
            format!("▼{}", child_count),
            Style::default().fg(Color::Cyan),
        ));
    }

    let content = if content_spans.is_empty() {
        vec![]
    } else {
        vec![Line::from(content_spans)]
    };

    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_dataflow_help_bar(frame: &mut Frame, _app: &App, area: Rect) {
    let help_text = vec![
        Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Flat view "),
        Span::styled("Q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Quit "),
        Span::styled("z", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Hold the line "),
        Span::styled("?", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Help"),
    ];

    let paragraph =
        Paragraph::new(Line::from(help_text)).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}
