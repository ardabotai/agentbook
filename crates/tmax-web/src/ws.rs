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

/// Handle a single WebSocket connection.
///
/// Architecture:
/// - One tmax-server connection per WS subscription (session).
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

    // Track active subscriptions: session_id -> abort handle for the forwarding task.
    let mut subscriptions: HashMap<SessionId, tokio::task::JoinHandle<()>> = HashMap::new();

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
                        // Check for duplicate subscription.
                        if subscriptions.contains_key(&session_id) {
                            let err = WsServerMessage::Error {
                                message: "already subscribed".to_string(),
                                session_id: Some(session_id),
                            };
                            let _ = tx
                                .send(Message::text(serde_json::to_string(&err).unwrap()))
                                .await;
                            continue;
                        }

                        // Connect to tmax-server for this subscription.
                        let mut client =
                            match crate::client::connect(Some(&state.socket_path)).await {
                                Ok(c) => c,
                                Err(e) => {
                                    let err = WsServerMessage::Error {
                                        message: format!("server connection failed: {e}"),
                                        session_id: Some(session_id),
                                    };
                                    let _ = tx
                                        .send(Message::text(serde_json::to_string(&err).unwrap()))
                                        .await;
                                    continue;
                                }
                            };

                        // Attach if mode is specified.
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
                                    session_id: Some(session_id),
                                };
                                let _ = tx
                                    .send(Message::text(serde_json::to_string(&err).unwrap()))
                                    .await;
                                continue;
                            }
                        };

                        if let TmaxResponse::Error { message, .. } = &attach_resp {
                            let err = WsServerMessage::Error {
                                message: message.clone(),
                                session_id: Some(session_id),
                            };
                            let _ = tx
                                .send(Message::text(serde_json::to_string(&err).unwrap()))
                                .await;
                            continue;
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
                                    session_id: Some(session_id),
                                };
                                let _ = tx
                                    .send(Message::text(serde_json::to_string(&err).unwrap()))
                                    .await;
                                continue;
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
                                    session_id: Some(session_id),
                                };
                                let _ = tx
                                    .send(Message::text(serde_json::to_string(&err).unwrap()))
                                    .await;
                                continue;
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

                        // Spawn task to forward events from this tmax-server connection.
                        let tx_clone = tx.clone();
                        let sid = session_id.clone();
                        let max_lag = state.max_lag_chunks;
                        let batch_interval = state.batch_interval;
                        let handle = tokio::spawn(async move {
                            forward_session_events(client, sid, tx_clone, batch_interval, max_lag)
                                .await;
                        });

                        subscriptions.insert(session_id, handle);
                    }

                    WsClientMessage::Unsubscribe { session_id } => {
                        if let Some(handle) = subscriptions.remove(&session_id) {
                            handle.abort();
                        }
                        let msg = WsServerMessage::Unsubscribed {
                            session_id: session_id.clone(),
                        };
                        let _ = tx
                            .send(Message::text(serde_json::to_string(&msg).unwrap()))
                            .await;
                    }

                    WsClientMessage::Input { session_id, data } => {
                        // Decode base64 input and send to tmax-server.
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

                        // We need a connection to send input. Use a one-shot connection.
                        if let Ok(mut client) =
                            crate::client::connect(Some(&state.socket_path)).await
                        {
                            let _ = client
                                .request(&Request::SendInput {
                                    session_id: session_id.clone(),
                                    data: bytes,
                                })
                                .await;
                        }
                    }

                    WsClientMessage::Resize {
                        session_id,
                        cols,
                        rows,
                    } => {
                        if let Ok(mut client) =
                            crate::client::connect(Some(&state.socket_path)).await
                        {
                            let _ = client
                                .request(&Request::Resize {
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
                    if let Ok(mut client) =
                        crate::client::connect(Some(&state.socket_path)).await
                    {
                        let _ = client
                            .request(&Request::SendInput {
                                session_id: session_id.to_string(),
                                data: input_data.to_vec(),
                            })
                            .await;
                    }
                }
            }

            Message::Close(_) => break,
            _ => {}
        }
    }

    // Cleanup: abort all subscription forwarding tasks.
    for (_, handle) in subscriptions {
        handle.abort();
    }

    // Drop tx so write_task exits.
    drop(tx);
    let _ = write_task.await;
    debug!("ws connection closed");
}

/// Forward events from a tmax-server subscription to the WebSocket send channel.
async fn forward_session_events(
    mut client: TmaxClient,
    session_id: SessionId,
    tx: mpsc::Sender<Message>,
    _batch_interval: Duration,
    _max_lag: u64,
) {
    // Read events from the tmax-server connection and forward to WS.
    // Events arrive as JSON-lines Response::Event(...) messages.
    loop {
        let line = match client.read_line().await {
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
                // Send as binary frame for efficiency (xterm.js consumes raw bytes).
                let frame = encode_binary_frame(&sid, &data);
                if tx.send(Message::binary(frame)).await.is_err() {
                    break;
                }
            }
            TmaxResponse::Event(event) => {
                // Non-output events go as JSON text frames.
                let msg = WsServerMessage::Event(event);
                if let Ok(json) = serde_json::to_string(&msg) {
                    if tx.send(Message::text(json)).await.is_err() {
                        break;
                    }
                }
            }
            _ => {
                // Catchup responses or other responses - forward as events.
            }
        }
    }
}
