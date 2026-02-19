use super::{error_response, now_ms, ok_response, NodeState};
use agentbook::protocol::{Event, InboxEntry, MessageType, Response, RoomInfo};
use agentbook_crypto::crypto::{decrypt_with_key, encrypt_with_key, verify_signature};
use agentbook_crypto::recovery::derive_key_from_passphrase;
use agentbook_mesh::inbox::{InboxMessage, MessageType as MeshMessageType};
use agentbook_proto::host::v1 as host_pb;
use agentbook_proto::mesh::v1 as mesh_pb;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum room message body length.
const MAX_ROOM_MESSAGE_LEN: usize = 140;

/// Per-room send cooldown.
const ROOM_COOLDOWN: Duration = Duration::from_secs(3);

/// Configuration for a joined room.
#[derive(Clone, Serialize, Deserialize)]
pub struct RoomConfig {
    pub room: String,
    /// If set, this is a secure room. Stores the passphrase-derived 32-byte key (hex-encoded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_key_hex: Option<String>,
}

impl RoomConfig {
    /// Get the 32-byte encryption key, if this is a secure room.
    pub fn key(&self) -> Option<[u8; 32]> {
        self.encrypted_key_hex.as_ref().and_then(|hex| {
            let bytes = hex::decode(hex).ok()?;
            <[u8; 32]>::try_from(bytes.as_slice()).ok()
        })
    }
}

/// Validate a room name: lowercase alphanumeric + hyphens, 1-32 chars.
fn validate_room_name(room: &str) -> Result<(), String> {
    if room.is_empty() || room.len() > 32 {
        return Err("room name must be 1-32 characters".to_string());
    }
    if !room
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(
            "room name must contain only lowercase letters, digits, and hyphens".to_string(),
        );
    }
    Ok(())
}

pub async fn handle_join_room(
    state: &Arc<NodeState>,
    room: &str,
    passphrase: Option<&str>,
) -> Response {
    if let Err(e) = validate_room_name(room) {
        return error_response("invalid_room", &e);
    }

    // Derive encryption key if passphrase provided
    let encrypted_key_hex = if let Some(pass) = passphrase {
        match derive_key_from_passphrase(pass, room.as_bytes()) {
            Ok(key) => Some(hex::encode(key)),
            Err(e) => return error_response("key_derivation_failed", &e.to_string()),
        }
    } else {
        None
    };

    // Send RoomSubscribeFrame via transport
    if let Some(transport) = &state.transport {
        let frame = host_pb::NodeFrame {
            frame: Some(host_pb::node_frame::Frame::RoomSubscribe(
                host_pb::RoomSubscribeFrame {
                    room_id: room.to_string(),
                },
            )),
        };
        if let Err(e) = transport.send_control_frame(frame).await {
            return error_response("transport_error", &e.to_string());
        }
    } else {
        return error_response("no_relay", "not connected to any relay");
    }

    let config = RoomConfig {
        room: room.to_string(),
        encrypted_key_hex,
    };

    let mut rooms = state.rooms.lock().await;
    rooms.insert(room.to_string(), config);

    // Persist rooms
    if let Err(e) = save_rooms(&state.wallet.state_dir, &rooms) {
        tracing::error!(err = %e, "failed to save rooms.json");
    }

    ok_response(None)
}

pub async fn handle_leave_room(state: &Arc<NodeState>, room: &str) -> Response {
    let mut rooms = state.rooms.lock().await;
    if rooms.remove(room).is_none() {
        return error_response("not_joined", &format!("not in room #{room}"));
    }

    // Send RoomUnsubscribeFrame via transport
    if let Some(transport) = &state.transport {
        let frame = host_pb::NodeFrame {
            frame: Some(host_pb::node_frame::Frame::RoomUnsubscribe(
                host_pb::RoomUnsubscribeFrame {
                    room_id: room.to_string(),
                },
            )),
        };
        if let Err(e) = transport.send_control_frame(frame).await {
            tracing::warn!(err = %e, "failed to send room unsubscribe frame");
        }
    }

    // Persist rooms
    if let Err(e) = save_rooms(&state.wallet.state_dir, &rooms) {
        tracing::error!(err = %e, "failed to save rooms.json");
    }

    ok_response(None)
}

