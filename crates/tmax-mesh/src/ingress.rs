use crate::crypto::verify_signature;
use crate::friends::{FriendsStore, TrustTier};
use crate::inbox::MessageType;
use crate::invite;
use crate::rate_limit::RateLimiter;

/// Result of ingress validation.
pub enum IngressResult {
    /// Message accepted (caller should decrypt + store).
    Accept,
    /// Sender is not a friend and provided a valid invite — auto-added.
    AcceptViaInvite(invite::InvitePayload),
    /// Rejected with reason.
    Reject(String),
}

/// Parameters for an ingress check.
pub struct IngressRequest<'a> {
    pub from_node_id: &'a str,
    pub from_public_key_b64: &'a str,
    pub payload: &'a [u8],
    pub signature_b64: &'a str,
    pub invite_token: Option<&'a str>,
    pub my_node_id: &'a str,
    pub message_type: MessageType,
}

/// Validates inbound messages against signature, friend list, and rate limits.
pub struct IngressPolicy<'a> {
    friends: &'a FriendsStore,
    rate_limiter: &'a mut RateLimiter,
}

impl<'a> IngressPolicy<'a> {
    pub fn new(friends: &'a FriendsStore, rate_limiter: &'a mut RateLimiter) -> Self {
        Self {
            friends,
            rate_limiter,
        }
    }

    /// Returns the minimum trust tier required to send a given message type.
    pub fn minimum_trust_for(message_type: MessageType) -> TrustTier {
        match message_type {
            MessageType::Unspecified | MessageType::Broadcast => TrustTier::Public,
            MessageType::DmText => TrustTier::Follower,
            MessageType::TaskUpdate => TrustTier::Trusted,
            MessageType::Command => TrustTier::Operator,
        }
    }

    /// Check whether an inbound message should be accepted.
    ///
    /// Steps:
    /// 1. Verify signature
    /// 2. Check friend list OR valid invite token (auto-add friend)
    /// 3. Trust-tier enforcement
    /// 4. Rate limit
    pub fn check(&mut self, req: &IngressRequest<'_>) -> IngressResult {
        let from_node_id = req.from_node_id;
        let from_public_key_b64 = req.from_public_key_b64;
        let payload = req.payload;
        let signature_b64 = req.signature_b64;
        let invite_token = req.invite_token;
        let my_node_id = req.my_node_id;
        let message_type = req.message_type;
        // 1. Verify signature
        if !verify_signature(from_public_key_b64, payload, signature_b64) {
            return IngressResult::Reject("invalid signature".to_string());
        }

        // 2. Check friend or invite
        let is_friend = self.friends.is_friend(from_node_id);
        let invite_payload = if !is_friend {
            if let Some(token) = invite_token {
                match invite::accept_invite(token) {
                    // The invite must have been issued by us (the recipient)
                    Ok(payload) if payload.inviter_node_id == my_node_id => Some(payload),
                    Ok(_) => {
                        return IngressResult::Reject(
                            "invite was not issued by this node".to_string(),
                        );
                    }
                    Err(e) => {
                        return IngressResult::Reject(format!("invalid invite: {e}"));
                    }
                }
            } else {
                return IngressResult::Reject("sender is not a friend".to_string());
            }
        } else {
            if let Some(record) = self.friends.get(from_node_id)
                && record.blocked
            {
                return IngressResult::Reject("sender is blocked".to_string());
            }
            None
        };

        // 3. Trust-tier enforcement
        let sender_tier = if invite_payload.is_some() {
            // New friend via invite defaults to Follower
            TrustTier::Follower
        } else {
            self.friends
                .get(from_node_id)
                .map(|f| f.trust_tier)
                .unwrap_or(TrustTier::Public)
        };
        let required = Self::minimum_trust_for(message_type);
        if sender_tier < required {
            return IngressResult::Reject(format!(
                "insufficient trust: sender is {:?}, requires {:?} for {:?}",
                sender_tier, required, message_type
            ));
        }

        // 4. Rate limit
        if !self.rate_limiter.check(from_node_id) {
            return IngressResult::Reject("rate limited".to_string());
        }

        match invite_payload {
            Some(payload) => IngressResult::AcceptViaInvite(payload),
            None => IngressResult::Accept,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{evm_address_from_public_key, sign_payload};
    use crate::friends::FriendRecord;
    use base64::Engine;
    use k256::SecretKey;
    use rand::rngs::OsRng;

    fn setup_friend_store(dir: &std::path::Path) -> FriendsStore {
        FriendsStore::load(dir).unwrap()
    }

    fn make_friend_record(node_id: &str, pub_b64: &str) -> FriendRecord {
        FriendRecord {
            node_id: node_id.to_string(),
            public_key_b64: pub_b64.to_string(),
            alias: None,
            relay_hosts: vec![],
            blocked: false,
            added_at_ms: 0,
            trust_tier: TrustTier::Follower,
        }
    }

    #[test]
    fn accept_from_friend() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = setup_friend_store(dir.path());
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        store.add(make_friend_record(&node_id, &pub_b64)).unwrap();

        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let payload = b"test";
        let sig = sign_payload(&secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &node_id,
            from_public_key_b64: &pub_b64,
            payload,
            signature_b64: &sig,
            invite_token: None,
            my_node_id: "my_node",
            message_type: MessageType::DmText,
        };
        match policy.check(&req) {
            IngressResult::Accept => {}
            other => panic!("expected Accept, got: {}", ingress_label(&other)),
        }
    }

    #[test]
    fn reject_bad_signature() {
        let dir = tempfile::tempdir().unwrap();
        let store = setup_friend_store(dir.path());
        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let req = IngressRequest {
            from_node_id: "node",
            from_public_key_b64: "bad_key",
            payload: b"test",
            signature_b64: "bad_sig",
            invite_token: None,
            my_node_id: "my_node",
            message_type: MessageType::DmText,
        };
        match policy.check(&req) {
            IngressResult::Reject(msg) => assert!(msg.contains("signature")),
            other => panic!("expected Reject, got: {}", ingress_label(&other)),
        }
    }

    #[test]
    fn reject_unknown_sender() {
        let dir = tempfile::tempdir().unwrap();
        let store = setup_friend_store(dir.path());
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
            invite_token: None,
            my_node_id: "my_node",
            message_type: MessageType::DmText,
        };
        match policy.check(&req) {
            IngressResult::Reject(msg) => assert!(msg.contains("not a friend")),
            other => panic!("expected Reject, got: {}", ingress_label(&other)),
        }
    }

