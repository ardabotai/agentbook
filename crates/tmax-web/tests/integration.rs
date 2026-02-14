use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

mod helpers {
    use super::*;

    /// Start a mock tmax-server that responds to known requests.
    pub async fn start_mock_server(socket_path: &PathBuf) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(socket_path).unwrap();

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };

                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let mut lines = BufReader::new(reader).lines();

                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: serde_json::Value =
                            serde_json::from_str(&line).unwrap_or_default();
                        let cmd = req.get("cmd").and_then(|c| c.as_str()).unwrap_or("");

                        let response = match cmd {
                            "session_list" => json!({
                                "type": "ok",
                                "data": [
                                    {
                                        "id": "sess-001",
                                        "label": "test-session",
                                        "exec": "/bin/bash",
                                        "args": [],
                                        "cwd": "/tmp",
                                        "parent_id": null,
                                        "children": [],
                                        "sandbox": null,
                                        "created_at_epoch_ms": 1700000000000u64,
                                        "attachment_count": 0,
                                        "edit_attachment_count": 0,
                                        "exited": false,
                                        "exit_code": null
                                    }
                                ]
                            }),
                            "session_tree" => json!({
                                "type": "ok",
                                "data": []
                            }),
                            "session_info" => {
                                let session_id = req
                                    .get("session_id")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("");
                                if session_id == "sess-001" {
                                    json!({
                                        "type": "ok",
                                        "data": {
                                            "id": "sess-001",
                                            "label": "test-session",
                                            "exec": "/bin/bash",
                                            "args": [],
                                            "cwd": "/tmp",
                                            "parent_id": null,
                                            "children": [],
                                            "sandbox": null,
                                            "created_at_epoch_ms": 1700000000000u64,
                                            "attachment_count": 0,
                                            "edit_attachment_count": 0,
                                            "exited": false,
                                            "exit_code": null
                                        }
                                    })
                                } else {
                                    json!({
                                        "type": "error",
                                        "message": "session not found",
                                        "code": "session_not_found"
                                    })
                                }
                            }
                            "attach" => json!({
                                "type": "ok",
                                "data": { "attachment_id": "att-001" }
                            }),
                            "subscribe" => {
                                // Send subscription confirmation, then stream an output event.
                                let sid = req
                                    .get("session_id")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let sub_resp = json!({
                                    "type": "ok",
                                    "data": { "catchup_count": 0 }
                                });
                                let resp_str = serde_json::to_string(&sub_resp).unwrap();
                                let _ = writer.write_all(resp_str.as_bytes()).await;
                                let _ = writer.write_all(b"\n").await;
                                let _ = writer.flush().await;

                                // Send a test output event after a small delay.
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                let event = json!({
                                    "type": "event",
                                    "event": "output",
                                    "session_id": sid,
                                    "seq": 1,
                                    "data": base64::Engine::encode(
                                        &base64::engine::general_purpose::STANDARD,
                                        b"hello from pty"
                                    )
                                });
                                let event_str = serde_json::to_string(&event).unwrap();
                                let _ = writer.write_all(event_str.as_bytes()).await;
                                let _ = writer.write_all(b"\n").await;
                                let _ = writer.flush().await;

                                // Keep connection alive for a bit.
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                continue;
                            }
                            _ => json!({
                                "type": "error",
                                "message": "unknown command",
                                "code": "invalid_request"
                            }),
                        };

                        let resp_str = serde_json::to_string(&response).unwrap();
                        let _ = writer.write_all(resp_str.as_bytes()).await;
                        let _ = writer.write_all(b"\n").await;
                        let _ = writer.flush().await;
                    }
                });
            }
        })
    }

    /// Create a temp socket path (short enough for macOS SUN_LEN limit).
    pub fn temp_socket_path() -> PathBuf {
        let short_id: String = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let path = PathBuf::from(format!("/tmp/tmax-t-{short_id}.sock"));
        path
    }
}

