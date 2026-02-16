use anyhow::{Context, Result, anyhow};
use base64::Engine;
use clap::Parser;
use futures_util::SinkExt;
use libtmax::handler::{
    CommsPolicy, enforce_comms_policy_for_pair, enforce_session_binding_if_present,
    enqueue_response, parse_comms_policy, resolve_sender_session, task_created_by_session,
    task_policy_peer_session,
};
use libtmax::{SessionCreateOptions, SessionManager, SessionManagerConfig};
use nix::unistd::Uid;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tmax_mesh::crypto;
use tmax_mesh::friends::{FriendRecord, FriendsStore, TrustTier};
use tmax_mesh::identity::NodeIdentity;
use tmax_mesh::inbox::MessageType;
use tmax_mesh::inbox::{InboxMessage, NodeInbox};
use tmax_mesh::ingress::{IngressPolicy, IngressRequest, IngressResult};
use tmax_mesh::invite;
use tmax_mesh::rate_limit::RateLimiter;
use tmax_mesh::recovery::load_or_create_recovery_key;
use tmax_mesh::state_dir::default_state_dir;
use tmax_mesh::transport::MeshTransport;
use tmax_mesh_proto::host::v1 as host_pb;
use tmax_mesh_proto::mesh::v1 as mesh_pb;
use tmax_mesh_proto::mesh::v1::peer_service_server::{PeerService, PeerServiceServer};
use tmax_protocol::{ErrorCode, MAX_JSON_LINE_BYTES, PROTOCOL_VERSION};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::{RwLock, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use tonic::transport::Server;
use tonic::{Request, Response, Status};

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Unix socket path for JSON-lines client API.
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    history_dir: Option<PathBuf>,
    #[arg(long)]
    recovery_key_path: Option<PathBuf>,
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Address for the PeerService (node-to-node messaging). If set, a gRPC server is spawned for peer envelope delivery.
    #[arg(long)]
    peer_listen: Option<String>,
    /// Relay host addresses to connect to for NAT traversal (comma-separated or repeated).
    #[arg(long)]
    relay_host: Vec<String>,
    /// Communication policy for the Unix socket API.
    #[arg(long, default_value = "open")]
    comms_policy: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tmax_node=info".into()),
        )
        .init();

    let args = Args::parse();

    let state_dir = match args.state_dir {
        Some(d) => d,
        None => default_state_dir().context("failed to determine state dir")?,
    };
    let recovery_key_path = args.recovery_key_path.or_else(|| {
        std::env::var("TMAX_RECOVERY_KEY_PATH")
            .ok()
            .map(PathBuf::from)
    });
    let recovery_key = load_or_create_recovery_key(recovery_key_path.as_deref())
        .context("failed to load recovery key")?;

    let identity = Arc::new(
        NodeIdentity::load_or_create(&state_dir, &recovery_key)
            .context("failed to load node identity")?,
    );
    tracing::info!(node_id = %identity.node_id, "node identity loaded");

    let friends = Arc::new(RwLock::new(
        FriendsStore::load(&state_dir).context("failed to load friends store")?,
    ));
    let inbox = Arc::new(RwLock::new(
        NodeInbox::load(&state_dir).context("failed to load node inbox")?,
    ));
    let rate_limiter = Arc::new(Mutex::new(RateLimiter::new(60, 10.0)));

    let manager = Arc::new(SessionManager::new(SessionManagerConfig {
        history_dir: args
            .history_dir
            .or_else(|| std::env::var("TMAX_HISTORY_DIR").ok().map(PathBuf::from)),
        recovery_key_path: recovery_key_path.clone(),
        ..SessionManagerConfig::default()
    }));

    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Optionally spawn PeerService gRPC on a separate port for node-to-node messaging
    if let Some(peer_addr_str) = &args.peer_listen {
        let peer_addr: SocketAddr = peer_addr_str
            .parse()
            .with_context(|| format!("invalid --peer-listen {peer_addr_str}"))?;
        let peer_listener = TcpListener::bind(peer_addr)
            .await
            .with_context(|| format!("failed to bind peer {peer_addr}"))?;
        let peer_local = peer_listener.local_addr()?;
        tracing::info!("peer service listening addr={peer_local}");

        let peer_svc = PeerServiceImpl {
            identity: identity.clone(),
            friends: friends.clone(),
            inbox: inbox.clone(),
            rate_limiter: rate_limiter.clone(),
        };

        tokio::spawn(async move {
            let _ = Server::builder()
                .add_service(PeerServiceServer::new(peer_svc))
                .serve_with_incoming(TcpListenerStream::new(peer_listener))
                .await;
        });
    }

    // Connect to relay hosts if configured
    let transport = if !args.relay_host.is_empty() {
        let sig_payload = format!("relay-register:{}", identity.node_id);
        let sig = identity
            .sign(sig_payload.as_bytes())
            .context("failed to sign relay register")?;
        let transport = Arc::new(MeshTransport::new(
            args.relay_host.clone(),
            identity.node_id.clone(),
            identity.public_key_b64.clone(),
            sig,
        ));
        tracing::info!(
            relay_count = transport.relay_count(),
            "connected to relay hosts"
        );

        // Spawn delivery processor: reads envelopes from relays and processes through ingress/inbox
        {
            let identity = identity.clone();
            let friends = friends.clone();
            let inbox = inbox.clone();
            let rate_limiter = rate_limiter.clone();
            let transport = transport.clone();
            tokio::spawn(async move {
                process_relay_deliveries(transport, identity, friends, inbox, rate_limiter).await;
            });
        }
        Some(transport)
    } else {
        None
    };

    let comms_policy = parse_comms_policy(&args.comms_policy).context("invalid --comms-policy")?;

    let socket_path = resolve_node_socket_path(args.socket);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_stale_socket(&socket_path)?;
    let sock_listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(socket = %socket_path.display(), "unix socket server listening");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shared = SharedNodeState {
        manager,
        identity: identity.clone(),
        friends: friends.clone(),
        inbox: inbox.clone(),
        rate_limiter: rate_limiter.clone(),
        started_at_ms,
        transport: transport.clone(),
        relay_hosts: args.relay_host.clone(),
        comms_policy,
    };

    let socket_path_owned = socket_path.clone();
    let result = socket_accept_loop(sock_listener, shared, shutdown_rx, shutdown_tx.clone()).await;

    shutdown_tx.send_replace(true);
    let _ = std::fs::remove_file(&socket_path_owned);

    result
}

