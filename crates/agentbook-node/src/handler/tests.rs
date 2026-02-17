use super::*;
use agentbook::protocol::{
    FollowInfo, HealthStatus, IdentityInfo, InboxEntry, MessageType, Request, Response,
    TotpSetupInfo, WalletType as ProtoWalletType,
};
use agentbook_mesh::crypto::{encrypt_with_key, random_key_material};
use agentbook_mesh::follow::{FollowRecord, FollowStore};
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::NodeInbox;
use agentbook_proto::mesh::v1 as mesh_pb;
use agentbook_wallet::spending_limit::SpendingLimitConfig;
use base64::Engine;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Create a test NodeState with no relay transport and yolo disabled.
fn make_test_state() -> (Arc<NodeState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().to_path_buf();
    let kek = random_key_material();
    let identity = NodeIdentity::load_or_create(&state_dir, &kek).unwrap();
    let follow_store = FollowStore::load(&state_dir).unwrap();
    let inbox = NodeInbox::load(&state_dir).unwrap();

    let wallet_config = WalletConfig {
        rpc_url: "https://mainnet.base.org".to_string(),
        yolo_enabled: false,
        state_dir,
        kek: Zeroizing::new(kek),
        spending_limit_config: SpendingLimitConfig::default(),
    };

    let state = NodeState::new(identity, follow_store, inbox, None, vec![], wallet_config);
    (state, dir)
}

/// Create a test NodeState with yolo enabled (but no key file on disk).
fn make_test_state_yolo_enabled() -> (Arc<NodeState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().to_path_buf();
    let kek = random_key_material();
    let identity = NodeIdentity::load_or_create(&state_dir, &kek).unwrap();
    let follow_store = FollowStore::load(&state_dir).unwrap();
    let inbox = NodeInbox::load(&state_dir).unwrap();

    let wallet_config = WalletConfig {
        rpc_url: "https://mainnet.base.org".to_string(),
        yolo_enabled: true,
        state_dir,
        kek: Zeroizing::new(kek),
        spending_limit_config: SpendingLimitConfig::default(),
    };

    let state = NodeState::new(identity, follow_store, inbox, None, vec![], wallet_config);
    (state, dir)
}

fn assert_ok(resp: &Response) -> Option<serde_json::Value> {
    match resp {
        Response::Ok { data } => data.clone(),
        other => panic!("expected Response::Ok, got: {other:?}"),
    }
}

fn assert_error(resp: &Response, expected_code: &str) -> String {
    match resp {
        Response::Error { code, message } => {
            assert_eq!(
                code, expected_code,
                "unexpected error code, message: {message}"
            );
            message.clone()
        }
        other => panic!("expected Response::Error({expected_code}), got: {other:?}"),
    }
}

fn assert_any_error(resp: &Response) -> (String, String) {
    match resp {
        Response::Error { code, message } => (code.clone(), message.clone()),
        other => panic!("expected Response::Error, got: {other:?}"),
    }
}

/// Create a properly encrypted DM envelope from a sender identity to the
/// test node's identity using ECDH + ChaCha20-Poly1305, with a valid signature.
fn make_encrypted_dm_envelope(
    sender: &NodeIdentity,
    recipient: &NodeIdentity,
    msg_id: &str,
    body: &str,
) -> mesh_pb::Envelope {
    let shared_key = sender.derive_shared_key(&recipient.public_key);
    let (ciphertext_b64, nonce_b64) = encrypt_with_key(&shared_key, body.as_bytes()).unwrap();
    let signature_b64 = sender.sign(ciphertext_b64.as_bytes()).unwrap();

    mesh_pb::Envelope {
        message_id: msg_id.into(),
        from_node_id: sender.node_id.clone(),
        to_node_id: recipient.node_id.clone(),
        from_public_key_b64: sender.public_key_b64.clone(),
        message_type: mesh_pb::MessageType::DmText as i32,
        ciphertext_b64,
        nonce_b64,
        signature_b64,
        timestamp_ms: 12345,
        topic: None,
    }
}

