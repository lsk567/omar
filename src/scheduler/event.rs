use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ScheduledEvent {
    pub id: String,
    pub sender: String,
    pub receiver: String,
    pub timestamp: u64,
    pub payload: String,
    pub created_at: u64,
    /// If set, the event re-schedules itself with `timestamp = now + recurring_ns`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurring_ns: Option<u64>,
    /// EA that owns this event. Mandatory, from path parameter.
    #[serde(default)]
    pub ea_id: u32,
}

// Reverse ordering so BinaryHeap (max-heap) behaves as a min-heap.
// Events with the smallest timestamp have the highest priority.
impl Ord for ScheduledEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .timestamp
            .cmp(&self.timestamp)
            .then_with(|| other.created_at.cmp(&self.created_at))
    }
}

impl PartialOrd for ScheduledEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BinaryHeap;

    fn make_event(timestamp: u64, receiver: &str, payload: &str) -> ScheduledEvent {
        ScheduledEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sender: "test".to_string(),
            receiver: receiver.to_string(),
            timestamp,
            payload: payload.to_string(),
            created_at: 0,
            recurring_ns: None,
            ea_id: 0,
        }
    }

    #[test]
    fn test_min_heap_ordering() {
        let mut heap = BinaryHeap::new();
        heap.push(make_event(300, "a", "third"));
        heap.push(make_event(100, "a", "first"));
        heap.push(make_event(200, "a", "second"));

        assert_eq!(heap.pop().unwrap().timestamp, 100);
        assert_eq!(heap.pop().unwrap().timestamp, 200);
        assert_eq!(heap.pop().unwrap().timestamp, 300);
    }

    #[test]
    fn test_equal_timestamps_ordered_by_created_at() {
        let mut heap = BinaryHeap::new();
        let mut e1 = make_event(100, "a", "later");
        e1.created_at = 20;
        let mut e2 = make_event(100, "a", "earlier");
        e2.created_at = 10;

        heap.push(e1);
        heap.push(e2);

        assert_eq!(heap.pop().unwrap().payload, "earlier");
        assert_eq!(heap.pop().unwrap().payload, "later");
    }
}
