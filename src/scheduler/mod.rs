pub mod event;

pub use event::ScheduledEvent;

use std::collections::{BinaryHeap, VecDeque};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;

/// A ticker message with its creation time.
struct TickerEntry {
    text: String,
    created_at: Instant,
}

/// Thread-safe scrolling ticker buffer shared between scheduler and UI.
#[derive(Clone)]
pub struct TickerBuffer {
    entries: Arc<Mutex<VecDeque<TickerEntry>>>,
}

impl TickerBuffer {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Push a new message into the ticker. Caps at 50 entries.
    pub fn push(&self, msg: impl Into<String>) {
        let mut buf = self.entries.lock().unwrap();
        if buf.len() >= 50 {
            buf.pop_front();
        }
        buf.push_back(TickerEntry {
            text: msg.into(),
            created_at: Instant::now(),
        });
    }

    /// Return the joined ticker content, pruning entries older than `ttl`.
    pub fn render(&self, ttl: std::time::Duration) -> String {
        let mut buf = self.entries.lock().unwrap();
        let now = Instant::now();
        buf.retain(|e| now.duration_since(e.created_at) < ttl);
        buf.iter()
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join(" +++ ")
    }

    /// Return the last `n` messages regardless of age (for debug console).
    pub fn latest(&self, n: usize) -> Vec<String> {
        let buf = self.entries.lock().unwrap();
        buf.iter()
            .rev()
            .take(n)
            .map(|e| e.text.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

pub struct Scheduler {
    queue: Mutex<BinaryHeap<ScheduledEvent>>,
    notify: Notify,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            notify: Notify::new(),
        }
    }

    pub fn insert(&self, event: ScheduledEvent) {
        self.queue.lock().unwrap().push(event);
        self.notify.notify_one();
    }

    pub fn cancel(&self, event_id: &str) -> Option<ScheduledEvent> {
        let mut queue = self.queue.lock().unwrap();
        let events: Vec<ScheduledEvent> = queue.drain().collect();
        let mut cancelled = None;
        let mut remaining = BinaryHeap::new();
        for ev in events {
            if ev.id == event_id && cancelled.is_none() {
                cancelled = Some(ev);
            } else {
                remaining.push(ev);
            }
        }
        *queue = remaining;
        cancelled
    }

    pub fn list(&self) -> Vec<ScheduledEvent> {
        let queue = self.queue.lock().unwrap();
        queue.iter().cloned().collect()
    }

    pub fn list_by_receiver(&self, receiver: &str) -> Vec<ScheduledEvent> {
        let queue = self.queue.lock().unwrap();
        queue
            .iter()
            .filter(|e| e.receiver == receiver)
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    pub fn peek_next_timestamp(&self) -> Option<u64> {
        let queue = self.queue.lock().unwrap();
        queue.peek().map(|e| e.timestamp)
    }

    /// Pop all events matching the given receiver and timestamp.
    pub fn pop_batch(&self, receiver: &str, timestamp: u64) -> Vec<ScheduledEvent> {
        let mut queue = self.queue.lock().unwrap();
        let events: Vec<ScheduledEvent> = queue.drain().collect();
        let mut batch = Vec::new();
        let mut remaining = BinaryHeap::new();
        for ev in events {
            if ev.receiver == receiver && ev.timestamp == timestamp {
                batch.push(ev);
            } else {
                remaining.push(ev);
            }
        }
        *queue = remaining;
        batch
    }
}

pub(crate) fn deliver_to_tmux(receiver: &str, message: &str, ticker: &TickerBuffer) {
    let target = format!("omar-agent-{}", receiver);
    let result = Command::new("tmux")
        .args(["send-keys", "-t", &target, "-l", message])
        .output();

    match result {
        Ok(output) if output.status.success() => {
            // Send Enter to submit the message
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", &target, "Enter"])
                .output();
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ticker.push(format!("tmux send-keys failed for {}: {}", target, stderr));
        }
        Err(e) => {
            ticker.push(format!("failed to run tmux for {}: {}", target, e));
        }
    }
}

fn format_delivery(events: &[ScheduledEvent], timestamp: u64) -> String {
    if events.len() == 1 {
        let ev = &events[0];
        format!(
            "[EVENT at t={}]\nFrom {}: {}",
            timestamp, ev.sender, ev.payload
        )
    } else {
        let mut msg = format!("[EVENT BATCH at t={}]", timestamp);
        for ev in events {
            msg.push_str(&format!("\nFrom {}: {}", ev.sender, ev.payload));
        }
        msg
    }
}

pub async fn run_event_loop(scheduler: Arc<Scheduler>, ticker: TickerBuffer) {
    loop {
        let next_ts = {
            let queue = scheduler.queue.lock().unwrap();
            queue.peek().map(|e| e.timestamp)
        };

        match next_ts {
            None => {
                // No events — wait for a notification.
                scheduler.notify.notified().await;
                continue;
            }
            Some(ts) => {
                let now = now_ns();
                if ts > now {
                    let sleep_ns = ts - now;
                    let duration = std::time::Duration::from_nanos(sleep_ns);
                    tokio::select! {
                        _ = tokio::time::sleep(duration) => {
                            // Timer fired — fall through to delivery.
                        }
                        _ = scheduler.notify.notified() => {
                            // Queue changed — re-check from the top.
                            continue;
                        }
                    }
                }

                // Deliver all events at the earliest timestamp, grouped by receiver.
                let earliest_ts = {
                    let queue = scheduler.queue.lock().unwrap();
                    match queue.peek() {
                        Some(e) if e.timestamp <= now_ns() => e.timestamp,
                        _ => continue,
                    }
                };

                // Collect all receivers that have events at this timestamp.
                let receivers: Vec<String> = {
                    let queue = scheduler.queue.lock().unwrap();
                    let mut seen = Vec::new();
                    for ev in queue.iter() {
                        if ev.timestamp == earliest_ts && !seen.contains(&ev.receiver) {
                            seen.push(ev.receiver.clone());
                        }
                    }
                    seen
                };

                for receiver in &receivers {
                    let batch = scheduler.pop_batch(receiver, earliest_ts);
                    if batch.is_empty() {
                        continue;
                    }
                    let message = format_delivery(&batch, earliest_ts);
                    deliver_to_tmux(receiver, &message, &ticker);

                    let lag_ns = now_ns().saturating_sub(earliest_ts);
                    let lag_ms = lag_ns as f64 / 1_000_000.0;
                    ticker.push(format!(
                        "delivered {} event(s) to {}, lag={:.2}ms",
                        batch.len(),
                        receiver,
                        lag_ms
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(receiver: &str, sender: &str, timestamp: u64, payload: &str) -> ScheduledEvent {
        ScheduledEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sender: sender.to_string(),
            receiver: receiver.to_string(),
            timestamp,
            payload: payload.to_string(),
            created_at: now_ns(),
        }
    }

    #[test]
    fn test_insert_and_peek() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 200, "hello"));
        sched.insert(make_event("bob", "alice", 100, "earlier"));

        assert_eq!(sched.peek_next_timestamp(), Some(100));
    }

    #[test]
    fn test_cancel() {
        let sched = Scheduler::new();
        let ev = make_event("bob", "alice", 100, "cancel me");
        let id = ev.id.clone();
        sched.insert(ev);
        sched.insert(make_event("bob", "alice", 200, "keep"));

        let cancelled = sched.cancel(&id);
        assert!(cancelled.is_some());
        assert_eq!(cancelled.unwrap().payload, "cancel me");
        assert_eq!(sched.list().len(), 1);
    }

    #[test]
    fn test_cancel_nonexistent() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "keep"));
        assert!(sched.cancel("no-such-id").is_none());
        assert_eq!(sched.list().len(), 1);
    }

    #[test]
    fn test_list_by_receiver() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "for bob"));
        sched.insert(make_event("carol", "alice", 200, "for carol"));
        sched.insert(make_event("bob", "dave", 300, "also for bob"));

        let bob_events = sched.list_by_receiver("bob");
        assert_eq!(bob_events.len(), 2);
        assert!(bob_events.iter().all(|e| e.receiver == "bob"));

        let carol_events = sched.list_by_receiver("carol");
        assert_eq!(carol_events.len(), 1);
    }

    #[test]
    fn test_pop_batch() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "a"));
        sched.insert(make_event("bob", "carol", 100, "b"));
        sched.insert(make_event("bob", "dave", 200, "c"));
        sched.insert(make_event("carol", "alice", 100, "d"));

        let batch = sched.pop_batch("bob", 100);
        assert_eq!(batch.len(), 2);
        assert!(batch
            .iter()
            .all(|e| e.receiver == "bob" && e.timestamp == 100));

        // Remaining: bob@200 and carol@100
        assert_eq!(sched.list().len(), 2);
    }

    #[test]
    fn test_format_single_event() {
        let ev = make_event("bob", "alice", 1000, "hello world");
        let msg = format_delivery(&[ev], 1000);
        assert!(msg.contains("[EVENT at t=1000]"));
        assert!(msg.contains("From alice: hello world"));
        assert!(!msg.contains("BATCH"));
    }

    #[test]
    fn test_format_batch() {
        let e1 = make_event("bob", "alice", 1000, "msg1");
        let e2 = make_event("bob", "carol", 1000, "msg2");
        let msg = format_delivery(&[e1, e2], 1000);
        assert!(msg.contains("[EVENT BATCH at t=1000]"));
        assert!(msg.contains("From alice: msg1"));
        assert!(msg.contains("From carol: msg2"));
    }

    /// Helper: check if tmux is available on this machine.
    fn tmux_available() -> bool {
        Command::new("tmux").arg("-V").output().is_ok()
    }

    /// Helper: create a tmux session, returning true if successful.
    fn create_test_session(name: &str) -> bool {
        Command::new("tmux")
            .args(["new-session", "-d", "-s", name, "-x", "200", "-y", "50"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Helper: kill a tmux session (best-effort cleanup).
    fn kill_test_session(name: &str) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", name])
            .output();
    }

    /// Helper: capture pane content from a tmux session.
    fn capture_pane(name: &str) -> String {
        Command::new("tmux")
            .args(["capture-pane", "-t", name, "-p"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn test_deliver_to_tmux() {
        if !tmux_available() {
            eprintln!("skipping test_deliver_to_tmux: tmux not available");
            return;
        }

        let session = "omar-agent-test-deliver";
        // Clean up any leftover session from a previous run
        kill_test_session(session);

        if !create_test_session(session) {
            eprintln!("skipping test_deliver_to_tmux: could not create tmux session");
            return;
        }

        // deliver_to_tmux prepends "omar-agent-" to the receiver name
        let ticker = TickerBuffer::new();
        deliver_to_tmux("test-deliver", "hello-from-scheduler", &ticker);

        // Give tmux a moment to process the send-keys
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let content = capture_pane(session);
        kill_test_session(session);

        assert!(
            content.contains("hello-from-scheduler"),
            "expected pane to contain 'hello-from-scheduler', got: {}",
            content
        );
    }

    #[tokio::test]
    async fn test_scheduler_event_delivery_cycle() {
        if !tmux_available() {
            eprintln!("skipping test_scheduler_event_delivery_cycle: tmux not available");
            return;
        }

        let session = "omar-agent-test-sched";
        kill_test_session(session);

        if !create_test_session(session) {
            eprintln!(
                "skipping test_scheduler_event_delivery_cycle: could not create tmux session"
            );
            return;
        }

        let scheduler = Arc::new(Scheduler::new());

        // Insert an event with timestamp=1 (immediately due — way in the past)
        let event = ScheduledEvent {
            id: "test-cycle-1".to_string(),
            sender: "alice".to_string(),
            receiver: "test-sched".to_string(),
            timestamp: 1,
            payload: "cycle-delivery-payload".to_string(),
            created_at: now_ns(),
        };
        scheduler.insert(event);

        // Spawn the event loop with a timeout
        let sched = Arc::clone(&scheduler);
        let ticker = TickerBuffer::new();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = run_event_loop(sched, ticker) => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
            }
        });

        // Wait for delivery
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let content = capture_pane(session);
        kill_test_session(session);

        // Abort the event loop task
        handle.abort();
        let _ = handle.await;

        assert!(
            content.contains("cycle-delivery-payload"),
            "expected pane to contain 'cycle-delivery-payload', got: {}",
            content
        );
    }

    // ── TickerBuffer tests ──

    #[test]
    fn test_ticker_push_and_render() {
        let ticker = TickerBuffer::new();
        ticker.push("hello");
        ticker.push("world");
        let out = ticker.render(std::time::Duration::from_secs(30));
        assert_eq!(out, "hello +++ world");
    }

    #[test]
    fn test_ticker_empty_render() {
        let ticker = TickerBuffer::new();
        let out = ticker.render(std::time::Duration::from_secs(30));
        assert_eq!(out, "");
    }

    #[test]
    fn test_ticker_expiry() {
        let ticker = TickerBuffer::new();
        ticker.push("old");
        // Render with zero TTL — everything expires immediately
        let out = ticker.render(std::time::Duration::ZERO);
        assert_eq!(out, "");
    }

    #[test]
    fn test_ticker_capacity_cap() {
        let ticker = TickerBuffer::new();
        for i in 0..60 {
            ticker.push(format!("msg{}", i));
        }
        let buf = ticker.entries.lock().unwrap();
        assert_eq!(buf.len(), 50);
        // Oldest entries should have been dropped
        assert_eq!(buf.front().unwrap().text, "msg10");
    }
}
