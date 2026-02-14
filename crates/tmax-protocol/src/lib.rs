use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Unique identifier for a session.
pub type SessionId = String;

/// Client-to-server requests sent as JSON-lines over the Unix socket.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    // Session management
    SessionCreate {
        exec: String,
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        sandbox: Option<SandboxConfig>,
        #[serde(default)]
        parent_id: Option<SessionId>,
        #[serde(default = "default_cols")]
        cols: u16,
        #[serde(default = "default_rows")]
        rows: u16,
    },
    SessionDestroy {
        session_id: SessionId,
        #[serde(default)]
        cascade: bool,
    },
    SessionList,
    SessionTree,
    SessionInfo {
        session_id: SessionId,
    },

    // Attachments
    Attach {
        session_id: SessionId,
        mode: AttachMode,
    },
    Detach {
        session_id: SessionId,
    },

    // I/O
    SendInput {
        session_id: SessionId,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    Resize {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },

    // Markers
    MarkerInsert {
        session_id: SessionId,
        name: String,
    },
    MarkerList {
        session_id: SessionId,
    },

    // Event streaming
    Subscribe {
        session_id: SessionId,
        #[serde(default)]
        last_seq: Option<u64>,
    },
    Unsubscribe {
        session_id: SessionId,
    },
}

/// Server-to-client responses.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },
    Error {
        message: String,
        code: ErrorCode,
    },
    Event(Event),
}

/// Events streamed to subscribers.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Output {
        session_id: SessionId,
        seq: u64,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    SessionCreated {
        session_id: SessionId,
        label: Option<String>,
    },
    SessionExited {
        session_id: SessionId,
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    SessionDestroyed {
        session_id: SessionId,
    },
    MarkerInserted {
        session_id: SessionId,
        name: String,
        seq: u64,
    },
    Attached {
        session_id: SessionId,
        mode: AttachMode,
        attachment_id: String,
    },
    Detached {
        session_id: SessionId,
        attachment_id: String,
    },
}

/// Attachment mode: edit allows input, view is read-only.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachMode {
    Edit,
    View,
}

/// Error codes for structured error handling.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    SessionNotFound,
    SessionAlreadyExists,
    AttachmentDenied,
    InputDenied,
    InvalidRequest,
    SandboxViolation,
    ServerError,
}

/// Filesystem sandbox configuration for a session.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SandboxConfig {
    /// Directories the session process can write to.
    pub writable_paths: Vec<PathBuf>,
    /// If true, inherit parent session's sandbox constraints.
    #[serde(default = "default_true")]
    pub inherit_parent: bool,
}

/// Summary info returned by session list/info commands.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionInfo {
    pub id: SessionId,
    pub label: Option<String>,
    pub exec: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub parent_id: Option<SessionId>,
    pub children: Vec<SessionId>,
    pub sandbox: Option<SandboxConfig>,
    pub created_at_epoch_ms: u64,
    pub attachment_count: usize,
    pub edit_attachment_count: usize,
    pub exited: bool,
    pub exit_code: Option<i32>,
}

/// Session tree node for hierarchical display.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionTreeNode {
    pub info: SessionInfo,
    pub children: Vec<SessionTreeNode>,
}

/// Marker stored per session.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarkerInfo {
    pub name: String,
    pub seq: u64,
    pub timestamp_epoch_ms: u64,
}

fn default_cols() -> u16 {
    80
}

fn default_rows() -> u16 {
    24
}

fn default_true() -> bool {
    true
}