fn resolve_node_socket_path(override_path: Option<PathBuf>) -> PathBuf {
    if let Some(path) = override_path {
        return path;
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("tmax").join("tmax-node.sock");
    }
    let uid = Uid::effective().as_raw();
    PathBuf::from(format!("/tmp/tmax-{uid}/tmax-node.sock"))
}

// NOTE: TmaxNode gRPC service removed. All client API is now via Unix socket.
// Only PeerService gRPC remains for node-to-node envelope delivery.

// --- Unix socket server for JSON-lines client API ---

#[derive(Clone)]
struct SharedNodeState {
    manager: Arc<SessionManager>,
    identity: Arc<NodeIdentity>,
    friends: Arc<RwLock<FriendsStore>>,
    inbox: Arc<RwLock<NodeInbox>>,
    #[allow(dead_code)]
    rate_limiter: Arc<Mutex<RateLimiter>>,
    started_at_ms: u64,
    transport: Option<Arc<MeshTransport>>,
    relay_hosts: Vec<String>,
    comms_policy: CommsPolicy,
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove stale socket {}", path.display()))?;
    }
    Ok(())
}

fn verify_peer_uid(stream: &UnixStream, expected: u32) -> Result<()> {
    let cred = stream
        .peer_cred()
        .context("failed to get peer credentials")?;
    if cred.uid() != expected {
        return Err(anyhow!("peer uid {} != expected {}", cred.uid(), expected));
    }
    Ok(())
}

async fn socket_accept_loop(
    listener: UnixListener,
    state: SharedNodeState,
    mut shutdown_rx: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<()> {
    let active: Arc<RwLock<Vec<JoinHandle<()>>>> = Arc::new(RwLock::new(Vec::new()));
    let uid = Uid::effective().as_raw();

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("unix socket shutdown requested");
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(v) => v,
                    Err(err) => {
                        tracing::warn!("unix accept failed: {err}");
                        continue;
                    }
                };
                if let Err(err) = verify_peer_uid(&stream, uid) {
                    tracing::warn!("rejected peer: {err}");
                    continue;
                }
                let handle = tokio::spawn(handle_socket_connection(
                    stream,
                    state.clone(),
                    shutdown_tx.clone(),
                ));
                active.write().await.push(handle);
            }
        }
    }

    for h in active.write().await.drain(..) {
        h.abort();
    }
    Ok(())
}