    #[test]
    fn accept_via_invite() {
        let dir = tempfile::tempdir().unwrap();
        let store = setup_friend_store(dir.path());

        let my_secret = SecretKey::random(&mut OsRng);
        let my_public = my_secret.public_key();
        let my_pub_b64 =
            base64::engine::general_purpose::STANDARD.encode(my_public.to_sec1_bytes());
        let my_node_id = evm_address_from_public_key(&my_public);

        let token = crate::invite::create_invite(
            &my_node_id,
            &my_pub_b64,
            &my_secret,
            vec![],
            vec![],
            60_000,
        )
        .unwrap();

        let sender_secret = SecretKey::random(&mut OsRng);
        let sender_public = sender_secret.public_key();
        let sender_pub_b64 =
            base64::engine::general_purpose::STANDARD.encode(sender_public.to_sec1_bytes());
        let sender_node_id = evm_address_from_public_key(&sender_public);

        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let payload = b"test";
        let sig = sign_payload(&sender_secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &sender_node_id,
            from_public_key_b64: &sender_pub_b64,
            payload,
            signature_b64: &sig,
            invite_token: Some(&token),
            my_node_id: &my_node_id,
            message_type: MessageType::DmText,
        };
        match policy.check(&req) {
            IngressResult::AcceptViaInvite(_) => {}
            other => panic!("expected AcceptViaInvite, got: {}", ingress_label(&other)),
        }
    }

    #[test]
    fn rate_limited() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = setup_friend_store(dir.path());
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        store.add(make_friend_record(&node_id, &pub_b64)).unwrap();

        let mut rl = RateLimiter::new(1, 0.0);
        let payload = b"test";
        let sig = sign_payload(&secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &node_id,
            from_public_key_b64: &pub_b64,
            payload,
            signature_b64: &sig,
            invite_token: None,
            my_node_id: "my_node",
            message_type: MessageType::DmText,
        };
        {
            let mut policy = IngressPolicy::new(&store, &mut rl);
            match policy.check(&req) {
                IngressResult::Accept => {}
                other => panic!("expected Accept, got: {}", ingress_label(&other)),
            }
        }
        {
            let mut policy = IngressPolicy::new(&store, &mut rl);
            match policy.check(&req) {
                IngressResult::Reject(msg) => assert!(msg.contains("rate limited")),
                other => panic!(
                    "expected Reject(rate limited), got: {}",
                    ingress_label(&other)
                ),
            }
        }
    }

    #[test]
    fn command_rejected_for_follower() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = setup_friend_store(dir.path());
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        store.add(make_friend_record(&node_id, &pub_b64)).unwrap();

        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let payload = b"test";
        let sig = sign_payload(&secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &node_id,
            from_public_key_b64: &pub_b64,
            payload,
            signature_b64: &sig,
            invite_token: None,
            my_node_id: "my_node",
            message_type: MessageType::Command,
        };
        match policy.check(&req) {
            IngressResult::Reject(msg) => assert!(msg.contains("insufficient trust")),
            other => panic!(
                "expected Reject(insufficient trust), got: {}",
                ingress_label(&other)
            ),
        }
    }

    #[test]
    fn command_accepted_for_operator() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = setup_friend_store(dir.path());
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        let mut record = make_friend_record(&node_id, &pub_b64);
        record.trust_tier = TrustTier::Operator;
        store.add(record).unwrap();

        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let payload = b"test";
        let sig = sign_payload(&secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &node_id,
            from_public_key_b64: &pub_b64,
            payload,
            signature_b64: &sig,
            invite_token: None,
            my_node_id: "my_node",
            message_type: MessageType::Command,
        };
        match policy.check(&req) {
            IngressResult::Accept => {}
            other => panic!("expected Accept, got: {}", ingress_label(&other)),
        }
    }

    #[test]
    fn broadcast_accepted_for_public() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = setup_friend_store(dir.path());
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        let mut record = make_friend_record(&node_id, &pub_b64);
        record.trust_tier = TrustTier::Public;
        store.add(record).unwrap();

        let mut rl = RateLimiter::new(10, 1.0);
        let mut policy = IngressPolicy::new(&store, &mut rl);

        let payload = b"test";
        let sig = sign_payload(&secret, payload).unwrap();

        let req = IngressRequest {
            from_node_id: &node_id,
            from_public_key_b64: &pub_b64,
            payload,
            signature_b64: &sig,
            invite_token: None,
            my_node_id: "my_node",
            message_type: MessageType::Broadcast,
        };
        match policy.check(&req) {
            IngressResult::Accept => {}
            other => panic!("expected Accept, got: {}", ingress_label(&other)),
        }
    }

    fn ingress_label(r: &IngressResult) -> &'static str {
        match r {
            IngressResult::Accept => "Accept",
            IngressResult::AcceptViaInvite(_) => "AcceptViaInvite",
            IngressResult::Reject(_) => "Reject",
        }
    }
}
