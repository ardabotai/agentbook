use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, error, warn};

use tmax_protocol::{ErrorCode, Event, Request, Response, SessionId};

use crate::server::SharedState;

/// Per-client state tracking attachments and subscriptions.
struct ClientState {
    attachments: Vec<(SessionId, String)>, // (session_id, attachment_id)
    subscriptions: Vec<SessionId>,
}

impl ClientState {
    fn new() -> Self {
        Self {
            attachments: Vec::new(),
            subscriptions: Vec::new(),
        }
    }
}

/// Handle a single client connection.
pub async fn handle_client(stream: UnixStream, state: SharedState) {
    let (reader, writer) = stream.into_split();
    let reader = BufReader::new(reader);
    let writer = Arc::new(Mutex::new(writer));
    let client_state = Arc::new(Mutex::new(ClientState::new()));

    let mut lines = reader.lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                debug!("client disconnected");
                break;
            }
            Err(e) => {
                error!("read error: {e}");
                break;
            }
        };

        let request: Request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let resp = Response::Error {
                    message: format!("invalid request: {e}"),
                    code: ErrorCode::InvalidRequest,
                };
                let mut w = writer.lock().await;
                let _ = write_response(&mut w, &resp).await;
                continue;
            }
        };

        let response =
            handle_request(request, &state, &writer, &client_state).await;

        let mut w = writer.lock().await;
        if let Err(e) = write_response(&mut w, &response).await {
            error!("write error: {e}");
            break;
        }
    }

    // Cleanup: detach all attachments, remove subscriptions
    let cs = client_state.lock().await;
    let mut mgr = state.lock().await;
    for (session_id, attachment_id) in &cs.attachments {
        let _ = mgr.detach(session_id, attachment_id);
    }
}

