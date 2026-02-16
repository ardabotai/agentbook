use anyhow::{Context, Result, anyhow};
use base64::Engine;
use futures_util::SinkExt;
use libtmax::handler::{
    CommsPolicy, enforce_comms_policy_for_pair, enforce_session_binding_if_present,
    enqueue_response, parse_comms_policy, resolve_sender_session, task_created_by_session,
    task_policy_peer_session,
};
use libtmax::{SessionCreateOptions, SessionManager, SessionManagerConfig};
use nix::unistd::Uid;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tmax_protocol::{MAX_JSON_LINE_BYTES, PROTOCOL_VERSION, Request, Response};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{RwLock, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
struct RuntimeConfig {
    socket_path: PathBuf,
    runtime_dir: PathBuf,
    pid_file: PathBuf,
    recovery_key_path: PathBuf,
    allowed_uid: u32,
    outbound_queue: usize,
    comms_policy: CommsPolicy,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    socket_path: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    pid_file: Option<PathBuf>,
    recovery_key_path: Option<PathBuf>,
    outbound_queue: Option<usize>,
    comms_policy: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tmax_local=info".into()),
        )
        .init();

    let args = Args::parse()?;
    let cfg = load_runtime_config(&args)?;
    ensure_runtime_dir(&cfg.runtime_dir)?;
    if let Some(parent) = cfg.socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_stale_socket(&cfg.socket_path)?;

    let listener = UnixListener::bind(&cfg.socket_path)
        .with_context(|| format!("failed to bind {}", cfg.socket_path.display()))?;
    fs::set_permissions(&cfg.socket_path, fs::Permissions::from_mode(0o600))?;
    fs::write(&cfg.pid_file, std::process::id().to_string())?;

    info!(
        "tmax-local started pid={} socket={} protocol_version={}",
        std::process::id(),
        cfg.socket_path.display(),
        PROTOCOL_VERSION
    );

    let manager = Arc::new(SessionManager::new(SessionManagerConfig {
        recovery_key_path: Some(cfg.recovery_key_path.clone()),
        ..SessionManagerConfig::default()
    }));
    let active_connections: Arc<RwLock<Vec<JoinHandle<()>>>> = Arc::new(RwLock::new(Vec::new()));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let accept_result = accept_loop(
        listener,
        manager,
        cfg.clone(),
        shutdown_rx,
        shutdown_tx.clone(),
        Arc::clone(&active_connections),
    )
    .await;

    shutdown_tx.send_replace(true);
    for handle in active_connections.write().await.drain(..) {
        handle.abort();
    }

    let _ = fs::remove_file(&cfg.socket_path);
    let _ = fs::remove_file(&cfg.pid_file);

    accept_result
}

async fn accept_loop(
    listener: UnixListener,
    manager: Arc<SessionManager>,
    cfg: RuntimeConfig,
    mut shutdown_rx: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
    active_connections: Arc<RwLock<Vec<JoinHandle<()>>>>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("shutdown requested, stopping accept loop");
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(v) => v,
                    Err(err) => {
                        warn!("accept failed: {err}");
                        continue;
                    }
                };

                if let Err(err) = verify_peer_uid(&stream, cfg.allowed_uid) {
                    warn!("rejected peer: {err}");
                    continue;
                }

                let handle = tokio::spawn(handle_connection(
                    stream,
                    Arc::clone(&manager),
                    cfg.clone(),
                    shutdown_tx.clone(),
                ));
                active_connections.write().await.push(handle);
            }
        }
    }

    Ok(())
}