pub async fn handle_send_room(state: &Arc<NodeState>, room: &str, body: &str) -> Response {
    // Validate message length
    if body.len() > MAX_ROOM_MESSAGE_LEN {
        return error_response(
            "message_too_long",
            &format!("room messages are limited to {MAX_ROOM_MESSAGE_LEN} characters"),
        );
    }

    if body.is_empty() {
        return error_response("empty_message", "message body cannot be empty");
    }

    // Check room is joined
    let rooms = state.rooms.lock().await;
    let config = match rooms.get(room) {
        Some(c) => c.clone(),
        None => return error_response("not_joined", &format!("not in room #{room}")),
    };
    drop(rooms);

    // Check cooldown
    {
        let mut cooldowns = state.room_cooldowns.lock().await;
        if let Some(last_sent) = cooldowns.get(room)
            && last_sent.elapsed() < ROOM_COOLDOWN
        {
            return error_response("cooldown", "please wait 3 seconds between room messages");
        }
        cooldowns.insert(room.to_string(), Instant::now());
    }

    let transport = match &state.transport {
        Some(t) => t,
        None => return error_response("no_relay", "not connected to any relay"),
    };

    // Build envelope
    let msg_id = uuid::Uuid::new_v4().to_string();
    let timestamp = now_ms();

    let (ciphertext_b64, nonce_b64) = if let Some(key) = config.key() {
        // Secure room: encrypt body with room key
        match encrypt_with_key(&key, body.as_bytes()) {
            Ok((ct, nonce)) => (ct, nonce),
            Err(e) => return error_response("encryption_failed", &e.to_string()),
        }
    } else {
        // Open room: plaintext in ciphertext_b64 field, empty nonce
        (body.to_string(), String::new())
    };

    // Sign the ciphertext content
    let signature_b64 = match state.identity.sign(ciphertext_b64.as_bytes()) {
        Ok(sig) => sig,
        Err(e) => return error_response("sign_failed", &e.to_string()),
    };

    let envelope = mesh_pb::Envelope {
        message_id: msg_id.clone(),
        from_node_id: state.identity.node_id.clone(),
        to_node_id: String::new(), // empty = broadcast to room
        from_public_key_b64: state.identity.public_key_b64.clone(),
        message_type: mesh_pb::MessageType::RoomMessage as i32,
        ciphertext_b64,
        nonce_b64,
        signature_b64,
        timestamp_ms: timestamp,
        topic: Some(room.to_string()),
    };

    if let Err(e) = transport.send_via_relay(envelope).await {
        return error_response("send_failed", &e.to_string());
    }

    // Store our own message in inbox
    let msg = InboxMessage {
        message_id: msg_id.clone(),
        from_node_id: state.identity.node_id.clone(),
        from_public_key_b64: state.identity.public_key_b64.clone(),
        topic: Some(room.to_string()),
        body: body.to_string(),
        timestamp_ms: timestamp,
        acked: true, // own messages are auto-acked
        message_type: MeshMessageType::RoomMessage,
    };

    let mut inbox = state.inbox.lock().await;
    if let Err(e) = inbox.push(msg) {
        tracing::error!(err = %e, "failed to store own room message");
    }

    // Emit event
    let _ = state.event_tx.send(Event::NewRoomMessage {
        message_id: msg_id,
        from: state.identity.node_id.clone(),
        room: room.to_string(),
        preview: body.chars().take(50).collect(),
    });

    ok_response(None)
}

pub async fn handle_room_inbox(
    state: &Arc<NodeState>,
    room: &str,
    limit: Option<usize>,
) -> Response {
    let inbox = state.inbox.lock().await;
    let follow_store = state.follow_store.lock().await;

    let messages: Vec<InboxEntry> = inbox
        .list_by_topic(room, limit)
        .into_iter()
        .filter(|m| !follow_store.is_blocked(&m.from_node_id))
        .map(|m| InboxEntry {
            message_id: m.message_id.clone(),
            from_node_id: m.from_node_id.clone(),
            from_username: None,
            message_type: MessageType::RoomMessage,
            body: m.body.clone(),
            timestamp_ms: m.timestamp_ms,
            acked: m.acked,
            room: m.topic.clone(),
        })
        .collect();

    ok_response(Some(serde_json::to_value(messages).unwrap()))
}

pub async fn handle_list_rooms(state: &Arc<NodeState>) -> Response {
    let rooms = state.rooms.lock().await;
    let list: Vec<RoomInfo> = rooms
        .values()
        .map(|config| RoomInfo {
            room: config.room.clone(),
            secure: config.encrypted_key_hex.is_some(),
        })
        .collect();
    ok_response(Some(serde_json::to_value(list).unwrap()))
}