async fn handle_socket_connection(
    stream: UnixStream,
    state: SharedNodeState,
    shutdown_tx: watch::Sender<bool>,
) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = FramedRead::new(
        read_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );

    let (out_tx, mut out_rx) = mpsc::channel::<tmax_protocol::Response>(128);
    let mut writer = FramedWrite::new(
        write_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );

    let writer_task = tokio::spawn(async move {
        while let Some(resp) = out_rx.recv().await {
            let line = match serde_json::to_string(&resp) {
                Ok(l) => l,
                Err(err) => {
                    tracing::error!("failed to encode response: {err}");
                    continue;
                }
            };
            if let Err(err) = writer.send(line).await {
                tracing::warn!("socket write failed: {err}");
                break;
            }
        }
    });

    if enqueue_response(
        &out_tx,
        tmax_protocol::Response::hello(vec![
            "single_writer_queue".to_string(),
            "attach_ids".to_string(),
            "replay".to_string(),
            "mesh".to_string(),
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
                tracing::warn!("socket read failed: {err}");
                break;
            }
            None => break,
        };

        let req: tmax_protocol::Request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(err) => {
                if enqueue_response(
                    &out_tx,
                    tmax_protocol::Response::error(
                        ErrorCode::InvalidRequest,
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

        match handle_socket_request(
            req,
            &state,
            out_tx.clone(),
            &mut subscriptions,
            &mut attach_tasks,
            &mut owned_attachments,
            shutdown_tx.clone(),
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => break,
            Err(err) => {
                let code = SessionManager::map_err_code(&err);
                if enqueue_response(
                    &out_tx,
                    tmax_protocol::Response::error(code, err.to_string()),
                )
                .is_err()
                {
                    break;
                }
            }
        }
    }

    for (_, h) in subscriptions.drain() {
        h.abort();
    }
    for (_, h) in attach_tasks.drain() {
        h.abort();
    }
    for attachment_id in owned_attachments.into_keys() {
        if let Err(err) = state.manager.detach(&attachment_id).await {
            tracing::warn!("detach cleanup failed for {attachment_id}: {err}");
        }
    }
    drop(out_tx);
    let _ = writer_task.await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_socket_request(
    req: tmax_protocol::Request,
    state: &SharedNodeState,
    out_tx: mpsc::Sender<tmax_protocol::Response>,
    subscriptions: &mut HashMap<String, JoinHandle<()>>,
    attach_tasks: &mut HashMap<String, JoinHandle<()>>,
    owned_attachments: &mut HashMap<String, String>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<bool> {
    let manager = &state.manager;
    let comms_policy = state.comms_policy;

    match req {
        // --- Core session requests (same as tmax-local) ---
        tmax_protocol::Request::SessionCreate {
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
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(summary)?)),
            )?;
        }
        tmax_protocol::Request::SessionDestroy {
            session_id,
            cascade,
        } => {
            manager.destroy_session(&session_id, cascade).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"session_id": session_id}))),
            )?;
        }
        tmax_protocol::Request::SessionList => {
            let list = manager.list_sessions().await;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(list)?)),
            )?;
        }
        tmax_protocol::Request::SessionTree => {
            let tree = manager.session_tree().await;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(tree)?)),
            )?;
        }
        tmax_protocol::Request::SessionInfo { session_id } => {
            let info = manager
                .session_info(&session_id)
                .await
                .ok_or_else(|| anyhow!("session not found"))?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(info)?)),
            )?;
        }
        tmax_protocol::Request::Attach {
            session_id,
            mode,
            last_seq_seen,
        } => {
            let handle = manager.attach(&session_id, mode, last_seq_seen).await?;
            owned_attachments.insert(handle.info.attachment_id.clone(), session_id.clone());
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"attachment": handle.info}))),
            )?;
            for event in handle.replay_events {
                enqueue_response(
                    &out_tx,
                    tmax_protocol::Response::Event {
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
                                tmax_protocol::Response::Event {
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
        tmax_protocol::Request::Detach { attachment_id } => {
            manager.detach(&attachment_id).await?;
            owned_attachments.remove(&attachment_id);
            if let Some(h) = attach_tasks.remove(&attachment_id) {
                h.abort();
            }
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"attachment_id": attachment_id}))),
            )?;
        }
        tmax_protocol::Request::SendInput {
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
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"written": data.len()}))),
            )?;
        }
        tmax_protocol::Request::Resize {
            session_id,
            cols,
            rows,
        } => {
            manager.resize(&session_id, cols, rows).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(
                    json!({"session_id": session_id, "cols": cols, "rows": rows}),
                )),
            )?;
        }
        tmax_protocol::Request::MarkerInsert { session_id, name } => {
            let seq = manager.insert_marker(&session_id, name.clone()).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(
                    json!({"session_id": session_id, "name": name, "seq": seq}),
                )),
            )?;
        }
        tmax_protocol::Request::MarkerList { session_id } => {
            let markers = manager.list_markers(&session_id).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(markers)?)),
            )?;
        }
        tmax_protocol::Request::MessageSend {
            from_session_id,
            to_session_id,
            topic,
            body,
            requires_response,
            encrypt,
            sign,
        } => {
            let from_session_id = resolve_sender_session(from_session_id, owned_attachments)?;
            if let Some(from) = from_session_id.as_deref() {
                enforce_comms_policy_for_pair(comms_policy, manager, from, &to_session_id).await?;
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
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(message)?)),
            )?;
        }
        tmax_protocol::Request::MessageList {
            session_id,
            unread_only,
            limit,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let messages = manager
                .list_messages(&session_id, unread_only, limit)
                .await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(messages)?)),
            )?;
        }
        tmax_protocol::Request::MessageAck {
            session_id,
            message_id,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let acked = manager.ack_message(&session_id, &message_id).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(acked)?)),
            )?;
        }
        tmax_protocol::Request::MessageUnreadCount { session_id } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let unread = manager.unread_message_count(&session_id).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(
                    json!({"session_id": session_id, "unread": unread}),
                )),
            )?;
        }
        tmax_protocol::Request::WalletInfo { session_id } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let wallet = manager.wallet_info(&session_id).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(wallet)?)),
            )?;
        }
        tmax_protocol::Request::WorkflowCreate {
            name,
            root_session_id,
        } => {
            enforce_session_binding_if_present(&root_session_id, owned_attachments)?;
            let workflow = manager.create_workflow(name, root_session_id).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(workflow)?)),
            )?;
        }
        tmax_protocol::Request::WorkflowJoin {
            workflow_id,
            session_id,
            parent_session_id,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let workflow = manager
                .join_workflow(&workflow_id, &session_id, &parent_session_id)
                .await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(workflow)?)),
            )?;
        }
        tmax_protocol::Request::WorkflowLeave {
            workflow_id,
            session_id,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let workflow = manager.leave_workflow(&workflow_id, &session_id).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(workflow)?)),
            )?;
        }
        tmax_protocol::Request::WorkflowList { session_id } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let workflows = manager.list_workflows(&session_id).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(workflows)?)),
            )?;
        }
        tmax_protocol::Request::TaskCreate {
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
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(task)?)),
            )?;
        }
        tmax_protocol::Request::TaskList {
            workflow_id,
            session_id,
            include_done,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let tasks = manager
                .list_shared_tasks(&workflow_id, &session_id, include_done)
                .await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(tasks)?)),
            )?;
        }
        tmax_protocol::Request::TaskClaim {
            task_id,
            session_id,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            if let Some(created_by) = task_created_by_session(manager, &task_id).await? {
                enforce_comms_policy_for_pair(comms_policy, manager, &created_by, &session_id)
                    .await?;
            }
            let task = manager.claim_shared_task(&task_id, &session_id).await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(task)?)),
            )?;
        }
        tmax_protocol::Request::TaskSetStatus {
            task_id,
            session_id,
            status,
        } => {
            enforce_session_binding_if_present(&session_id, owned_attachments)?;
            let actor = resolve_sender_session(Some(session_id), owned_attachments)?
                .ok_or_else(|| anyhow!("task status requires sender session"))?;
            if let Some(peer) = task_policy_peer_session(manager, &task_id, &actor).await? {
                enforce_comms_policy_for_pair(comms_policy, manager, &actor, &peer).await?;
            }
            let task = manager
                .set_shared_task_status(&task_id, &actor, status)
                .await?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(serde_json::to_value(task)?)),
            )?;
        }
        tmax_protocol::Request::Subscribe {
            session_id,
            last_seq_seen,
        } => {
            let sub = manager.subscribe(&session_id, last_seq_seen).await?;
            for event in sub.replay_events {
                enqueue_response(
                    &out_tx,
                    tmax_protocol::Response::Event {
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
                                tmax_protocol::Response::Event {
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
                tmax_protocol::Response::ok(Some(
                    json!({"session_id": session_id, "subscribed": true}),
                )),
            )?;
        }
        tmax_protocol::Request::Unsubscribe { session_id } => {
            if let Some(task) = subscriptions.remove(&session_id) {
                task.abort();
            }
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(
                    json!({"session_id": session_id, "subscribed": false}),
                )),
            )?;
        }
        tmax_protocol::Request::Health => {
            let list = manager.list_sessions().await;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({
                    "healthy": true,
                    "protocol_version": PROTOCOL_VERSION,
                    "session_count": list.len(),
                    "node_id": state.identity.node_id,
                }))),
            )?;
        }
        tmax_protocol::Request::ServerShutdown => {
            let _ = enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"shutdown": true}))),
            );
            shutdown_tx.send_replace(true);
            return Ok(false);
        }

        // --- Mesh-specific requests ---
        tmax_protocol::Request::NodeInfo => {
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({
                    "node_id": state.identity.node_id,
                    "public_key_b64": state.identity.public_key_b64,
                    "started_at_ms": state.started_at_ms,
                }))),
            )?;
        }
        tmax_protocol::Request::InviteCreate {
            relay_hosts,
            scopes,
            ttl_ms,
        } => {
            let ttl = if ttl_ms == 0 { 3_600_000 } else { ttl_ms };
            let token = invite::create_invite(
                &state.identity.node_id,
                &state.identity.public_key_b64,
                state.identity.secret_key(),
                relay_hosts,
                scopes,
                ttl,
            )?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"token": token}))),
            )?;
        }
        tmax_protocol::Request::InviteAccept { token } => {
            let payload = invite::accept_invite(&token)?;
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let record = FriendRecord {
                node_id: payload.inviter_node_id.clone(),
                public_key_b64: payload.inviter_public_key_b64.clone(),
                alias: None,
                relay_hosts: payload.relay_hosts,
                blocked: false,
                added_at_ms: now_ms,
                trust_tier: TrustTier::default(),
            };
            state.friends.write().await.add(record)?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({
                    "inviter_node_id": payload.inviter_node_id,
                    "inviter_public_key_b64": payload.inviter_public_key_b64,
                }))),
            )?;
        }
        tmax_protocol::Request::FriendsList => {
            let store = state.friends.read().await;
            let friends: Vec<serde_json::Value> = store
                .list()
                .iter()
                .map(|f| {
                    json!({
                        "node_id": f.node_id,
                        "public_key_b64": f.public_key_b64,
                        "alias": f.alias,
                        "relay_hosts": f.relay_hosts,
                        "blocked": f.blocked,
                        "added_at_ms": f.added_at_ms,
                        "trust_tier": format!("{:?}", f.trust_tier).to_lowercase(),
                    })
                })
                .collect();
            enqueue_response(&out_tx, tmax_protocol::Response::ok(Some(json!(friends))))?;
        }
        tmax_protocol::Request::FriendsBlock { node_id } => {
            state.friends.write().await.block(&node_id)?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"node_id": node_id, "blocked": true}))),
            )?;
        }
        tmax_protocol::Request::FriendsUnblock { node_id } => {
            state.friends.write().await.unblock(&node_id)?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"node_id": node_id, "blocked": false}))),
            )?;
        }
        tmax_protocol::Request::FriendsRemove { node_id } => {
            state.friends.write().await.remove(&node_id)?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({"node_id": node_id, "removed": true}))),
            )?;
        }
        tmax_protocol::Request::FriendsSetTrust {
            node_id,
            trust_tier,
        } => {
            let tier = protocol_trust_tier_to_mesh(trust_tier);
            state.friends.write().await.set_trust(&node_id, tier)?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(
                    json!({"node_id": node_id, "trust_tier": format!("{:?}", tier).to_lowercase()}),
                )),
            )?;
        }
        tmax_protocol::Request::NodeInboxList { unread_only, limit } => {
            let inbox = state.inbox.read().await;
            let messages: Vec<serde_json::Value> = inbox
                .list(unread_only, limit)
                .into_iter()
                .map(|m| {
                    json!({
                        "message_id": m.message_id,
                        "from_node_id": m.from_node_id,
                        "from_public_key_b64": m.from_public_key_b64,
                        "topic": m.topic,
                        "body": m.body,
                        "timestamp_ms": m.timestamp_ms,
                        "acked": m.acked,
                        "message_type": format!("{:?}", m.message_type).to_lowercase(),
                    })
                })
                .collect();
            enqueue_response(&out_tx, tmax_protocol::Response::ok(Some(json!(messages))))?;
        }
        tmax_protocol::Request::NodeInboxAck { message_id } => {
            let found = state.inbox.write().await.ack(&message_id)?;
            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(
                    json!({"message_id": message_id, "found": found}),
                )),
            )?;
        }
        tmax_protocol::Request::NodeSendRemote {
            to_node_id,
            topic,
            body,
            encrypt,
            invite_token,
            message_type,
        } => {
            let friends_guard = state.friends.read().await;
            let friend = friends_guard
                .get(&to_node_id)
                .ok_or_else(|| anyhow!("recipient is not a friend"))?;

            let message_id = uuid::Uuid::new_v4().to_string();
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let (ciphertext_b64, nonce_b64, plaintext_body) = if encrypt {
                let peer_pub_bytes = base64::engine::general_purpose::STANDARD
                    .decode(&friend.public_key_b64)
                    .context("friend has invalid public key")?;
                let peer_pub = k256::PublicKey::from_sec1_bytes(&peer_pub_bytes)
                    .context("friend public key is not valid secp256k1")?;
                let shared_key = state.identity.derive_shared_key(&peer_pub);
                let (ct, nc) = crypto::encrypt_with_key(&shared_key, body.as_bytes())?;
                (ct, nc, None)
            } else {
                (String::new(), String::new(), Some(body.clone()))
            };

            let sig_payload = crypto::canonical_message_payload(
                Some(&state.identity.node_id),
                &to_node_id,
                topic.as_deref(),
                &body,
                false,
            );
            let signature_b64 = state.identity.sign(&sig_payload)?;

            let mesh_mt = protocol_message_type_to_mesh(message_type);
            let envelope = mesh_pb::Envelope {
                message_id: message_id.clone(),
                from_node_id: state.identity.node_id.clone(),
                to_node_id: to_node_id.clone(),
                timestamp_ms: now_ms,
                nonce_b64,
                ciphertext_b64,
                signature_b64,
                from_public_key_b64: state.identity.public_key_b64.clone(),
                invite_token: invite_token.clone(),
                topic,
                plaintext_body,
                message_type: mesh_message_type_to_envelope(mesh_mt).into(),
            };

            let peer_addr = friend.relay_hosts.first().cloned();
            drop(friends_guard);

            // Try direct peer delivery
            if let Some(addr) = peer_addr {
                match send_envelope_to_peer(&addr, envelope.clone()).await {
                    Ok(ack) if ack.accepted => {
                        enqueue_response(
                            &out_tx,
                            tmax_protocol::Response::ok(Some(
                                json!({"message_id": message_id, "delivered": true}),
                            )),
                        )?;
                        return Ok(true);
                    }
                    _ => {}
                }
            }

            // Try rendezvous lookup
            if !state.relay_hosts.is_empty() {
                for relay in &state.relay_hosts {
                    if let Ok(endpoints) = lookup_node_endpoints(relay, &to_node_id).await {
                        for ep in endpoints {
                            if let Ok(ack) = send_envelope_to_peer(&ep, envelope.clone()).await
                                && ack.accepted
                            {
                                enqueue_response(
                                    &out_tx,
                                    tmax_protocol::Response::ok(Some(
                                        json!({"message_id": message_id, "delivered": true}),
                                    )),
                                )?;
                                return Ok(true);
                            }
                        }
                    }
                }
            }

            // Fall back to relay
            if let Some(transport) = &state.transport {
                match transport.send_via_relay(envelope).await {
                    Ok(()) => {
                        enqueue_response(
                            &out_tx,
                            tmax_protocol::Response::ok(Some(
                                json!({"message_id": message_id, "delivered": true}),
                            )),
                        )?;
                        return Ok(true);
                    }
                    Err(e) => {
                        enqueue_response(
                            &out_tx,
                            tmax_protocol::Response::ok(Some(json!({
                                "message_id": message_id,
                                "delivered": false,
                                "error": format!("relay failed: {e}"),
                            }))),
                        )?;
                        return Ok(true);
                    }
                }
            }

            enqueue_response(
                &out_tx,
                tmax_protocol::Response::ok(Some(json!({
                    "message_id": message_id,
                    "delivered": false,
                    "error": "no peer address or relay available",
                }))),
            )?;
        }
    }

    Ok(true)
}