/// Add a sender as a followed node so ingress validation passes for DMs.
async fn follow_sender(state: &Arc<NodeState>, sender: &NodeIdentity) {
    let record = FollowRecord {
        node_id: sender.node_id.clone(),
        public_key_b64: sender.public_key_b64.clone(),
        username: None,
        relay_hints: vec![],
        followed_at_ms: now_ms(),
    };
    state.follow_store.lock().await.follow(record).unwrap();
}

/// Create a second identity for use as a sender in encryption tests.
fn make_sender_identity() -> (NodeIdentity, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kek = random_key_material();
    let identity = NodeIdentity::load_or_create(dir.path(), &kek).unwrap();
    (identity, dir)
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn identity_returns_node_info() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(&state, Request::Identity).await;
    let data = assert_ok(&resp).expect("identity should return data");
    let info: IdentityInfo = serde_json::from_value(data).unwrap();
    assert!(!info.node_id.is_empty());
    assert!(!info.public_key_b64.is_empty());
    assert!(info.username.is_none());
}

#[tokio::test]
async fn identity_includes_username_when_set() {
    let (state, _dir) = make_test_state();
    *state.username.lock().await = Some("alice".to_string());
    let resp = handle_request(&state, Request::Identity).await;
    let data = assert_ok(&resp).unwrap();
    let info: IdentityInfo = serde_json::from_value(data).unwrap();
    assert_eq!(info.username.as_deref(), Some("alice"));
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_no_relay() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(&state, Request::Health).await;
    let data = assert_ok(&resp).unwrap();
    let status: HealthStatus = serde_json::from_value(data).unwrap();
    assert!(status.healthy);
    assert!(!status.relay_connected);
    assert_eq!(status.following_count, 0);
    assert_eq!(status.unread_count, 0);
}

// ---------------------------------------------------------------------------
// Follow / Unfollow / Block
// ---------------------------------------------------------------------------

#[tokio::test]
async fn follow_and_following_list() {
    let (state, _dir) = make_test_state();

    let resp = handle_request(
        &state,
        Request::Follow {
            target: "node-a".into(),
        },
    )
    .await;
    assert_ok(&resp);
    let resp = handle_request(
        &state,
        Request::Follow {
            target: "node-b".into(),
        },
    )
    .await;
    assert_ok(&resp);

    let resp = handle_request(&state, Request::Following).await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<FollowInfo> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 2);
    let ids: Vec<&str> = list.iter().map(|f| f.node_id.as_str()).collect();
    assert!(ids.contains(&"node-a"));
    assert!(ids.contains(&"node-b"));

    // Health reflects following count
    let resp = handle_request(&state, Request::Health).await;
    let data = assert_ok(&resp).unwrap();
    let status: HealthStatus = serde_json::from_value(data).unwrap();
    assert_eq!(status.following_count, 2);
}

#[tokio::test]
async fn follow_deduplicates() {
    let (state, _dir) = make_test_state();
    handle_request(
        &state,
        Request::Follow {
            target: "node-a".into(),
        },
    )
    .await;
    handle_request(
        &state,
        Request::Follow {
            target: "node-a".into(),
        },
    )
    .await;

    let resp = handle_request(&state, Request::Following).await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<FollowInfo> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 1);
}

#[tokio::test]
async fn unfollow_succeeds() {
    let (state, _dir) = make_test_state();
    handle_request(
        &state,
        Request::Follow {
            target: "node-a".into(),
        },
    )
    .await;

    let resp = handle_request(
        &state,
        Request::Unfollow {
            target: "node-a".into(),
        },
    )
    .await;
    assert_ok(&resp);

    let resp = handle_request(&state, Request::Following).await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<FollowInfo> = serde_json::from_value(data).unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn unfollow_nonexistent_fails() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::Unfollow {
            target: "nobody".into(),
        },
    )
    .await;
    assert_error(&resp, "unfollow_failed");
}

