pub mod messaging;
pub mod rooms;
pub mod social;
pub mod username_cache;
pub mod wallet;

use agentbook::protocol::{Event, MessageType, Request, Response};
use agentbook_crypto::rate_limit::RateLimiter;
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::{InboxMessage, MessageType as MeshMessageType, NodeInbox};
use agentbook_mesh::ingress::{IngressPolicy, IngressRequest, IngressResult};
use agentbook_mesh::transport::MeshTransport;
use agentbook_proto::host::v1::host_service_client::HostServiceClient;
use agentbook_proto::mesh::v1 as mesh_pb;
use agentbook_wallet::spending_limit::{SpendingLimitConfig, SpendingLimiter};
use agentbook_wallet::wallet::BaseWallet;
use alloy::providers::RootProvider;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::{Mutex, broadcast};
use tonic::transport::Channel;
use zeroize::Zeroizing;

/// Configuration for wallet features in the node.
pub struct WalletConfig {
    /// Base RPC URL.
    pub rpc_url: String,
    /// Whether yolo mode is enabled.
    pub yolo_enabled: bool,
    /// Node state directory (for TOTP, yolo key files).
    pub state_dir: PathBuf,
    /// Key encryption key derived from passphrase (for TOTP verification).
    /// Wrapped in `Zeroizing` so it is wiped from memory when dropped.
    pub kek: Zeroizing<[u8; 32]>,
    /// Spending limits for the yolo wallet.
    pub spending_limit_config: SpendingLimitConfig,
}

/// Shared node state accessible by all client connections.
pub struct NodeState {
    pub identity: NodeIdentity,
    pub follow_store: Mutex<FollowStore>,
    pub inbox: Mutex<NodeInbox>,
    pub transport: Option<MeshTransport>,
    pub username: Mutex<Option<String>>,
    /// Relay host addresses (for unary RPCs like username registration).
    pub relay_hosts: Vec<String>,
    pub event_tx: broadcast::Sender<Event>,
    /// Human wallet (node's own secp256k1 key), lazily initialized.
    pub human_wallet: OnceLock<BaseWallet>,
    /// Yolo wallet (separate hot wallet, no auth), lazily initialized.
    pub yolo_wallet: OnceLock<BaseWallet>,
    /// Wallet configuration.
    pub wallet: WalletConfig,
    /// Spending limiter for yolo wallet transactions.
    pub spending_limiter: Mutex<SpendingLimiter>,
    /// Rate limiter for inbound message ingress validation.
    pub rate_limiter: Mutex<RateLimiter>,
    /// Joined rooms: room name → config (includes optional encryption key).
    pub rooms: Mutex<HashMap<String, rooms::RoomConfig>>,
    /// Per-room send cooldown tracking.
    pub room_cooldowns: Mutex<HashMap<String, Instant>>,
    /// Local cache of node_id → username (persisted, populated from follows + relay lookups).
    pub username_cache: Mutex<username_cache::UsernameCache>,
    /// Cached gRPC clients per relay host endpoint (reused across requests).
    grpc_clients: Mutex<HashMap<String, HostServiceClient<Channel>>>,
    /// Cached read-only blockchain provider for contract reads.
    read_provider: OnceLock<RootProvider>,
}

impl NodeState {
    pub fn new(
        identity: NodeIdentity,
        follow_store: FollowStore,
        inbox: NodeInbox,
        transport: Option<MeshTransport>,
        relay_hosts: Vec<String>,
        wallet: WalletConfig,
    ) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(256);
        let spending_limiter = SpendingLimiter::new(wallet.spending_limit_config.clone());
        // Ingress rate limiter: burst of 20 messages, sustained 2/sec per sender.
        let rate_limiter = RateLimiter::new(20, 2.0);
        // Load username cache and seed from follow records with known usernames
        let mut cache = username_cache::UsernameCache::load(&wallet.state_dir);
        cache.seed_from_follows(
            follow_store
                .following()
                .iter()
                .filter_map(|f| f.username.as_deref().map(|u| (f.node_id.as_str(), u))),
        );