/// Convert protocol TrustTier to mesh TrustTier.
fn protocol_trust_tier_to_mesh(tier: tmax_protocol::TrustTier) -> TrustTier {
    match tier {
        tmax_protocol::TrustTier::Public => TrustTier::Public,
        tmax_protocol::TrustTier::Follower => TrustTier::Follower,
        tmax_protocol::TrustTier::Trusted => TrustTier::Trusted,
        tmax_protocol::TrustTier::Operator => TrustTier::Operator,
    }
}

/// Convert protocol MeshMessageType to mesh inbox MessageType.
fn protocol_message_type_to_mesh(mt: tmax_protocol::MeshMessageType) -> MessageType {
    match mt {
        tmax_protocol::MeshMessageType::Unspecified => MessageType::Unspecified,
        tmax_protocol::MeshMessageType::DmText => MessageType::DmText,
        tmax_protocol::MeshMessageType::Broadcast => MessageType::Broadcast,
        tmax_protocol::MeshMessageType::TaskUpdate => MessageType::TaskUpdate,
        tmax_protocol::MeshMessageType::Command => MessageType::Command,
    }
}

fn mesh_message_type_to_envelope(mt: MessageType) -> mesh_pb::MessageType {
    match mt {
        MessageType::Unspecified => mesh_pb::MessageType::Unspecified,
        MessageType::DmText => mesh_pb::MessageType::DmText,
        MessageType::Broadcast => mesh_pb::MessageType::Broadcast,
        MessageType::TaskUpdate => mesh_pb::MessageType::TaskUpdate,
        MessageType::Command => mesh_pb::MessageType::Command,
    }
}

