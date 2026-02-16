pub mod event;

pub use event::ScheduledEvent;

use std::collections::BinaryHeap;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;

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

fn deliver_to_tmux(receiver: &str, message: &str) {
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
            eprintln!(
                "[scheduler] tmux send-keys failed for {}: {}",
                target, stderr
            );
        }
        Err(e) => {
            eprintln!("[scheduler] failed to run tmux for {}: {}", target, e);
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

pub async fn run_event_loop(scheduler: Arc<Scheduler>) {
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
                    deliver_to_tmux(receiver, &message);

                    let lag_ns = now_ns().saturating_sub(earliest_ts);
                    let lag_ms = lag_ns as f64 / 1_000_000.0;
                    eprintln!(
                        "[scheduler] delivered {} event(s) to {} at t={}, lag={:.2}ms",
                        batch.len(),
                        receiver,
                        earliest_ts,
                        lag_ms
                    );
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
}
