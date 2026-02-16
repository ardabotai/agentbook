use anyhow::{Context, Result, anyhow, bail};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use futures_util::{Sink, SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tmax_protocol::{
    AttachMode, Event, MAX_INPUT_CHUNK_BYTES, MAX_JSON_LINE_BYTES, Request, Response,
};
use tokio::net::UnixStream;
use tokio::time::MissedTickBehavior;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

#[derive(Clone)]
struct AppState {
    socket_path: PathBuf,
    ws_tuning: WsTuning,
}

#[derive(Debug)]
struct Args {
    listen: SocketAddr,
    socket_path: PathBuf,
    allow_origins: Vec<String>,
    ws_tuning: WsTuning,
}

#[derive(Debug, Clone)]
struct WsTuning {
    batch_interval_ms: u64,
    max_pending_bytes: usize,
    max_frame_bytes: usize,
    max_control_text_bytes: usize,
    max_input_bytes: usize,
}

impl Default for WsTuning {
    fn default() -> Self {
        Self {
            batch_interval_ms: 20,
            max_pending_bytes: 256 * 1024,
            max_frame_bytes: 64 * 1024,
            max_control_text_bytes: 8 * 1024,
            max_input_bytes: MAX_INPUT_CHUNK_BYTES,
        }
    }
}

#[derive(Debug, Deserialize)]
struct WsQuery {
    mode: Option<String>,
    last_seq: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tmax_web=info".into()),
        )
        .init();

    let args = Args::parse()?;
    let state = Arc::new(AppState {
        socket_path: args.socket_path,
        ws_tuning: args.ws_tuning,
    });

    let cors = build_cors(&args.allow_origins)?;

    let app = Router::new()
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/tree", get(api_sessions_tree))
        .route("/api/sessions/{id}", get(api_session_info))
        .route("/ws/session/{id}", get(ws_session))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to bind web listener on {}", args.listen))?;

    tracing::info!("tmax-web listening on http://{}", args.listen);
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_cors(origins: &[String]) -> Result<CorsLayer> {
    if origins.iter().any(|o| o == "*") {
        return Ok(CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers(Any));
    }

    let mut headers = Vec::with_capacity(origins.len());
    for origin in origins {
        headers.push(
            HeaderValue::from_str(origin)
                .with_context(|| format!("invalid --allow-origin value: {origin}"))?,
        );
    }

    Ok(CorsLayer::new()
        .allow_origin(AllowOrigin::list(headers))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any))
}

impl Args {
    fn parse() -> Result<Self> {
        let mut listen = SocketAddr::from_str("127.0.0.1:8787")?;
        let mut socket_path = default_socket_path();
        let mut allow_origins = vec!["http://localhost:3000".to_string()];
        let mut ws_tuning = WsTuning::default();

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--listen" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--listen requires a value"))?;
                    listen = SocketAddr::from_str(&value)
                        .with_context(|| format!("invalid --listen value: {value}"))?;
                }
                "--socket" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--socket requires a value"))?;
                    socket_path = PathBuf::from(value);
                }
                "--allow-origin" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--allow-origin requires a value"))?;
                    if allow_origins == ["http://localhost:3000".to_string()] {
                        allow_origins.clear();
                    }
                    allow_origins.push(value);
                }
                "--batch-ms" => {
                    ws_tuning.batch_interval_ms = args
                        .next()
                        .ok_or_else(|| anyhow!("--batch-ms requires a value"))?
                        .parse()
                        .context("invalid --batch-ms value")?;
                }
                "--max-pending-bytes" => {
                    ws_tuning.max_pending_bytes = args
                        .next()
                        .ok_or_else(|| anyhow!("--max-pending-bytes requires a value"))?
                        .parse()
                        .context("invalid --max-pending-bytes value")?;
                }
                "--max-frame-bytes" => {
                    ws_tuning.max_frame_bytes = args
                        .next()
                        .ok_or_else(|| anyhow!("--max-frame-bytes requires a value"))?
                        .parse()
                        .context("invalid --max-frame-bytes value")?;
                }
                "--max-control-bytes" => {
                    ws_tuning.max_control_text_bytes = args
                        .next()
                        .ok_or_else(|| anyhow!("--max-control-bytes requires a value"))?
                        .parse()
                        .context("invalid --max-control-bytes value")?;
                }
                "--max-input-bytes" => {
                    ws_tuning.max_input_bytes = args
                        .next()
                        .ok_or_else(|| anyhow!("--max-input-bytes requires a value"))?
                        .parse()
                        .context("invalid --max-input-bytes value")?;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }

        if ws_tuning.max_frame_bytes == 0 || ws_tuning.max_pending_bytes == 0 {
            bail!("max frame/pending bytes must be > 0");
        }

        Ok(Self {
            listen,
            socket_path,
            allow_origins,
            ws_tuning,
        })
    }
}

