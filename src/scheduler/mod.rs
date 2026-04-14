pub mod event;

pub use event::ScheduledEvent;

use std::collections::{BinaryHeap, VecDeque};
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

    /// Return the joined ticker content, filtering entries older than `ttl`.
    /// Does NOT prune the buffer — old entries remain for `latest()` / debug console.
    ///
    /// When there are many recent messages, collapses them into a summary
    /// (e.g. "3 errors, 2 info — press D for details") to avoid spilling
    /// across the dashboard.
    pub fn render(&self, ttl: std::time::Duration) -> String {
        let buf = self.entries.lock().unwrap();
        let now = Instant::now();
        let recent: Vec<&str> = buf
            .iter()
            .filter(|e| now.duration_since(e.created_at) < ttl)
            .map(|e| e.text.as_str())
            .collect();

        if recent.len() <= 2 {
            return recent.join(" +++ ");
        }

        // Collapse into a summary when there are many messages.
        let errors = recent
            .iter()
            .filter(|t| t.contains("failed") || t.contains("error") || t.contains("Error"))
            .count();
        let other = recent.len() - errors;

        let mut parts = Vec::new();
        if errors > 0 {
            parts.push(format!("{} error(s)", errors));
        }
        if other > 0 {
            parts.push(format!("{} info", other));
        }
        parts.push("press D for details".to_string());
        parts.join(", ")
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

    /// Cancel all events for a given receiver. Returns the number cancelled.
    pub fn cancel_by_receiver(&self, receiver: &str) -> usize {
        let mut queue = self.queue.lock().unwrap();
        let events: Vec<ScheduledEvent> = queue.drain().collect();
        let mut count = 0;
        let mut remaining = BinaryHeap::new();
        for ev in events {
            if ev.receiver == receiver {
                count += 1;
            } else {
                remaining.push(ev);
            }
        }
        *queue = remaining;
        count
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

pub(crate) fn deliver_to_tmux(
    receiver: &str,
    message: &str,
    event_count: usize,
    scheduled_ts: u64,
    ticker: &TickerBuffer,
) {
    // Use the reliable delivery path so scheduled events / inter-agent messages
    // land deterministically regardless of backend startup / input buffering.
    //
    // Scheduler targets are already-running agents (not fresh spawns), so use
    // tighter timeouts than the spawn-agent defaults.
    let target = format!("omar-agent-{}", receiver);
    let client = crate::tmux::TmuxClient::new("omar-agent-");
    let opts = crate::tmux::DeliveryOptions {
        startup_timeout: std::time::Duration::from_secs(3),
        stable_quiet: std::time::Duration::from_millis(200),
        verify_timeout: std::time::Duration::from_secs(2),
        max_retries: 3,
        poll_interval: std::time::Duration::from_millis(50),
        retry_delay: std::time::Duration::from_millis(150),
    };
    match client.deliver_prompt(&target, message, &opts) {
        Ok(()) => {
            let lag_ms = now_ns().saturating_sub(scheduled_ts) as f64 / 1_000_000.0;
            ticker.push(format!(
                "delivered {} event(s) to {}, lag={:.2}ms",
                event_count, receiver, lag_ms
            ));
        }
        Err(e) => {
            ticker.push(format!("event delivery failed for {}: {}", target, e));
        }
    }
}

fn format_delivery(events: &[ScheduledEvent], timestamp: u64) -> String {
    if events.len() == 1 {
        let ev = &events[0];
        let tag = if ev.recurring_ns.is_some() {
            "CRON"
        } else {
            "EVENT"
        };
        format!(
            "[{} at t={}]\nFrom {}: {}",
            tag, timestamp, ev.sender, ev.payload
        )
    } else {
        let mut msg = format!("[EVENT BATCH at t={}]", timestamp);
        for ev in events {
            let tag = if ev.recurring_ns.is_some() {
                "CRON"
            } else {
                "EVENT"
            };
            msg.push_str(&format!("\n[{}] From {}: {}", tag, ev.sender, ev.payload));
        }
        msg
    }
}

/// Shared state: the short name of the agent whose popup is currently open, if any.
pub type PopupReceiver = Arc<Mutex<Option<String>>>;

pub fn new_popup_receiver() -> PopupReceiver {
    Arc::new(Mutex::new(None))
}

pub async fn run_event_loop(
    scheduler: Arc<Scheduler>,
    ticker: TickerBuffer,
    popup_receiver: PopupReceiver,
) {
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
                    // If the user has a popup open for this receiver, defer by 30s.
                    // The event will keep getting rescheduled on each attempt until
                    // the popup is closed.
                    let popup_active = popup_receiver
                        .lock()
                        .unwrap()
                        .as_deref()
                        .is_some_and(|r| r == receiver);
                    if popup_active {
                        let batch = scheduler.pop_batch(receiver, earliest_ts);
                        let defer_ns: u64 = 30_000_000_000;
                        for mut ev in batch {
                            ev.timestamp = now_ns() + defer_ns;
                            scheduler.insert(ev);
                        }
                        ticker.push(format!("deferred event(s) for {} (popup open)", receiver));
                        continue;
                    }

                    let batch = scheduler.pop_batch(receiver, earliest_ts);
                    if batch.is_empty() {
                        continue;
                    }
                    let message = format_delivery(&batch, earliest_ts);
                    let batch_len = batch.len();

                    // Re-insert recurring events with a fresh timestamp and ID
                    // BEFORE spawning delivery, so the queue stays consistent
                    // regardless of whether delivery succeeds.
                    for ev in &batch {
                        if let Some(interval) = ev.recurring_ns {
                            let next = ScheduledEvent {
                                id: uuid::Uuid::new_v4().to_string(),
                                sender: ev.sender.clone(),
                                receiver: ev.receiver.clone(),
                                timestamp: now_ns() + interval,
                                payload: ev.payload.clone(),
                                created_at: now_ns(),
                                recurring_ns: Some(interval),
                            };
                            scheduler.insert(next);
                        }
                    }

                    // Run blocking delivery off the async runtime so the loop
                    // stays responsive for other scheduled events. Success /
                    // failure telemetry is logged from inside the blocking
                    // task (see deliver_to_tmux) so the ticker reflects
                    // actual delivery, not queuing.
                    let receiver_owned = receiver.clone();
                    let message_owned = message.clone();
                    let ticker_clone = ticker.clone();
                    tokio::task::spawn_blocking(move || {
                        deliver_to_tmux(
                            &receiver_owned,
                            &message_owned,
                            batch_len,
                            earliest_ts,
                            &ticker_clone,
                        );
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn make_event(receiver: &str, sender: &str, timestamp: u64, payload: &str) -> ScheduledEvent {
        ScheduledEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sender: sender.to_string(),
            receiver: receiver.to_string(),
            timestamp,
            payload: payload.to_string(),
            created_at: now_ns(),
            recurring_ns: None,
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
    fn test_cancel_by_receiver() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "a"));
        sched.insert(make_event("bob", "carol", 200, "b"));
        sched.insert(make_event("carol", "alice", 300, "c"));
        sched.insert(make_event("bob", "dave", 400, "d"));

        let cancelled = sched.cancel_by_receiver("bob");
        assert_eq!(cancelled, 3);
        let remaining = sched.list();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].receiver, "carol");
    }

    #[test]
    fn test_cancel_by_receiver_none() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "a"));
        let cancelled = sched.cancel_by_receiver("nobody");
        assert_eq!(cancelled, 0);
        assert_eq!(sched.list().len(), 1);
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
        assert!(msg.contains("[EVENT] From alice: msg1"));
        assert!(msg.contains("[EVENT] From carol: msg2"));
    }

    #[test]
    fn test_format_cron_event() {
        let mut ev = make_event("bob", "alice", 1000, "periodic check");
        ev.recurring_ns = Some(60_000_000_000);
        let msg = format_delivery(&[ev], 1000);
        assert!(msg.contains("[CRON at t=1000]"));
        assert!(msg.contains("From alice: periodic check"));
        assert!(!msg.contains("[EVENT"));
    }

    #[test]
    fn test_format_batch_mixed() {
        let e1 = make_event("bob", "alice", 1000, "one-time");
        let mut e2 = make_event("bob", "carol", 1000, "recurring");
        e2.recurring_ns = Some(30_000_000_000);
        let msg = format_delivery(&[e1, e2], 1000);
        assert!(msg.contains("[EVENT] From alice: one-time"));
        assert!(msg.contains("[CRON] From carol: recurring"));
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
        deliver_to_tmux("test-deliver", "hello-from-scheduler", 1, now_ns(), &ticker);

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
            recurring_ns: None,
        };
        scheduler.insert(event);

        // Spawn the event loop with a timeout
        let sched = Arc::clone(&scheduler);
        let ticker = TickerBuffer::new();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = run_event_loop(sched, ticker, new_popup_receiver()) => {}
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
    fn test_ticker_collapse_many_messages() {
        let ticker = TickerBuffer::new();
        ticker.push("delivered 1 event(s) to worker-1");
        ticker.push("event delivery failed for worker-2: timeout");
        ticker.push("event delivery failed for worker-3: timeout");
        ticker.push("delivered 1 event(s) to worker-4");
        let out = ticker.render(std::time::Duration::from_secs(30));
        assert!(
            out.contains("2 error(s)"),
            "expected error count, got: {}",
            out
        );
        assert!(out.contains("2 info"), "expected info count, got: {}", out);
        assert!(
            out.contains("press D for details"),
            "expected D hint, got: {}",
            out
        );
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
