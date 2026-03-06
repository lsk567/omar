pub mod dashboard;
mod dataflow;

use crate::app::{App, ViewMode};
use ratatui::Frame;

pub fn render(frame: &mut Frame, app: &App) {
    match app.view_mode {
        ViewMode::Flat => dashboard::render(frame, app),
        ViewMode::Dataflow => dataflow::render(frame, app),
    }
}
