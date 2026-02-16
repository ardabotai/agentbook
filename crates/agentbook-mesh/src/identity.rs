use crate::crypto::{
    ENVELOPE_KEY_BYTES, decrypt_with_key, derive_symmetric_key, encrypt_with_key,
    evm_address_from_public_key,
};
use crate::state_dir::ensure_state_dir;
use anyhow::{Context, Result, bail};
use base64::Engine;
use k256::ecdh::diffie_hellman;
use k256::{PublicKey, SecretKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const NODE_KEY_FILE: &str = "node.key";
const NODE_PUB_FILE: &str = "node.pub";
const NODE_JSON_FILE: &str = "node.json";
const KEYSTORE_LABEL: &[u8] = b"agentbook-node-keystore-v1";

/// Persistent node identity backed by a secp256k1 key pair.
#[derive(Clone)]
pub struct NodeIdentity {
    secret_key: SecretKey,
    pub public_key: PublicKey,
    pub node_id: String,
    pub public_key_b64: String,
    pub state_dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct NodeMetadata {
    node_id: String,
    public_key_b64: String,
    created_at_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct EncryptedKeystore {
    ciphertext_b64: String,
    nonce_b64: String,
}

impl NodeIdentity {
    /// Load an existing identity from `state_dir`, or create a new one.
    ///
    /// The private key is encrypted at rest using `kek` (key-encryption-key)
    /// derived from the recovery key via ChaCha20-Poly1305.
    pub fn load_or_create(state_dir: &Path, kek: &[u8; ENVELOPE_KEY_BYTES]) -> Result<Self> {
        ensure_state_dir(state_dir)?;

        let key_path = state_dir.join(NODE_KEY_FILE);
        let pub_path = state_dir.join(NODE_PUB_FILE);
        let meta_path = state_dir.join(NODE_JSON_FILE);

        if key_path.exists() {
            return Self::load(state_dir, &key_path, &pub_path, &meta_path, kek);
        }

        Self::create(state_dir, &key_path, &pub_path, &meta_path, kek)
    }

    fn load(
        state_dir: &Path,
        key_path: &Path,
        pub_path: &Path,
        meta_path: &Path,
        kek: &[u8; ENVELOPE_KEY_BYTES],
    ) -> Result<Self> {
        let keystore_json = std::fs::read_to_string(key_path)
            .with_context(|| format!("failed to read {}", key_path.display()))?;
        let keystore: EncryptedKeystore =
            serde_json::from_str(&keystore_json).context("invalid keystore format")?;

        let decryption_key = derive_symmetric_key(KEYSTORE_LABEL, kek);
        let secret_bytes = decrypt_with_key(
            &decryption_key,
            &keystore.ciphertext_b64,
            &keystore.nonce_b64,
        )
        .context("failed to decrypt node key (wrong recovery key?)")?;

        let secret_key =
            SecretKey::from_slice(&secret_bytes).context("decrypted key is not valid secp256k1")?;
        let public_key = secret_key.public_key();
        let public_key_b64 =
            base64::engine::general_purpose::STANDARD.encode(public_key.to_sec1_bytes());

        // Verify consistency with stored pub/meta if present
        if pub_path.exists() {
            let stored_pub = std::fs::read_to_string(pub_path)
                .context("failed to read node.pub")?
                .trim()
                .to_string();
            if stored_pub != public_key_b64 {
                bail!("node.pub does not match decrypted key");
            }
        }

        let node_id = if meta_path.exists() {
            let meta_json =
                std::fs::read_to_string(meta_path).context("failed to read node.json")?;
            let meta: NodeMetadata =
                serde_json::from_str(&meta_json).context("invalid node.json")?;
            meta.node_id
        } else {
            evm_address_from_public_key(&public_key)
        };

        Ok(Self {
            secret_key,
            public_key,
            node_id,
            public_key_b64,
            state_dir: state_dir.to_path_buf(),
        })
    }

    fn create(
        state_dir: &Path,
        key_path: &Path,
        pub_path: &Path,
        meta_path: &Path,
        kek: &[u8; ENVELOPE_KEY_BYTES],
    ) -> Result<Self> {
        let secret_key = SecretKey::random(&mut OsRng);
        let public_key = secret_key.public_key();
        let public_key_b64 =
            base64::engine::general_purpose::STANDARD.encode(public_key.to_sec1_bytes());
        let node_id = evm_address_from_public_key(&public_key);

        // Encrypt and persist the private key
        let encryption_key = derive_symmetric_key(KEYSTORE_LABEL, kek);
        let (ciphertext_b64, nonce_b64) =
            encrypt_with_key(&encryption_key, &secret_key.to_bytes())?;
        let keystore = EncryptedKeystore {
            ciphertext_b64,
            nonce_b64,
        };
        let keystore_json = serde_json::to_string_pretty(&keystore)?;
        std::fs::write(key_path, &keystore_json)
            .with_context(|| format!("failed to write {}", key_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to set permissions on {}", key_path.display()))?;
        }

        // Write public key
        std::fs::write(pub_path, &public_key_b64)
            .with_context(|| format!("failed to write {}", pub_path.display()))?;

        // Write metadata
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let meta = NodeMetadata {
            node_id: node_id.clone(),
            public_key_b64: public_key_b64.clone(),
            created_at_ms: now_ms,
        };
        let meta_json = serde_json::to_string_pretty(&meta)?;
        std::fs::write(meta_path, &meta_json)
            .with_context(|| format!("failed to write {}", meta_path.display()))?;

        Ok(Self {
            secret_key,
            public_key,
            node_id,
            public_key_b64,
            state_dir: state_dir.to_path_buf(),
        })
    }

    /// Sign arbitrary payload bytes with this node's key.
    pub fn sign(&self, payload: &[u8]) -> Result<String> {
        crate::crypto::sign_payload(&self.secret_key, payload)
    }

    /// Derive a shared secret with a peer's public key via ECDH.
    pub fn derive_shared_key(&self, peer_public: &PublicKey) -> [u8; ENVELOPE_KEY_BYTES] {
        let shared = diffie_hellman(self.secret_key.to_nonzero_scalar(), peer_public.as_affine());
        derive_symmetric_key(b"agentbook-mesh-v1", shared.raw_secret_bytes().as_slice())
    }

    /// Get a reference to the secret key (for advanced use).
    pub fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// Get the raw 32-byte secret key material (for wallet initialization).
    ///
    /// Wrapped in `Zeroizing` so the copy is wiped from memory when dropped.
    pub fn secret_key_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.secret_key.to_bytes().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{random_key_material, verify_signature};

    #[test]
    fn create_then_load_same_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("node");
        let kek = random_key_material();

        let created = NodeIdentity::load_or_create(&state, &kek).unwrap();
        let loaded = NodeIdentity::load_or_create(&state, &kek).unwrap();

        assert_eq!(created.node_id, loaded.node_id);
        assert_eq!(created.public_key_b64, loaded.public_key_b64);
    }

    #[cfg(unix)]
    #[test]
    fn keystore_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("node");
        let kek = random_key_material();
        let _ = NodeIdentity::load_or_create(&state, &kek).unwrap();

        let key_meta = std::fs::metadata(state.join("node.key")).unwrap();
        assert_eq!(key_meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn sign_verify_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let kek = random_key_material();
        let identity = NodeIdentity::load_or_create(dir.path(), &kek).unwrap();

        let payload = b"test message";
        let sig = identity.sign(payload).unwrap();
        assert!(verify_signature(&identity.public_key_b64, payload, &sig));
    }

    #[test]
    fn wrong_kek_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("node");
        let kek1 = random_key_material();
        let kek2 = random_key_material();

        let _ = NodeIdentity::load_or_create(&state, &kek1).unwrap();
        let result = NodeIdentity::load_or_create(&state, &kek2);
        assert!(result.is_err());
    }

    #[test]
    fn ecdh_shared_key_is_symmetric() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let kek = random_key_material();

        let node_a = NodeIdentity::load_or_create(dir1.path(), &kek).unwrap();
        let node_b = NodeIdentity::load_or_create(dir2.path(), &kek).unwrap();

        let key_ab = node_a.derive_shared_key(&node_b.public_key);
        let key_ba = node_b.derive_shared_key(&node_a.public_key);
        assert_eq!(key_ab, key_ba);
    }
}