async fn handle_connection(
    stream: UnixStream,
    manager: Arc<SessionManager>,
    cfg: RuntimeConfig,
    shutdown_tx: watch::Sender<bool>,
) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = FramedRead::new(
        read_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );

    let (out_tx, mut out_rx) = mpsc::channel::<Response>(cfg.outbound_queue);
    let mut writer = FramedWrite::new(
        write_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );

    let writer_task = tokio::spawn(async move {
        while let Some(resp) = out_rx.recv().await {
            let line = match serde_json::to_string(&resp) {
                Ok(line) => line,
                Err(err) => {
                    error!("failed to encode response: {err}");
                    continue;
                }
            };

            if let Err(err) = writer.send(line).await {
                warn!("socket write failed: {err}");
                break;
            }
        }
    });

    if enqueue_response(
        &out_tx,
        Response::hello(vec![
            "single_writer_queue".to_string(),
            "attach_ids".to_string(),
            "replay".to_string(),
            "queue_drop_disconnect".to_string(),
        ]),
    )
    .is_err()
    {
        writer_task.abort();
        return;
    }

    let mut subscriptions: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut attach_tasks: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut owned_attachments: HashMap<String, String> = HashMap::new();

    loop {
        let line = match futures_util::StreamExt::next(&mut reader).await {
            Some(Ok(line)) => line,
            Some(Err(err)) => {
                warn!("socket read failed: {err}");
                break;
            }
            None => break,
        };

        let req: Request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(err) => {
                if enqueue_response(
                    &out_tx,
                    Response::error(
                        tmax_protocol::ErrorCode::InvalidRequest,
                        format!("invalid request: {err}"),
                    ),
                )
                .is_err()
                {
                    break;
                }
                continue;
            }
        };

        match handle_request(
            req,
            Arc::clone(&manager),
            out_tx.clone(),
            &mut subscriptions,
            &mut attach_tasks,
            &mut owned_attachments,
            cfg.comms_policy,
            shutdown_tx.clone(),
        )
        .await
        {
            Ok(continue_loop) => {
                if !continue_loop {
                    break;
                }
            }
            Err(err) => {
                let code = SessionManager::map_err_code(&err);
                if enqueue_response(&out_tx, Response::error(code, err.to_string())).is_err() {
                    break;
                }
            }
        }
    }

    for (_, handle) in subscriptions.drain() {
        handle.abort();
    }
    for (_, handle) in attach_tasks.drain() {
        handle.abort();
    }

    for attachment_id in owned_attachments.into_keys() {
        if let Err(err) = manager.detach(&attachment_id).await {
            warn!("detach cleanup failed for {attachment_id}: {err}");
        }
    }

    drop(out_tx);
    let _ = writer_task.await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_request(
    req: Request,
    manager: Arc<SessionManager>,
    out_tx: mpsc::Sender<Response>,
    subscriptions: &mut HashMap<String, JoinHandle<()>>,
    attach_tasks: &mut HashMap<String, JoinHandle<()>>,
    owned_attachments: &mut HashMap<String, String>,
    comms_policy: CommsPolicy,
    shutdown_tx: watch::Sender<bool>,
) -> Result<bool> {
    match req {
        Request::SessionCreate {
            exec,
            args,
            tags,
            cwd,
            label,
            sandbox,
            parent_id,
            cols,
            rows,
        } => {
            let summary = manager
                .create_session(SessionCreateOptions {
                    exec,
                    args,
                    tags,
                    cwd,
                    label,
                    sandbox,
                    parent_id,
                    cols,
                    rows,
                })
                .await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(summary)?)))?;
        }
        Request::SessionDestroy {
            session_id,
            cascade,
        } => {
            manager.destroy_session(&session_id, cascade).await?;
            enqueue_response(
                &out_tx,
                Response::ok(Some(json!({"session_id": session_id}))),
            )?;
        }
        Request::SessionList => {
            let list = manager.list_sessions().await;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(list)?)))?;
        }
        Request::SessionTree => {
            let tree = manager.session_tree().await;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(tree)?)))?;
        }
        Request::SessionInfo { session_id } => {
            let Some(info) = manager.session_info(&session_id).await else {
                return Err(anyhow!("session not found"));
            };
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(info)?)))?;
        }
        Request::Attach {
            session_id,
            mode,
            last_seq_seen,
        } => {
            let handle = manager.attach(&session_id, mode, last_seq_seen).await?;
            owned_attachments.insert(handle.info.attachment_id.clone(), session_id.clone());

            enqueue_response(
                &out_tx,
                Response::ok(Some(json!({
                    "attachment": handle.info,
                }))),
            )?;

            for event in handle.replay_events {
                enqueue_response(
                    &out_tx,
                    Response::Event {
                        event: Box::new(event),
                    },
                )?;
            }

            let attachment_id = handle.info.attachment_id.clone();
            let mut rx = handle.receiver;
            let out = out_tx.clone();
            let task = tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            if enqueue_response(
                                &out,
                                Response::Event {
                                    event: Box::new(event),
                                },
                            )
                            .is_err()
                            {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            if let Some(old) = attach_tasks.insert(attachment_id, task) {
                old.abort();
            }
        }
        Request::Detach { attachment_id } => {
            manager.detach(&attachment_id).await?;
            owned_attachments.remove(&attachment_id);
            if let Some(handle) = attach_tasks.remove(&attachment_id) {
                handle.abort();
            }
            enqueue_response(
                &out_tx,
                Response::ok(Some(json!({"attachment_id": attachment_id}))),
            )?;
        }
        Request::SendInput {
            session_id,
            attachment_id,
            data_b64,
        } => {
            let data = base64::engine::general_purpose::STANDARD
                .decode(data_b64)
                .context("input is not valid base64")?;
            manager
                .send_input(&session_id, &attachment_id, &data)
                .await?;
            enqueue_response(&out_tx, Response::ok(Some(json!({"written": data.len()}))))?;
        }
        Request::Resize {
            session_id,
            cols,
            rows,
        } => {
            manager.resize(&session_id, cols, rows).await?;
            enqueue_response(
                &out_tx,
                Response::ok(Some(
                    json!({"session_id": session_id, "cols": cols, "rows": rows}),
                )),
            )?;
        }
        Request::MarkerInsert { session_id, name } => {
            let seq = manager.insert_marker(&session_id, name.clone()).await?;
            enqueue_response(
                &out_tx,
                Response::ok(Some(
                    json!({"session_id": session_id, "name": name, "seq": seq}),
                )),
            )?;
        }
        Request::MarkerList { session_id } => {
            let markers = manager.list_markers(&session_id).await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(markers)?)))?;
        }
        Request::MessageSend {
            from_session_id,
            to_session_id,
            topic,
            body,
            requires_response,
            encrypt,
            sign,
        } => {
            let from_session_id = resolve_sender_session(from_session_id, owned_attachments)?;
            if let Some(from_session_id) = from_session_id.as_deref() {
                enforce_comms_policy_for_pair(
                    comms_policy,
                    &manager,
                    from_session_id,
                    &to_session_id,
                )
                .await?;
            }
            let message = manager
                .send_message(
                    from_session_id.as_deref(),
                    &to_session_id,
                    topic,
                    body,
                    requires_response,
                    encrypt,
                    sign,
                )
                .await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(message)?)))?;
        }
        Request::MessageList {
            session_id,
            unread_only,
            limit,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let messages = manager
                .list_messages(&session_id, unread_only, limit)
                .await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(messages)?)))?;
        }
        Request::MessageAck {
            session_id,
            message_id,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let acked = manager.ack_message(&session_id, &message_id).await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(acked)?)))?;
        }
        Request::MessageUnreadCount { session_id } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let unread = manager.unread_message_count(&session_id).await?;
            enqueue_response(
                &out_tx,
                Response::ok(Some(json!({
                    "session_id": session_id,
                    "unread": unread,
                }))),
            )?;
        }
        Request::WalletInfo { session_id } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let wallet = manager.wallet_info(&session_id).await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(wallet)?)))?;
        }
        Request::WorkflowCreate {
            name,
            root_session_id,
        } => {
            enforce_session_binding_if_present(&root_session_id, owned_attachments)?;
            let workflow = manager.create_workflow(name, root_session_id).await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(workflow)?)))?;
        }
        Request::WorkflowJoin {
            workflow_id,
            session_id,
            parent_session_id,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let workflow = manager
                .join_workflow(&workflow_id, &session_id, &parent_session_id)
                .await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(workflow)?)))?;
        }
        Request::WorkflowLeave {
            workflow_id,
            session_id,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let workflow = manager.leave_workflow(&workflow_id, &session_id).await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(workflow)?)))?;
        }
        Request::WorkflowList { session_id } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let workflows = manager.list_workflows(&session_id).await?;
            enqueue_response(
                &out_tx,
                Response::ok(Some(serde_json::to_value(workflows)?)),
            )?;
        }
        Request::TaskCreate {
            workflow_id,
            title,
            description,
            created_by,
            depends_on,
        } => {
            let created_by = resolve_sender_session(Some(created_by), owned_attachments)?
                .ok_or_else(|| anyhow!("task create requires sender session"))?;
            let task = manager
                .create_shared_task(workflow_id, title, description, created_by, depends_on)
                .await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(task)?)))?;
        }
        Request::TaskList {
            workflow_id,
            session_id,
            include_done,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let tasks = manager
                .list_shared_tasks(&workflow_id, &session_id, include_done)
                .await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(tasks)?)))?;
        }
        Request::TaskClaim {
            task_id,
            session_id,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            if let Some(created_by) = task_created_by_session(&manager, &task_id).await? {
                enforce_comms_policy_for_pair(comms_policy, &manager, &created_by, &session_id)
                    .await?;
            }
            let task = manager.claim_shared_task(&task_id, &session_id).await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(task)?)))?;
        }
        Request::TaskSetStatus {
            task_id,
            session_id,
            status,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let actor_session = resolve_sender_session(Some(session_id), owned_attachments)?
                .ok_or_else(|| anyhow!("task status requires sender session"))?;
            if let Some(peer) = task_policy_peer_session(&manager, &task_id, &actor_session).await?
            {
                enforce_comms_policy_for_pair(comms_policy, &manager, &actor_session, &peer)
                    .await?;
            }
            let task = manager
                .set_shared_task_status(&task_id, &actor_session, status)
                .await?;
            enqueue_response(&out_tx, Response::ok(Some(serde_json::to_value(task)?)))?;
        }
        Request::Subscribe {
            session_id,
            last_seq_seen,
        } => {
            let sub = manager.subscribe(&session_id, last_seq_seen).await?;
            for event in sub.replay_events {
                enqueue_response(
                    &out_tx,
                    Response::Event {
                        event: Box::new(event),
                    },
                )?;
            }

            let mut rx = sub.receiver;
            let out = out_tx.clone();
            let key = session_id.clone();
            let task = tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            if enqueue_response(
                                &out,
                                Response::Event {
                                    event: Box::new(event),
                                },
                            )
                            .is_err()
                            {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            if let Some(old) = subscriptions.insert(key, task) {
                old.abort();
            }
            enqueue_response(
                &out_tx,
                Response::ok(Some(json!({"session_id": session_id, "subscribed": true}))),
            )?;
        }
        Request::Unsubscribe { session_id } => {
            if let Some(task) = subscriptions.remove(&session_id) {
                task.abort();
            }
            enqueue_response(
                &out_tx,
                Response::ok(Some(json!({"session_id": session_id, "subscribed": false}))),
            )?;
        }
        Request::Health => {
            let list = manager.list_sessions().await;
            enqueue_response(
                &out_tx,
                Response::ok(Some(json!({
                    "healthy": true,
                    "protocol_version": PROTOCOL_VERSION,
                    "session_count": list.len(),
                }))),
            )?;
        }
        Request::ServerShutdown => {
            let _ = enqueue_response(&out_tx, Response::ok(Some(json!({"shutdown": true}))));
            shutdown_tx.send_replace(true);
            return Ok(false);
        }
        _ => {
            enqueue_response(
                &out_tx,
                Response::error(
                    tmax_protocol::ErrorCode::Unsupported,
                    "mesh commands require tmax-node (not supported by tmax-local)",
                ),
            )?;
        }
    }

    Ok(true)
}

