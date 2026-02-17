use super::social::fetch_followers_from_relay;
use super::{NodeState, error_response, now_ms, ok_response, to_protocol_message_type};
use agentbook::protocol::{InboxEntry, Response};
use agentbook_mesh::crypto::{decrypt_with_key, encrypt_with_key, random_key_material};
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::MessageType as MeshMessageType;
use agentbook_proto::mesh::v1 as mesh_pb;
use base64::Engine;
use k256::PublicKey;
use std::sync::Arc;
use uuid::Uuid;

pub async fn handle_send_dm(state: &Arc<NodeState>, to: &str, body: &str) -> Response {
    let transport = match &state.transport {
        Some(t) => t,
        None => return error_response("no_relay", "not connected to any relay"),
    };

    // Resolve @username → node_id if needed, then look up public key from follow store
    let resolved_to = if let Some(username) = to.strip_prefix('@') {
        // Resolve via relay
        let mut resolved_node_id = None;
        for host in &state.relay_hosts {
            let mut client = match state.get_grpc_client(host).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            if let Ok(resp) = client
                .lookup_username(agentbook_proto::host::v1::LookupUsernameRequest {
                    username: username.to_string(),
                })
                .await
            {
                let r = resp.into_inner();
                if r.found {
                    resolved_node_id = Some(r.node_id);
                    break;
                }
                return error_response(
                    "not_found",
                    &format!("username @{} not found", username.to_lowercase()),
                );
            }
        }
        match resolved_node_id {
            Some(id) => id,
            None => {
                return error_response(
                    "relay_unavailable",
                    "could not reach any relay for username resolution",
                );
            }
        }
    } else {
        to.to_string()
    };

    // Look up recipient's public key from follow store
    let peer_public_key = {
        let follow_store = state.follow_store.lock().await;
        match resolve_peer_public_key(&follow_store, &resolved_to) {
            Ok(pk) => pk,
            Err(e) => return error_response("encryption_error", &e),
        }
    };

    // Derive ECDH shared key and encrypt message body
    let shared_key = state.identity.derive_shared_key(&peer_public_key);
    let (ciphertext_b64, nonce_b64) = match encrypt_with_key(&shared_key, body.as_bytes()) {
        Ok(pair) => pair,
        Err(e) => return error_response("encryption_error", &format!("encryption failed: {e}")),
    };

    // Sign the ciphertext (what actually goes on the wire)
    let signature_b64 = state
        .identity
        .sign(ciphertext_b64.as_bytes())
        .unwrap_or_default();

    let msg_id = Uuid::new_v4().to_string();
    let envelope = mesh_pb::Envelope {
        message_id: msg_id.clone(),
        from_node_id: state.identity.node_id.clone(),
        to_node_id: resolved_to,
        from_public_key_b64: state.identity.public_key_b64.clone(),
        message_type: mesh_pb::MessageType::DmText as i32,
        ciphertext_b64,
        nonce_b64,
        signature_b64,
        timestamp_ms: now_ms(),
        topic: None,
    };

    match transport.send_via_relay(envelope).await {
        Ok(()) => ok_response(Some(serde_json::json!({ "message_id": msg_id }))),
        Err(e) => error_response("send_failed", &e.to_string()),
    }
}

