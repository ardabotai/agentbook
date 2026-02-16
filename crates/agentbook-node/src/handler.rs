use agentbook::protocol::{
    ContractReadResult, Event, FollowInfo, HealthStatus, IdentityInfo, InboxEntry, Request,
    Response, SignatureResult, TotpSetupInfo, TxResult, WalletInfo,
};
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::{InboxMessage, MessageType, NodeInbox};
use agentbook_mesh::transport::MeshTransport;
use agentbook_proto::mesh::v1 as mesh_pb;
use agentbook_wallet::wallet::{self, BaseWallet};
use alloy::primitives::Address;
use base64::Engine;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

/// Configuration for wallet features in the node.
pub struct WalletConfig {
    /// Base RPC URL.
    pub rpc_url: String,
    /// Whether yolo mode is enabled.
    pub yolo_enabled: bool,
    /// Node state directory (for TOTP, yolo key files).
    pub state_dir: PathBuf,
    /// Key encryption key derived from passphrase (for TOTP verification).
    pub kek: [u8; 32],
}

/// Shared node state accessible by all client connections.
pub struct NodeState {
    pub identity: NodeIdentity,
    pub follow_store: Mutex<FollowStore>,
    pub inbox: Mutex<NodeInbox>,
    pub transport: Option<MeshTransport>,
    pub username: Mutex<Option<String>>,
    pub event_tx: broadcast::Sender<Event>,
    /// Human wallet (node's own secp256k1 key).
    pub human_wallet: Mutex<Option<BaseWallet>>,
    /// Yolo wallet (separate hot wallet, no auth).
    pub yolo_wallet: Mutex<Option<BaseWallet>>,
    /// Wallet configuration.
    pub wallet: WalletConfig,
}