async fn handle_request(
    request: Request,
    state: &SharedState,
    writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    client_state: &Arc<Mutex<ClientState>>,
) -> Response {
    match request {
        Request::SessionCreate {
            exec,
            args,
            cwd,
            label,
            sandbox,
            parent_id,
            cols,
            rows,
        } => {
            let mut mgr = state.lock().await;
            match mgr.create_session(libtmax::session::SessionCreateConfig {
                exec,
                args,
                cwd,
                label,
                sandbox,
                parent_id,
                cols,
                rows,
            }) {
                Ok((session_id, _rx)) => {
                    // Spawn PTY I/O loop
                    let pty_reader = match mgr.take_pty_reader(&session_id) {
                        Ok(r) => r,
                        Err(e) => {
                            return Response::Error {
                                message: e.to_string(),
                                code: ErrorCode::ServerError,
                            };
                        }
                    };

                    let state_clone = Arc::clone(state);
                    let sid = session_id.clone();
                    tokio::spawn(async move {
                        pty_io_loop(pty_reader, sid, state_clone).await;
                    });

                    Response::Ok {
                        data: Some(serde_json::json!({ "session_id": session_id })),
                    }
                }
                Err(e) => Response::Error {
                    message: e.to_string(),
                    code: ErrorCode::ServerError,
                },
            }
        }

        Request::SessionDestroy {
            session_id,
            cascade,
        } => {
            let mut mgr = state.lock().await;
            match mgr.destroy_session(&session_id, cascade) {
                Ok(destroyed) => Response::Ok {
                    data: Some(serde_json::json!({ "destroyed": destroyed })),
                },
                Err(e) => Response::Error {
                    message: e.to_string(),
                    code: ErrorCode::SessionNotFound,
                },
            }
        }

        Request::SessionList => {
            let mgr = state.lock().await;
            let sessions = mgr.list_sessions();
            Response::Ok {
                data: Some(serde_json::to_value(&sessions).unwrap_or_default()),
            }
        }

        Request::SessionTree => {
            let mgr = state.lock().await;
            let tree = mgr.session_tree();
            Response::Ok {
                data: Some(serde_json::to_value(&tree).unwrap_or_default()),
            }
        }

        Request::SessionInfo { session_id } => {
            let mgr = state.lock().await;
            match mgr.get_session_info(&session_id) {
                Ok(info) => Response::Ok {
                    data: Some(serde_json::to_value(&info).unwrap_or_default()),
                },
                Err(e) => Response::Error {
                    message: e.to_string(),
                    code: ErrorCode::SessionNotFound,
                },
            }
        }

        Request::Attach { session_id, mode } => {
            let mut mgr = state.lock().await;
            match mgr.attach(&session_id, mode) {
                Ok(attachment_id) => {
                    let mut cs = client_state.lock().await;
                    cs.attachments
                        .push((session_id.clone(), attachment_id.clone()));
                    Response::Ok {
                        data: Some(serde_json::json!({ "attachment_id": attachment_id })),
                    }
                }
                Err(e) => Response::Error {
                    message: e.to_string(),
                    code: ErrorCode::AttachmentDenied,
                },
            }
        }

        Request::Detach { session_id } => {
            let mut cs = client_state.lock().await;
            if let Some(pos) = cs.attachments.iter().position(|(sid, _)| sid == &session_id) {
                let (sid, att_id) = cs.attachments.remove(pos);
                let mut mgr = state.lock().await;
                match mgr.detach(&sid, &att_id) {
                    Ok(()) => Response::Ok { data: None },
                    Err(e) => Response::Error {
                        message: e.to_string(),
                        code: ErrorCode::AttachmentDenied,
                    },
                }
            } else {
                Response::Error {
                    message: "not attached to this session".to_string(),
                    code: ErrorCode::AttachmentDenied,
                }
            }
        }

        Request::SendInput { session_id, data } => {
            // Verify client has an edit attachment
            let cs = client_state.lock().await;
            let has_edit = cs
                .attachments
                .iter()
                .any(|(sid, _)| sid == &session_id);

            if !has_edit {
                return Response::Error {
                    message: "no attachment to this session".to_string(),
                    code: ErrorCode::InputDenied,
                };
            }
            drop(cs);

            let mut mgr = state.lock().await;
            match mgr.send_input(&session_id, &data) {
                Ok(()) => Response::Ok { data: None },
                Err(e) => Response::Error {
                    message: e.to_string(),
                    code: ErrorCode::InputDenied,
                },
            }
        }

        Request::Resize {
            session_id,
            cols,
            rows,
        } => {
            let mut mgr = state.lock().await;
            match mgr.resize(&session_id, cols, rows) {
                Ok(()) => Response::Ok { data: None },
                Err(e) => Response::Error {
                    message: e.to_string(),
                    code: ErrorCode::SessionNotFound,
                },
            }
        }

        Request::MarkerInsert { session_id, name } => {
            let mut mgr = state.lock().await;
            match mgr.insert_marker(&session_id, name) {
                Ok(seq) => Response::Ok {
                    data: Some(serde_json::json!({ "seq": seq })),
                },
                Err(e) => Response::Error {
                    message: e.to_string(),
                    code: ErrorCode::SessionNotFound,
                },
            }
        }

        Request::MarkerList { session_id } => {
            let mgr = state.lock().await;
            match mgr.list_markers(&session_id) {
                Ok(markers) => Response::Ok {
                    data: Some(serde_json::to_value(&markers).unwrap_or_default()),
                },
                Err(e) => Response::Error {
                    message: e.to_string(),
                    code: ErrorCode::SessionNotFound,
                },
            }
        }

        Request::Subscribe {
            session_id,
            last_seq,
        } => {
            let mgr = state.lock().await;

            // Get catch-up chunks
            let catchup = match mgr.get_catchup(&session_id, last_seq) {
                Ok(Some(chunks)) => chunks,
                Ok(None) => {
                    // Gap too large, need snapshot - for now just send empty
                    vec![]
                }
                Err(e) => {
                    return Response::Error {
                        message: e.to_string(),
                        code: ErrorCode::SessionNotFound,
                    };
                }
            };

            // Subscribe to live events
            let rx = match mgr.subscribe(&session_id) {
                Ok(rx) => rx,
                Err(e) => {
                    return Response::Error {
                        message: e.to_string(),
                        code: ErrorCode::SessionNotFound,
                    };
                }
            };

            drop(mgr);

            // Track subscription
            let mut cs = client_state.lock().await;
            cs.subscriptions.push(session_id.clone());
            drop(cs);

            // Send catch-up chunks as events
            let writer_clone = Arc::clone(writer);
            for chunk in &catchup {
                let event = Response::Event(Event::Output {
                    session_id: session_id.clone(),
                    seq: chunk.seq,
                    data: chunk.data.clone(),
                });
                let mut w = writer_clone.lock().await;
                let _ = write_response(&mut w, &event).await;
            }

            // Spawn task to forward live events
            let writer_clone = Arc::clone(writer);
            let sid = session_id.clone();
            tokio::spawn(async move {
                forward_events(rx, writer_clone, sid).await;
            });

            Response::Ok {
                data: Some(serde_json::json!({
                    "catchup_count": catchup.len(),
                })),
            }
        }

        Request::Unsubscribe { session_id } => {
            let mut cs = client_state.lock().await;
            cs.subscriptions.retain(|sid| sid != &session_id);
            // The subscription task will naturally stop when the receiver drops
            Response::Ok { data: None }
        }
    }
}

/// Forward broadcast events to a client's write stream.
async fn forward_events(
    mut rx: broadcast::Receiver<Event>,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    session_id: SessionId,
) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                let resp = Response::Event(event);
                let mut w = writer.lock().await;
                if write_response(&mut w, &resp).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(session_id = %session_id, skipped = n, "subscriber lagged");
                // Continue - client missed some events but can catch up
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!(session_id = %session_id, "broadcast channel closed");
                break;
            }
        }
    }
}

/// PTY I/O loop: reads from PTY and records output.
async fn pty_io_loop(
    mut reader: Box<dyn std::io::Read + Send>,
    session_id: SessionId,
    state: SharedState,
) {
    let mut buf = [0u8; 4096];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                // PTY closed (process exited)
                let mut mgr = state.lock().await;
                let _ = mgr.record_exit(&session_id, Some(0), None);
                break;
            }
            Ok(n) => {
                let data = buf[..n].to_vec();
                let mut mgr = state.lock().await;
                if mgr.record_output(&session_id, data).is_err() {
                    break;
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    continue;
                }
                debug!(session_id = %session_id, error = %e, "pty read error");
                let mut mgr = state.lock().await;
                let _ = mgr.record_exit(&session_id, None, None);
                break;
            }
        }
    }
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &Response,
) -> Result<(), std::io::Error> {
    let json = serde_json::to_string(response).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}
