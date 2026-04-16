use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteDecision {
    Accept,
    Decline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteStatus {
    Pending,
    Accepted,
    Declined,
    Expired,
    Cancelled,
}

impl InviteStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InviteStatus::Pending => "pending",
            InviteStatus::Accepted => "accepted",
            InviteStatus::Declined => "declined",
            InviteStatus::Expired => "expired",
            InviteStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InviteRecord {
    pub id: String,
    pub invited_agent: String,
    pub invited_by: String,
    pub message: Option<String>,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub status: InviteStatus,
    pub responded_at: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RoomMessage {
    pub id: String,
    pub sender: String,
    pub payload: String,
    pub created_at: u64,
    pub delivered_to: Vec<String>,
    pub system: bool,
}

#[derive(Debug, Clone)]
pub struct RoomSummary {
    pub name: String,
    pub created_by: String,
    pub participant_count: usize,
    pub message_count: usize,
    pub last_activity_at: u64,
}

#[derive(Debug, Clone)]
pub struct RoomSnapshot {
    pub name: String,
    pub created_by: String,
    pub participants: Vec<String>,
    pub transcript: Vec<RoomMessage>,
    pub invites: Vec<InviteRecord>,
    pub created_at: u64,
    pub last_activity_at: u64,
}

#[derive(Debug, Clone)]
struct Room {
    name: String,
    created_by: String,
    participants: HashSet<String>,
    transcript: Vec<RoomMessage>,
    invites: HashMap<String, InviteRecord>,
    created_at: u64,
    last_activity_at: u64,
}

#[derive(Debug, Clone)]
pub enum RoomError {
    NotFound(String),
    AlreadyExists(String),
    Forbidden(String),
    Invalid(String),
}

impl fmt::Display for RoomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoomError::NotFound(msg)
            | RoomError::AlreadyExists(msg)
            | RoomError::Forbidden(msg)
            | RoomError::Invalid(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for RoomError {}

pub struct RoomRegistry {
    rooms: RwLock<HashMap<u32, HashMap<String, Room>>>,
    omar_dir: PathBuf,
    transcript_cap: usize,
}

impl RoomRegistry {
    pub fn new(omar_dir: PathBuf) -> Self {
        Self {
            rooms: RwLock::new(HashMap::new()),
            omar_dir,
            transcript_cap: 2_000,
        }
    }

    pub fn create_room(
        &self,
        ea_id: u32,
        name: &str,
        created_by: &str,
    ) -> Result<RoomSummary, RoomError> {
        validate_room_name(name)?;
        let mut guard = self.rooms.write().unwrap();
        let per_ea = guard.entry(ea_id).or_default();
        if per_ea.contains_key(name) {
            return Err(RoomError::AlreadyExists(format!(
                "Room '{}' already exists",
                name
            )));
        }

        let now = now_ns();
        let mut participants = HashSet::new();
        if !created_by.trim().is_empty() {
            participants.insert(created_by.trim().to_string());
        }
        let room = Room {
            name: name.to_string(),
            created_by: created_by.to_string(),
            participants,
            transcript: Vec::new(),
            invites: HashMap::new(),
            created_at: now,
            last_activity_at: now,
        };
        per_ea.insert(name.to_string(), room);
        Ok(self
            .room_summary_from_map(per_ea, name)
            .expect("just inserted"))
    }

    pub fn list_rooms(&self, ea_id: u32) -> Vec<RoomSummary> {
        let guard = self.rooms.read().unwrap();
        let Some(per_ea) = guard.get(&ea_id) else {
            return Vec::new();
        };
        let mut rooms: Vec<RoomSummary> = per_ea
            .values()
            .map(|room| RoomSummary {
                name: room.name.clone(),
                created_by: room.created_by.clone(),
                participant_count: room.participants.len(),
                message_count: room.transcript.len(),
                last_activity_at: room.last_activity_at,
            })
            .collect();
        rooms.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
        rooms
    }

    pub fn get_room(&self, ea_id: u32, name: &str) -> Option<RoomSnapshot> {
        let guard = self.rooms.read().unwrap();
        let room = guard.get(&ea_id)?.get(name)?;
        Some(room_snapshot(room))
    }

    pub fn close_room(
        &self,
        ea_id: u32,
        name: &str,
        reason: &str,
    ) -> Result<RoomSummary, RoomError> {
        let mut guard = self.rooms.write().unwrap();
        let per_ea = guard
            .get_mut(&ea_id)
            .ok_or_else(|| RoomError::NotFound(format!("Room '{}' not found", name)))?;
        let room = per_ea
            .remove(name)
            .ok_or_else(|| RoomError::NotFound(format!("Room '{}' not found", name)))?;
        let summary = RoomSummary {
            name: room.name.clone(),
            created_by: room.created_by.clone(),
            participant_count: room.participants.len(),
            message_count: room.transcript.len(),
            last_activity_at: room.last_activity_at,
        };
        self.write_minutes(ea_id, &room, reason);
        Ok(summary)
    }

    pub fn close_inactive(&self, ea_id: u32, idle_ns: u64) -> Vec<String> {
        let mut guard = self.rooms.write().unwrap();
        let Some(per_ea) = guard.get_mut(&ea_id) else {
            return Vec::new();
        };
        let now = now_ns();
        let to_close: Vec<String> = per_ea
            .iter()
            .filter_map(|(name, room)| {
                if now.saturating_sub(room.last_activity_at) >= idle_ns {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();
        for name in &to_close {
            if let Some(room) = per_ea.remove(name) {
                self.write_minutes(ea_id, &room, "inactive timeout");
            }
        }
        to_close
    }

    pub fn create_invite(
        &self,
        ea_id: u32,
        room_name: &str,
        invited_by: &str,
        invited_agent: &str,
        message: Option<String>,
        expires_at: Option<u64>,
    ) -> Result<InviteRecord, RoomError> {
        let mut guard = self.rooms.write().unwrap();
        let room = get_room_mut(&mut guard, ea_id, room_name)?;
        if !room.participants.contains(invited_by) {
            return Err(RoomError::Forbidden(format!(
                "'{}' is not a participant in room '{}'",
                invited_by, room_name
            )));
        }
        if room.participants.contains(invited_agent) {
            return Err(RoomError::Invalid(format!(
                "'{}' is already in room '{}'",
                invited_agent, room_name
            )));
        }

        let now = now_ns();
        let invite = InviteRecord {
            id: uuid::Uuid::new_v4().to_string(),
            invited_agent: invited_agent.to_string(),
            invited_by: invited_by.to_string(),
            message,
            created_at: now,
            expires_at,
            status: InviteStatus::Pending,
            responded_at: None,
            reason: None,
        };
        let invite_id = invite.id.clone();
        room.invites.insert(invite_id, invite.clone());
        room.last_activity_at = now;
        append_system_message(
            room,
            format!("{} invited {}", invited_by, invited_agent),
            now,
            self.transcript_cap,
        );
        Ok(invite)
    }

    pub fn list_invites(
        &self,
        ea_id: u32,
        room_name: &str,
    ) -> Result<Vec<InviteRecord>, RoomError> {
        let mut invites = {
            let guard = self.rooms.read().unwrap();
            let room = guard
                .get(&ea_id)
                .and_then(|m| m.get(room_name))
                .ok_or_else(|| RoomError::NotFound(format!("Room '{}' not found", room_name)))?;
            room.invites.values().cloned().collect::<Vec<_>>()
        };
        let now = now_ns();
        for inv in &mut invites {
            if inv.status == InviteStatus::Pending && inv.expires_at.is_some_and(|t| t <= now) {
                inv.status = InviteStatus::Expired;
            }
        }
        invites.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(invites)
    }

    pub fn cancel_invite(
        &self,
        ea_id: u32,
        room_name: &str,
        invite_id: &str,
        cancelled_by: &str,
    ) -> Result<InviteRecord, RoomError> {
        let mut guard = self.rooms.write().unwrap();
        let room = get_room_mut(&mut guard, ea_id, room_name)?;
        if !room.participants.contains(cancelled_by) {
            return Err(RoomError::Forbidden(format!(
                "'{}' is not a participant in room '{}'",
                cancelled_by, room_name
            )));
        }

        let now = now_ns();
        let cancelled_agent = {
            let invite = room
                .invites
                .get_mut(invite_id)
                .ok_or_else(|| RoomError::NotFound(format!("Invite '{}' not found", invite_id)))?;
            if invite.status != InviteStatus::Pending {
                return Err(RoomError::Invalid(format!(
                    "Invite '{}' is not pending",
                    invite_id
                )));
            }
            if invite.expires_at.is_some_and(|t| t <= now) {
                invite.status = InviteStatus::Expired;
                return Err(RoomError::Invalid(format!(
                    "Invite '{}' has expired",
                    invite_id
                )));
            }
            invite.status = InviteStatus::Cancelled;
            invite.responded_at = Some(now);
            invite.invited_agent.clone()
        };
        room.last_activity_at = now;
        append_system_message(
            room,
            format!("{} cancelled invite for {}", cancelled_by, cancelled_agent),
            now,
            self.transcript_cap,
        );
        let invite = room
            .invites
            .get(invite_id)
            .cloned()
            .ok_or_else(|| RoomError::NotFound(format!("Invite '{}' not found", invite_id)))?;
        Ok(invite)
    }

    pub fn respond_invite(
        &self,
        ea_id: u32,
        room_name: &str,
        invite_id: &str,
        agent: &str,
        decision: InviteDecision,
        reason: Option<String>,
    ) -> Result<InviteRecord, RoomError> {
        let mut guard = self.rooms.write().unwrap();
        let room = get_room_mut(&mut guard, ea_id, room_name)?;
        let now = now_ns();
        {
            let invite = room
                .invites
                .get_mut(invite_id)
                .ok_or_else(|| RoomError::NotFound(format!("Invite '{}' not found", invite_id)))?;
            if invite.invited_agent != agent {
                return Err(RoomError::Forbidden(format!(
                    "Invite '{}' is not addressed to '{}'",
                    invite_id, agent
                )));
            }
            if invite.status != InviteStatus::Pending {
                return Err(RoomError::Invalid(format!(
                    "Invite '{}' is not pending",
                    invite_id
                )));
            }
            if invite.expires_at.is_some_and(|t| t <= now) {
                invite.status = InviteStatus::Expired;
                return Err(RoomError::Invalid(format!(
                    "Invite '{}' has expired",
                    invite_id
                )));
            }

            invite.responded_at = Some(now);
            invite.reason = reason;
            match decision {
                InviteDecision::Accept => {
                    invite.status = InviteStatus::Accepted;
                }
                InviteDecision::Decline => {
                    invite.status = InviteStatus::Declined;
                }
            }
        }
        match decision {
            InviteDecision::Accept => {
                room.participants.insert(agent.to_string());
                append_system_message(
                    room,
                    format!("{} accepted invite", agent),
                    now,
                    self.transcript_cap,
                );
            }
            InviteDecision::Decline => {
                append_system_message(
                    room,
                    format!("{} declined invite", agent),
                    now,
                    self.transcript_cap,
                );
            }
        }
        room.last_activity_at = now;
        let invite = room
            .invites
            .get(invite_id)
            .cloned()
            .ok_or_else(|| RoomError::NotFound(format!("Invite '{}' not found", invite_id)))?;
        Ok(invite)
    }

    pub fn post_message(
        &self,
        ea_id: u32,
        room_name: &str,
        sender: &str,
        payload: &str,
    ) -> Result<(RoomMessage, Vec<String>), RoomError> {
        if payload.trim().is_empty() {
            return Err(RoomError::Invalid(
                "Message payload cannot be empty".to_string(),
            ));
        }
        let mut guard = self.rooms.write().unwrap();
        let room = get_room_mut(&mut guard, ea_id, room_name)?;
        if !room.participants.contains(sender) {
            return Err(RoomError::Forbidden(format!(
                "'{}' is not a participant in room '{}'",
                sender, room_name
            )));
        }
        let now = now_ns();
        let mut delivered_to: Vec<String> = room
            .participants
            .iter()
            .filter(|name| name.as_str() != sender)
            .cloned()
            .collect();
        delivered_to.sort();
        let msg = RoomMessage {
            id: uuid::Uuid::new_v4().to_string(),
            sender: sender.to_string(),
            payload: payload.to_string(),
            created_at: now,
            delivered_to: delivered_to.clone(),
            system: false,
        };
        room.transcript.push(msg.clone());
        cap_transcript(&mut room.transcript, self.transcript_cap);
        room.last_activity_at = now;
        Ok((msg, delivered_to))
    }

    fn room_summary_from_map(
        &self,
        per_ea: &HashMap<String, Room>,
        room_name: &str,
    ) -> Option<RoomSummary> {
        let room = per_ea.get(room_name)?;
        Some(RoomSummary {
            name: room.name.clone(),
            created_by: room.created_by.clone(),
            participant_count: room.participants.len(),
            message_count: room.transcript.len(),
            last_activity_at: room.last_activity_at,
        })
    }

    fn write_minutes(&self, ea_id: u32, room: &Room, reason: &str) {
        let meetings_dir = self.omar_dir.join("meetings");
        if fs::create_dir_all(&meetings_dir).is_err() {
            return;
        }
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let filename = format!("ea{}-{}-{}.md", ea_id, sanitize_name(&room.name), ts);
        let path = meetings_dir.join(filename);

        let mut participants: Vec<String> = room.participants.iter().cloned().collect();
        participants.sort();
        let mut transcript = String::new();
        for msg in &room.transcript {
            let local =
                chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(msg.created_at as i64)
                    .with_timezone(&chrono::Local);
            let prefix = if msg.system { "[system]" } else { "" };
            transcript.push_str(&format!(
                "- {} {} **{}**: {}\n",
                local.format("%Y-%m-%d %H:%M:%S"),
                prefix,
                msg.sender,
                msg.payload.replace('\n', " ")
            ));
        }

        let content = format!(
            "# Meeting Minutes: {}\n\n- EA: {}\n- Created by: {}\n- Created at: {}\n- Closed at: {}\n- Closed reason: {}\n- Participants: {}\n\n## Transcript\n{}\n",
            room.name,
            ea_id,
            room.created_by,
            fmt_ns(room.created_at),
            fmt_ns(now_ns()),
            reason,
            participants.join(", "),
            transcript
        );
        let _ = fs::write(path, content);
    }
}

fn room_snapshot(room: &Room) -> RoomSnapshot {
    let mut participants: Vec<String> = room.participants.iter().cloned().collect();
    participants.sort();
    let mut invites: Vec<InviteRecord> = room.invites.values().cloned().collect();
    let now = now_ns();
    for inv in &mut invites {
        if inv.status == InviteStatus::Pending && inv.expires_at.is_some_and(|t| t <= now) {
            inv.status = InviteStatus::Expired;
        }
    }
    invites.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    RoomSnapshot {
        name: room.name.clone(),
        created_by: room.created_by.clone(),
        participants,
        transcript: room.transcript.clone(),
        invites,
        created_at: room.created_at,
        last_activity_at: room.last_activity_at,
    }
}

fn get_room_mut<'a>(
    all: &'a mut HashMap<u32, HashMap<String, Room>>,
    ea_id: u32,
    room_name: &str,
) -> Result<&'a mut Room, RoomError> {
    all.get_mut(&ea_id)
        .and_then(|m| m.get_mut(room_name))
        .ok_or_else(|| RoomError::NotFound(format!("Room '{}' not found", room_name)))
}

fn append_system_message(room: &mut Room, text: String, now: u64, cap: usize) {
    room.transcript.push(RoomMessage {
        id: uuid::Uuid::new_v4().to_string(),
        sender: "system".to_string(),
        payload: text,
        created_at: now,
        delivered_to: Vec::new(),
        system: true,
    });
    cap_transcript(&mut room.transcript, cap);
}

fn cap_transcript(transcript: &mut Vec<RoomMessage>, cap: usize) {
    if transcript.len() > cap {
        let drop_n = transcript.len() - cap;
        transcript.drain(0..drop_n);
    }
}

fn validate_room_name(name: &str) -> Result<(), RoomError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(RoomError::Invalid("Room name cannot be empty".to_string()));
    }
    if trimmed.len() < 2 || trimmed.len() > 64 {
        return Err(RoomError::Invalid(
            "Room name must be 2..64 characters".to_string(),
        ));
    }
    let mut chars = trimmed.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(RoomError::Invalid(
            "Room name must start with an alphanumeric character".to_string(),
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(RoomError::Invalid(
            "Room name supports only alphanumeric, '-' and '_'".to_string(),
        ));
    }
    Ok(())
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_nanos() as u64
}

fn fmt_ns(ns: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(ns as i64)
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> RoomRegistry {
        let dir = std::env::temp_dir().join(format!("omar-rooms-test-{}", uuid::Uuid::new_v4()));
        RoomRegistry::new(dir)
    }

    #[test]
    fn invite_accept_adds_participant() {
        let rooms = test_registry();
        let _ = rooms.create_room(1, "audit", "firm-head").unwrap();
        let inv = rooms
            .create_invite(1, "audit", "firm-head", "auditor", None, None)
            .unwrap();
        let accepted = rooms
            .respond_invite(1, "audit", &inv.id, "auditor", InviteDecision::Accept, None)
            .unwrap();
        assert_eq!(accepted.status, InviteStatus::Accepted);
        let room = rooms.get_room(1, "audit").unwrap();
        assert!(room.participants.contains(&"auditor".to_string()));
    }

    #[test]
    fn room_message_fanout_excludes_sender() {
        let rooms = test_registry();
        let _ = rooms.create_room(1, "audit", "firm-head").unwrap();
        let inv = rooms
            .create_invite(1, "audit", "firm-head", "auditor", None, None)
            .unwrap();
        let _ = rooms
            .respond_invite(1, "audit", &inv.id, "auditor", InviteDecision::Accept, None)
            .unwrap();

        let (_msg, recipients) = rooms
            .post_message(1, "audit", "firm-head", "hello")
            .unwrap();
        assert_eq!(recipients, vec!["auditor".to_string()]);
    }
}
