use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use tokio::sync::mpsc;

/// Application events
#[derive(Debug)]
pub enum AppEvent {
    /// A key was pressed
    Key(KeyEvent),
    /// Time to refresh the display
    Tick,
    /// Advance the scrolling ticker by one character
    TickerScroll,
    /// Terminal was resized
    #[allow(dead_code)]
    Resize(u16, u16),
}

fn app_event_from_crossterm(event: Event) -> Option<AppEvent> {
    match event {
        Event::Key(key) => Some(AppEvent::Key(key)),
        Event::Resize(w, h) => Some(AppEvent::Resize(w, h)),
        _ => None,
    }
}

fn coalesce_alt_arrow(first: KeyEvent, next: Option<Event>) -> (AppEvent, Option<AppEvent>) {
    if first.code == KeyCode::Esc && first.modifiers.is_empty() {
        if let Some(Event::Key(next_key)) = next {
            if next_key.modifiers.is_empty()
                && matches!(next_key.code, KeyCode::Left | KeyCode::Right)
            {
                return (
                    AppEvent::Key(KeyEvent::new(next_key.code, KeyModifiers::ALT)),
                    None,
                );
            }

            return (AppEvent::Key(first), Some(AppEvent::Key(next_key)));
        }

        return (
            AppEvent::Key(first),
            next.and_then(app_event_from_crossterm),
        );
    }

    (
        AppEvent::Key(first),
        next.and_then(app_event_from_crossterm),
    )
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
            let mut ticker_interval = tokio::time::interval(Duration::from_millis(150));
            let mut pending_event = None;

            loop {
                if let Some(event) = pending_event.take() {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                    continue;
                }

                let event = tokio::select! {
                    _ = tick_interval.tick() => {
                        AppEvent::Tick
                    }
                    _ = ticker_interval.tick() => {
                        AppEvent::TickerScroll
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        // Poll for crossterm events
                        if event::poll(Duration::from_millis(0)).unwrap_or(false) {
                            match event::read() {
                                Ok(Event::Key(key)) if key.code == KeyCode::Esc && key.modifiers.is_empty() => {
                                    let next = if event::poll(Duration::from_millis(10)).unwrap_or(false) {
                                        event::read().ok()
                                    } else {
                                        None
                                    };
                                    let (event, pending) = coalesce_alt_arrow(key, next);
                                    pending_event = pending;
                                    event
                                }
                                Ok(event) => match app_event_from_crossterm(event) {
                                    Some(event) => event,
                                    None => continue,
                                },
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

    /// Drain all buffered events, discarding them.
    /// Call after a blocking operation (e.g. tmux popup) to skip
    /// stale ticks and only process fresh events going forward.
    pub fn drain(&mut self) {
        while self.rx.try_recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_escape_left_into_alt_left() {
        let (event, pending) = coalesce_alt_arrow(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Left,
                KeyModifiers::empty(),
            ))),
        );

        match event {
            AppEvent::Key(key) => {
                assert_eq!(key.code, KeyCode::Left);
                assert!(key.modifiers.contains(KeyModifiers::ALT));
            }
            _ => panic!("expected key event"),
        }
        assert!(pending.is_none());
    }

    #[test]
    fn preserves_plain_escape_without_followup() {
        let (event, pending) =
            coalesce_alt_arrow(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()), None);

        match event {
            AppEvent::Key(key) => {
                assert_eq!(key.code, KeyCode::Esc);
                assert!(key.modifiers.is_empty());
            }
            _ => panic!("expected key event"),
        }
        assert!(pending.is_none());
    }

    #[test]
    fn preserves_non_arrow_followup_after_escape() {
        let (event, pending) = coalesce_alt_arrow(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::empty(),
            ))),
        );

        match event {
            AppEvent::Key(key) => {
                assert_eq!(key.code, KeyCode::Esc);
                assert!(key.modifiers.is_empty());
            }
            _ => panic!("expected key event"),
        }

        match pending {
            Some(AppEvent::Key(key)) => {
                assert_eq!(key.code, KeyCode::Char('x'));
                assert!(key.modifiers.is_empty());
            }
            _ => panic!("expected pending key event"),
        }
    }
}