fn envelope_message_type_to_mesh(mt: i32) -> MessageType {
    match mesh_pb::MessageType::try_from(mt) {
        Ok(mesh_pb::MessageType::DmText) => MessageType::DmText,
        Ok(mesh_pb::MessageType::Broadcast) => MessageType::Broadcast,
        Ok(mesh_pb::MessageType::TaskUpdate) => MessageType::TaskUpdate,
        Ok(mesh_pb::MessageType::Command) => MessageType::Command,
        _ => MessageType::Unspecified,
    }
}

// --- Shared delivery logic ---

/// Result of delivering an envelope to the local node.
enum DeliveryResult {
    Accepted,
    Rejected(String),
}

/// Validate, decrypt, and store an inbound envelope in the inbox.
async fn deliver_message(
    envelope: &mesh_pb::Envelope,
    identity: &NodeIdentity,
    friends: &RwLock<FriendsStore>,
    inbox: &RwLock<NodeInbox>,
    rate_limiter: &Mutex<RateLimiter>,
) -> DeliveryResult {
    // Verify addressed to us
    if envelope.to_node_id != identity.node_id {
        return DeliveryResult::Rejected("wrong recipient".to_string());
    }

    let message_type = envelope_message_type_to_mesh(envelope.message_type);

    // Build canonical payload for sig verification
    let body_for_sig = envelope
        .plaintext_body
        .as_deref()
        .unwrap_or(&envelope.ciphertext_b64);
    let sig_payload = crypto::canonical_message_payload(
        Some(&envelope.from_node_id),
        &envelope.to_node_id,
        envelope.topic.as_deref(),
        body_for_sig,
        false,
    );

    // Ingress check
    let ingress_result = {
        let friends_guard = friends.read().await;
        let mut rl = rate_limiter.lock().unwrap();
        let mut policy = IngressPolicy::new(&friends_guard, &mut rl);
        let req = IngressRequest {
            from_node_id: &envelope.from_node_id,
            from_public_key_b64: &envelope.from_public_key_b64,
            payload: &sig_payload,
            signature_b64: &envelope.signature_b64,
            invite_token: envelope.invite_token.as_deref(),
            my_node_id: &identity.node_id,
            message_type,
        };
        policy.check(&req)
    };

    match ingress_result {
        IngressResult::Reject(reason) => {
            return DeliveryResult::Rejected(reason);
        }
        IngressResult::AcceptViaInvite(invite_payload) => {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let record = FriendRecord {
                node_id: envelope.from_node_id.clone(),
                public_key_b64: envelope.from_public_key_b64.clone(),
                alias: None,
                relay_hosts: invite_payload.relay_hosts,
                blocked: false,
                added_at_ms: now_ms,
                trust_tier: TrustTier::default(),
            };
            if let Err(e) = friends.write().await.add(record) {
                tracing::warn!("failed to auto-add friend: {e}");
            }
        }
        IngressResult::Accept => {}
    }

    // Decrypt if encrypted
    let body = if !envelope.ciphertext_b64.is_empty() {
        let Ok(peer_pub_bytes) =
            base64::engine::general_purpose::STANDARD.decode(&envelope.from_public_key_b64)
        else {
            return DeliveryResult::Rejected("invalid from_public_key_b64".to_string());
        };
        let Ok(peer_pub) = k256::PublicKey::from_sec1_bytes(&peer_pub_bytes) else {
            return DeliveryResult::Rejected("invalid secp256k1 key".to_string());
        };
        let shared_key = identity.derive_shared_key(&peer_pub);
        match crypto::decrypt_with_key(&shared_key, &envelope.ciphertext_b64, &envelope.nonce_b64) {
            Ok(plaintext) => match String::from_utf8(plaintext) {
                Ok(s) => s,
                Err(_) => {
                    return DeliveryResult::Rejected("decrypted body is not utf-8".to_string());
                }
            },
            Err(e) => return DeliveryResult::Rejected(format!("decryption failed: {e}")),
        }
    } else {
        envelope.plaintext_body.clone().unwrap_or_default()
    };

    // Store in inbox
    let msg = InboxMessage {
        message_id: envelope.message_id.clone(),
        from_node_id: envelope.from_node_id.clone(),
        from_public_key_b64: envelope.from_public_key_b64.clone(),
        topic: envelope.topic.clone(),
        body,
        timestamp_ms: envelope.timestamp_ms,
        acked: false,
        message_type,
    };
    if let Err(e) = inbox.write().await.push(msg) {
        return DeliveryResult::Rejected(format!("inbox write failed: {e}"));
    }

    DeliveryResult::Accepted
}

