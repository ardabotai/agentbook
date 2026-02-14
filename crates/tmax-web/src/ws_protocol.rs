use serde::{Deserialize, Serialize};
use tmax_protocol::SessionId;

/// Client-to-server control messages sent as JSON text frames.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WsClientMessage {
    /// Subscribe to a session's output stream.
    Subscribe {
        session_id: SessionId,
        mode: Option<tmax_protocol::AttachMode>,
        #[serde(default)]
        last_seq: Option<u64>,
    },
    /// Unsubscribe from a session.
    Unsubscribe {
        session_id: SessionId,
    },
    /// Send input to a session (edit mode only).
    Input {
        session_id: SessionId,
        /// Base64-encoded input data.
        data: String,
    },
    /// Resize a session's PTY.
    Resize {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },
}

/// Server-to-client control messages sent as JSON text frames.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerMessage {
    /// Subscription confirmed.
    Subscribed {
        session_id: SessionId,
        catchup_count: usize,
    },
    /// Unsubscription confirmed.
    Unsubscribed {
        session_id: SessionId,
    },
    /// An error occurred.
    Error {
        message: String,
        session_id: Option<SessionId>,
    },
    /// A session event (created, exited, destroyed, marker, attach/detach).
    Event(tmax_protocol::Event),
}

/// Binary output frame header.
/// Binary WebSocket frames are structured as:
///   [session_id_len: u8][session_id: bytes][pty_data: bytes]
/// This allows the client to demux output from multiple sessions
/// over a single WebSocket connection.
pub fn encode_binary_frame(session_id: &str, data: &[u8]) -> Vec<u8> {
    let sid_bytes = session_id.as_bytes();
    let sid_len = sid_bytes.len().min(255) as u8;
    let mut frame = Vec::with_capacity(1 + sid_len as usize + data.len());
    frame.push(sid_len);
    frame.extend_from_slice(&sid_bytes[..sid_len as usize]);
    frame.extend_from_slice(data);
    frame
}

/// Decode a binary frame into (session_id, data).
pub fn decode_binary_frame(frame: &[u8]) -> Option<(&str, &[u8])> {
    if frame.is_empty() {
        return None;
    }
    let sid_len = frame[0] as usize;
    if frame.len() < 1 + sid_len {
        return None;
    }
    let session_id = std::str::from_utf8(&frame[1..1 + sid_len]).ok()?;
    let data = &frame[1 + sid_len..];
    Some((session_id, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_frame_roundtrip() {
        let sid = "abc-123-def";
        let data = b"hello world";
        let frame = encode_binary_frame(sid, data);
        let (decoded_sid, decoded_data) = decode_binary_frame(&frame).unwrap();
        assert_eq!(decoded_sid, sid);
        assert_eq!(decoded_data, data);
    }

    #[test]
    fn binary_frame_empty_data() {
        let frame = encode_binary_frame("sess", &[]);
        let (sid, data) = decode_binary_frame(&frame).unwrap();
        assert_eq!(sid, "sess");
        assert!(data.is_empty());
    }

    #[test]
    fn decode_empty_frame() {
        assert!(decode_binary_frame(&[]).is_none());
    }

    #[test]
    fn ws_client_message_roundtrip() {
        let msg = WsClientMessage::Subscribe {
            session_id: "sess-1".to_string(),
            mode: Some(tmax_protocol::AttachMode::View),
            last_seq: Some(42),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: WsClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            WsClientMessage::Subscribe {
                session_id,
                last_seq,
                ..
            } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(last_seq, Some(42));
            }
            _ => panic!("wrong variant"),
        }
    }
}
