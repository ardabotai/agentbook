use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use tmax_protocol::{AttachMode, Event, Request, Response as TmaxResponse, SessionId};

use crate::client::TmaxClient;
use crate::ws_protocol::{
    decode_binary_frame, encode_binary_frame, WsClientMessage, WsServerMessage,
};

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub socket_path: PathBuf,
    pub batch_interval: Duration,
    pub max_lag_chunks: u64,
}

/// WebSocket upgrade handler.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Tracks an active session subscription.
struct SessionSub {
    /// Handle for the forwarding task.
    task: tokio::task::JoinHandle<()>,
    /// Channel to send input/resize commands to the session's tmax-server connection.
    input_tx: mpsc::Sender<Request>,
}

/// Handle a single WebSocket connection.
///
/// Architecture:
/// - One tmax-server connection per WS subscription (session).
/// - Each subscription task reads events AND accepts input via an mpsc channel.
/// - A central send task writes to the WS.
/// - The recv task reads client messages and manages subscriptions.
async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (ws_sender, mut ws_receiver) = socket.split();

    // Channel for sending messages to the WebSocket (from multiple subscription tasks).
    let (tx, mut rx) = mpsc::channel::<Message>(256);

    // Spawn the write loop: reads from mpsc and writes to WS.
    let mut ws_sender = ws_sender;
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Track active subscriptions: session_id -> subscription state.
    let mut subscriptions: HashMap<SessionId, SessionSub> = HashMap::new();

    // Read loop: handle client messages.
    while let Some(msg_result) = ws_receiver.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                debug!("ws read error: {e}");
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                let client_msg: WsClientMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        let err = WsServerMessage::Error {
                            message: format!("invalid message: {e}"),
                            session_id: None,
                        };
                        let _ = tx
                            .send(Message::text(serde_json::to_string(&err).unwrap()))
                            .await;
                        continue;
                    }
                };

                match client_msg {
                    WsClientMessage::Subscribe {
                        session_id,
                        mode,
                        last_seq,
                    } => {
                        handle_subscribe(
                            &session_id,
                            mode,
                            last_seq,
                            &state,
                            &tx,
                            &mut subscriptions,
                        )
                        .await;
                    }

                    WsClientMessage::Unsubscribe { session_id } => {
                        if let Some(sub) = subscriptions.remove(&session_id) {
                            sub.task.abort();
                        }
                        let msg = WsServerMessage::Unsubscribed {
                            session_id: session_id.clone(),
                        };
                        let _ = tx
                            .send(Message::text(serde_json::to_string(&msg).unwrap()))
                            .await;
                    }

                    WsClientMessage::Input { session_id, data } => {
                        let bytes = match STANDARD.decode(&data) {
                            Ok(b) => b,
                            Err(e) => {
                                let err = WsServerMessage::Error {
                                    message: format!("invalid base64: {e}"),
                                    session_id: Some(session_id),
                                };
                                let _ = tx
                                    .send(Message::text(serde_json::to_string(&err).unwrap()))
                                    .await;
                                continue;
                            }
                        };
                        send_input_to_session(&session_id, bytes, &subscriptions, &tx).await;
                    }

                    WsClientMessage::Resize {
                        session_id,
                        cols,
                        rows,
                    } => {
                        if let Some(sub) = subscriptions.get(&session_id) {
                            let _ = sub
                                .input_tx
                                .send(Request::Resize {
                                    session_id,
                                    cols,
                                    rows,
                                })
                                .await;
                        }
                    }
                }
            }

            Message::Binary(data) => {
                // Binary frame: client input for a session.
                // Format: [sid_len: u8][sid: bytes][input_data: bytes]
                if let Some((session_id, input_data)) = decode_binary_frame(&data) {
                    send_input_to_session(
                        &session_id.to_string(),
                        input_data.to_vec(),
                        &subscriptions,
                        &tx,
                    )
                    .await;
                }
            }

            Message::Close(_) => break,
            _ => {}
        }
    }

    // Cleanup: abort all subscription forwarding tasks.
    for (_, sub) in subscriptions {
        sub.task.abort();
    }

    // Drop tx so write_task exits.
    drop(tx);
    let _ = write_task.await;
    debug!("ws connection closed");
}

/// Send input to a session via its subscription's input channel.
async fn send_input_to_session(
    session_id: &SessionId,
    data: Vec<u8>,
    subscriptions: &HashMap<SessionId, SessionSub>,
    tx: &mpsc::Sender<Message>,
) {
    if let Some(sub) = subscriptions.get(session_id) {
        let _ = sub
            .input_tx
            .send(Request::SendInput {
                session_id: session_id.clone(),
                data,
            })
            .await;
    } else {
        let err = WsServerMessage::Error {
            message: "not subscribed to this session".to_string(),
            session_id: Some(session_id.clone()),
        };
        let _ = tx
            .send(Message::text(serde_json::to_string(&err).unwrap()))
            .await;
    }
}