        Arc::new(Self {
            identity,
            follow_store: Mutex::new(follow_store),
            inbox: Mutex::new(inbox),
            transport,
            username: Mutex::new(None),
            relay_hosts,
            event_tx,
            human_wallet: OnceLock::new(),
            yolo_wallet: OnceLock::new(),
            wallet,
            spending_limiter: Mutex::new(spending_limiter),
            rate_limiter: Mutex::new(rate_limiter),
            rooms: Mutex::new(HashMap::new()),
            room_cooldowns: Mutex::new(HashMap::new()),
            grpc_clients: Mutex::new(HashMap::new()),
            read_provider: OnceLock::new(),
            username_cache: Mutex::new(cache),
        })
    }

    /// Get or create a cached gRPC `HostServiceClient` for the given relay host.
    /// Returns a cloned client (gRPC clients are cheap to clone -- they share the
    /// underlying HTTP/2 connection).
    pub async fn get_grpc_client(
        &self,
        host: &str,
    ) -> Result<HostServiceClient<Channel>, tonic::transport::Error> {
        let mut clients = self.grpc_clients.lock().await;
        if let Some(client) = clients.get(host) {
            return Ok(client.clone());
        }

        let endpoint = agentbook_mesh::transport::relay_endpoint(host);

        let client = HostServiceClient::connect(endpoint).await?;
        clients.insert(host.to_string(), client.clone());
        Ok(client)
    }

    /// Get the cached read-only blockchain provider, creating it on first use.
    pub fn get_read_provider(&self) -> Result<&RootProvider, String> {
        if let Some(p) = self.read_provider.get() {
            return Ok(p);
        }
        let provider = agentbook_wallet::contract::create_read_provider(&self.wallet.rpc_url)
            .map_err(|e| format!("failed to create read provider: {e}"))?;
        // If another thread beat us, that's fine -- just use whichever won.
        let _ = self.read_provider.set(provider);
        self.read_provider
            .get()
            .ok_or_else(|| "read_provider not set".to_string())
    }
}

/// Handle a single request from a client.
pub async fn handle_request(state: &Arc<NodeState>, req: Request) -> Response {
    match req {
        // Social / identity
        Request::Identity => social::handle_identity(state).await,
        Request::Health => social::handle_health(state).await,
        Request::Follow { target } => social::handle_follow(state, &target).await,
        Request::Unfollow { target } => social::handle_unfollow(state, &target).await,
        Request::Block { target } => social::handle_block(state, &target).await,
        Request::Following => social::handle_following(state).await,
        Request::Followers => social::handle_followers(state).await,
        Request::RegisterUsername { username } => {
            social::handle_register_username(state, &username).await
        }
        Request::LookupUsername { username } => {
            social::handle_lookup_username(state, &username).await
        }
        Request::LookupNodeId { node_id } => {
            social::handle_lookup_node_id(state, &node_id).await
        }
        Request::SyncPush { confirm } => social::handle_sync_push(state, confirm).await,
        Request::SyncPull { confirm } => social::handle_sync_pull(state, confirm).await,

        // Rooms
        Request::JoinRoom { room, passphrase } => {
            rooms::handle_join_room(state, &room, passphrase.as_deref()).await
        }
        Request::LeaveRoom { room } => rooms::handle_leave_room(state, &room).await,
        Request::SendRoom { room, body } => rooms::handle_send_room(state, &room, &body).await,
        Request::RoomInbox { room, limit } => rooms::handle_room_inbox(state, &room, limit).await,
        Request::ListRooms => rooms::handle_list_rooms(state).await,

        // Messaging
        Request::SendDm { to, body } => messaging::handle_send_dm(state, &to, &body).await,
        Request::PostFeed { body } => messaging::handle_post_feed(state, &body).await,
        Request::Inbox { unread_only, limit } => {
            messaging::handle_inbox(state, unread_only, limit).await
        }
        Request::InboxAck { message_id } => messaging::handle_inbox_ack(state, &message_id).await,

        // Wallet
        Request::WalletBalance { wallet: w } => wallet::handle_wallet_balance(state, w).await,
        Request::SendEth { to, amount, otp } => {
            wallet::handle_send_eth(state, &to, &amount, &otp).await
        }
        Request::SendUsdc { to, amount, otp } => {
            wallet::handle_send_usdc(state, &to, &amount, &otp).await
        }
        Request::YoloSendEth { to, amount } => {
            wallet::handle_yolo_send_eth(state, &to, &amount).await
        }
        Request::YoloSendUsdc { to, amount } => {
            wallet::handle_yolo_send_usdc(state, &to, &amount).await
        }
        Request::SetupTotp => wallet::handle_setup_totp(state).await,
        Request::VerifyTotp { code } => wallet::handle_verify_totp(state, &code).await,
        Request::ReadContract {
            contract,
            abi,
            function,
            args,
        } => wallet::handle_read_contract(state, &contract, &abi, &function, &args).await,
        Request::WriteContract {
            contract,
            abi,
            function,
            args,
            value,
            otp,
        } => {
            wallet::handle_write_contract(
                state,
                &contract,
                &abi,
                &function,
                &args,
                value.as_deref(),
                &otp,
            )
            .await
        }
        Request::YoloWriteContract {
            contract,
            abi,
            function,
            args,
            value,
        } => {
            wallet::handle_yolo_write_contract(
                state,
                &contract,
                &abi,
                &function,
                &args,
                value.as_deref(),
            )
            .await
        }
        Request::SignMessage { message, otp } => {
            wallet::handle_sign_message(state, &message, &otp).await
        }
        Request::YoloSignMessage { message } => {
            wallet::handle_yolo_sign_message(state, &message).await
        }
        Request::Shutdown => handle_shutdown().await,
    }
}