/// Base64 encoding for byte arrays in JSON.
mod base64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::Error;
        let encoded = base64_encode(bytes).map_err(S::Error::custom)?;
        serializer.serialize_str(&encoded)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        base64_decode(&s).map_err(serde::de::Error::custom)
    }

    fn base64_encode(bytes: &[u8]) -> Result<String, String> {
        use std::io::Write;
        // Simple base64 encoding without external dependency
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = Vec::new();
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
            let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
            let triple = (b0 << 16) | (b1 << 8) | b2;

            let _ = result.write_all(&[CHARS[((triple >> 18) & 0x3F) as usize]]);
            let _ = result.write_all(&[CHARS[((triple >> 12) & 0x3F) as usize]]);
            if chunk.len() > 1 {
                let _ = result.write_all(&[CHARS[((triple >> 6) & 0x3F) as usize]]);
            } else {
                let _ = result.write_all(b"=");
            }
            if chunk.len() > 2 {
                let _ = result.write_all(&[CHARS[(triple & 0x3F) as usize]]);
            } else {
                let _ = result.write_all(b"=");
            }
        }
        String::from_utf8(result).map_err(|e| e.to_string())
    }

    fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
        const DECODE: [u8; 128] = {
            let mut table = [255u8; 128];
            let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut i = 0;
            while i < chars.len() {
                table[chars[i] as usize] = i as u8;
                i += 1;
            }
            table
        };

        let bytes: Vec<u8> = input.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
        if bytes.len() % 4 != 0 {
            return Err("invalid base64 length".to_string());
        }

        let mut result = Vec::with_capacity(bytes.len() * 3 / 4);
        for chunk in bytes.chunks(4) {
            let mut vals = [0u32; 4];
            for (i, &b) in chunk.iter().enumerate() {
                if b == b'=' {
                    vals[i] = 0;
                } else if b < 128 && DECODE[b as usize] != 255 {
                    vals[i] = DECODE[b as usize] as u32;
                } else {
                    return Err(format!("invalid base64 character: {}", b as char));
                }
            }
            let triple = (vals[0] << 18) | (vals[1] << 12) | (vals[2] << 6) | vals[3];
            result.push(((triple >> 16) & 0xFF) as u8);
            if chunk[2] != b'=' {
                result.push(((triple >> 8) & 0xFF) as u8);
            }
            if chunk[3] != b'=' {
                result.push((triple & 0xFF) as u8);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_session_create_roundtrip() {
        let req = Request::SessionCreate {
            exec: "/bin/bash".to_string(),
            args: vec!["-l".to_string()],
            cwd: Some(PathBuf::from("/tmp")),
            label: Some("test-session".to_string()),
            sandbox: None,
            parent_id: None,
            cols: 120,
            rows: 40,
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: Request = serde_json::from_str(&json).unwrap();

        match parsed {
            Request::SessionCreate {
                exec, cols, rows, ..
            } => {
                assert_eq!(exec, "/bin/bash");
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_tag_format() {
        let req = Request::SessionList;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"cmd":"session_list"}"#);
    }

    #[test]
    fn response_ok_roundtrip() {
        let resp = Response::Ok {
            data: Some(serde_json::json!({"session_id": "abc-123"})),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::Ok { data } => {
                assert!(data.is_some());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_error_roundtrip() {
        let resp = Response::Error {
            message: "session not found".to_string(),
            code: ErrorCode::SessionNotFound,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("session_not_found"));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::Error { code, .. } => {
                assert_eq!(code, ErrorCode::SessionNotFound);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn event_output_roundtrip() {
        let event = Event::Output {
            session_id: "sess-1".to_string(),
            seq: 42,
            data: b"hello world".to_vec(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("output"));
        let parsed: Event = serde_json::from_str(&json).unwrap();
        match parsed {
            Event::Output {
                session_id,
                seq,
                data,
            } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(seq, 42);
                assert_eq!(data, b"hello world");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn event_session_exited_roundtrip() {
        let event = Event::SessionExited {
            session_id: "sess-1".to_string(),
            exit_code: Some(0),
            signal: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();
        match parsed {
            Event::SessionExited {
                exit_code, signal, ..
            } => {
                assert_eq!(exit_code, Some(0));
                assert_eq!(signal, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn attach_mode_roundtrip() {
        let edit = AttachMode::Edit;
        let view = AttachMode::View;
        assert_eq!(serde_json::to_string(&edit).unwrap(), "\"edit\"");
        assert_eq!(serde_json::to_string(&view).unwrap(), "\"view\"");
        assert_eq!(
            serde_json::from_str::<AttachMode>("\"edit\"").unwrap(),
            AttachMode::Edit
        );
        assert_eq!(
            serde_json::from_str::<AttachMode>("\"view\"").unwrap(),
            AttachMode::View
        );
    }

    #[test]
    fn sandbox_config_roundtrip() {
        let config = SandboxConfig {
            writable_paths: vec![PathBuf::from("/tmp/workspace")],
            inherit_parent: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: SandboxConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.writable_paths.len(), 1);
        assert!(parsed.inherit_parent);
    }

    #[test]
    fn session_info_roundtrip() {
        let info = SessionInfo {
            id: "sess-1".to_string(),
            label: Some("test".to_string()),
            exec: "/bin/bash".to_string(),
            args: vec![],
            cwd: PathBuf::from("/home/user"),
            parent_id: None,
            children: vec!["sess-2".to_string()],
            sandbox: None,
            created_at_epoch_ms: 1700000000000,
            attachment_count: 1,
            edit_attachment_count: 1,
            exited: false,
            exit_code: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "sess-1");
        assert_eq!(parsed.children.len(), 1);
    }

    #[test]
    fn request_defaults() {
        // Test that optional fields and defaults work when parsing minimal JSON
        let json = r#"{"cmd":"session_create","exec":"/bin/sh","args":[]}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::SessionCreate { cols, rows, cwd, .. } => {
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
                assert!(cwd.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn send_input_base64_roundtrip() {
        let req = Request::SendInput {
            session_id: "sess-1".to_string(),
            data: b"ls -la\n".to_vec(),
        };
        let json = serde_json::to_string(&req).unwrap();
        // Should contain base64, not raw bytes
        assert!(!json.contains("ls -la"));
        let parsed: Request = serde_json::from_str(&json).unwrap();
        match parsed {
            Request::SendInput { data, .. } => {
                assert_eq!(data, b"ls -la\n");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn all_error_codes_roundtrip() {
        let codes = vec![
            ErrorCode::SessionNotFound,
            ErrorCode::SessionAlreadyExists,
            ErrorCode::AttachmentDenied,
            ErrorCode::InputDenied,
            ErrorCode::InvalidRequest,
            ErrorCode::SandboxViolation,
            ErrorCode::ServerError,
        ];
        for code in codes {
            let json = serde_json::to_string(&code).unwrap();
            let parsed: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, code);
        }
    }
}