/// Handle a subscribe request: connect to tmax-server, attach, subscribe, spawn forwarder.
async fn handle_subscribe(
    session_id: &SessionId,
    mode: Option<AttachMode>,
    last_seq: Option<u64>,
    state: &AppState,
    tx: &mpsc::Sender<Message>,
    subscriptions: &mut HashMap<SessionId, SessionSub>,
) {
    // Check for duplicate subscription.
    if subscriptions.contains_key(session_id) {
        let err = WsServerMessage::Error {
            message: "already subscribed".to_string(),
            session_id: Some(session_id.clone()),
        };
        let _ = tx
            .send(Message::text(serde_json::to_string(&err).unwrap()))
            .await;
        return;
    }

    // Connect to tmax-server for this subscription.
    let mut client = match crate::client::connect(Some(&state.socket_path)).await {
        Ok(c) => c,
        Err(e) => {
            let err = WsServerMessage::Error {
                message: format!("server connection failed: {e}"),
                session_id: Some(session_id.clone()),
            };
            let _ = tx
                .send(Message::text(serde_json::to_string(&err).unwrap()))
                .await;
            return;
        }
    };

    // Attach with the requested mode.
    let attach_mode = mode.unwrap_or(AttachMode::View);
    let attach_resp = match client
        .request(&Request::Attach {
            session_id: session_id.clone(),
            mode: attach_mode,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let err = WsServerMessage::Error {
                message: format!("attach failed: {e}"),
                session_id: Some(session_id.clone()),
            };
            let _ = tx
                .send(Message::text(serde_json::to_string(&err).unwrap()))
                .await;
            return;
        }
    };

    if let TmaxResponse::Error { message, .. } = &attach_resp {
        let err = WsServerMessage::Error {
            message: message.clone(),
            session_id: Some(session_id.clone()),
        };
        let _ = tx
            .send(Message::text(serde_json::to_string(&err).unwrap()))
            .await;
        return;
    }

    // Subscribe to events.
    let sub_resp = match client
        .request(&Request::Subscribe {
            session_id: session_id.clone(),
            last_seq,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let err = WsServerMessage::Error {
                message: format!("subscribe failed: {e}"),
                session_id: Some(session_id.clone()),
            };
            let _ = tx
                .send(Message::text(serde_json::to_string(&err).unwrap()))
                .await;
            return;
        }
    };

    let catchup_count = match &sub_resp {
        TmaxResponse::Ok { data } => data
            .as_ref()
            .and_then(|d| d.get("catchup_count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        TmaxResponse::Error { message, .. } => {
            let err = WsServerMessage::Error {
                message: message.clone(),
                session_id: Some(session_id.clone()),
            };
            let _ = tx
                .send(Message::text(serde_json::to_string(&err).unwrap()))
                .await;
            return;
        }
        _ => 0,
    };

    // Send subscription confirmation.
    let confirmed = WsServerMessage::Subscribed {
        session_id: session_id.clone(),
        catchup_count,
    };
    let _ = tx
        .send(Message::text(serde_json::to_string(&confirmed).unwrap()))
        .await;

    // Create input channel for this session's connection.
    let (input_tx, input_rx) = mpsc::channel::<Request>(64);

    // Spawn task to forward events and handle input on the same connection.
    let tx_clone = tx.clone();
    let sid = session_id.clone();
    let handle = tokio::spawn(async move {
        session_io_loop(client, sid, tx_clone, input_rx).await;
    });

    subscriptions.insert(
        session_id.clone(),
        SessionSub {
            task: handle,
            input_tx,
        },
    );
}

/// Bidirectional I/O loop for a session's tmax-server connection.
/// Reads events from the server and writes input/resize commands to it.
async fn session_io_loop(
    mut client: TmaxClient,
    session_id: SessionId,
    tx: mpsc::Sender<Message>,
    mut input_rx: mpsc::Receiver<Request>,
) {
    loop {
        tokio::select! {
            // Read events from tmax-server.
            line = client.read_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        debug!(session_id = %session_id, "server connection closed");
                        break;
                    }
                    Err(e) => {
                        warn!(session_id = %session_id, error = %e, "read error");
                        break;
                    }
                };

                let resp: TmaxResponse = match serde_json::from_str(line.trim()) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(session_id = %session_id, error = %e, "parse error");
                        continue;
                    }
                };

                match resp {
                    TmaxResponse::Event(Event::Output {
                        session_id: sid,
                        data,
                        ..
                    }) => {
                        let frame = encode_binary_frame(&sid, &data);
                        if tx.send(Message::binary(frame)).await.is_err() {
                            break;
                        }
                    }
                    TmaxResponse::Event(event) => {
                        let msg = WsServerMessage::Event(event);
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if tx.send(Message::text(json)).await.is_err() {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Handle input/resize commands from the WS client.
            cmd = input_rx.recv() => {
                match cmd {
                    Some(req) => {
                        if let Err(e) = client.send(&req).await {
                            warn!(session_id = %session_id, error = %e, "send input error");
                            break;
                        }
                        // Read and discard the response (Ok/Error for SendInput/Resize).
                        // These arrive as the next line, but we might also get events.
                        // The response will be interleaved with events — it's fine to
                        // let the event loop handle it (non-event responses are ignored).
                    }
                    None => {
                        // Input channel closed, WS client disconnected.
                        break;
                    }
                }
            }
        }
    }
}
