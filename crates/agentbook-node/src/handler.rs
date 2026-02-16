use agentbook::protocol::{
    Event, FollowInfo, HealthStatus, IdentityInfo, InboxEntry, Request, Response,
};
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::{InboxMessage, MessageType, NodeInbox};
use agentbook_mesh::transport::MeshTransport;
use agentbook_proto::mesh::v1 as mesh_pb;
use base64::Engine;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

/// Shared node state accessible by all client connections.
pub struct NodeState {
    pub identity: NodeIdentity,
    pub follow_store: Mutex<FollowStore>,
    pub inbox: Mutex<NodeInbox>,
    pub transport: Option<MeshTransport>,
    pub username: Mutex<Option<String>>,
    pub event_tx: broadcast::Sender<Event>,
}

impl NodeState {
    pub fn new(
        identity: NodeIdentity,
        follow_store: FollowStore,
        inbox: NodeInbox,
        transport: Option<MeshTransport>,
    ) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(256);
        Arc::new(Self {
            identity,
            follow_store: Mutex::new(follow_store),
            inbox: Mutex::new(inbox),
            transport,
            username: Mutex::new(None),
            event_tx,
        })
    }
}

/// Handle a single request from a client.
pub async fn handle_request(state: &Arc<NodeState>, req: Request) -> Response {
    match req {
        Request::Identity => handle_identity(state).await,
        Request::Health => handle_health(state).await,
        Request::Follow { target } => handle_follow(state, &target).await,
        Request::Unfollow { target } => handle_unfollow(state, &target).await,
        Request::Block { target } => handle_block(state, &target).await,
        Request::Following => handle_following(state).await,
        Request::Followers => handle_followers(state).await,
        Request::RegisterUsername { username } => handle_register_username(state, &username).await,
        Request::LookupUsername { username } => handle_lookup_username(state, &username).await,
        Request::SendDm { to, body } => handle_send_dm(state, &to, &body).await,
        Request::PostFeed { body } => handle_post_feed(state, &body).await,
        Request::Inbox { unread_only, limit } => handle_inbox(state, unread_only, limit).await,
        Request::InboxAck { message_id } => handle_inbox_ack(state, &message_id).await,
        Request::Shutdown => handle_shutdown().await,
    }
}

async fn handle_identity(state: &Arc<NodeState>) -> Response {
    let username = state.username.lock().await.clone();
    let info = IdentityInfo {
        node_id: state.identity.node_id.clone(),
        public_key_b64: state.identity.public_key_b64.clone(),
        username,
    };
    Response::Ok {
        data: Some(serde_json::to_value(info).unwrap()),
    }
}

async fn handle_health(state: &Arc<NodeState>) -> Response {
    let follow_store = state.follow_store.lock().await;
    let inbox = state.inbox.lock().await;
    let status = HealthStatus {
        healthy: true,
        relay_connected: state.transport.is_some(),
        following_count: follow_store.following().len(),
        unread_count: inbox.unread_count(),
    };
    Response::Ok {
        data: Some(serde_json::to_value(status).unwrap()),
    }
}

async fn handle_follow(state: &Arc<NodeState>, target: &str) -> Response {
    let mut follow_store = state.follow_store.lock().await;

    // For now, follow by node_id directly. Username resolution would go through relay.
    let record = agentbook_mesh::follow::FollowRecord {
        node_id: target.to_string(),
        public_key_b64: String::new(), // Will be filled in when we discover the peer
        username: None,
        relay_hints: vec![],
        followed_at_ms: now_ms(),
    };

    match follow_store.follow(record) {
        Ok(()) => ok_response(None),
        Err(e) => error_response("follow_failed", &e.to_string()),
    }
}

async fn handle_unfollow(state: &Arc<NodeState>, target: &str) -> Response {
    let mut follow_store = state.follow_store.lock().await;
    match follow_store.unfollow(target) {
        Ok(()) => ok_response(None),
        Err(e) => error_response("unfollow_failed", &e.to_string()),
    }
}

async fn handle_block(state: &Arc<NodeState>, target: &str) -> Response {
    let mut follow_store = state.follow_store.lock().await;
    match follow_store.block(target) {
        Ok(()) => ok_response(None),
        Err(e) => error_response("block_failed", &e.to_string()),
    }
}

async fn handle_following(state: &Arc<NodeState>) -> Response {
    let follow_store = state.follow_store.lock().await;
    let list: Vec<FollowInfo> = follow_store
        .following()
        .iter()
        .map(|f| FollowInfo {
            node_id: f.node_id.clone(),
            username: f.username.clone(),
            followed_at_ms: f.followed_at_ms,
        })
        .collect();
    Response::Ok {
        data: Some(serde_json::to_value(list).unwrap()),
    }
}

async fn handle_followers(_state: &Arc<NodeState>) -> Response {
    // For now, we don't track who follows us — that requires the other side to announce.
    // Return empty list; this will be populated when we implement follow notifications.
    let list: Vec<FollowInfo> = vec![];
    Response::Ok {
        data: Some(serde_json::to_value(list).unwrap()),
    }
}

async fn handle_register_username(_state: &Arc<NodeState>, _username: &str) -> Response {
    // TODO: call relay host's RegisterUsername RPC
    error_response(
        "not_implemented",
        "username registration not yet implemented",
    )
}

async fn handle_lookup_username(_state: &Arc<NodeState>, _username: &str) -> Response {
    // TODO: call relay host's LookupUsername RPC
    error_response("not_implemented", "username lookup not yet implemented")
}

