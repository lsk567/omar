pub mod event;

pub use event::ScheduledEvent;

use std::collections::{BinaryHeap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;

use crate::ea;
use crate::tmux::DeliveryOptions;

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
    pub fn render(&self, ttl: std::time::Duration) -> String {
        let buf = self.entries.lock().unwrap();
        let now = Instant::now();
        buf.iter()
            .filter(|e| now.duration_since(e.created_at) < ttl)
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
        .unwrap_or(std::time::Duration::ZERO)
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

    /// Cancel an event only if it belongs to the specified EA.
    /// Fix S1: Atomic EA-scoped cancellation — no TOCTOU window where the event
    /// is temporarily absent from the queue (as happens with cancel + re-insert).
    /// Returns:
    ///   Ok(event) if found and ea_id matches (event removed)
    ///   Err(true)  if found but ea_id doesn't match (event stays in queue)
    ///   Err(false) if not found
    pub fn cancel_if_ea(&self, event_id: &str, ea_id: u32) -> Result<ScheduledEvent, bool> {
        let mut queue = self.queue.lock().unwrap();
        let events: Vec<ScheduledEvent> = queue.drain().collect();
        let mut cancelled = None;
        let mut wrong_ea = false;
        let mut remaining = BinaryHeap::new();
        for ev in events {
            if ev.id == event_id && cancelled.is_none() {
                if ev.ea_id == ea_id {
                    cancelled = Some(ev);
                } else {
                    wrong_ea = true;
                    remaining.push(ev); // put it back — wrong EA
                }
            } else {
                remaining.push(ev);
            }
        }
        *queue = remaining;
        match cancelled {
            Some(ev) => Ok(ev),
            None => Err(wrong_ea),
        }
    }

    /// List events for a specific EA only.
    pub fn list_by_ea(&self, ea_id: u32) -> Vec<ScheduledEvent> {
        let queue = self.queue.lock().unwrap();
        queue.iter().filter(|e| e.ea_id == ea_id).cloned().collect()
    }

    /// Cancel all events for a specific EA. Returns the number cancelled.
    pub fn cancel_by_ea(&self, ea_id: u32) -> usize {
        let mut queue = self.queue.lock().unwrap();
        let events: Vec<ScheduledEvent> = queue.drain().collect();
        let mut count = 0;
        let mut remaining = BinaryHeap::new();
        for ev in events {
            if ev.ea_id == ea_id {
                count += 1;
            } else {
                remaining.push(ev);
            }
        }
        *queue = remaining;
        count
    }

    /// Cancel all events for a given receiver within a specific EA only.
    /// Fix V5: EA-scoped receiver cancellation prevents cross-EA event leaks.
    pub fn cancel_by_receiver_and_ea(&self, receiver: &str, ea_id: u32) -> usize {
        let mut queue = self.queue.lock().unwrap();
        let events: Vec<ScheduledEvent> = queue.drain().collect();
        let mut count = 0;
        let mut remaining = BinaryHeap::new();
        for ev in events {
            if ev.receiver == receiver && ev.ea_id == ea_id {
                count += 1;
            } else {
                remaining.push(ev);
            }
        }
        *queue = remaining;
        count
    }

    /// Pop all events matching the given receiver, EA, and timestamp.
    /// Fix V7: EA-scoped batching prevents cross-EA event delivery.
    pub fn pop_batch(&self, receiver: &str, ea_id: u32, timestamp: u64) -> Vec<ScheduledEvent> {
        let mut queue = self.queue.lock().unwrap();
        let events: Vec<ScheduledEvent> = queue.drain().collect();
        let mut batch = Vec::new();
        let mut remaining = BinaryHeap::new();
        for ev in events {
            if ev.receiver == receiver && ev.ea_id == ea_id && ev.timestamp == timestamp {
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
    ea_id: u32,
    receiver: &str,
    message: &str,
    base_prefix: &str,
    ticker: &TickerBuffer,
) {
    // Keep one consistent delivery method for all agents, including EA.
    let target = if receiver == "ea" || receiver == "omar" {
        ea::ea_manager_session(ea_id, base_prefix)
    } else {
        let prefix = ea::ea_prefix(ea_id, base_prefix);
        format!("{}{}", prefix, receiver)
    };
    let client = crate::tmux::TmuxClient::new("");
    let opts = DeliveryOptions::default();
    if let Err(e) = client.deliver_prompt(&target, message, &opts) {
        ticker.push(format!("tmux prompt delivery failed for {}: {}", target, e));
        return;
    }
    ticker.push(format!("delivered event(s) to {}", receiver));
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

fn split_batch_for_quota(
    mut batch: Vec<ScheduledEvent>,
    remaining_quota: usize,
) -> (Vec<ScheduledEvent>, Vec<ScheduledEvent>) {
    if batch.is_empty() || remaining_quota >= batch.len() {
        return (batch, Vec::new());
    }

    batch.sort_by_key(|ev| ev.created_at);
    let deferred = batch.split_off(remaining_quota);
    (batch, deferred)
}

/// Shared state: the (short name, ea_id) of the agent whose popup is currently open, if any.
/// Both fields are required so suppression is scoped per-EA and does not affect same-named
/// agents in other EAs.
pub type PopupReceiver = Arc<Mutex<Option<(String, ea::EaId)>>>;

pub fn new_popup_receiver() -> PopupReceiver {
    Arc::new(Mutex::new(None))
}

/// How long to push delivery out while the user's popup is open.
/// If the popup stays open past this, the loop simply defers again —
/// no unbounded fan-out, one event stays one event.
pub(crate) const POPUP_DEFER_NS: u64 = 30_000_000_000;

/// Decide whether to defer an event for `(receiver, ea_id)` because the user
/// has an agent popup open on that exact pane. Scoped per-EA so same-named
/// workers in other EAs deliver as normal.
///
/// `deliver_to_tmux` treats the EA pane as either `"ea"` or `"omar"`, so both
/// forms are accepted as aliases: if the popup is on the EA pane, any event
/// addressed to that EA manager (via either alias) is deferred.
pub(crate) fn should_defer_for_popup(
    popup_receiver: &PopupReceiver,
    receiver: &str,
    ea_id: ea::EaId,
) -> bool {
    popup_receiver
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(r, eid)| *eid == ea_id && receivers_match(r, receiver))
}

/// True when `a` and `b` refer to the same receiver pane.
/// "ea" and "omar" are aliases for the EA manager.
fn receivers_match(a: &str, b: &str) -> bool {
    a == b || (is_ea_alias(a) && is_ea_alias(b))
}

fn is_ea_alias(name: &str) -> bool {
    name == "ea" || name == "omar"
}

pub async fn run_event_loop(
    scheduler: Arc<Scheduler>,
    ticker: TickerBuffer,
    popup_receiver: PopupReceiver,
    base_prefix: String,
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

                // Fix V7: Collect (receiver, ea_id) pairs — not just receivers.
                // This prevents cross-EA event batching where events for same-named
                // agents in different EAs would be merged and delivered to only one EA.
                let receiver_ea_pairs: Vec<(String, u32)> = {
                    let queue = scheduler.queue.lock().unwrap();
                    let mut seen: Vec<(String, u32)> = Vec::new();
                    for ev in queue.iter() {
                        if ev.timestamp == earliest_ts {
                            let pair = (ev.receiver.clone(), ev.ea_id);
                            if !seen.contains(&pair) {
                                seen.push(pair);
                            }
                        }
                    }
                    seen
                };

                // Per-EA fairness cap: process at most this many events per EA per tick.
                // If an EA has a burst of events, the excess stays in the queue for the
                // next iteration so other EAs are not starved.
                const MAX_EVENTS_PER_EA_PER_TICK: usize = 10;
                let mut ea_delivery_count: std::collections::HashMap<u32, usize> =
                    std::collections::HashMap::new();

                for (receiver, ea_id) in &receiver_ea_pairs {
                    // Enforce per-EA delivery cap.
                    let delivered_so_far = *ea_delivery_count.get(ea_id).unwrap_or(&0);
                    if delivered_so_far >= MAX_EVENTS_PER_EA_PER_TICK {
                        // Leave remaining events for this EA in the queue for the next tick.
                        continue;
                    }

                    // If the user has a popup open for this receiver, defer by
                    // POPUP_DEFER_NS. The event keeps rescheduling on each attempt
                    // until the popup closes (bounded work: one event per tick).
                    if should_defer_for_popup(&popup_receiver, receiver, *ea_id) {
                        let batch = scheduler.pop_batch(receiver, *ea_id, earliest_ts);
                        for mut ev in batch {
                            ev.timestamp = now_ns() + POPUP_DEFER_NS;
                            scheduler.insert(ev);
                        }
                        ticker.push(format!("deferred event(s) for {} (popup open)", receiver));
                        continue;
                    }

                    let batch = scheduler.pop_batch(receiver, *ea_id, earliest_ts);
                    if batch.is_empty() {
                        continue;
                    }
                    let remaining_quota = MAX_EVENTS_PER_EA_PER_TICK - delivered_so_far;
                    let (batch, deferred_batch) = split_batch_for_quota(batch, remaining_quota);
                    for ev in deferred_batch {
                        scheduler.insert(ev);
                    }
                    if batch.is_empty() {
                        continue;
                    }
                    // Track events delivered for this EA this tick.
                    *ea_delivery_count.entry(*ea_id).or_insert(0) += batch.len();
                    let message = format_delivery(&batch, earliest_ts);
                    let receiver_name = receiver.clone();
                    let base_prefix_clone = base_prefix.clone();
                    let ticker_clone = ticker.clone();
                    let ea = *ea_id;
                    let delivery_result = tokio::task::spawn_blocking(move || {
                        deliver_to_tmux(
                            ea,
                            &receiver_name,
                            &message,
                            &base_prefix_clone,
                            &ticker_clone,
                        );
                    })
                    .await;
                    if let Err(e) = delivery_result {
                        ticker.push(format!(
                            "delivery task failed for {} (ea {}): {}",
                            receiver, ea_id, e
                        ));
                    }

                    // Re-insert recurring events with a fresh timestamp and ID
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
                                ea_id: ev.ea_id,
                            };
                            scheduler.insert(next);
                        }
                    }

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
            recurring_ns: None,
            ea_id: 0,
        }
    }

    #[test]
    fn test_insert_and_peek() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 200, "hello"));
        sched.insert(make_event("bob", "alice", 100, "earlier"));

        let min_ts = sched.list_by_ea(0).iter().map(|e| e.timestamp).min();
        assert_eq!(min_ts, Some(100));
    }

    #[test]
    fn test_cancel() {
        let sched = Scheduler::new();
        let ev = make_event("bob", "alice", 100, "cancel me");
        let id = ev.id.clone();
        sched.insert(ev);
        sched.insert(make_event("bob", "alice", 200, "keep"));

        let cancelled = sched.cancel_if_ea(&id, 0).ok();
        assert!(cancelled.is_some());
        assert_eq!(cancelled.unwrap().payload, "cancel me");
        assert_eq!(sched.list_by_ea(0).len(), 1);
    }

    #[test]
    fn test_cancel_nonexistent() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "keep"));
        assert!(sched.cancel_if_ea("no-such-id", 0).is_err());
        assert_eq!(sched.list_by_ea(0).len(), 1);
    }

    #[test]
    fn test_list_by_receiver() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "for bob"));
        sched.insert(make_event("carol", "alice", 200, "for carol"));
        sched.insert(make_event("bob", "dave", 300, "also for bob"));

        let bob_events: Vec<_> = sched
            .list_by_ea(0)
            .into_iter()
            .filter(|e| e.receiver == "bob")
            .collect();
        assert_eq!(bob_events.len(), 2);
        assert!(bob_events.iter().all(|e| e.receiver == "bob"));

        let carol_events: Vec<_> = sched
            .list_by_ea(0)
            .into_iter()
            .filter(|e| e.receiver == "carol")
            .collect();
        assert_eq!(carol_events.len(), 1);
    }

    #[test]
    fn test_list_by_ea() {
        let sched = Scheduler::new();
        let mut ev1 = make_event("bob", "alice", 100, "ea0");
        ev1.ea_id = 0;
        let mut ev2 = make_event("carol", "alice", 200, "ea1");
        ev2.ea_id = 1;
        let mut ev3 = make_event("dave", "alice", 300, "ea0 too");
        ev3.ea_id = 0;
        sched.insert(ev1);
        sched.insert(ev2);
        sched.insert(ev3);

        assert_eq!(sched.list_by_ea(0).len(), 2);
        assert_eq!(sched.list_by_ea(1).len(), 1);
        assert_eq!(sched.list_by_ea(99).len(), 0);
    }

    #[test]
    fn test_cancel_by_ea() {
        let sched = Scheduler::new();
        let mut ev1 = make_event("bob", "alice", 100, "ea0");
        ev1.ea_id = 0;
        let mut ev2 = make_event("carol", "alice", 200, "ea1");
        ev2.ea_id = 1;
        sched.insert(ev1);
        sched.insert(ev2);

        let count = sched.cancel_by_ea(0);
        assert_eq!(count, 1);
        assert_eq!(sched.list_by_ea(1).len(), 1);
        assert_eq!(sched.list_by_ea(1)[0].ea_id, 1);
    }

    #[test]
    fn test_pop_batch() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "a"));
        sched.insert(make_event("bob", "carol", 100, "b"));
        sched.insert(make_event("bob", "dave", 200, "c"));
        sched.insert(make_event("carol", "alice", 100, "d"));

        let batch = sched.pop_batch("bob", 0, 100);
        assert_eq!(batch.len(), 2);
        assert!(batch
            .iter()
            .all(|e| e.receiver == "bob" && e.timestamp == 100));

        // Remaining: bob@200 and carol@100
        assert_eq!(sched.list_by_ea(0).len(), 2);
    }

    #[test]
    fn test_pop_batch_ea_scoped() {
        // Fix V7: pop_batch must scope by ea_id to prevent cross-EA batching
        let sched = Scheduler::new();
        let mut ev0 = make_event("auth", "alice", 100, "ea0-event");
        ev0.ea_id = 0;
        let mut ev1 = make_event("auth", "bob", 100, "ea1-event");
        ev1.ea_id = 1;
        sched.insert(ev0);
        sched.insert(ev1);

        // Pop only EA 0's auth events at timestamp 100
        let batch = sched.pop_batch("auth", 0, 100);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].ea_id, 0);
        assert_eq!(batch[0].payload, "ea0-event");

        // EA 1's auth event should still be in the queue
        let remaining = sched.list_by_ea(1);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].ea_id, 1);
        assert_eq!(remaining[0].payload, "ea1-event");
    }

    #[test]
    fn test_cancel_by_receiver() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "a"));
        sched.insert(make_event("bob", "carol", 200, "b"));
        sched.insert(make_event("carol", "alice", 300, "c"));
        sched.insert(make_event("bob", "dave", 400, "d"));

        let cancelled = sched.cancel_by_receiver_and_ea("bob", 0);
        assert_eq!(cancelled, 3);
        let remaining = sched.list_by_ea(0);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].receiver, "carol");
    }

    #[test]
    fn test_cancel_by_receiver_none() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "a"));
        let cancelled = sched.cancel_by_receiver_and_ea("nobody", 0);
        assert_eq!(cancelled, 0);
        assert_eq!(sched.list_by_ea(0).len(), 1);
    }

    #[test]
    fn test_cancel_by_receiver_and_ea() {
        let sched = Scheduler::new();
        // EA 0 has "auth" events
        let mut ev1 = make_event("auth", "alice", 100, "ea0-auth-1");
        ev1.ea_id = 0;
        let mut ev2 = make_event("auth", "carol", 200, "ea0-auth-2");
        ev2.ea_id = 0;
        // EA 1 also has "auth" events
        let mut ev3 = make_event("auth", "dave", 300, "ea1-auth-1");
        ev3.ea_id = 1;
        // EA 0 has "bob" events
        let mut ev4 = make_event("bob", "alice", 400, "ea0-bob");
        ev4.ea_id = 0;
        sched.insert(ev1);
        sched.insert(ev2);
        sched.insert(ev3);
        sched.insert(ev4);

        // Cancel only EA 0's "auth" events — EA 1's "auth" and EA 0's "bob" survive
        let cancelled = sched.cancel_by_receiver_and_ea("auth", 0);
        assert_eq!(cancelled, 2);
        let remaining_ea0 = sched.list_by_ea(0);
        let remaining_ea1 = sched.list_by_ea(1);
        assert_eq!(remaining_ea0.len() + remaining_ea1.len(), 2);
        // Verify EA 1's auth event survived
        assert!(remaining_ea1.iter().any(|e| e.receiver == "auth"));
        // Verify EA 0's bob event survived
        assert!(remaining_ea0.iter().any(|e| e.receiver == "bob"));
    }

    #[test]
    fn test_cancel_if_ea_correct() {
        let sched = Scheduler::new();
        let mut ev = make_event("auth", "alice", 100, "ea0-event");
        ev.ea_id = 0;
        let id = ev.id.clone();
        sched.insert(ev);

        // Cancel with correct EA — should succeed
        let result = sched.cancel_if_ea(&id, 0);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().payload, "ea0-event");
        assert_eq!(sched.list_by_ea(0).len(), 0);
    }

    #[test]
    fn test_cancel_if_ea_wrong() {
        let sched = Scheduler::new();
        let mut ev = make_event("auth", "alice", 100, "ea0-event");
        ev.ea_id = 0;
        let id = ev.id.clone();
        sched.insert(ev);

        // Cancel with wrong EA — should fail and leave event in queue
        let result = sched.cancel_if_ea(&id, 1);
        assert!(result.is_err());
        assert!(result.unwrap_err()); // true = wrong EA (event exists but wrong owner)
                                      // Event must still be in the queue (atomic — no TOCTOU window)
        let remaining = sched.list_by_ea(0);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].payload, "ea0-event");
    }

    #[test]
    fn test_cancel_if_ea_not_found() {
        let sched = Scheduler::new();
        sched.insert(make_event("bob", "alice", 100, "keep"));

        // Cancel nonexistent event
        let result = sched.cancel_if_ea("no-such-id", 0);
        assert!(result.is_err());
        assert!(!result.unwrap_err()); // false = not found at all
        assert_eq!(sched.list_by_ea(0).len(), 1);
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

    #[test]
    fn test_split_batch_for_quota_defers_excess_events() {
        let mut e1 = make_event("bob", "alice", 1000, "first");
        e1.created_at = 1;
        let mut e2 = make_event("bob", "alice", 1000, "second");
        e2.created_at = 2;
        let mut e3 = make_event("bob", "alice", 1000, "third");
        e3.created_at = 3;

        let (deliver_now, deferred) = split_batch_for_quota(vec![e3, e1, e2], 2);

        assert_eq!(deliver_now.len(), 2);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deliver_now[0].payload, "first");
        assert_eq!(deliver_now[1].payload, "second");
        assert_eq!(deferred[0].payload, "third");
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

    // ── Popup-defer decision (pure helper) ──
    //
    // When the user opens the tmux popup for an agent pane, the scheduler
    // must defer events bound for that pane so injected [EVENT]/[CRON]/
    // [SLACK MESSAGE] lines don't corrupt input the user is typing.
    //
    // `should_defer_for_popup` makes the decision based only on the popup
    // state; the surrounding loop then picks the defer window.

    #[test]
    fn popup_decision_defer_when_receiver_and_ea_match() {
        let popup = new_popup_receiver();
        *popup.lock().unwrap() = Some(("worker".to_string(), 7));
        assert!(should_defer_for_popup(&popup, "worker", 7));
    }

    #[test]
    fn popup_decision_no_defer_when_receiver_differs() {
        let popup = new_popup_receiver();
        *popup.lock().unwrap() = Some(("worker".to_string(), 7));
        assert!(!should_defer_for_popup(&popup, "other", 7));
    }

    #[test]
    fn popup_decision_no_defer_when_ea_differs() {
        // Fix V5 style per-EA scoping: same-named worker in a different EA
        // must NOT be deferred.
        let popup = new_popup_receiver();
        *popup.lock().unwrap() = Some(("worker".to_string(), 7));
        assert!(!should_defer_for_popup(&popup, "worker", 8));
    }

    #[test]
    fn popup_decision_no_defer_when_no_popup_open() {
        let popup = new_popup_receiver();
        assert!(!should_defer_for_popup(&popup, "worker", 7));
    }

    #[test]
    fn popup_decision_defer_ea_alias_matches_omar() {
        // Popup opened on the EA pane (stored as "ea") must defer events
        // addressed either as "ea" or as "omar" — both route to the manager
        // session in `deliver_to_tmux`.
        let popup = new_popup_receiver();
        *popup.lock().unwrap() = Some(("ea".to_string(), 3));
        assert!(should_defer_for_popup(&popup, "ea", 3));
        assert!(should_defer_for_popup(&popup, "omar", 3));
        // Sanity: worker in the same EA still delivers normally
        assert!(!should_defer_for_popup(&popup, "worker", 3));
    }

    #[test]
    fn popup_decision_ea_alias_still_scoped_per_ea() {
        let popup = new_popup_receiver();
        *popup.lock().unwrap() = Some(("ea".to_string(), 3));
        // Events for the EA in a different EA namespace are NOT deferred.
        assert!(!should_defer_for_popup(&popup, "ea", 4));
        assert!(!should_defer_for_popup(&popup, "omar", 4));
    }

    // ── Event-loop behaviour with popup state ──
    //
    // Regression check: with the popup open for `(receiver, ea_id)`, a
    // past-due event for the same target must be re-queued ~30s into the
    // future instead of being delivered. Without the popup it would be
    // popped and handed to `deliver_to_tmux`.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn event_loop_defers_event_while_popup_open() {
        let scheduler = Arc::new(Scheduler::new());
        let ticker = TickerBuffer::new();
        let popup_receiver = new_popup_receiver();
        *popup_receiver.lock().unwrap() = Some(("popup-target".to_string(), 42));

        // Past-due event — loop will try to deliver immediately
        let mut ev = make_event(
            "popup-target",
            "sender",
            now_ns().saturating_sub(1_000_000_000),
            "hi",
        );
        ev.ea_id = 42;
        scheduler.insert(ev);

        let loop_handle = tokio::spawn(run_event_loop(
            scheduler.clone(),
            ticker.clone(),
            popup_receiver.clone(),
            "omar-agent-".to_string(),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        loop_handle.abort();

        let events = scheduler.list_by_ea(42);
        assert_eq!(
            events.len(),
            1,
            "event must stay queued while popup is open"
        );
        let delta_secs = (events[0].timestamp as i128 - now_ns() as i128) / 1_000_000_000;
        assert!(
            (25..=31).contains(&delta_secs),
            "event must be deferred ~30s into future; delta={}s",
            delta_secs
        );

        let ticker_text = ticker.render(std::time::Duration::from_secs(60));
        assert!(
            ticker_text.contains("deferred event(s) for popup-target"),
            "ticker must note the defer: {}",
            ticker_text
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn event_loop_keeps_deferring_while_popup_stays_open() {
        // The popup can stay open longer than 30s. Each delivery attempt
        // must re-defer another 30s — no eventual delivery, no unbounded
        // queue growth.
        let scheduler = Arc::new(Scheduler::new());
        let ticker = TickerBuffer::new();
        let popup_receiver = new_popup_receiver();
        *popup_receiver.lock().unwrap() = Some(("popup-target".to_string(), 1));

        let mut ev = make_event(
            "popup-target",
            "sender",
            now_ns().saturating_sub(500_000_000),
            "hi",
        );
        ev.ea_id = 1;
        scheduler.insert(ev);

        let loop_handle = tokio::spawn(run_event_loop(
            scheduler.clone(),
            ticker.clone(),
            popup_receiver.clone(),
            "omar-agent-".to_string(),
        ));

        // Run the loop for long enough to see multiple defer decisions.
        // We can't wait 30s in a unit test, so instead artificially pull
        // the event back into "due" territory and observe that the loop
        // re-defers each time.
        for _ in 0..3 {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            let mut q = scheduler.queue.lock().unwrap();
            let mut events: Vec<ScheduledEvent> = q.drain().collect();
            for e in events.iter_mut() {
                e.timestamp = now_ns().saturating_sub(10_000_000);
            }
            for e in events {
                q.push(e);
            }
            drop(q);
            scheduler.notify.notify_one();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        loop_handle.abort();

        // Queue still holds exactly one event (no duplicates, no delivery).
        let events = scheduler.list_by_ea(1);
        assert_eq!(events.len(), 1, "event must not be lost or duplicated");
        let delta_secs = (events[0].timestamp as i128 - now_ns() as i128) / 1_000_000_000;
        assert!(
            delta_secs > 25,
            "event must still be deferred; delta={}s",
            delta_secs
        );
    }
}
