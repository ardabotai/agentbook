use crate::crypto::{sign_payload, verify_signature};
use anyhow::{Context, Result, bail};
use base64::Engine;
use k256::SecretKey;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Payload carried inside an invite link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitePayload {
    pub token_id: String,
    pub inviter_node_id: String,
    pub inviter_public_key_b64: String,
    pub relay_hosts: Vec<String>,
    pub scopes: Vec<String>,
    pub expires_at_ms: u64,
}

/// A signed invite ready for transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedInvite {
    pub payload: InvitePayload,
    pub signature_b64: String,
}

impl InvitePayload {
    /// Build canonical bytes for signing.
    fn canonical_bytes(&self) -> Vec<u8> {
        // Deterministic serialization: sorted JSON
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// Create a signed invite token.
pub fn create_invite(
    inviter_node_id: &str,
    inviter_public_key_b64: &str,
    inviter_secret: &SecretKey,
    relay_hosts: Vec<String>,
    scopes: Vec<String>,
    ttl_ms: u64,
) -> Result<String> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let payload = InvitePayload {
        token_id: uuid::Uuid::new_v4().to_string(),
        inviter_node_id: inviter_node_id.to_string(),
        inviter_public_key_b64: inviter_public_key_b64.to_string(),
        relay_hosts,
        scopes,
        expires_at_ms: now_ms + ttl_ms,
    };

    let canonical = payload.canonical_bytes();
    let signature_b64 = sign_payload(inviter_secret, &canonical)?;

    let signed = SignedInvite {
        payload,
        signature_b64,
    };

    let json = serde_json::to_vec(&signed).context("failed to serialize invite")?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
}

/// Decode and verify a signed invite token. Returns the payload if valid.
pub fn accept_invite(token: &str) -> Result<InvitePayload> {
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .context("invite token is not valid base64url")?;
    let signed: SignedInvite =
        serde_json::from_slice(&json).context("invite token is not valid JSON")?;

    // Check expiry
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if now_ms > signed.payload.expires_at_ms {
        bail!("invite token has expired");
    }

    // Verify signature
    let canonical = signed.payload.canonical_bytes();
    if !verify_signature(
        &signed.payload.inviter_public_key_b64,
        &canonical,
        &signed.signature_b64,
    ) {
        bail!("invite token signature is invalid");
    }

    Ok(signed.payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::evm_address_from_public_key;
    use rand::rngs::OsRng;

    #[test]
    fn create_accept_round_trip() {
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        let token = create_invite(&node_id, &pub_b64, &secret, vec![], vec![], 60_000).unwrap();
        let payload = accept_invite(&token).unwrap();
        assert_eq!(payload.inviter_node_id, node_id);
    }

    #[test]
    fn expired_invite_rejected() {
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        // TTL of 0 means already expired
        let token = create_invite(&node_id, &pub_b64, &secret, vec![], vec![], 0).unwrap();
        // Sleep to ensure expiry
        std::thread::sleep(std::time::Duration::from_millis(2));
        let result = accept_invite(&token);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    #[test]
    fn tampered_invite_rejected() {
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public);

        let token = create_invite(&node_id, &pub_b64, &secret, vec![], vec![], 60_000).unwrap();

        // Decode, tamper, re-encode
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&token)
            .unwrap();
        let mut signed: SignedInvite = serde_json::from_slice(&json).unwrap();
        signed.payload.inviter_node_id = "0xfake".to_string();
        let tampered_json = serde_json::to_vec(&signed).unwrap();
        let tampered_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tampered_json);

        let result = accept_invite(&tampered_token);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("signature"));
    }

    #[test]
    fn malformed_token_rejected() {
        assert!(accept_invite("not-a-valid-token!!!").is_err());
    }
}