fn print_help() {
    println!(
        "tmax-web [--listen HOST:PORT] [--socket PATH] [--allow-origin ORIGIN] [--batch-ms N] [--max-pending-bytes N] [--max-frame-bytes N] [--max-control-bytes N] [--max-input-bytes N]"
    );
}

fn default_socket_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("tmax").join("tmax.sock");
    }
    let uid = nix::unistd::Uid::effective().as_raw();
    PathBuf::from(format!("/tmp/tmax-{uid}/tmax.sock"))
}

async fn api_sessions(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<Value>, (StatusCode, String)> {
    json_request(&state.socket_path, Request::SessionList).await
}

async fn api_sessions_tree(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<Value>, (StatusCode, String)> {
    json_request(&state.socket_path, Request::SessionTree).await
}

async fn api_session_info(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> std::result::Result<Json<Value>, (StatusCode, String)> {
    json_request(&state.socket_path, Request::SessionInfo { session_id }).await
}

async fn json_request(
    socket_path: &FsPath,
    req: Request,
) -> std::result::Result<Json<Value>, (StatusCode, String)> {
    match request_data(socket_path, req).await {
        Ok(value) => Ok(Json(value.unwrap_or_else(|| json!({})))),
        Err(err) => Err((StatusCode::BAD_GATEWAY, err.to_string())),
    }
}

async fn ws_session(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<WsQuery>,
) -> impl IntoResponse {
    let mode = parse_mode(query.mode.as_deref());
    let last_seq = query.last_seq;
    ws.on_upgrade(move |socket| ws_session_task(socket, state, session_id, mode, last_seq))
}

fn parse_mode(value: Option<&str>) -> AttachMode {
    match value {
        Some("edit") => AttachMode::Edit,
        _ => AttachMode::View,
    }
}

async fn ws_session_task(
    socket: WebSocket,
    state: Arc<AppState>,
    session_id: String,
    mode: AttachMode,
    last_seq: Option<u64>,
) {
    if let Err(err) = ws_session_inner(socket, state, session_id, mode, last_seq).await {
        tracing::warn!("ws session closed with error: {err}");
    }
}

async fn ws_session_inner(
    socket: WebSocket,
    state: Arc<AppState>,
    session_id: String,
    mode: AttachMode,
    last_seq: Option<u64>,
) -> Result<()> {
    let stream = UnixStream::connect(&state.socket_path)
        .await
        .with_context(|| format!("failed to connect {}", state.socket_path.display()))?;
    let (read_half, write_half) = stream.into_split();
    let mut server_reader = FramedRead::new(
        read_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );
    let mut server_writer = FramedWrite::new(
        write_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );

    send_server_request(
        &mut server_writer,
        &Request::Attach {
            session_id: session_id.clone(),
            mode,
            last_seq_seen: last_seq,
        },
    )
    .await?;

    let attachment_id = wait_for_attach_ack(&mut server_reader).await?;
    let (mut ws_tx, mut ws_rx) = socket.split();

    let mut pending_output = Vec::new();
    let mut dropped_chunks = 0u64;
    let mut batch_tick =
        tokio::time::interval(Duration::from_millis(state.ws_tuning.batch_interval_ms));
    batch_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = batch_tick.tick() => {
                maybe_send_drop_notice(&mut ws_tx, &mut dropped_chunks).await?;
                flush_pending_output(&mut ws_tx, &mut pending_output, state.ws_tuning.max_frame_bytes).await?;
            }
            maybe_line = server_reader.next() => {
                let Some(line) = maybe_line else {
                    break;
                };
                let line = line?;
                let resp: Response = serde_json::from_str(&line)?;
                match resp {
                    Response::Event { event } => {
                        match *event {
                            Event::Output { data_b64, .. } => {
                                let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
                                for chunk in bytes.chunks(state.ws_tuning.max_frame_bytes) {
                                    enqueue_output(
                                        &mut pending_output,
                                        chunk,
                                        state.ws_tuning.max_pending_bytes,
                                        &mut dropped_chunks,
                                    );
                                }
                            }
                            other => {
                                maybe_send_drop_notice(&mut ws_tx, &mut dropped_chunks).await?;
                                flush_pending_output(&mut ws_tx, &mut pending_output, state.ws_tuning.max_frame_bytes).await?;
                                ws_tx.send(Message::Text(serde_json::to_string(&other)?.into())).await?;
                            }
                        }
                    }
                    Response::Error { message, .. } => {
                        maybe_send_drop_notice(&mut ws_tx, &mut dropped_chunks).await?;
                        flush_pending_output(&mut ws_tx, &mut pending_output, state.ws_tuning.max_frame_bytes).await?;
                        ws_tx.send(Message::Text(json!({"error": message}).to_string().into())).await?;
                        break;
                    }
                    Response::Ok { .. } | Response::Hello { .. } => {}
                }
            }
            maybe_msg = ws_rx.next() => {
                let Some(msg) = maybe_msg else {
                    break;
                };
                match msg? {
                    Message::Binary(bytes) => {
                        if mode == AttachMode::Edit {
                            if bytes.len() > state.ws_tuning.max_input_bytes {
                                ws_tx.send(Message::Text(json!({
                                    "error": format!("input frame too large: {} > {}", bytes.len(), state.ws_tuning.max_input_bytes)
                                }).to_string().into())).await?;
                                break;
                            }

                            let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
                            send_server_request(
                                &mut server_writer,
                                &Request::SendInput {
                                    session_id: session_id.clone(),
                                    attachment_id: attachment_id.clone(),
                                    data_b64: payload,
                                },
                            ).await?;
                        }
                    }
                    Message::Text(text) => {
                        if text.len() > state.ws_tuning.max_control_text_bytes {
                            ws_tx.send(Message::Text(json!({
                                "error": format!("control frame too large: {} > {}", text.len(), state.ws_tuning.max_control_text_bytes)
                            }).to_string().into())).await?;
                            continue;
                        }

                        if let Some(request) = control_message_to_request(&session_id, &text)? {
                            send_server_request(&mut server_writer, &request).await?;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(v) => ws_tx.send(Message::Pong(v)).await?,
                    Message::Pong(_) => {}
                }
            }
        }
    }

    maybe_send_drop_notice(&mut ws_tx, &mut dropped_chunks).await?;
    flush_pending_output(
        &mut ws_tx,
        &mut pending_output,
        state.ws_tuning.max_frame_bytes,
    )
    .await?;

    let _ = send_server_request(
        &mut server_writer,
        &Request::Detach {
            attachment_id: attachment_id.clone(),
        },
    )
    .await;

    Ok(())
}

fn enqueue_output(
    pending: &mut Vec<u8>,
    chunk: &[u8],
    max_pending_bytes: usize,
    dropped_chunks: &mut u64,
) {
    if chunk.is_empty() {
        return;
    }

    if chunk.len() > max_pending_bytes {
        pending.clear();
        pending.extend_from_slice(&chunk[chunk.len() - max_pending_bytes..]);
        *dropped_chunks = dropped_chunks.saturating_add(1);
        return;
    }

    if pending.len() + chunk.len() > max_pending_bytes {
        pending.clear();
        *dropped_chunks = dropped_chunks.saturating_add(1);
    }

    pending.extend_from_slice(chunk);
}

async fn maybe_send_drop_notice<S>(ws_tx: &mut S, dropped_chunks: &mut u64) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    if *dropped_chunks == 0 {
        return Ok(());
    }

    let payload = json!({
        "event": "output_dropped",
        "dropped_chunks": *dropped_chunks,
    })
    .to_string();
    ws_tx.send(Message::Text(payload.into())).await?;
    *dropped_chunks = 0;
    Ok(())
}

async fn flush_pending_output<S>(
    ws_tx: &mut S,
    pending: &mut Vec<u8>,
    max_frame_bytes: usize,
) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    if pending.is_empty() {
        return Ok(());
    }

    let emit = pending.len().min(max_frame_bytes);
    let frame: Vec<u8> = pending.drain(..emit).collect();
    ws_tx.send(Message::Binary(frame.into())).await?;
    Ok(())
}

async fn wait_for_attach_ack(
    reader: &mut FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
) -> Result<String> {
    let mut attachment_id = None;
    while attachment_id.is_none() {
        let Some(line) = reader.next().await else {
            bail!("server closed before attach ack");
        };
        let line = line?;
        let resp: Response = serde_json::from_str(&line)?;
        match resp {
            Response::Hello { .. } => {}
            Response::Ok { data } => {
                if let Some(value) = data {
                    let id = value
                        .get("attachment")
                        .and_then(|a| a.get("attachment_id"))
                        .and_then(|v| v.as_str());
                    if let Some(id) = id {
                        attachment_id = Some(id.to_string());
                    }
                }
            }
            Response::Error { message, .. } => bail!("attach failed: {message}"),
            Response::Event { .. } => {}
        }
    }

    attachment_id.ok_or_else(|| anyhow!("missing attachment id"))
}

fn control_message_to_request(session_id: &str, text: &str) -> Result<Option<Request>> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let Some(cmd) = value.get("cmd").and_then(|v| v.as_str()) else {
        return Ok(None);
    };

    match cmd {
        "resize" => {
            let cols = value
                .get("cols")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("resize missing cols"))? as u16;
            let rows = value
                .get("rows")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("resize missing rows"))? as u16;
            Ok(Some(Request::Resize {
                session_id: session_id.to_string(),
                cols,
                rows,
            }))
        }
        "marker_insert" => {
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("marker_insert missing name"))?
                .to_string();
            Ok(Some(Request::MarkerInsert {
                session_id: session_id.to_string(),
                name,
            }))
        }
        _ => Ok(None),
    }
}