pub async fn handle_post_feed(state: &Arc<NodeState>, body: &str) -> Response {
    let transport = match &state.transport {
        Some(t) => t,
        None => return error_response("no_relay", "not connected to any relay"),
    };

    // Fetch actual followers from relay (people who follow us).
    let follower_entries = match fetch_followers_from_relay(state, &state.identity.node_id).await {
        Ok(entries) => entries,
        Err(e) => {
            return error_response("relay_unavailable", &e);
        }
    };

    if follower_entries.is_empty() {
        return error_response(
            "no_followers",
            "Bro you have no followers, get your friends on here",
        );
    }

    // Convert relay FollowEntry to a simple (node_id, public_key_b64) list
    let followers: Vec<(String, String)> = follower_entries
        .into_iter()
        .map(|e| (e.node_id, e.public_key_b64))
        .collect();

    let msg_id = Uuid::new_v4().to_string();

    // Generate a random content key and encrypt the body once
    let content_key = random_key_material();
    let (content_ciphertext_b64, content_nonce_b64) =
        match encrypt_with_key(&content_key, body.as_bytes()) {
            Ok(pair) => pair,
            Err(e) => {
                return error_response("encryption_error", &format!("feed encryption failed: {e}"));
            }
        };

    // Sign the ciphertext
    let signature_b64 = state
        .identity
        .sign(content_ciphertext_b64.as_bytes())
        .unwrap_or_default();
    let timestamp = now_ms();

    // Build and send envelopes to all followers concurrently.
    // Each follower gets the content key wrapped with their ECDH shared key.
    let send_futures: Vec<_> = followers
        .iter()
        .filter_map(|(follower_node_id, follower_pubkey_b64)| {
            let peer_public_key = match parse_public_key_b64(follower_pubkey_b64) {
                Ok(pk) => pk,
                Err(_) => {
                    tracing::warn!(
                        to = %follower_node_id,
                        "skipping feed post: no valid public key for follower"
                    );
                    return None;
                }
            };

            // Wrap the content key with the per-follower ECDH shared key
            let shared_key = state.identity.derive_shared_key(&peer_public_key);
            let (wrapped_key_b64, wrapped_key_nonce_b64) =
                match encrypt_with_key(&shared_key, &content_key) {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(
                            to = %follower_node_id, err = %e,
                            "skipping feed post: failed to wrap content key"
                        );
                        return None;
                    }
                };

            // Pack wrapped key + encrypted body into the envelope ciphertext field.
            // Format: "<wrapped_key_b64>:<wrapped_key_nonce_b64>:<content_ciphertext_b64>"
            let combined_ciphertext =
                format!("{wrapped_key_b64}:{wrapped_key_nonce_b64}:{content_ciphertext_b64}");

            let envelope = mesh_pb::Envelope {
                message_id: msg_id.clone(),
                from_node_id: state.identity.node_id.clone(),
                to_node_id: follower_node_id.clone(),
                from_public_key_b64: state.identity.public_key_b64.clone(),
                message_type: mesh_pb::MessageType::FeedPost as i32,
                ciphertext_b64: combined_ciphertext,
                nonce_b64: content_nonce_b64.clone(),
                signature_b64: signature_b64.clone(),
                timestamp_ms: timestamp,
                topic: None,
            };

            let node_id = follower_node_id.clone();
            Some(async move {
                if let Err(e) = transport.send_via_relay(envelope).await {
                    tracing::warn!(to = %node_id, err = %e, "failed to send feed post");
                }
            })
        })
        .collect();

    futures_util::future::join_all(send_futures).await;

    ok_response(Some(serde_json::json!({ "message_id": msg_id })))
}

pub async fn handle_inbox(
    state: &Arc<NodeState>,
    unread_only: bool,
    limit: Option<usize>,
) -> Response {
    let follow_store = state.follow_store.lock().await;
    let inbox = state.inbox.lock().await;
    let messages: Vec<InboxEntry> = inbox
        .list(unread_only, limit)
        .into_iter()
        .map(|m| {
            // Look up from_username via the follow store
            let from_username = follow_store
                .get(&m.from_node_id)
                .and_then(|r| r.username.clone());
            InboxEntry {
                message_id: m.message_id.clone(),
                from_node_id: m.from_node_id.clone(),
                from_username,
                message_type: to_protocol_message_type(m.message_type),
                body: m.body.clone(),
                timestamp_ms: m.timestamp_ms,
                acked: m.acked,
            }
        })
        .collect();
    ok_response(Some(serde_json::to_value(messages).unwrap()))
}