// --- PeerService implementation ---

#[derive(Clone)]
struct PeerServiceImpl {
    identity: Arc<NodeIdentity>,
    friends: Arc<RwLock<FriendsStore>>,
    inbox: Arc<RwLock<NodeInbox>>,
    rate_limiter: Arc<Mutex<RateLimiter>>,
}

#[tonic::async_trait]
impl PeerService for PeerServiceImpl {
    async fn send_message(
        &self,
        req: Request<mesh_pb::Envelope>,
    ) -> Result<Response<mesh_pb::Ack>, Status> {
        let envelope = req.into_inner();
        let message_id = envelope.message_id.clone();

        match deliver_message(
            &envelope,
            &self.identity,
            &self.friends,
            &self.inbox,
            &self.rate_limiter,
        )
        .await
        {
            DeliveryResult::Accepted => Ok(Response::new(mesh_pb::Ack {
                message_id,
                accepted: true,
                error: None,
            })),
            DeliveryResult::Rejected(reason) => Ok(Response::new(mesh_pb::Ack {
                message_id,
                accepted: false,
                error: Some(reason),
            })),
        }
    }
}

/// Call Lookup RPC on a relay host to get observed endpoints for a node.
async fn lookup_node_endpoints(relay_addr: &str, node_id: &str) -> Result<Vec<String>> {
    let endpoint = if relay_addr.starts_with("http") {
        relay_addr.to_string()
    } else {
        format!("http://{relay_addr}")
    };
    let mut client = host_pb::host_service_client::HostServiceClient::connect(endpoint)
        .await
        .context("failed to connect to relay for lookup")?;
    let resp = client
        .lookup(host_pb::LookupRequest {
            node_id: node_id.to_string(),
        })
        .await
        .context("lookup RPC failed")?;
    Ok(resp.into_inner().observed_endpoints)
}