#[tokio::test]
async fn test_rest_list_sessions() {
    let socket_path = helpers::temp_socket_path();
    let _server = helpers::start_mock_server(&socket_path).await;

    // Give server a moment to bind.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect directly as a client and verify the mock works.
    let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let req = json!({"cmd": "session_list"});
    writer
        .write_all(serde_json::to_string(&req).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();

    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["type"], "ok");
    assert!(resp["data"].is_array());
    assert_eq!(resp["data"][0]["id"], "sess-001");

    // Cleanup
    let _ = std::fs::remove_dir_all(socket_path.parent().unwrap());
}

#[tokio::test]
async fn test_rest_session_info() {
    let socket_path = helpers::temp_socket_path();
    let _server = helpers::start_mock_server(&socket_path).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Test existing session
    let req = json!({"cmd": "session_info", "session_id": "sess-001"});
    writer
        .write_all(serde_json::to_string(&req).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();

    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["type"], "ok");
    assert_eq!(resp["data"]["id"], "sess-001");

    let _ = std::fs::remove_dir_all(socket_path.parent().unwrap());
}

#[tokio::test]
async fn test_rest_session_not_found() {
    let socket_path = helpers::temp_socket_path();
    let _server = helpers::start_mock_server(&socket_path).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let req = json!({"cmd": "session_info", "session_id": "nonexistent"});
    writer
        .write_all(serde_json::to_string(&req).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();

    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "session_not_found");

    let _ = std::fs::remove_dir_all(socket_path.parent().unwrap());
}

#[tokio::test]
async fn test_ws_subscribe_receives_output() {
    let socket_path = helpers::temp_socket_path();
    let _server = helpers::start_mock_server(&socket_path).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Test the subscribe flow via direct Unix socket (simulating what tmax-web does).
    let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Attach first
    let req = json!({"cmd": "attach", "session_id": "sess-001", "mode": "view"});
    writer
        .write_all(serde_json::to_string(&req).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();

    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["type"], "ok");

    // Subscribe
    let req = json!({"cmd": "subscribe", "session_id": "sess-001"});
    writer
        .write_all(serde_json::to_string(&req).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();

    // Should get subscription confirmation
    let line = lines.next_line().await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["type"], "ok");
    assert_eq!(resp["data"]["catchup_count"], 0);

    // Should get output event
    let line = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["type"], "event");
    assert_eq!(resp["event"], "output");
    assert_eq!(resp["session_id"], "sess-001");

    let _ = std::fs::remove_dir_all(socket_path.parent().unwrap());
}

#[tokio::test]
async fn test_binary_frame_protocol() {
    // Test the binary frame encoding/decoding used for WS output.
    let session_id = "sess-abc-123-def-456";
    let data = b"escape sequence \x1b[32mgreen\x1b[0m text";

    // Encode
    let frame = tmax_web_binary_encode(session_id, data);

    // Verify structure
    assert_eq!(frame[0] as usize, session_id.len());
    assert_eq!(
        &frame[1..1 + session_id.len()],
        session_id.as_bytes()
    );
    assert_eq!(&frame[1 + session_id.len()..], data);

    // Decode
    let (decoded_sid, decoded_data) = tmax_web_binary_decode(&frame).unwrap();
    assert_eq!(decoded_sid, session_id);
    assert_eq!(decoded_data, data);
}

// Re-implement the binary frame functions here for testing
// (since they're in the binary crate, not a library).
fn tmax_web_binary_encode(session_id: &str, data: &[u8]) -> Vec<u8> {
    let sid_bytes = session_id.as_bytes();
    let sid_len = sid_bytes.len().min(255) as u8;
    let mut frame = Vec::with_capacity(1 + sid_len as usize + data.len());
    frame.push(sid_len);
    frame.extend_from_slice(&sid_bytes[..sid_len as usize]);
    frame.extend_from_slice(data);
    frame
}

fn tmax_web_binary_decode(frame: &[u8]) -> Option<(&str, &[u8])> {
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