pub async fn handle_inbox_ack(state: &Arc<NodeState>, message_id: &str) -> Response {
    let mut inbox = state.inbox.lock().await;
    match inbox.ack(message_id) {
        Ok(true) => ok_response(None),
        Ok(false) => error_response("not_found", &format!("message {message_id} not found")),
        Err(e) => error_response("ack_failed", &e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Encryption helpers
// ---------------------------------------------------------------------------

/// Parse a base64-encoded SEC1 public key.
pub(crate) fn parse_public_key_b64(public_key_b64: &str) -> Result<PublicKey, String> {
    if public_key_b64.is_empty() {
        return Err("public key is empty".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64)
        .map_err(|e| format!("invalid base64 public key: {e}"))?;
    PublicKey::from_sec1_bytes(&bytes).map_err(|e| format!("invalid secp256k1 public key: {e}"))
}

/// Resolve a peer's public key from the follow store.
fn resolve_peer_public_key(follow_store: &FollowStore, node_id: &str) -> Result<PublicKey, String> {
    match follow_store.get(node_id) {
        Some(record) if !record.public_key_b64.is_empty() => {
            parse_public_key_b64(&record.public_key_b64)
        }
        _ => Err(format!(
            "no public key for {node_id} -- follow them first (with their public key)"
        )),
    }
}

/// Decrypt an inbound envelope using ECDH.
pub(crate) fn decrypt_envelope(
    identity: &NodeIdentity,
    envelope: &mesh_pb::Envelope,
    message_type: MeshMessageType,
) -> Result<String, String> {
    // Parse sender's public key from the envelope
    let sender_public_key = parse_public_key_b64(&envelope.from_public_key_b64)?;
    let shared_key = identity.derive_shared_key(&sender_public_key);

    match message_type {
        MeshMessageType::DmText => {
            // DM: ciphertext_b64 is directly encrypted with ECDH shared key
            let plaintext_bytes =
                decrypt_with_key(&shared_key, &envelope.ciphertext_b64, &envelope.nonce_b64)
                    .map_err(|e| format!("DM decryption failed: {e}"))?;
            String::from_utf8(plaintext_bytes)
                .map_err(|e| format!("decrypted DM is not valid UTF-8: {e}"))
        }
        MeshMessageType::FeedPost => {
            // Feed: ciphertext_b64 format is
            // "<wrapped_key_b64>:<wrapped_key_nonce_b64>:<content_ciphertext_b64>"
            // nonce_b64 in the envelope is the content nonce.
            let parts: Vec<&str> = envelope.ciphertext_b64.splitn(3, ':').collect();
            if parts.len() != 3 {
                return Err("invalid feed post envelope format".to_string());
            }
            let (wrapped_key_b64, wrapped_key_nonce_b64, content_ciphertext_b64) =
                (parts[0], parts[1], parts[2]);

            // Unwrap the content key using the ECDH shared key
            let content_key_bytes =
                decrypt_with_key(&shared_key, wrapped_key_b64, wrapped_key_nonce_b64)
                    .map_err(|e| format!("failed to unwrap feed content key: {e}"))?;
            if content_key_bytes.len() != 32 {
                return Err("unwrapped content key has wrong length".to_string());
            }
            let mut content_key = [0u8; 32];
            content_key.copy_from_slice(&content_key_bytes);

            // Decrypt the content with the unwrapped content key
            let plaintext_bytes =
                decrypt_with_key(&content_key, content_ciphertext_b64, &envelope.nonce_b64)
                    .map_err(|e| format!("feed content decryption failed: {e}"))?;
            String::from_utf8(plaintext_bytes)
                .map_err(|e| format!("decrypted feed post is not valid UTF-8: {e}"))
        }
        MeshMessageType::Unspecified => {
            Err("cannot decrypt message with unspecified type".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentbook_mesh::crypto::random_key_material;
    use agentbook_mesh::identity::NodeIdentity;
    use agentbook_proto::mesh::v1 as mesh_pb;

    /// Create a test identity in a temp directory.
    fn make_identity() -> (NodeIdentity, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let kek = random_key_material();
        let id = NodeIdentity::load_or_create(dir.path(), &kek).unwrap();
        (id, dir)
    }

    #[test]
    fn dm_encrypt_decrypt_round_trip() {
        let (sender, _d1) = make_identity();
        let (receiver, _d2) = make_identity();

        let plaintext = "hello from sender to receiver!";

        // Sender encrypts for receiver
        let shared_key = sender.derive_shared_key(&receiver.public_key);
        let (ciphertext_b64, nonce_b64) =
            encrypt_with_key(&shared_key, plaintext.as_bytes()).unwrap();

        let signature_b64 = sender.sign(ciphertext_b64.as_bytes()).unwrap();

        let envelope = mesh_pb::Envelope {
            message_id: "test-dm-1".to_string(),
            from_node_id: sender.node_id.clone(),
            to_node_id: receiver.node_id.clone(),
            from_public_key_b64: sender.public_key_b64.clone(),
            message_type: mesh_pb::MessageType::DmText as i32,
            ciphertext_b64,
            nonce_b64,
            signature_b64,
            timestamp_ms: 1000,
            topic: None,
        };

        // Receiver decrypts
        let decrypted = decrypt_envelope(&receiver, &envelope, MeshMessageType::DmText).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn dm_wrong_recipient_cannot_decrypt() {
        let (sender, _d1) = make_identity();
        let (receiver, _d2) = make_identity();
        let (wrong_recipient, _d3) = make_identity();

        let plaintext = "secret message";

        let shared_key = sender.derive_shared_key(&receiver.public_key);
        let (ciphertext_b64, nonce_b64) =
            encrypt_with_key(&shared_key, plaintext.as_bytes()).unwrap();

        let envelope = mesh_pb::Envelope {
            message_id: "test-dm-2".to_string(),
            from_node_id: sender.node_id.clone(),
            to_node_id: receiver.node_id.clone(),
            from_public_key_b64: sender.public_key_b64.clone(),
            message_type: mesh_pb::MessageType::DmText as i32,
            ciphertext_b64,
            nonce_b64,
            signature_b64: String::new(),
            timestamp_ms: 1000,
            topic: None,
        };

        // Wrong recipient cannot decrypt
        let result = decrypt_envelope(&wrong_recipient, &envelope, MeshMessageType::DmText);
        assert!(result.is_err());
    }

    #[test]
    fn feed_encrypt_decrypt_round_trip() {
        let (sender, _d1) = make_identity();
        let (follower, _d2) = make_identity();

        let plaintext = "this is a feed post for all followers";

        // Sender creates content key and encrypts body
        let content_key = random_key_material();
        let (content_ciphertext_b64, content_nonce_b64) =
            encrypt_with_key(&content_key, plaintext.as_bytes()).unwrap();

        // Wrap content key for this follower
        let shared_key = sender.derive_shared_key(&follower.public_key);
        let (wrapped_key_b64, wrapped_key_nonce_b64) =
            encrypt_with_key(&shared_key, &content_key).unwrap();

        let combined_ciphertext =
            format!("{wrapped_key_b64}:{wrapped_key_nonce_b64}:{content_ciphertext_b64}");

        let envelope = mesh_pb::Envelope {
            message_id: "test-feed-1".to_string(),
            from_node_id: sender.node_id.clone(),
            to_node_id: follower.node_id.clone(),
            from_public_key_b64: sender.public_key_b64.clone(),
            message_type: mesh_pb::MessageType::FeedPost as i32,
            ciphertext_b64: combined_ciphertext,
            nonce_b64: content_nonce_b64,
            signature_b64: String::new(),
            timestamp_ms: 1000,
            topic: None,
        };

        // Follower decrypts
        let decrypted = decrypt_envelope(&follower, &envelope, MeshMessageType::FeedPost).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn feed_wrong_recipient_cannot_decrypt() {
        let (sender, _d1) = make_identity();
        let (follower, _d2) = make_identity();
        let (outsider, _d3) = make_identity();

        let plaintext = "private feed post";
        let content_key = random_key_material();
        let (content_ciphertext_b64, content_nonce_b64) =
            encrypt_with_key(&content_key, plaintext.as_bytes()).unwrap();

        let shared_key = sender.derive_shared_key(&follower.public_key);
        let (wrapped_key_b64, wrapped_key_nonce_b64) =
            encrypt_with_key(&shared_key, &content_key).unwrap();

        let combined_ciphertext =
            format!("{wrapped_key_b64}:{wrapped_key_nonce_b64}:{content_ciphertext_b64}");

        let envelope = mesh_pb::Envelope {
            message_id: "test-feed-2".to_string(),
            from_node_id: sender.node_id.clone(),
            to_node_id: follower.node_id.clone(),
            from_public_key_b64: sender.public_key_b64.clone(),
            message_type: mesh_pb::MessageType::FeedPost as i32,
            ciphertext_b64: combined_ciphertext,
            nonce_b64: content_nonce_b64,
            signature_b64: String::new(),
            timestamp_ms: 1000,
            topic: None,
        };

        // Outsider cannot unwrap the content key
        let result = decrypt_envelope(&outsider, &envelope, MeshMessageType::FeedPost);
        assert!(result.is_err());
    }

    #[test]
    fn feed_each_follower_gets_unique_wrapped_key() {
        let (sender, _d1) = make_identity();
        let (follower_a, _d2) = make_identity();
        let (follower_b, _d3) = make_identity();

        let plaintext = "broadcast to multiple followers";
        let content_key = random_key_material();
        let (content_ciphertext_b64, content_nonce_b64) =
            encrypt_with_key(&content_key, plaintext.as_bytes()).unwrap();

        // Wrap for follower A
        let shared_key_a = sender.derive_shared_key(&follower_a.public_key);
        let (wrapped_a, wrapped_nonce_a) = encrypt_with_key(&shared_key_a, &content_key).unwrap();
        let combined_a = format!("{wrapped_a}:{wrapped_nonce_a}:{content_ciphertext_b64}");

        // Wrap for follower B
        let shared_key_b = sender.derive_shared_key(&follower_b.public_key);
        let (wrapped_b, wrapped_nonce_b) = encrypt_with_key(&shared_key_b, &content_key).unwrap();
        let combined_b = format!("{wrapped_b}:{wrapped_nonce_b}:{content_ciphertext_b64}");

        // Wrapped keys differ per follower
        assert_ne!(wrapped_a, wrapped_b);

        // Both followers can decrypt
        let env_a = mesh_pb::Envelope {
            message_id: "f1".to_string(),
            from_node_id: sender.node_id.clone(),
            to_node_id: follower_a.node_id.clone(),
            from_public_key_b64: sender.public_key_b64.clone(),
            message_type: mesh_pb::MessageType::FeedPost as i32,
            ciphertext_b64: combined_a,
            nonce_b64: content_nonce_b64.clone(),
            signature_b64: String::new(),
            timestamp_ms: 1000,
            topic: None,
        };
        let env_b = mesh_pb::Envelope {
            message_id: "f2".to_string(),
            from_node_id: sender.node_id.clone(),
            to_node_id: follower_b.node_id.clone(),
            from_public_key_b64: sender.public_key_b64.clone(),
            message_type: mesh_pb::MessageType::FeedPost as i32,
            ciphertext_b64: combined_b,
            nonce_b64: content_nonce_b64,
            signature_b64: String::new(),
            timestamp_ms: 1000,
            topic: None,
        };

        assert_eq!(
            decrypt_envelope(&follower_a, &env_a, MeshMessageType::FeedPost).unwrap(),
            plaintext
        );
        assert_eq!(
            decrypt_envelope(&follower_b, &env_b, MeshMessageType::FeedPost).unwrap(),
            plaintext
        );

        // Follower A cannot decrypt B's envelope
        assert!(decrypt_envelope(&follower_a, &env_b, MeshMessageType::FeedPost).is_err());
    }

    #[test]
    fn decrypt_unspecified_message_type_fails() {
        let (sender, _d1) = make_identity();
        let (receiver, _d2) = make_identity();

        let envelope = mesh_pb::Envelope {
            message_id: "test".to_string(),
            from_node_id: sender.node_id.clone(),
            to_node_id: receiver.node_id.clone(),
            from_public_key_b64: sender.public_key_b64.clone(),
            message_type: mesh_pb::MessageType::Unspecified as i32,
            ciphertext_b64: "anything".to_string(),
            nonce_b64: String::new(),
            signature_b64: String::new(),
            timestamp_ms: 1000,
            topic: None,
        };

        let result = decrypt_envelope(&receiver, &envelope, MeshMessageType::Unspecified);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unspecified"));
    }

    #[test]
    fn parse_public_key_b64_empty_fails() {
        assert!(parse_public_key_b64("").is_err());
    }

    #[test]
    fn parse_public_key_b64_invalid_base64_fails() {
        assert!(parse_public_key_b64("not-valid-base64!!!").is_err());
    }

    #[test]
    fn parse_public_key_b64_valid_round_trip() {
        let (id, _dir) = make_identity();
        let pk = parse_public_key_b64(&id.public_key_b64).unwrap();
        assert_eq!(pk, id.public_key);
    }

    #[test]
    fn dm_nonces_are_unique_per_message() {
        let (sender, _d1) = make_identity();
        let (receiver, _d2) = make_identity();

        let shared_key = sender.derive_shared_key(&receiver.public_key);
        let (_, nonce1) = encrypt_with_key(&shared_key, b"msg1").unwrap();
        let (_, nonce2) = encrypt_with_key(&shared_key, b"msg2").unwrap();

        // Random nonces should differ (probability of collision is negligible)
        assert_ne!(nonce1, nonce2);
    }
}