/// Send an envelope to a peer's PeerService.
async fn send_envelope_to_peer(
    peer_addr: &str,
    envelope: mesh_pb::Envelope,
) -> Result<mesh_pb::Ack> {
    let endpoint = if peer_addr.starts_with("http") {
        peer_addr.to_string()
    } else {
        format!("http://{peer_addr}")
    };
    let mut client = mesh_pb::peer_service_client::PeerServiceClient::connect(endpoint)
        .await
        .context("failed to connect to peer")?;
    let resp = client
        .send_message(envelope)
        .await
        .context("peer rejected message")?;
    Ok(resp.into_inner())
}

/// Background task: process envelopes received via relay and store in inbox.
async fn process_relay_deliveries(
    transport: Arc<MeshTransport>,
    identity: Arc<NodeIdentity>,
    friends: Arc<RwLock<FriendsStore>>,
    inbox: Arc<RwLock<NodeInbox>>,
    rate_limiter: Arc<Mutex<RateLimiter>>,
) {
    let mut incoming = transport.incoming.lock().await;
    while let Some(envelope) = incoming.recv().await {
        match deliver_message(&envelope, &identity, &friends, &inbox, &rate_limiter).await {
            DeliveryResult::Accepted => {
                tracing::debug!(from = %envelope.from_node_id, "relay delivery accepted");
            }
            DeliveryResult::Rejected(reason) => {
                tracing::debug!(from = %envelope.from_node_id, reason = %reason, "relay delivery rejected");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use futures_util::StreamExt;
    use std::time::Duration;
    use tmax_protocol::{AttachMode, Request, Response};
    use tokio::time::timeout;

    struct TestClient {
        reader: FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
        writer: FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
    }

    impl TestClient {
        async fn connect(socket_path: &Path) -> Result<Self> {
            let stream = UnixStream::connect(socket_path).await?;
            let (r, w) = stream.into_split();
            let mut client = Self {
                reader: FramedRead::new(r, LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES)),
                writer: FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES)),
            };
            match client.next_response().await? {
                Response::Hello { .. } => Ok(client),
                other => Err(anyhow!("expected Hello, got {other:?}")),
            }
        }

        async fn send(&mut self, req: Request) -> Result<()> {
            let line = serde_json::to_string(&req)?;
            self.writer.send(line).await?;
            Ok(())
        }

        async fn next_response(&mut self) -> Result<Response> {
            let Some(line) = self.reader.next().await else {
                return Err(anyhow!("server disconnected"));
            };
            Ok(serde_json::from_str(&line?)?)
        }

        async fn request_ok(&mut self, req: Request) -> Result<Option<serde_json::Value>> {
            self.send(req).await?;
            loop {
                match self.next_response().await? {
                    Response::Hello { .. } | Response::Event { .. } => continue,
                    Response::Ok { data } => return Ok(data),
                    Response::Error { message, .. } => return Err(anyhow!("{message}")),
                }
            }
        }
    }

    async fn spawn_test_server() -> Result<(PathBuf, watch::Sender<bool>)> {
        let dir = tempfile::tempdir()?;
        let socket_path = dir.path().join("tmax-node-test.sock");
        let kek = tmax_mesh::crypto::random_key_material();
        let identity = Arc::new(NodeIdentity::load_or_create(dir.path(), &kek)?);
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let friends = Arc::new(RwLock::new(FriendsStore::load(dir.path())?));
        let inbox = Arc::new(RwLock::new(NodeInbox::load(dir.path())?));
        let rate_limiter = Arc::new(Mutex::new(RateLimiter::new(60, 10.0)));
        let manager = Arc::new(SessionManager::new(SessionManagerConfig::default()));

        let state = SharedNodeState {
            manager,
            identity,
            friends,
            inbox,
            rate_limiter,
            started_at_ms,
            transport: None,
            relay_hosts: vec![],
            comms_policy: CommsPolicy::Open,
        };

        let listener = UnixListener::bind(&socket_path)?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let tx_clone = shutdown_tx.clone();

        // Keep tempdir alive by leaking it (test is short-lived)
        let leaked_dir = std::mem::ManuallyDrop::new(dir);
        tokio::spawn(async move {
            let _keep = leaked_dir;
            let _ = socket_accept_loop(listener, state, shutdown_rx, tx_clone).await;
        });

        // Wait for socket to become connectable
        let start = std::time::Instant::now();
        loop {
            match UnixStream::connect(&socket_path).await {
                Ok(s) => {
                    drop(s);
                    break;
                }
                Err(_) if start.elapsed() < Duration::from_secs(3) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(e) => return Err(anyhow!("socket not ready: {e}")),
            }
        }

        Ok((socket_path, shutdown_tx))
    }

    fn extract_str(data: &Option<serde_json::Value>, path: &[&str]) -> String {
        let mut v = data.as_ref().unwrap().clone();
        for key in path {
            v = v.get(*key).unwrap().clone();
        }
        v.as_str().unwrap().to_string()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_round_trip_sessions_messages_and_workflows() -> Result<()> {
        let (socket_path, shutdown) = spawn_test_server().await?;
        let mut client = TestClient::connect(&socket_path).await?;

        // Create root session
        let root_data = client
            .request_ok(Request::SessionCreate {
                exec: "/bin/sh".to_string(),
                args: vec![
                    "-lc".to_string(),
                    "echo hello-socket; sleep 0.2".to_string(),
                ],
                tags: vec![],
                cwd: None,
                label: Some("root".to_string()),
                sandbox: None,
                parent_id: None,
                cols: 80,
                rows: 24,
            })
            .await?;
        let root_id = extract_str(&root_data, &["session_id"]);

        // Create worker session
        let worker_data = client
            .request_ok(Request::SessionCreate {
                exec: "/bin/sh".to_string(),
                args: vec!["-lc".to_string(), "sleep 1".to_string()],
                tags: vec![],
                cwd: None,
                label: Some("worker".to_string()),
                sandbox: None,
                parent_id: None,
                cols: 80,
                rows: 24,
            })
            .await?;
        let worker_id = extract_str(&worker_data, &["session_id"]);

        // Attach and collect output
        client
            .send(Request::Attach {
                session_id: root_id.clone(),
                mode: AttachMode::View,
                last_seq_seen: None,
            })
            .await?;

        let mut out = Vec::new();
        timeout(Duration::from_secs(3), async {
            loop {
                match client.next_response().await? {
                    Response::Event { event, .. } => {
                        if let tmax_protocol::Event::Output { data_b64, .. } = *event {
                            let decoded =
                                base64::engine::general_purpose::STANDARD.decode(&data_b64)?;
                            out.extend_from_slice(&decoded);
                            if String::from_utf8_lossy(&out).contains("hello-socket") {
                                break;
                            }
                        }
                    }
                    Response::Ok { .. } => {} // attach ack
                    _ => {}
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .await??;
        assert!(
            String::from_utf8_lossy(&out).contains("hello-socket"),
            "expected to see hello output"
        );

        // Send message
        let sent = client
            .request_ok(Request::MessageSend {
                from_session_id: Some(root_id.clone()),
                to_session_id: worker_id.clone(),
                topic: Some("question".to_string()),
                body: "ping".to_string(),
                requires_response: false,
                encrypt: false,
                sign: false,
            })
            .await?;
        let msg_id = extract_str(&sent, &["message_id"]);
        assert!(!msg_id.is_empty());

        // Check unread count (need fresh connection since first is subscribed to events)
        let mut client2 = TestClient::connect(&socket_path).await?;
        let unread_data = client2
            .request_ok(Request::MessageUnreadCount {
                session_id: worker_id.clone(),
            })
            .await?;
        let unread = unread_data
            .as_ref()
            .and_then(|v| v.get("unread"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(unread, 1);

        // List messages
        let listed = client2
            .request_ok(Request::MessageList {
                session_id: worker_id.clone(),
                unread_only: true,
                limit: None,
            })
            .await?;
        let messages = listed.as_ref().and_then(|v| v.as_array()).unwrap();
        assert_eq!(messages.len(), 1);

        // Create workflow
        let wf_data = client2
            .request_ok(Request::WorkflowCreate {
                name: "wf".to_string(),
                root_session_id: root_id.clone(),
            })
            .await?;
        let wf_id = extract_str(&wf_data, &["workflow_id"]);

        // Join worker to workflow
        client2
            .request_ok(Request::WorkflowJoin {
                workflow_id: wf_id.clone(),
                session_id: worker_id.clone(),
                parent_session_id: root_id.clone(),
            })
            .await?;

        // Create task
        let task_data = client2
            .request_ok(Request::TaskCreate {
                workflow_id: wf_id.clone(),
                title: "Parent".to_string(),
                description: None,
                created_by: root_id.clone(),
                depends_on: vec![],
            })
            .await?;
        let task_id = extract_str(&task_data, &["task_id"]);

        // Set task status to done
        client2
            .request_ok(Request::TaskSetStatus {
                task_id: task_id.clone(),
                session_id: root_id.clone(),
                status: tmax_protocol::SharedTaskStatus::Done,
            })
            .await?;

        // List tasks
        let tasks_data = client2
            .request_ok(Request::TaskList {
                workflow_id: wf_id.clone(),
                session_id: worker_id.clone(),
                include_done: true,
            })
            .await?;
        let tasks = tasks_data.as_ref().and_then(|v| v.as_array()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].get("status").unwrap().as_str().unwrap(), "done");

        // Cleanup
        let _ = client2
            .request_ok(Request::SessionDestroy {
                session_id: root_id,
                cascade: false,
            })
            .await;
        let _ = client2
            .request_ok(Request::SessionDestroy {
                session_id: worker_id,
                cascade: false,
            })
            .await;

        let _ = shutdown.send(true);
        Ok(())
    }
}