impl NodeState {
    pub fn new(
        identity: NodeIdentity,
        follow_store: FollowStore,
        inbox: NodeInbox,
        transport: Option<MeshTransport>,
        wallet: WalletConfig,
    ) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(256);
        Arc::new(Self {
            identity,
            follow_store: Mutex::new(follow_store),
            inbox: Mutex::new(inbox),
            transport,
            username: Mutex::new(None),
            event_tx,
            human_wallet: Mutex::new(None),
            yolo_wallet: Mutex::new(None),
            wallet,
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
        Request::WalletBalance { wallet } => handle_wallet_balance(state, &wallet).await,
        Request::SendEth { to, amount, otp } => handle_send_eth(state, &to, &amount, &otp).await,
        Request::SendUsdc { to, amount, otp } => handle_send_usdc(state, &to, &amount, &otp).await,
        Request::YoloSendEth { to, amount } => handle_yolo_send_eth(state, &to, &amount).await,
        Request::YoloSendUsdc { to, amount } => handle_yolo_send_usdc(state, &to, &amount).await,
        Request::SetupTotp => handle_setup_totp(state).await,
        Request::VerifyTotp { code } => handle_verify_totp(state, &code).await,
        Request::ReadContract {
            contract,
            abi,
            function,
            args,
        } => handle_read_contract(state, &contract, &abi, &function, &args).await,
        Request::WriteContract {
            contract,
            abi,
            function,
            args,
            value,
            otp,
        } => {
            handle_write_contract(
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
            handle_yolo_write_contract(state, &contract, &abi, &function, &args, value.as_deref())
                .await
        }
        Request::SignMessage { message, otp } => handle_sign_message(state, &message, &otp).await,
        Request::YoloSignMessage { message } => handle_yolo_sign_message(state, &message).await,
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

// ---------------------------------------------------------------------------
// Wallet handlers
// ---------------------------------------------------------------------------

async fn get_or_init_human_wallet(state: &Arc<NodeState>) -> Result<(), String> {
    let mut guard = state.human_wallet.lock().await;
    if guard.is_some() {
        return Ok(());
    }
    let key_bytes = state.identity.secret_key_bytes();
    match BaseWallet::new(&key_bytes, &state.wallet.rpc_url) {
        Ok(w) => {
            *guard = Some(w);
            Ok(())
        }
        Err(e) => Err(format!("failed to init human wallet: {e}")),
    }
}

async fn get_or_init_yolo_wallet(state: &Arc<NodeState>) -> Result<(), String> {
    if !state.wallet.yolo_enabled {
        return Err("yolo mode is not enabled — start with --yolo".to_string());
    }
    let mut guard = state.yolo_wallet.lock().await;
    if guard.is_some() {
        return Ok(());
    }
    let key_bytes = agentbook_wallet::yolo::load_yolo_key(&state.wallet.state_dir)
        .map_err(|e| format!("failed to load yolo key: {e}"))?;
    match BaseWallet::new(&key_bytes, &state.wallet.rpc_url) {
        Ok(w) => {
            *guard = Some(w);
            Ok(())
        }
        Err(e) => Err(format!("failed to init yolo wallet: {e}")),
    }
}

async fn handle_wallet_balance(state: &Arc<NodeState>, wallet_type: &str) -> Response {
    match wallet_type {
        "human" => {
            if let Err(e) = get_or_init_human_wallet(state).await {
                return error_response("wallet_error", &e);
            }
            let guard = state.human_wallet.lock().await;
            let w = guard.as_ref().unwrap();
            match (w.get_eth_balance().await, w.get_usdc_balance().await) {
                (Ok(eth), Ok(usdc)) => {
                    let info = WalletInfo {
                        address: format!("{:#x}", w.address()),
                        eth_balance: wallet::format_eth(eth),
                        usdc_balance: wallet::format_usdc(usdc),
                        wallet_type: "human".to_string(),
                    };
                    ok_response(Some(serde_json::to_value(info).unwrap()))
                }
                (Err(e), _) | (_, Err(e)) => {
                    error_response("balance_error", &format!("failed to fetch balance: {e}"))
                }
            }
        }
        "yolo" => {
            if let Err(e) = get_or_init_yolo_wallet(state).await {
                return error_response("wallet_error", &e);
            }
            let guard = state.yolo_wallet.lock().await;
            let w = guard.as_ref().unwrap();
            match (w.get_eth_balance().await, w.get_usdc_balance().await) {
                (Ok(eth), Ok(usdc)) => {
                    let info = WalletInfo {
                        address: format!("{:#x}", w.address()),
                        eth_balance: wallet::format_eth(eth),
                        usdc_balance: wallet::format_usdc(usdc),
                        wallet_type: "yolo".to_string(),
                    };
                    ok_response(Some(serde_json::to_value(info).unwrap()))
                }
                (Err(e), _) | (_, Err(e)) => {
                    error_response("balance_error", &format!("failed to fetch balance: {e}"))
                }
            }
        }
        _ => error_response("invalid_wallet", "wallet must be 'human' or 'yolo'"),
    }
}

async fn handle_send_eth(state: &Arc<NodeState>, to: &str, amount: &str, otp: &str) -> Response {
    // Verify TOTP
    match agentbook_wallet::totp::verify_totp(&state.wallet.state_dir, otp, &state.wallet.kek) {
        Ok(true) => {}
        Ok(false) => return error_response("invalid_otp", "invalid authenticator code"),
        Err(e) => return error_response("totp_error", &format!("TOTP verification failed: {e}")),
    }

    if let Err(e) = get_or_init_human_wallet(state).await {
        return error_response("wallet_error", &e);
    }

    let to_addr: Address = match to.parse() {
        Ok(a) => a,
        Err(e) => return error_response("invalid_address", &format!("invalid address: {e}")),
    };

    let amount_wei = match wallet::parse_eth_amount(amount) {
        Ok(a) => a,
        Err(e) => return error_response("invalid_amount", &format!("invalid amount: {e}")),
    };

    let guard = state.human_wallet.lock().await;
    let w = guard.as_ref().unwrap();
    match w.send_eth(to_addr, amount_wei).await {
        Ok(tx_hash) => {
            let result = TxResult {
                tx_hash: format!("{tx_hash:#x}"),
                explorer_url: wallet::explorer_url(&tx_hash),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("send_failed", &format!("ETH send failed: {e}")),
    }
}

async fn handle_send_usdc(state: &Arc<NodeState>, to: &str, amount: &str, otp: &str) -> Response {
    // Verify TOTP
    match agentbook_wallet::totp::verify_totp(&state.wallet.state_dir, otp, &state.wallet.kek) {
        Ok(true) => {}
        Ok(false) => return error_response("invalid_otp", "invalid authenticator code"),
        Err(e) => return error_response("totp_error", &format!("TOTP verification failed: {e}")),
    }

    if let Err(e) = get_or_init_human_wallet(state).await {
        return error_response("wallet_error", &e);
    }

    let to_addr: Address = match to.parse() {
        Ok(a) => a,
        Err(e) => return error_response("invalid_address", &format!("invalid address: {e}")),
    };

    let amount_units = match wallet::parse_usdc_amount(amount) {
        Ok(a) => a,
        Err(e) => return error_response("invalid_amount", &format!("invalid amount: {e}")),
    };

    let guard = state.human_wallet.lock().await;
    let w = guard.as_ref().unwrap();
    match w.send_usdc(to_addr, amount_units).await {
        Ok(tx_hash) => {
            let result = TxResult {
                tx_hash: format!("{tx_hash:#x}"),
                explorer_url: wallet::explorer_url(&tx_hash),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("send_failed", &format!("USDC send failed: {e}")),
    }
}

async fn handle_yolo_send_eth(state: &Arc<NodeState>, to: &str, amount: &str) -> Response {
    if let Err(e) = get_or_init_yolo_wallet(state).await {
        return error_response("wallet_error", &e);
    }

    let to_addr: Address = match to.parse() {
        Ok(a) => a,
        Err(e) => return error_response("invalid_address", &format!("invalid address: {e}")),
    };

    let amount_wei = match wallet::parse_eth_amount(amount) {
        Ok(a) => a,
        Err(e) => return error_response("invalid_amount", &format!("invalid amount: {e}")),
    };

    let guard = state.yolo_wallet.lock().await;
    let w = guard.as_ref().unwrap();
    match w.send_eth(to_addr, amount_wei).await {
        Ok(tx_hash) => {
            let result = TxResult {
                tx_hash: format!("{tx_hash:#x}"),
                explorer_url: wallet::explorer_url(&tx_hash),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("send_failed", &format!("ETH send failed: {e}")),
    }
}

async fn handle_yolo_send_usdc(state: &Arc<NodeState>, to: &str, amount: &str) -> Response {
    if let Err(e) = get_or_init_yolo_wallet(state).await {
        return error_response("wallet_error", &e);
    }

    let to_addr: Address = match to.parse() {
        Ok(a) => a,
        Err(e) => return error_response("invalid_address", &format!("invalid address: {e}")),
    };

    let amount_units = match wallet::parse_usdc_amount(amount) {
        Ok(a) => a,
        Err(e) => return error_response("invalid_amount", &format!("invalid amount: {e}")),
    };

    let guard = state.yolo_wallet.lock().await;
    let w = guard.as_ref().unwrap();
    match w.send_usdc(to_addr, amount_units).await {
        Ok(tx_hash) => {
            let result = TxResult {
                tx_hash: format!("{tx_hash:#x}"),
                explorer_url: wallet::explorer_url(&tx_hash),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("send_failed", &format!("USDC send failed: {e}")),
    }
}

async fn handle_setup_totp(state: &Arc<NodeState>) -> Response {
    if agentbook_wallet::totp::has_totp(&state.wallet.state_dir) {
        return error_response("already_configured", "TOTP is already configured");
    }

    match agentbook_wallet::totp::generate_totp_secret(
        &state.wallet.state_dir,
        &state.wallet.kek,
        &state.identity.node_id,
    ) {
        Ok(setup) => {
            let info = TotpSetupInfo {
                secret_base32: setup.secret_base32,
                otpauth_url: setup.otpauth_url,
            };
            ok_response(Some(serde_json::to_value(info).unwrap()))
        }
        Err(e) => error_response("setup_failed", &format!("TOTP setup failed: {e}")),
    }
}

async fn handle_verify_totp(state: &Arc<NodeState>, code: &str) -> Response {
    match agentbook_wallet::totp::verify_totp(&state.wallet.state_dir, code, &state.wallet.kek) {
        Ok(true) => ok_response(Some(serde_json::json!({ "verified": true }))),
        Ok(false) => error_response("invalid_code", "invalid authenticator code"),
        Err(e) => error_response("verify_failed", &format!("verification failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Contract & signing handlers
// ---------------------------------------------------------------------------

async fn handle_read_contract(
    state: &Arc<NodeState>,
    contract: &str,
    abi: &str,
    function: &str,
    args: &[serde_json::Value],
) -> Response {
    let address: Address = match contract.parse() {
        Ok(a) => a,
        Err(e) => {
            return error_response("invalid_address", &format!("invalid contract address: {e}"));
        }
    };

    match agentbook_wallet::contract::read_contract(
        &state.wallet.rpc_url,
        address,
        abi,
        function,
        args,
    )
    .await
    {
        Ok(result) => {
            let data = ContractReadResult { result };
            ok_response(Some(serde_json::to_value(data).unwrap()))
        }
        Err(e) => error_response("contract_error", &format!("read_contract failed: {e}")),
    }
}

async fn handle_write_contract(
    state: &Arc<NodeState>,
    contract: &str,
    abi: &str,
    function: &str,
    args: &[serde_json::Value],
    value: Option<&str>,
    otp: &str,
) -> Response {
    // Verify TOTP
    match agentbook_wallet::totp::verify_totp(&state.wallet.state_dir, otp, &state.wallet.kek) {
        Ok(true) => {}
        Ok(false) => return error_response("invalid_otp", "invalid authenticator code"),
        Err(e) => return error_response("totp_error", &format!("TOTP verification failed: {e}")),
    }

    if let Err(e) = get_or_init_human_wallet(state).await {
        return error_response("wallet_error", &e);
    }

    let address: Address = match contract.parse() {
        Ok(a) => a,
        Err(e) => {
            return error_response("invalid_address", &format!("invalid contract address: {e}"));
        }
    };

    let eth_value = match value {
        Some(v) => match wallet::parse_eth_amount(v) {
            Ok(a) => Some(a),
            Err(e) => return error_response("invalid_value", &format!("invalid ETH value: {e}")),
        },
        None => None,
    };

    let guard = state.human_wallet.lock().await;
    let w = guard.as_ref().unwrap();
    match agentbook_wallet::contract::write_contract(w, address, abi, function, args, eth_value)
        .await
    {
        Ok(tx_hash) => {
            let result = TxResult {
                tx_hash: format!("{tx_hash:#x}"),
                explorer_url: wallet::explorer_url(&tx_hash),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("contract_error", &format!("write_contract failed: {e}")),
    }
}

async fn handle_yolo_write_contract(
    state: &Arc<NodeState>,
    contract: &str,
    abi: &str,
    function: &str,
    args: &[serde_json::Value],
    value: Option<&str>,
) -> Response {
    if let Err(e) = get_or_init_yolo_wallet(state).await {
        return error_response("wallet_error", &e);
    }

    let address: Address = match contract.parse() {
        Ok(a) => a,
        Err(e) => {
            return error_response("invalid_address", &format!("invalid contract address: {e}"));
        }
    };

    let eth_value = match value {
        Some(v) => match wallet::parse_eth_amount(v) {
            Ok(a) => Some(a),
            Err(e) => return error_response("invalid_value", &format!("invalid ETH value: {e}")),
        },
        None => None,
    };

    let guard = state.yolo_wallet.lock().await;
    let w = guard.as_ref().unwrap();
    match agentbook_wallet::contract::write_contract(w, address, abi, function, args, eth_value)
        .await
    {
        Ok(tx_hash) => {
            let result = TxResult {
                tx_hash: format!("{tx_hash:#x}"),
                explorer_url: wallet::explorer_url(&tx_hash),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("contract_error", &format!("write_contract failed: {e}")),
    }
}

async fn handle_sign_message(state: &Arc<NodeState>, message: &str, otp: &str) -> Response {
    // Verify TOTP
    match agentbook_wallet::totp::verify_totp(&state.wallet.state_dir, otp, &state.wallet.kek) {
        Ok(true) => {}
        Ok(false) => return error_response("invalid_otp", "invalid authenticator code"),
        Err(e) => return error_response("totp_error", &format!("TOTP verification failed: {e}")),
    }

    if let Err(e) = get_or_init_human_wallet(state).await {
        return error_response("wallet_error", &e);
    }

    let msg_bytes = parse_message_bytes(message);

    let guard = state.human_wallet.lock().await;
    let w = guard.as_ref().unwrap();
    match w.sign_message(&msg_bytes) {
        Ok(sig) => {
            let result = SignatureResult {
                signature: sig,
                address: format!("{:#x}", w.address()),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("sign_error", &format!("signing failed: {e}")),
    }
}

async fn handle_yolo_sign_message(state: &Arc<NodeState>, message: &str) -> Response {
    if let Err(e) = get_or_init_yolo_wallet(state).await {
        return error_response("wallet_error", &e);
    }

    let msg_bytes = parse_message_bytes(message);

    let guard = state.yolo_wallet.lock().await;
    let w = guard.as_ref().unwrap();
    match w.sign_message(&msg_bytes) {
        Ok(sig) => {
            let result = SignatureResult {
                signature: sig,
                address: format!("{:#x}", w.address()),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("sign_error", &format!("signing failed: {e}")),
    }
}

/// Parse a message string: if it starts with 0x, treat as hex bytes; otherwise UTF-8.
fn parse_message_bytes(message: &str) -> Vec<u8> {
    if let Some(hex) = message.strip_prefix("0x") {
        alloy::hex::decode(hex).unwrap_or_else(|_| message.as_bytes().to_vec())
    } else {
        message.as_bytes().to_vec()
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