async fn send_server_request(
    writer: &mut FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
    req: &Request,
) -> Result<()> {
    let line = serde_json::to_string(req)?;
    writer.send(line).await?;
    Ok(())
}

async fn request_data(socket_path: &FsPath, req: Request) -> Result<Option<Value>> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("failed to connect {}", socket_path.display()))?;
    let (read_half, write_half) = stream.into_split();
    let mut reader = FramedRead::new(
        read_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );
    let mut writer = FramedWrite::new(
        write_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );

    send_server_request(&mut writer, &req).await?;

    loop {
        let Some(line) = reader.next().await else {
            bail!("server disconnected");
        };
        let line = line?;
        let resp: Response = serde_json::from_str(&line)?;
        match resp {
            Response::Hello { .. } => continue,
            Response::Event { .. } => continue,
            Response::Ok { data } => return Ok(data),
            Response::Error { message, .. } => bail!(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_defaults_to_view() {
        assert_eq!(parse_mode(None), AttachMode::View);
        assert_eq!(parse_mode(Some("view")), AttachMode::View);
        assert_eq!(parse_mode(Some("edit")), AttachMode::Edit);
    }

    #[test]
    fn enqueue_output_drops_when_capacity_exceeded() {
        let mut pending = Vec::new();
        let mut dropped = 0;
        enqueue_output(&mut pending, b"abcdef", 4, &mut dropped);
        assert_eq!(pending, b"cdef");
        assert_eq!(dropped, 1);
    }

    #[test]
    fn control_message_resize_parses() {
        let req = control_message_to_request("s1", r#"{"cmd":"resize","cols":100,"rows":40}"#)
            .expect("parse")
            .expect("request");
        match req {
            Request::Resize {
                session_id,
                cols,
                rows,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(cols, 100);
                assert_eq!(rows, 40);
            }
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn build_cors_accepts_wildcard() {
        let layer = build_cors(&["*".to_string()]);
        assert!(layer.is_ok());
    }
}