async fn handle_send_dm(state: &Arc<NodeState>, to: &str, body: &str) -> Response {
    let transport = match &state.transport {
        Some(t) => t,
        None => return error_response("no_relay", "not connected to any relay"),
    };

    // Build envelope
    // TODO: proper ECDH encryption — for now, base64-encode the body as plaintext
    let msg_id = Uuid::new_v4().to_string();
    let b64 = base64::engine::general_purpose::STANDARD;
    let envelope = mesh_pb::Envelope {
        message_id: msg_id.clone(),
        from_node_id: state.identity.node_id.clone(),
        to_node_id: to.to_string(),
        from_public_key_b64: state.identity.public_key_b64.clone(),
        message_type: mesh_pb::MessageType::DmText as i32,
        ciphertext_b64: b64.encode(body.as_bytes()),
        nonce_b64: String::new(),
        signature_b64: state.identity.sign(body.as_bytes()).unwrap_or_default(),
        timestamp_ms: now_ms(),
        topic: None,
    };

    match transport.send_via_relay(envelope).await {
        Ok(()) => ok_response(Some(serde_json::json!({ "message_id": msg_id }))),
        Err(e) => error_response("send_failed", &e.to_string()),
    }
}

async fn handle_post_feed(state: &Arc<NodeState>, body: &str) -> Response {
    let transport = match &state.transport {
        Some(t) => t,
        None => return error_response("no_relay", "not connected to any relay"),
    };

    let follow_store = state.follow_store.lock().await;
    let followers = follow_store.following(); // For now, broadcast to all we follow

    let msg_id = Uuid::new_v4().to_string();

    // Send to each follower
    // TODO: proper per-follower encryption
    for follower in followers {
        // TODO: proper per-follower encryption
        let b64 = base64::engine::general_purpose::STANDARD;
        let envelope = mesh_pb::Envelope {
            message_id: msg_id.clone(),
            from_node_id: state.identity.node_id.clone(),
            to_node_id: follower.node_id.clone(),
            from_public_key_b64: state.identity.public_key_b64.clone(),
            message_type: mesh_pb::MessageType::FeedPost as i32,
            ciphertext_b64: b64.encode(body.as_bytes()),
            nonce_b64: String::new(),
            signature_b64: state.identity.sign(body.as_bytes()).unwrap_or_default(),
            timestamp_ms: now_ms(),
            topic: None,
        };

        if let Err(e) = transport.send_via_relay(envelope).await {
            tracing::warn!(to = %follower.node_id, err = %e, "failed to send feed post");
        }
    }

    ok_response(Some(serde_json::json!({ "message_id": msg_id })))
}

async fn handle_inbox(state: &Arc<NodeState>, unread_only: bool, limit: Option<usize>) -> Response {
    let inbox = state.inbox.lock().await;
    let messages: Vec<InboxEntry> = inbox
        .list(unread_only, limit)
        .into_iter()
        .map(|m| InboxEntry {
            message_id: m.message_id.clone(),
            from_node_id: m.from_node_id.clone(),
            from_username: None,
            message_type: format!("{:?}", m.message_type),
            body: m.body.clone(),
            timestamp_ms: m.timestamp_ms,
            acked: m.acked,
        })
        .collect();
    Response::Ok {
        data: Some(serde_json::to_value(messages).unwrap()),
    }
}

async fn handle_inbox_ack(state: &Arc<NodeState>, message_id: &str) -> Response {
    let mut inbox = state.inbox.lock().await;
    match inbox.ack(message_id) {
        Ok(true) => ok_response(None),
        Ok(false) => error_response("not_found", &format!("message {message_id} not found")),
        Err(e) => error_response("ack_failed", &e.to_string()),
    }
}

async fn handle_shutdown() -> Response {
    ok_response(None)
}

/// Process an inbound envelope from the relay into the inbox.
pub async fn process_inbound(state: &Arc<NodeState>, envelope: mesh_pb::Envelope) {
    // TODO: ingress validation (signature, follow check, rate limit)
    // TODO: decrypt message body

    let message_type = match mesh_pb::MessageType::try_from(envelope.message_type) {
        Ok(mesh_pb::MessageType::DmText) => MessageType::DmText,
        Ok(mesh_pb::MessageType::FeedPost) => MessageType::FeedPost,
        _ => MessageType::Unspecified,
    };

    let msg = InboxMessage {
        message_id: envelope.message_id.clone(),
        from_node_id: envelope.from_node_id.clone(),
        from_public_key_b64: envelope.from_public_key_b64.clone(),
        topic: None,
        body: {
            // TODO: proper ECDH decryption
            let b64 = base64::engine::general_purpose::STANDARD;
            b64.decode(&envelope.ciphertext_b64)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| envelope.ciphertext_b64.clone())
        },
        timestamp_ms: envelope.timestamp_ms,
        acked: false,
        message_type,
    };

    let preview = msg.body.chars().take(50).collect::<String>();
    let from = msg.from_node_id.clone();
    let msg_id = msg.message_id.clone();
    let msg_type_str = format!("{:?}", msg.message_type);

    let mut inbox = state.inbox.lock().await;
    if let Err(e) = inbox.push(msg) {
        tracing::error!(err = %e, "failed to store inbound message");
        return;
    }

    // Broadcast event to connected clients
    let _ = state.event_tx.send(Event::NewMessage {
        message_id: msg_id,
        from,
        message_type: msg_type_str,
        preview,
    });
}

fn ok_response(data: Option<serde_json::Value>) -> Response {
    Response::Ok { data }
}

fn error_response(code: &str, message: &str) -> Response {
    Response::Error {
        code: code.to_string(),
        message: message.to_string(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
