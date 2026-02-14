use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use tmax_protocol::{AttachMode, ErrorCode, Event, Request, Response, SessionId};

use crate::server::SharedState;

/// Per-client state tracking attachments and subscriptions.
struct ClientState {
    attachments: Vec<(SessionId, String, AttachMode)>, // (session_id, attachment_id, mode)
    subscriptions: Vec<(SessionId, CancellationToken)>,
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
    for (session_id, attachment_id, _mode) in &cs.attachments {
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
                            let (code, message) = e.to_error_code();
                            return Response::Error { message, code };
                        }
                    };

                    let child = match mgr.take_child(&session_id) {
                        Ok(c) => Some(c),
                        Err(e) => {
                            warn!(session_id = %session_id, error = %e, "could not take child handle");
                            None
                        }
                    };

                    let state_clone = Arc::clone(state);
                    let sid = session_id.clone();
                    tokio::spawn(async move {
                        pty_io_loop(pty_reader, child, sid, state_clone).await;
                    });

                    Response::Ok {
                        data: Some(serde_json::json!({ "session_id": session_id })),
                    }
                }
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    Response::Error { message, code }
                }
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
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    Response::Error { message, code }
                }
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
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    Response::Error { message, code }
                }
            }
        }

        Request::Attach { session_id, mode } => {
            let mut mgr = state.lock().await;
            match mgr.attach(&session_id, mode) {
                Ok(attachment_id) => {
                    let mut cs = client_state.lock().await;
                    cs.attachments
                        .push((session_id.clone(), attachment_id.clone(), mode));
                    Response::Ok {
                        data: Some(serde_json::json!({ "attachment_id": attachment_id })),
                    }
                }
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    Response::Error { message, code }
                }
            }
        }

        Request::Detach { session_id } => {
            let mut cs = client_state.lock().await;
            if let Some(pos) = cs.attachments.iter().position(|(sid, _, _)| sid == &session_id) {
                let (sid, att_id, _mode) = cs.attachments.remove(pos);
                let mut mgr = state.lock().await;
                match mgr.detach(&sid, &att_id) {
                    Ok(()) => Response::Ok { data: None },
                    Err(e) => {
                        let (code, message) = e.to_error_code();
                        Response::Error { message, code }
                    }
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
                .any(|(sid, _, m)| sid == &session_id && *m == AttachMode::Edit);

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
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    Response::Error { message, code }
                }
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
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    Response::Error { message, code }
                }
            }
        }

        Request::MarkerInsert { session_id, name } => {
            let mut mgr = state.lock().await;
            match mgr.insert_marker(&session_id, name) {
                Ok(seq) => Response::Ok {
                    data: Some(serde_json::json!({ "seq": seq })),
                },
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    Response::Error { message, code }
                }
            }
        }

        Request::MarkerList { session_id } => {
            let mgr = state.lock().await;
            match mgr.list_markers(&session_id) {
                Ok(markers) => Response::Ok {
                    data: Some(serde_json::to_value(&markers).unwrap_or_default()),
                },
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    Response::Error { message, code }
                }
            }
        }

        Request::Subscribe {
            session_id,
            last_seq,
        } => {
            // Prevent duplicate subscriptions
            let cs = client_state.lock().await;
            if cs.subscriptions.iter().any(|(sid, _)| sid == &session_id) {
                return Response::Error {
                    message: "already subscribed to this session".to_string(),
                    code: ErrorCode::InvalidRequest,
                };
            }
            drop(cs);

            let mgr = state.lock().await;

            // Get catch-up chunks
            let catchup = match mgr.get_catchup(&session_id, last_seq) {
                Ok(Some(chunks)) => chunks,
                Ok(None) => {
                    // Gap too large, need snapshot - for now just send empty
                    vec![]
                }
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    return Response::Error { message, code };
                }
            };

            // Subscribe to live events
            let rx = match mgr.subscribe(&session_id) {
                Ok(rx) => rx,
                Err(e) => {
                    let (code, message) = e.to_error_code();
                    return Response::Error { message, code };
                }
            };

            drop(mgr);

            // Create cancellation token for this subscription
            let token = CancellationToken::new();

            // Track subscription with token
            let mut cs = client_state.lock().await;
            cs.subscriptions.push((session_id.clone(), token.clone()));
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
            let cancel = token.clone();
            tokio::spawn(async move {
                forward_events(rx, writer_clone, sid, cancel).await;
            });

            Response::Ok {
                data: Some(serde_json::json!({
                    "catchup_count": catchup.len(),
                })),
            }
        }

        Request::Unsubscribe { session_id } => {
            let mut cs = client_state.lock().await;
            if let Some(pos) = cs.subscriptions.iter().position(|(sid, _)| sid == &session_id) {
                let (_, token) = cs.subscriptions.remove(pos);
                token.cancel();
            }
            Response::Ok { data: None }
        }
    }
}

/// Forward broadcast events to a client's write stream.
async fn forward_events(
    mut rx: broadcast::Receiver<Event>,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    session_id: SessionId,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        let resp = Response::Event(event);
                        let mut w = writer.lock().await;
                        if write_response(&mut w, &resp).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(session_id = %session_id, skipped = n, "subscriber lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!(session_id = %session_id, "broadcast channel closed");
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                debug!(session_id = %session_id, "subscription cancelled");
                break;
            }
        }
    }
}

/// PTY I/O loop: reads from PTY and records output.
///
/// Uses `spawn_blocking` for the blocking PTY read to avoid blocking Tokio worker threads.
/// On EOF, attempts to capture the child process exit code via `try_wait`.
async fn pty_io_loop(
    reader: Box<dyn std::io::Read + Send>,
    mut child: Option<Box<dyn portable_pty::Child + Send>>,
    session_id: SessionId,
    state: SharedState,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Option<Vec<u8>>>(64);

    let sid_for_blocking = session_id.clone();

    // Spawn the blocking read loop on the blocking thread pool
    tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.blocking_send(None);
                    break;
                }
                Ok(n) => {
                    if tx.blocking_send(Some(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    debug!(session_id = %sid_for_blocking, error = %e, "pty read error");
                    let _ = tx.blocking_send(None);
                    break;
                }
            }
        }
    });

    // Async loop to process data from the blocking reader
    while let Some(msg) = rx.recv().await {
        match msg {
            Some(data) => {
                let mut mgr = state.lock().await;
                if mgr.record_output(&session_id, data).is_err() {
                    break;
                }
            }
            None => {
                // EOF or error - try to get exit code from child
                let exit_code = child.as_mut().and_then(|c| {
                    c.try_wait()
                        .ok()
                        .flatten()
                        .map(|status| status.exit_code() as i32)
                });
                let mut mgr = state.lock().await;
                let _ = mgr.record_exit(&session_id, exit_code, None);
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
