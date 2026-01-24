use crossterm::event::{self, Event, KeyEvent};
use std::time::Duration;
use tokio::sync::mpsc;

/// Application events
#[derive(Debug)]
pub enum AppEvent {
    /// A key was pressed
    Key(KeyEvent),
    /// Time to refresh the display
    Tick,
    /// Terminal was resized
    #[allow(dead_code)]
    Resize(u16, u16),
}

/// Handles input events and tick timing
pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<AppEvent>,
    _tx: mpsc::UnboundedSender<AppEvent>,
}

impl EventHandler {
    /// Create a new event handler with the given tick rate
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let event_tx = tx.clone();

        // Spawn event polling task
        tokio::spawn(async move {
            let mut tick_interval = tokio::time::interval(tick_rate);

            loop {
                let event = tokio::select! {
                    _ = tick_interval.tick() => {
                        AppEvent::Tick
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        // Poll for crossterm events
                        if event::poll(Duration::from_millis(0)).unwrap_or(false) {
                            match event::read() {
                                Ok(Event::Key(key)) => AppEvent::Key(key),
                                Ok(Event::Resize(w, h)) => AppEvent::Resize(w, h),
                                _ => continue,
                            }
                        } else {
                            continue;
                        }
                    }
                };

                if event_tx.send(event).is_err() {
                    break;
                }
            }
        });

        Self { rx, _tx: tx }
    }

    /// Get the next event
    pub async fn next(&mut self) -> Option<AppEvent> {
        self.rx.recv().await
    }
}