#[tokio::test]
async fn block_removes_follow() {
    let (state, _dir) = make_test_state();
    handle_request(
        &state,
        Request::Follow {
            target: "node-a".into(),
        },
    )
    .await;

    let resp = handle_request(
        &state,
        Request::Block {
            target: "node-a".into(),
        },
    )
    .await;
    assert_ok(&resp);

    let resp = handle_request(&state, Request::Following).await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<FollowInfo> = serde_json::from_value(data).unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn block_then_follow_unblocks() {
    let (state, _dir) = make_test_state();
    handle_request(
        &state,
        Request::Block {
            target: "node-a".into(),
        },
    )
    .await;
    handle_request(
        &state,
        Request::Follow {
            target: "node-a".into(),
        },
    )
    .await;

    let resp = handle_request(&state, Request::Following).await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<FollowInfo> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].node_id, "node-a");
}

// ---------------------------------------------------------------------------
// Followers (requires relay)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn followers_requires_relay() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(&state, Request::Followers).await;
    // Without a relay connection, followers returns an error
    assert_any_error(&resp);
}

// ---------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbox_empty() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn inbox_ack_nonexistent() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::InboxAck {
            message_id: "no-such-id".into(),
        },
    )
    .await;
    assert_error(&resp, "not_found");
}

// ---------------------------------------------------------------------------
// Process inbound: encrypted DM via ECDH
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_inbound_encrypted_dm() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();
    follow_sender(&state, &sender).await;
    let mut event_rx = state.event_tx.subscribe();

    let envelope = make_encrypted_dm_envelope(&sender, &state.identity, "msg-1", "hello world");
    process_inbound(&state, envelope).await;

    // Verify inbox has the decrypted message
    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].message_id, "msg-1");
    assert_eq!(list[0].from_node_id, sender.node_id);
    assert_eq!(list[0].body, "hello world");
    assert_eq!(list[0].message_type, MessageType::DmText);
    assert!(!list[0].acked);

    // Verify health unread count
    let resp = handle_request(&state, Request::Health).await;
    let data = assert_ok(&resp).unwrap();
    let status: HealthStatus = serde_json::from_value(data).unwrap();
    assert_eq!(status.unread_count, 1);

    // Verify event broadcast
    let event = event_rx.try_recv().unwrap();
    match event {
        Event::NewMessage {
            message_id, from, ..
        } => {
            assert_eq!(message_id, "msg-1");
            assert_eq!(from, sender.node_id);
        }
        _ => panic!("expected NewMessage event"),
    }
}

#[tokio::test]
async fn process_inbound_fallback_stores_raw_on_decryption_failure() {
    let (state, _dir) = make_test_state();

    // Send an envelope with invalid ciphertext (not properly encrypted)
    let b64 = base64::engine::general_purpose::STANDARD;
    let (sender, _sender_dir) = make_sender_identity();
    follow_sender(&state, &sender).await;
    let raw_ciphertext = b64.encode(b"not-really-encrypted");
    let signature_b64 = sender.sign(raw_ciphertext.as_bytes()).unwrap();
    let envelope = mesh_pb::Envelope {
        message_id: "bad-1".into(),
        from_node_id: sender.node_id.clone(),
        to_node_id: state.identity.node_id.clone(),
        from_public_key_b64: sender.public_key_b64.clone(),
        message_type: mesh_pb::MessageType::DmText as i32,
        ciphertext_b64: raw_ciphertext.clone(),
        nonce_b64: b64.encode(b"short"), // wrong nonce length
        signature_b64,
        timestamp_ms: 99999,
        topic: None,
    };

    process_inbound(&state, envelope).await;

    // Message should be stored with raw ciphertext as body (fallback)
    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].message_id, "bad-1");
    // The body should be the raw ciphertext_b64 (since decryption failed)
    assert_eq!(list[0].body, raw_ciphertext);
}

#[tokio::test]
async fn inbox_ack_after_inbound() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();
    follow_sender(&state, &sender).await;

    let envelope = make_encrypted_dm_envelope(&sender, &state.identity, "ack-test", "hi");
    process_inbound(&state, envelope).await;

    // Ack it
    let resp = handle_request(
        &state,
        Request::InboxAck {
            message_id: "ack-test".into(),
        },
    )
    .await;
    assert_ok(&resp);

    // Unread count should be 0
    let resp = handle_request(&state, Request::Health).await;
    let data = assert_ok(&resp).unwrap();
    let status: HealthStatus = serde_json::from_value(data).unwrap();
    assert_eq!(status.unread_count, 0);

    // Unread-only filter returns empty
    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: true,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn inbox_limit() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();
    follow_sender(&state, &sender).await;

    for i in 0..5u64 {
        let envelope = make_encrypted_dm_envelope(
            &sender,
            &state.identity,
            &format!("msg-{i}"),
            &format!("msg {i}"),
        );
        process_inbound(&state, envelope).await;
    }

    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: Some(3),
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 3);
}