async fn handle_shutdown() -> Response {
    ok_response(None)
}

/// Process an inbound envelope from the relay into the inbox.
pub async fn process_inbound(state: &Arc<NodeState>, envelope: mesh_pb::Envelope) {
    let mesh_msg_type = match mesh_pb::MessageType::try_from(envelope.message_type) {
        Ok(mesh_pb::MessageType::DmText) => MeshMessageType::DmText,
        Ok(mesh_pb::MessageType::FeedPost) => MeshMessageType::FeedPost,
        Ok(mesh_pb::MessageType::RoomMessage) => MeshMessageType::RoomMessage,
        Ok(mesh_pb::MessageType::RoomJoin) => MeshMessageType::RoomJoin,
        _ => MeshMessageType::Unspecified,
    };

    // Route room messages and join events to the rooms handler.
    if mesh_msg_type == MeshMessageType::RoomMessage
        || mesh_msg_type == MeshMessageType::RoomJoin
    {
        rooms::process_inbound_room(state, envelope).await;
        return;
    }

    // Ingress validation: signature, blocked, follow graph, rate limit.
    {
        let follow_store = state.follow_store.lock().await;
        let mut rate_limiter = state.rate_limiter.lock().await;
        let mut policy = IngressPolicy::new(&follow_store, &mut rate_limiter);

        let req = IngressRequest {
            from_node_id: &envelope.from_node_id,
            from_public_key_b64: &envelope.from_public_key_b64,
            payload: envelope.ciphertext_b64.as_bytes(),
            signature_b64: &envelope.signature_b64,
            my_node_id: &state.identity.node_id,
            message_type: mesh_msg_type,
        };

        if let IngressResult::Reject(reason) = policy.check(&req) {
            tracing::warn!(
                from = %envelope.from_node_id,
                msg_id = %envelope.message_id,
                reason = %reason,
                "ingress rejected"
            );
            return;
        }
    }

    // Decrypt the message body using ECDH shared key
    let body = match messaging::decrypt_envelope(&state.identity, &envelope, mesh_msg_type) {
        Ok(plaintext) => plaintext,
        Err(e) => {
            tracing::warn!(
                from = %envelope.from_node_id,
                msg_id = %envelope.message_id,
                err = %e,
                "failed to decrypt inbound message, storing raw"
            );
            // Fallback: store the ciphertext_b64 as-is so the message is not lost
            envelope.ciphertext_b64.clone()
        }
    };

    let msg = InboxMessage {
        message_id: envelope.message_id.clone(),
        from_node_id: envelope.from_node_id.clone(),
        from_public_key_b64: envelope.from_public_key_b64.clone(),
        topic: None,
        body,
        timestamp_ms: envelope.timestamp_ms,
        acked: false,
        message_type: mesh_msg_type,
    };

    let preview = msg.body.chars().take(50).collect::<String>();
    let from = msg.from_node_id.clone();
    let msg_id = msg.message_id.clone();
    let protocol_msg_type = to_protocol_message_type(msg.message_type);

    let mut inbox = state.inbox.lock().await;
    if let Err(e) = inbox.push(msg) {
        tracing::error!(err = %e, "failed to store inbound message");
        return;
    }

    // Broadcast event to connected clients
    let _ = state.event_tx.send(Event::NewMessage {
        message_id: msg_id,
        from,
        message_type: protocol_msg_type,
        preview,
    });
}

// ---- Shared helpers ----

/// Convert mesh-layer `MessageType` to protocol-layer `MessageType`.
pub fn to_protocol_message_type(mt: MeshMessageType) -> MessageType {
    match mt {
        MeshMessageType::Unspecified => MessageType::Unspecified,
        MeshMessageType::DmText => MessageType::DmText,
        MeshMessageType::FeedPost => MessageType::FeedPost,
        MeshMessageType::RoomMessage => MessageType::RoomMessage,
        MeshMessageType::RoomJoin => MessageType::RoomJoin,
    }
}

pub fn ok_response(data: Option<serde_json::Value>) -> Response {
    Response::Ok { data }
}

pub fn error_response(code: &str, message: &str) -> Response {
    Response::Error {
        code: code.to_string(),
        message: message.to_string(),
    }
}

pub use agentbook_crypto::time::now_ms;

#[cfg(test)]
mod tests;