/// Process an inbound room message from the relay.
pub async fn process_inbound_room(state: &Arc<NodeState>, envelope: mesh_pb::Envelope) {
    let room = match &envelope.topic {
        Some(t) => t.clone(),
        None => {
            tracing::warn!(msg_id = %envelope.message_id, "room message without topic, discarding");
            return;
        }
    };

    // Verify signature
    if !verify_signature(
        &envelope.from_public_key_b64,
        envelope.ciphertext_b64.as_bytes(),
        &envelope.signature_b64,
    ) {
        tracing::warn!(
            from = %envelope.from_node_id,
            msg_id = %envelope.message_id,
            "room message failed signature verification"
        );
        return;
    }

    // Check block list
    {
        let follow_store = state.follow_store.lock().await;
        if follow_store.is_blocked(&envelope.from_node_id) {
            return;
        }
    }

    // Look up room config
    let rooms = state.rooms.lock().await;
    let config = match rooms.get(&room) {
        Some(c) => c.clone(),
        None => {
            // Not subscribed to this room — discard silently
            return;
        }
    };
    drop(rooms);

    // Decrypt or extract body
    let body = if let Some(key) = config.key() {
        // Secure room: decrypt
        match decrypt_with_key(&key, &envelope.ciphertext_b64, &envelope.nonce_b64) {
            Ok(plaintext_bytes) => match String::from_utf8(plaintext_bytes) {
                Ok(s) => s,
                Err(_) => {
                    tracing::warn!(msg_id = %envelope.message_id, "room message decrypted but not valid UTF-8");
                    return;
                }
            },
            Err(_) => {
                // Wrong passphrase or corrupted — discard silently
                tracing::debug!(msg_id = %envelope.message_id, room = %room, "failed to decrypt room message (wrong key?)");
                return;
            }
        }
    } else {
        // Open room: body is plaintext in ciphertext_b64
        envelope.ciphertext_b64.clone()
    };

    let msg = InboxMessage {
        message_id: envelope.message_id.clone(),
        from_node_id: envelope.from_node_id.clone(),
        from_public_key_b64: envelope.from_public_key_b64.clone(),
        topic: Some(room.clone()),
        body: body.clone(),
        timestamp_ms: envelope.timestamp_ms,
        acked: false,
        message_type: MeshMessageType::RoomMessage,
    };

    let preview = body.chars().take(50).collect::<String>();
    let from = envelope.from_node_id.clone();
    let msg_id = envelope.message_id.clone();

    let mut inbox = state.inbox.lock().await;
    if let Err(e) = inbox.push(msg) {
        tracing::error!(err = %e, "failed to store room message");
        return;
    }

    // Emit event
    let _ = state.event_tx.send(Event::NewRoomMessage {
        message_id: msg_id,
        from,
        room,
        preview,
    });
}

/// Load persisted room configs from rooms.json.
pub fn load_rooms(state_dir: &Path) -> HashMap<String, RoomConfig> {
    let path = state_dir.join("rooms.json");
    if !path.exists() {
        return HashMap::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<Vec<RoomConfig>>(&data) {
            Ok(configs) => configs.into_iter().map(|c| (c.room.clone(), c)).collect(),
            Err(e) => {
                tracing::warn!(err = %e, "failed to parse rooms.json, starting fresh");
                HashMap::new()
            }
        },
        Err(e) => {
            tracing::warn!(err = %e, "failed to read rooms.json");
            HashMap::new()
        }
    }
}

/// Save room configs to rooms.json.
fn save_rooms(state_dir: &Path, rooms: &HashMap<String, RoomConfig>) -> anyhow::Result<()> {
    let path = state_dir.join("rooms.json");
    let configs: Vec<&RoomConfig> = rooms.values().collect();
    let data = serde_json::to_string_pretty(&configs)?;
    std::fs::write(path, data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_room_name_valid() {
        assert!(validate_room_name("test").is_ok());
        assert!(validate_room_name("my-room").is_ok());
        assert!(validate_room_name("room123").is_ok());
        assert!(validate_room_name("a").is_ok());
        assert!(validate_room_name(&"a".repeat(32)).is_ok());
    }

    #[test]
    fn validate_room_name_invalid() {
        assert!(validate_room_name("").is_err());
        assert!(validate_room_name(&"a".repeat(33)).is_err());
        assert!(validate_room_name("MyRoom").is_err()); // uppercase
        assert!(validate_room_name("my room").is_err()); // space
        assert!(validate_room_name("my_room").is_err()); // underscore
        assert!(validate_room_name("my.room").is_err()); // dot
    }

    #[test]
    fn room_config_key_roundtrip() {
        let key = [42u8; 32];
        let config = RoomConfig {
            room: "test".to_string(),
            encrypted_key_hex: Some(hex::encode(key)),
        };
        assert_eq!(config.key(), Some(key));
    }

    #[test]
    fn room_config_open_room_no_key() {
        let config = RoomConfig {
            room: "test".to_string(),
            encrypted_key_hex: None,
        };
        assert_eq!(config.key(), None);
    }

    #[test]
    fn load_rooms_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let rooms = load_rooms(dir.path());
        assert!(rooms.is_empty());
    }

    #[test]
    fn save_and_load_rooms() {
        let dir = tempfile::tempdir().unwrap();
        let mut rooms = HashMap::new();
        rooms.insert(
            "test".to_string(),
            RoomConfig {
                room: "test".to_string(),
                encrypted_key_hex: None,
            },
        );
        rooms.insert(
            "secret".to_string(),
            RoomConfig {
                room: "secret".to_string(),
                encrypted_key_hex: Some(hex::encode([1u8; 32])),
            },
        );

        save_rooms(dir.path(), &rooms).unwrap();
        let loaded = load_rooms(dir.path());
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains_key("test"));
        assert!(loaded.contains_key("secret"));
        assert!(loaded["test"].encrypted_key_hex.is_none());
        assert!(loaded["secret"].encrypted_key_hex.is_some());
    }
}