#[tokio::test]
async fn multiple_inbound_and_unread_filter() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();
    follow_sender(&state, &sender).await;

    for i in 0..3u64 {
        let envelope = make_encrypted_dm_envelope(
            &sender,
            &state.identity,
            &format!("m-{i}"),
            &format!("msg {i}"),
        );
        process_inbound(&state, envelope).await;
    }

    // Ack first two
    handle_request(
        &state,
        Request::InboxAck {
            message_id: "m-0".into(),
        },
    )
    .await;
    handle_request(
        &state,
        Request::InboxAck {
            message_id: "m-1".into(),
        },
    )
    .await;

    // Unread only returns 1
    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: true,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].message_id, "m-2");

    // All messages still 3
    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 3);
}

// ---------------------------------------------------------------------------
// SendDm / PostFeed without transport
// ---------------------------------------------------------------------------

#[tokio::test]
async fn send_dm_no_relay() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::SendDm {
            to: "node-b".into(),
            body: "hello".into(),
        },
    )
    .await;
    assert_error(&resp, "no_relay");
}

#[tokio::test]
async fn post_feed_no_relay() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::PostFeed {
            body: "my post".into(),
        },
    )
    .await;
    assert_error(&resp, "no_relay");
}

// ---------------------------------------------------------------------------
// RegisterUsername / LookupUsername without relay hosts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_username_no_relay() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::RegisterUsername {
            username: "alice".into(),
        },
    )
    .await;
    assert_error(&resp, "no_relay");
}

#[tokio::test]
async fn lookup_username_no_relay() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::LookupUsername {
            username: "bob".into(),
        },
    )
    .await;
    assert_error(&resp, "no_relay");
}

// ---------------------------------------------------------------------------
// Wallet: yolo not enabled
// ---------------------------------------------------------------------------

#[tokio::test]
async fn yolo_send_eth_not_enabled() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::YoloSendEth {
            to: "0x0000000000000000000000000000000000000001".into(),
            amount: "0.01".into(),
        },
    )
    .await;
    let msg = assert_error(&resp, "wallet_error");
    assert!(msg.contains("yolo mode is not enabled"));
}

#[tokio::test]
async fn yolo_send_usdc_not_enabled() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::YoloSendUsdc {
            to: "0x0000000000000000000000000000000000000001".into(),
            amount: "10.0".into(),
        },
    )
    .await;
    let msg = assert_error(&resp, "wallet_error");
    assert!(msg.contains("yolo mode is not enabled"));
}

#[tokio::test]
async fn yolo_write_contract_not_enabled() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::YoloWriteContract {
            contract: "0x0000000000000000000000000000000000000001".into(),
            abi: "[]".into(),
            function: "foo".into(),
            args: vec![],
            value: None,
        },
    )
    .await;
    let msg = assert_error(&resp, "wallet_error");
    assert!(msg.contains("yolo mode is not enabled"));
}

#[tokio::test]
async fn yolo_sign_message_not_enabled() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::YoloSignMessage {
            message: "hello".into(),
        },
    )
    .await;
    let msg = assert_error(&resp, "wallet_error");
    assert!(msg.contains("yolo mode is not enabled"));
}

// ---------------------------------------------------------------------------
// Wallet: yolo enabled but no key file on disk
// ---------------------------------------------------------------------------

#[tokio::test]
async fn yolo_send_eth_no_key_file() {
    let (state, _dir) = make_test_state_yolo_enabled();
    let resp = handle_request(
        &state,
        Request::YoloSendEth {
            to: "0x0000000000000000000000000000000000000001".into(),
            amount: "0.01".into(),
        },
    )
    .await;
    let msg = assert_error(&resp, "wallet_error");
    assert!(msg.contains("yolo key"));
}