#[derive(Debug)]
struct Args {
    socket_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
    comms_policy: Option<String>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut socket_path = None;
        let mut config_path = None;
        let mut comms_policy = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--socket" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--socket requires a value"))?;
                    socket_path = Some(PathBuf::from(value));
                }
                "--config" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--config requires a value"))?;
                    config_path = Some(PathBuf::from(value));
                }
                "--comms-policy" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--comms-policy requires a value"))?;
                    comms_policy = Some(value);
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(anyhow!("unknown argument: {other}"));
                }
            }
        }

        Ok(Self {
            socket_path,
            config_path,
            comms_policy,
        })
    }
}

fn print_help() {
    println!(
        "tmax-local [--socket PATH] [--config PATH] [--comms-policy open|same_subtree|parent_only]"
    );
}

fn load_runtime_config(args: &Args) -> Result<RuntimeConfig> {
    let file_cfg = if let Some(path) = &args.config_path {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        toml::from_str::<FileConfig>(&raw)
            .with_context(|| format!("failed to parse config {}", path.display()))?
    } else {
        FileConfig::default()
    };

    let uid = Uid::effective().as_raw();
    let runtime_dir = file_cfg.runtime_dir.unwrap_or_else(default_runtime_dir);

    let socket_path = args
        .socket_path
        .clone()
        .or(file_cfg.socket_path)
        .unwrap_or_else(|| runtime_dir.join("tmax.sock"));

    let pid_file = file_cfg
        .pid_file
        .unwrap_or_else(|| runtime_dir.join("tmax.pid"));
    let recovery_key_path = file_cfg
        .recovery_key_path
        .unwrap_or_else(|| runtime_dir.join("tmax-recovery.key"));
    let comms_policy_raw = args
        .comms_policy
        .clone()
        .or(file_cfg.comms_policy)
        .unwrap_or_else(|| "open".to_string());
    let comms_policy = parse_comms_policy(&comms_policy_raw)?;

    Ok(RuntimeConfig {
        socket_path,
        runtime_dir,
        pid_file,
        recovery_key_path,
        allowed_uid: uid,
        outbound_queue: file_cfg.outbound_queue.unwrap_or(1024),
        comms_policy,
    })
}

