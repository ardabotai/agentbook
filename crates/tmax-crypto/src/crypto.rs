use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use k256::ecdh::diffie_hellman;
use k256::ecdsa::signature::{Signer, Verifier};
use k256::ecdsa::{Signature, SigningKey, VerifyingKey};
use k256::{PublicKey, SecretKey};
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::Sha256;
use sha2::digest::Digest as Sha2Digest;
use sha3::Keccak256;
use std::fmt::Write as _;

pub const ENVELOPE_KEY_BYTES: usize = 32;
pub const ENVELOPE_NONCE_BYTES: usize = 12;

/// Derive a pairwise symmetric key from an ECDH shared secret.
pub fn derive_pairwise_key(
    secret: &SecretKey,
    peer_public: &PublicKey,
) -> [u8; ENVELOPE_KEY_BYTES] {
    let shared = diffie_hellman(secret.to_nonzero_scalar(), peer_public.as_affine());
    derive_symmetric_key(b"tmax-message-v1", shared.raw_secret_bytes().as_slice())
}

/// Derive a symmetric key from a label and input key material via SHA-256.
pub fn derive_symmetric_key(label: &[u8], ikm: &[u8]) -> [u8; ENVELOPE_KEY_BYTES] {
    let mut hasher = Sha256::new();
    hasher.update(label);
    hasher.update(ikm);
    let digest = hasher.finalize();
    let mut key = [0u8; ENVELOPE_KEY_BYTES];
    key.copy_from_slice(&digest[..ENVELOPE_KEY_BYTES]);
    key
}

/// Encrypt plaintext with a ChaCha20-Poly1305 key. Returns (ciphertext_b64, nonce_b64).
pub fn encrypt_with_key(
    key: &[u8; ENVELOPE_KEY_BYTES],
    plaintext: &[u8],
) -> Result<(String, String)> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).context("invalid envelope key length")?;
    let mut nonce = [0u8; ENVELOPE_NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| anyhow!("encryption failed"))?;
    Ok((
        base64::engine::general_purpose::STANDARD.encode(ciphertext),
        base64::engine::general_purpose::STANDARD.encode(nonce),
    ))
}

/// Decrypt ciphertext with a ChaCha20-Poly1305 key.
pub fn decrypt_with_key(
    key: &[u8; ENVELOPE_KEY_BYTES],
    ciphertext_b64: &str,
    nonce_b64: &str,
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).context("invalid envelope key length")?;
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(ciphertext_b64)
        .context("ciphertext is not valid base64")?;
    let nonce = decode_nonce_b64(nonce_b64)?;
    cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!("decryption failed"))
}

/// Decode a base64-encoded nonce.
pub fn decode_nonce_b64(nonce_b64: &str) -> Result<[u8; ENVELOPE_NONCE_BYTES]> {
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(nonce_b64)
        .context("nonce is not valid base64")?;
    if nonce.len() != ENVELOPE_NONCE_BYTES {
        bail!("invalid nonce length");
    }
    let mut out = [0u8; ENVELOPE_NONCE_BYTES];
    out.copy_from_slice(&nonce);
    Ok(out)
}

/// Sign a payload with an ECDSA signing key. Returns base64-encoded DER signature.
pub fn sign_payload(secret_key: &SecretKey, payload: &[u8]) -> Result<String> {
    let signing_key = SigningKey::from_slice(&secret_key.to_bytes())
        .context("failed to construct signing key")?;
    let sig: Signature = signing_key.sign(payload);
    Ok(base64::engine::general_purpose::STANDARD.encode(sig.to_der().as_bytes()))
}

/// Verify an ECDSA signature. Returns true if valid.
pub fn verify_signature(public_key_b64: &str, payload: &[u8], signature_b64: &str) -> bool {
    let public_key_bytes = match base64::engine::general_purpose::STANDARD.decode(public_key_b64) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let public_key = match PublicKey::from_sec1_bytes(&public_key_bytes) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let verifying_key = VerifyingKey::from(public_key);
    let signature_bytes = match base64::engine::general_purpose::STANDARD.decode(signature_b64) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let signature = match Signature::from_der(&signature_bytes) {
        Ok(v) => v,
        Err(_) => return false,
    };
    verifying_key.verify(payload, &signature).is_ok()
}

/// Derive an EVM-style address from a secp256k1 public key.
pub fn evm_address_from_public_key(public_key: &PublicKey) -> String {
    let public_bytes = public_key.to_sec1_bytes();
    let uncompressed = if public_bytes.first() == Some(&0x04) {
        &public_bytes[1..]
    } else {
        public_bytes.as_ref()
    };
    let digest = Keccak256::digest(uncompressed);
    let address_bytes = &digest[digest.len() - 20..];
    let mut address = String::from("0x");
    for byte in address_bytes {
        let _ = write!(&mut address, "{byte:02x}");
    }
    address
}

/// Build a canonical message payload for signing.
pub fn canonical_message_payload(
    from_id: Option<&str>,
    to_id: &str,
    topic: Option<&str>,
    body: &str,
    requires_response: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    append_length_prefixed(&mut out, from_id.unwrap_or(""));
    append_length_prefixed(&mut out, to_id);
    append_length_prefixed(&mut out, topic.unwrap_or(""));
    append_length_prefixed(&mut out, body);
    out.push(u8::from(requires_response));
    out
}

/// Append a length-prefixed string to a buffer.
pub fn append_length_prefixed(out: &mut Vec<u8>, value: &str) {
    let len = value.len() as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

/// Generate cryptographically random key material.
pub fn random_key_material() -> [u8; ENVELOPE_KEY_BYTES] {
    let mut out = [0u8; ENVELOPE_KEY_BYTES];
    OsRng.fill_bytes(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = random_key_material();
        let plaintext = b"hello world";
        let (ct, nonce) = encrypt_with_key(&key, plaintext).unwrap();
        let decrypted = decrypt_with_key(&key, &ct, &nonce).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn sign_verify_round_trip() {
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let payload = b"test payload";
        let sig = sign_payload(&secret, payload).unwrap();
        assert!(verify_signature(&pub_b64, payload, &sig));
        assert!(!verify_signature(&pub_b64, b"wrong", &sig));
    }

    #[test]
    fn derive_pairwise_key_is_symmetric() {
        let a = SecretKey::random(&mut OsRng);
        let b = SecretKey::random(&mut OsRng);
        let key_ab = derive_pairwise_key(&a, &b.public_key());
        let key_ba = derive_pairwise_key(&b, &a.public_key());
        assert_eq!(key_ab, key_ba);
    }

    #[test]
    fn evm_address_format() {
        let secret = SecretKey::random(&mut OsRng);
        let addr = evm_address_from_public_key(&secret.public_key());
        assert!(addr.starts_with("0x"));
        assert_eq!(addr.len(), 42);
    }

    #[test]
    fn canonical_payload_deterministic() {
        let p1 = canonical_message_payload(Some("a"), "b", Some("t"), "body", true);
        let p2 = canonical_message_payload(Some("a"), "b", Some("t"), "body", true);
        assert_eq!(p1, p2);
    }
}