// ---------------------------------------------------------------------------
// Wallet: yolo balance when yolo is disabled
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wallet_balance_yolo_not_enabled() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::WalletBalance {
            wallet: ProtoWalletType::Yolo,
        },
    )
    .await;
    let msg = assert_error(&resp, "wallet_error");
    assert!(msg.contains("yolo mode is not enabled"));
}

// ---------------------------------------------------------------------------
// TOTP-gated handlers: no TOTP configured
// ---------------------------------------------------------------------------

#[tokio::test]
async fn send_eth_rejects_bad_otp() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::SendEth {
            to: "0x0000000000000000000000000000000000000001".into(),
            amount: "0.01".into(),
            otp: "000000".into(),
        },
    )
    .await;
    let (code, _) = assert_any_error(&resp);
    assert!(
        code == "totp_error" || code == "invalid_otp",
        "expected totp_error or invalid_otp, got: {code}"
    );
}

#[tokio::test]
async fn send_usdc_rejects_bad_otp() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::SendUsdc {
            to: "0x0000000000000000000000000000000000000001".into(),
            amount: "10.0".into(),
            otp: "000000".into(),
        },
    )
    .await;
    let (code, _) = assert_any_error(&resp);
    assert!(
        code == "totp_error" || code == "invalid_otp",
        "expected totp_error or invalid_otp, got: {code}"
    );
}

#[tokio::test]
async fn write_contract_rejects_bad_otp() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::WriteContract {
            contract: "0x0000000000000000000000000000000000000001".into(),
            abi: "[]".into(),
            function: "foo".into(),
            args: vec![],
            value: None,
            otp: "000000".into(),
        },
    )
    .await;
    let (code, _) = assert_any_error(&resp);
    assert!(
        code == "totp_error" || code == "invalid_otp",
        "expected totp_error or invalid_otp, got: {code}"
    );
}

#[tokio::test]
async fn sign_message_rejects_bad_otp() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::SignMessage {
            message: "hello".into(),
            otp: "000000".into(),
        },
    )
    .await;
    let (code, _) = assert_any_error(&resp);
    assert!(
        code == "totp_error" || code == "invalid_otp",
        "expected totp_error or invalid_otp, got: {code}"
    );
}

// ---------------------------------------------------------------------------
// TOTP setup and verify
// ---------------------------------------------------------------------------

#[tokio::test]
async fn setup_totp_first_time_then_reject_second() {
    let (state, _dir) = make_test_state();

    // First setup succeeds
    let resp = handle_request(&state, Request::SetupTotp).await;
    let data = assert_ok(&resp).expect("setup should return data");
    let info: TotpSetupInfo = serde_json::from_value(data).unwrap();
    assert!(!info.secret_base32.is_empty());
    assert!(info.otpauth_url.starts_with("otpauth://"));

    // Second setup fails
    let resp = handle_request(&state, Request::SetupTotp).await;
    assert_error(&resp, "already_configured");
}

#[tokio::test]
async fn verify_totp_invalid_code_after_setup() {
    let (state, _dir) = make_test_state();

    // Setup TOTP first
    let resp = handle_request(&state, Request::SetupTotp).await;
    assert_ok(&resp);

    // Verify with wrong code
    let resp = handle_request(
        &state,
        Request::VerifyTotp {
            code: "000000".into(),
        },
    )
    .await;
    assert_error(&resp, "invalid_otp");
}

// ---------------------------------------------------------------------------
// Sync push/pull
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_push_requires_confirm() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(&state, Request::SyncPush { confirm: false }).await;
    let msg = assert_error(&resp, "confirm_required");
    assert!(msg.contains("--confirm"));
}

#[tokio::test]
async fn sync_pull_requires_confirm() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(&state, Request::SyncPull { confirm: false }).await;
    let msg = assert_error(&resp, "confirm_required");
    assert!(msg.contains("--confirm"));
}

#[tokio::test]
async fn sync_push_no_relay() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(&state, Request::SyncPush { confirm: true }).await;
    assert_error(&resp, "no_relay");
}

#[tokio::test]
async fn sync_pull_no_relay() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(&state, Request::SyncPull { confirm: true }).await;
    assert_error(&resp, "no_relay");
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_returns_ok() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(&state, Request::Shutdown).await;
    assert_ok(&resp);
}

