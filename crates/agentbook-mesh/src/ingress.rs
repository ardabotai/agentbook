use crate::crypto::verify_signature;
use crate::follow::FollowStore;
use crate::inbox::MessageType;
use agentbook_crypto::rate_limit::{CheckResult, RateLimiter};

/// Result of ingress validation.
pub enum IngressResult {
    /// Message accepted.
    Accept,
    /// Rejected with reason.
    Reject(String),
}

/// Parameters for an ingress check.
pub struct IngressRequest<'a> {
    pub from_node_id: &'a str,
    pub from_public_key_b64: &'a str,
    pub payload: &'a [u8],
    pub signature_b64: &'a str,
    pub my_node_id: &'a str,
    pub message_type: MessageType,
}

/// Validates inbound messages against signature, follow graph, and rate limits.
pub struct IngressPolicy<'a> {
    follow_store: &'a FollowStore,
    rate_limiter: &'a mut RateLimiter,
}

impl<'a> IngressPolicy<'a> {
    pub fn new(follow_store: &'a FollowStore, rate_limiter: &'a mut RateLimiter) -> Self {
        Self {
            follow_store,
            rate_limiter,
        }
    }

    /// Check whether an inbound message should be accepted.
    ///
    /// Steps:
    /// 1. Verify signature
    /// 2. Check blocked list
    /// 3. For DMs: require that we follow the sender (mutual follow gating
    ///    is enforced at the sender side — we accept if we follow them)
    /// 4. For feed posts: accept from anyone we follow
    /// 5. Rate limit
    pub fn check(&mut self, req: &IngressRequest<'_>) -> IngressResult {
        // RoomJoin events are relay-generated system messages — no signature to verify.
        if req.message_type == MessageType::RoomJoin {
            if self.follow_store.is_blocked(req.from_node_id) {
                return IngressResult::Reject("sender is blocked".to_string());
            }
            return IngressResult::Accept;
        }

        // 1. Verify signature
        if !verify_signature(req.from_public_key_b64, req.payload, req.signature_b64) {
            return IngressResult::Reject("invalid signature".to_string());
        }

        // 2. Check blocked
        if self.follow_store.is_blocked(req.from_node_id) {
            return IngressResult::Reject("sender is blocked".to_string());
        }

        // 3. Check follow relationship
        let is_following = self.follow_store.is_following(req.from_node_id);
        match req.message_type {
            MessageType::DmText => {
                if !is_following {
                    return IngressResult::Reject(
                        "DMs require mutual follow (you don't follow sender)".to_string(),
                    );
                }
            }
            MessageType::FeedPost => {
                if !is_following {
                    return IngressResult::Reject("not following sender".to_string());
                }
            }
            MessageType::RoomMessage | MessageType::RoomJoin => {
                // Room messages and join events skip follow-graph check.
            }
            MessageType::Unspecified => {}
        }

        // 4. Rate limit
        match self.rate_limiter.check(req.from_node_id) {
            CheckResult::Allowed => {}
            CheckResult::RateLimited | CheckResult::Banned { .. } => {
                return IngressResult::Reject("rate limited".to_string());
            }
        }

        IngressResult::Accept
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{evm_address_from_public_key, sign_payload};
    use crate::follow::FollowRecord;
    use agentbook_crypto::time::now_ms;
    use base64::Engine;
    use k256::SecretKey;
    use rand::rngs::OsRng;

    fn make_follow_record(node_id: &str, pub_b64: &str) -> FollowRecord {
        FollowRecord {
            node_id: node_id.to_string(),
            public_key_b64: pub_b64.to_string(),
            username: None,
            relay_hints: vec![],
            followed_at_ms: now_ms(),
        }
    }

    #[test]
    fn accept_dm_from_followed() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FollowStore::load(dir.path()).unwrap();
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        store
            .follow(make_follow_record(&node_id, &pub_b64))
            .unwrap();

        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let payload = b"test";
        let sig = sign_payload(&secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &node_id,
            from_public_key_b64: &pub_b64,
            payload,
            signature_b64: &sig,
            my_node_id: "my_node",
            message_type: MessageType::DmText,
        };
        assert!(matches!(policy.check(&req), IngressResult::Accept));
    }

    #[test]
    fn reject_dm_from_unfollowed() {
        let dir = tempfile::tempdir().unwrap();
        let store = FollowStore::load(dir.path()).unwrap();
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let payload = b"test";
        let sig = sign_payload(&secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &node_id,
            from_public_key_b64: &pub_b64,
            payload,
            signature_b64: &sig,
            my_node_id: "my_node",
            message_type: MessageType::DmText,
        };
        match policy.check(&req) {
            IngressResult::Reject(msg) => assert!(msg.contains("mutual follow")),
            IngressResult::Accept => panic!("expected Reject"),
        }
    }

    #[test]
    fn reject_bad_signature() {
        let dir = tempfile::tempdir().unwrap();
        let store = FollowStore::load(dir.path()).unwrap();
        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let req = IngressRequest {
            from_node_id: "node",
            from_public_key_b64: "bad_key",
            payload: b"test",
            signature_b64: "bad_sig",
            my_node_id: "my_node",
            message_type: MessageType::DmText,
        };
        match policy.check(&req) {
            IngressResult::Reject(msg) => assert!(msg.contains("signature")),
            IngressResult::Accept => panic!("expected Reject"),
        }
    }

    #[test]
    fn reject_from_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FollowStore::load(dir.path()).unwrap();
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        store.block(&node_id).unwrap();

        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let payload = b"test";
        let sig = sign_payload(&secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &node_id,
            from_public_key_b64: &pub_b64,
            payload,
            signature_b64: &sig,
            my_node_id: "my_node",
            message_type: MessageType::FeedPost,
        };
        match policy.check(&req) {
            IngressResult::Reject(msg) => assert!(msg.contains("blocked")),
            IngressResult::Accept => panic!("expected Reject"),
        }
    }
}