fn default_runtime_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(xdg);
        return path.join("tmax");
    }

    let uid = Uid::effective().as_raw();
    PathBuf::from(format!("/tmp/tmax-{uid}"))
}

fn ensure_runtime_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove stale socket {}", path.display()))?;
    }
    Ok(())
}

fn verify_peer_uid(stream: &UnixStream, expected_uid: u32) -> Result<()> {
    let creds = stream
        .peer_cred()
        .context("failed to query peer credentials")?;
    let uid = creds.uid();
    if uid != expected_uid {
        return Err(anyhow!(
            "peer uid {uid} is not allowed (expected {expected_uid})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;

    #[test]
    fn default_runtime_dir_resolves() {
        let dir = default_runtime_dir();
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn enqueue_response_fails_when_queue_full() {
        let (tx, _rx) = mpsc::channel(1);
        enqueue_response(&tx, Response::ok(None)).expect("first send should fit");
        let err = enqueue_response(&tx, Response::ok(None)).expect_err("second send should fail");
        assert!(err.to_string().contains("queue full"));
    }

    #[tokio::test]
    async fn verify_peer_uid_accepts_matching_uid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("uid-ok.sock");
        let listener = UnixListener::bind(&socket).expect("bind listener");

        let client_task = tokio::spawn(async move {
            tokio::net::UnixStream::connect(&socket)
                .await
                .expect("client connect")
        });

        let (server_stream, _) = listener.accept().await.expect("accept");
        let _client = client_task.await.expect("join client task");

        let uid = Uid::effective().as_raw();
        verify_peer_uid(&server_stream, uid).expect("expected matching uid to pass");
    }

    #[tokio::test]
    async fn verify_peer_uid_rejects_mismatched_uid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("uid-bad.sock");
        let listener = UnixListener::bind(&socket).expect("bind listener");

        let client_task = tokio::spawn(async move {
            tokio::net::UnixStream::connect(&socket)
                .await
                .expect("client connect")
        });

        let (server_stream, _) = listener.accept().await.expect("accept");
        let _client = client_task.await.expect("join client task");

        let wrong_uid = Uid::effective().as_raw().saturating_add(1);
        let err =
            verify_peer_uid(&server_stream, wrong_uid).expect_err("expected mismatched uid fail");
        assert!(err.to_string().contains("not allowed"));
    }

    #[test]
    fn ensure_runtime_dir_sets_strict_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("runtime");
        ensure_runtime_dir(&target).expect("create runtime dir");
        let mode = fs::metadata(&target)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn parse_comms_policy_accepts_expected_values() {
        assert_eq!(parse_comms_policy("open").expect("open"), CommsPolicy::Open);
        assert_eq!(
            parse_comms_policy("same_subtree").expect("same_subtree"),
            CommsPolicy::SameSubtree
        );
        assert_eq!(
            parse_comms_policy("parent_only").expect("parent_only"),
            CommsPolicy::ParentOnly
        );
        let err = parse_comms_policy("invalid").expect_err("invalid policy should fail");
        assert!(err.to_string().contains("invalid --comms-policy"));
    }
}