// ---------------------------------------------------------------------------
// Dispatch: all basic requests route correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_routes_all_basic_requests() {
    let (state, _dir) = make_test_state();

    // Follow first so Unfollow can succeed
    handle_request(&state, Request::Follow { target: "x".into() }).await;

    let ok_cases: Vec<Request> = vec![
        Request::Identity,
        Request::Health,
        Request::Following,
        // Followers requires relay — tested separately in followers_requires_relay
        Request::Unfollow { target: "x".into() },
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
        Request::Shutdown,
    ];

    for req in ok_cases {
        let label = format!("{req:?}");
        let resp = handle_request(&state, req).await;
        match &resp {
            Response::Ok { .. } => {}
            _ => panic!("expected Ok for {label}, got: {resp:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn send_eth_bad_input_does_not_panic() {
    let (state, _dir) = make_test_state();
    let resp = handle_request(
        &state,
        Request::SendEth {
            to: "not-an-address".into(),
            amount: "0.01".into(),
            otp: "123456".into(),
        },
    )
    .await;
    // Should get some error (TOTP check happens before address validation)
    assert_any_error(&resp);
}

#[tokio::test]
async fn process_inbound_unspecified_message_type_stores_fallback() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();

    // Envelope with unspecified message type -- decryption will fail
    // because decrypt_envelope returns Err for Unspecified
    let ciphertext = "some-raw-data";
    let signature_b64 = sender.sign(ciphertext.as_bytes()).unwrap();
    let envelope = mesh_pb::Envelope {
        message_id: "unspec-1".into(),
        from_node_id: sender.node_id.clone(),
        to_node_id: state.identity.node_id.clone(),
        from_public_key_b64: sender.public_key_b64.clone(),
        message_type: 0, // Unspecified
        ciphertext_b64: ciphertext.into(),
        nonce_b64: String::new(),
        signature_b64,
        timestamp_ms: 5000,
        topic: None,
    };

    process_inbound(&state, envelope).await;

    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].message_type, MessageType::Unspecified);
    // Body should be the raw ciphertext since decryption failed
    assert_eq!(list[0].body, "some-raw-data");
}

// ---------------------------------------------------------------------------
// Ingress validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ingress_rejects_dm_from_unfollowed_sender() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();
    // Do NOT follow the sender

    let envelope = make_encrypted_dm_envelope(&sender, &state.identity, "rejected-1", "spam");
    process_inbound(&state, envelope).await;

    // Inbox should be empty -- message was rejected
    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert!(
        list.is_empty(),
        "DM from unfollowed sender should be rejected"
    );
}

#[tokio::test]
async fn ingress_rejects_dm_from_blocked_sender() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();

    // Block the sender
    state
        .follow_store
        .lock()
        .await
        .block(&sender.node_id)
        .unwrap();

    let envelope = make_encrypted_dm_envelope(&sender, &state.identity, "blocked-1", "hi");
    process_inbound(&state, envelope).await;

    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert!(list.is_empty(), "DM from blocked sender should be rejected");
}

#[tokio::test]
async fn ingress_rejects_bad_signature() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();
    follow_sender(&state, &sender).await;

    let mut envelope = make_encrypted_dm_envelope(&sender, &state.identity, "badsig-1", "hello");
    // Corrupt the signature
    envelope.signature_b64 = "AAAA".to_string();

    process_inbound(&state, envelope).await;

    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert!(
        list.is_empty(),
        "message with bad signature should be rejected"
    );
}

#[tokio::test]
async fn ingress_accepts_dm_from_followed_sender() {
    let (state, _dir) = make_test_state();
    let (sender, _sender_dir) = make_sender_identity();
    follow_sender(&state, &sender).await;

    let envelope = make_encrypted_dm_envelope(&sender, &state.identity, "ok-1", "legitimate");
    process_inbound(&state, envelope).await;

    let resp = handle_request(
        &state,
        Request::Inbox {
            unread_only: false,
            limit: None,
        },
    )
    .await;
    let data = assert_ok(&resp).unwrap();
    let list: Vec<InboxEntry> = serde_json::from_value(data).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].body, "legitimate");
}
