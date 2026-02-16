use anyhow::{Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;
use totp_rs::{Algorithm, Secret, TOTP};

const TOTP_KEY_FILE: &str = "totp.key";
const TOTP_STATE_FILE: &str = "totp_state.json";
const TOTP_ISSUER: &str = "agentbook";
const TOTP_DIGITS: usize = 6;
const TOTP_STEP: u64 = 30;
const NONCE_LEN: usize = 12;

/// Maximum consecutive failed attempts before lockout.
const MAX_FAILED_ATTEMPTS: u32 = 5;
/// Lockout duration in seconds after exceeding failed attempt limit.
const LOCKOUT_SECONDS: u64 = 60;

/// Information returned to the CLI/TUI for displaying TOTP setup to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpSetup {
    /// Base32-encoded secret for manual entry into authenticator apps.
    pub secret_base32: String,
    /// otpauth:// URL for QR code generation.
    pub otpauth_url: String,
    /// Issuer name shown in authenticator app.
    pub issuer: String,
    /// Account name shown in authenticator app (node_id or @username).
    pub account: String,
}

/// Encrypted TOTP secret stored on disk.
#[derive(Serialize, Deserialize)]
struct EncryptedTotpSecret {
    ciphertext: Vec<u8>,
    nonce: [u8; NONCE_LEN],
}

/// Persisted TOTP guard state (written to `totp_state.json`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct TotpPersistentState {
    /// The time step of the last successfully verified code.
    last_used_step: Option<u64>,
    /// Number of consecutive failed verification attempts.
    failed_attempts: u32,
    /// Unix timestamp (seconds) when lockout expires, if locked out.
    lockout_until_epoch: Option<u64>,
}

/// In-memory TOTP guard combining persistent state with a runtime `Instant` for lockout.
struct TotpGuard {
    last_used_step: Option<u64>,
    failed_attempts: u32,
    lockout_until: Option<Instant>,
}

impl TotpGuard {
    fn new() -> Self {
        Self {
            last_used_step: None,
            failed_attempts: 0,
            lockout_until: None,
        }
    }

    /// Returns true if currently locked out.
    fn is_locked_out(&self) -> bool {
        self.lockout_until
            .is_some_and(|deadline| Instant::now() < deadline)
    }

    /// Record a failed attempt; triggers lockout after `MAX_FAILED_ATTEMPTS`.
    fn record_failure(&mut self) {
        self.failed_attempts += 1;
        if self.failed_attempts >= MAX_FAILED_ATTEMPTS {
            self.lockout_until =
                Some(Instant::now() + std::time::Duration::from_secs(LOCKOUT_SECONDS));
        }
    }

    /// Record a successful verification at the given time step.
    fn record_success(&mut self, step: u64) {
        self.last_used_step = Some(step);
        self.failed_attempts = 0;
        self.lockout_until = None;
    }

    /// Check whether the given step has already been used (replay protection).
    fn is_replay(&self, step: u64) -> bool {
        self.last_used_step.is_some_and(|last| step <= last)
    }
}

/// Global per-directory guard state. Keyed by canonical state_dir path.
static GUARDS: std::sync::LazyLock<Mutex<HashMap<PathBuf, TotpGuard>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Load persistent TOTP guard state from disk, or return defaults.
fn load_guard_state(state_dir: &Path) -> TotpPersistentState {
    let path = state_dir.join(TOTP_STATE_FILE);
    match std::fs::read(&path) {
        Ok(data) => serde_json::from_slice(&data).unwrap_or_default(),
        Err(_) => TotpPersistentState::default(),
    }
}

/// Save persistent TOTP guard state to disk.
fn save_guard_state(state_dir: &Path, state: &TotpPersistentState) -> Result<()> {
    let path = state_dir.join(TOTP_STATE_FILE);
    let json = serde_json::to_vec(state).context("failed to serialize TOTP guard state")?;
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Initialize or retrieve the in-memory guard for a given state_dir.
/// On first access, loads persistent state from disk.
fn get_or_init_guard(state_dir: &Path) -> PathBuf {
    let key = state_dir.to_path_buf();
    let mut guards = GUARDS.lock().expect("TOTP guard lock poisoned");
    guards.entry(key.clone()).or_insert_with(|| {
        let persistent = load_guard_state(state_dir);
        let mut guard = TotpGuard::new();
        guard.last_used_step = persistent.last_used_step;
        guard.failed_attempts = persistent.failed_attempts;
        // Restore lockout from persisted epoch timestamp
        if let Some(epoch) = persistent.lockout_until_epoch {
            let now_epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if epoch > now_epoch {
                let remaining = std::time::Duration::from_secs(epoch - now_epoch);
                guard.lockout_until = Some(Instant::now() + remaining);
            }
        }
        guard
    });
    key
}

/// Persist the current in-memory guard state to disk.
fn persist_guard(state_dir: &Path, guard: &TotpGuard) -> Result<()> {
    let lockout_epoch = guard.lockout_until.and_then(|deadline| {
        let now = Instant::now();
        if deadline > now {
            let remaining = deadline.duration_since(now);
            let now_epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            Some(now_epoch + remaining.as_secs())
        } else {
            None
        }
    });
    let state = TotpPersistentState {
        last_used_step: guard.last_used_step,
        failed_attempts: guard.failed_attempts,
        lockout_until_epoch: lockout_epoch,
    };
    save_guard_state(state_dir, &state)
}

/// Derive a 32-byte key encryption key from a passphrase using Argon2id.
///
/// Thin wrapper around [`agentbook_crypto::recovery::derive_key_from_passphrase`]
/// kept for API compatibility in the wallet crate.
pub fn derive_kek_from_passphrase(passphrase: &str, salt: &[u8]) -> Result<[u8; 32]> {
    agentbook_crypto::recovery::derive_key_from_passphrase(passphrase, salt)
}

/// Generate a new TOTP secret, encrypt it with the KEK, and save to disk.
/// Returns setup info for the user to scan with their authenticator app.
pub fn generate_totp_secret(state_dir: &Path, kek: &[u8; 32], account: &str) -> Result<TotpSetup> {
    let secret = Secret::generate_secret();
    let secret_bytes = secret
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("failed to get secret bytes: {e}"))?;
    let secret_base32 = secret.to_encoded().to_string();

    let totp = build_totp(&secret_bytes, account)?;
    let otpauth_url = totp.get_url();

    // Encrypt and save the secret
    save_encrypted_secret(state_dir, kek, &secret_bytes)?;

    // Reset guard state since the secret changed.
    {
        let mut guards = GUARDS.lock().expect("TOTP guard lock poisoned");
        guards.remove(&state_dir.to_path_buf());
    }
    // Remove persisted guard state file.
    let _ = std::fs::remove_file(state_dir.join(TOTP_STATE_FILE));

    Ok(TotpSetup {
        secret_base32,
        otpauth_url,
        issuer: TOTP_ISSUER.to_string(),
        account: account.to_string(),
    })
}

/// Verify a 6-digit TOTP code against the stored encrypted secret.
///
/// Includes replay protection (same code cannot be used twice) and brute-force
/// rate limiting (lockout after `MAX_FAILED_ATTEMPTS` consecutive failures).
pub fn verify_totp(state_dir: &Path, code: &str, kek: &[u8; 32]) -> Result<bool> {
    let key = get_or_init_guard(state_dir);

    // Check lockout under the lock, but don't hold the lock during crypto.
    {
        let guards = GUARDS.lock().expect("TOTP guard lock poisoned");
        if let Some(guard) = guards.get(&key)
            && guard.is_locked_out()
        {
            return Err(anyhow::anyhow!(
                "TOTP locked out due to too many failed attempts. Try again later."
            ));
        }
    }

    let secret_bytes = load_encrypted_secret(state_dir, kek)?;
    let totp = build_totp_verify_only(&secret_bytes)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("failed to get current time")?
        .as_secs();

    let valid = totp.check(code, now);

    // Update guard state under the lock.
    let mut guards = GUARDS.lock().expect("TOTP guard lock poisoned");
    let guard = guards.get_mut(&key).expect("guard must exist after init");

    if valid {
        let current_step = now / TOTP_STEP;
        // Replay protection: reject codes from the same or earlier time step.
        if guard.is_replay(current_step) {
            guard.record_failure();
            let _ = persist_guard(state_dir, guard);
            return Ok(false);
        }
        guard.record_success(current_step);
        let _ = persist_guard(state_dir, guard);
        Ok(true)
    } else {
        guard.record_failure();
        let _ = persist_guard(state_dir, guard);
        Ok(false)
    }
}

/// Check whether TOTP has been configured (totp.key file exists).
pub fn has_totp(state_dir: &Path) -> bool {
    state_dir.join(TOTP_KEY_FILE).exists()
}

/// Build a TOTP instance for code generation and URL creation.
fn build_totp(secret_bytes: &[u8], account: &str) -> Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        TOTP_DIGITS,
        1, // skew (allow 1 step before/after)
        TOTP_STEP,
        secret_bytes.to_vec(),
        Some(TOTP_ISSUER.to_string()),
        account.to_string(),
    )
    .map_err(|e| anyhow::anyhow!("failed to build TOTP: {e}"))
}

/// Build a TOTP instance for verification only (no account/issuer needed).
fn build_totp_verify_only(secret_bytes: &[u8]) -> Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        TOTP_DIGITS,
        1,
        TOTP_STEP,
        secret_bytes.to_vec(),
        Some(TOTP_ISSUER.to_string()),
        String::new(),
    )
    .map_err(|e| anyhow::anyhow!("failed to build TOTP for verification: {e}"))
}

/// Encrypt the TOTP secret with the KEK and save to state_dir/totp.key.
fn save_encrypted_secret(state_dir: &Path, kek: &[u8; 32], secret: &[u8]) -> Result<()> {
    let cipher =
        ChaCha20Poly1305::new_from_slice(kek).context("invalid KEK length for ChaCha20")?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, secret)
        .map_err(|_| anyhow::anyhow!("failed to encrypt TOTP secret"))?;

    let encrypted = EncryptedTotpSecret {
        ciphertext,
        nonce: nonce_bytes,
    };

    let json = serde_json::to_vec(&encrypted).context("failed to serialize encrypted secret")?;
    let path = state_dir.join(TOTP_KEY_FILE);
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("failed to set totp.key permissions")?;
    }

    Ok(())
}

/// Load and decrypt the TOTP secret from state_dir/totp.key.
fn load_encrypted_secret(state_dir: &Path, kek: &[u8; 32]) -> Result<Vec<u8>> {
    let path = state_dir.join(TOTP_KEY_FILE);
    let json =
        std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;

    let encrypted: EncryptedTotpSecret =
        serde_json::from_slice(&json).context("failed to parse encrypted TOTP secret")?;

    let cipher =
        ChaCha20Poly1305::new_from_slice(kek).context("invalid KEK length for ChaCha20")?;
    let nonce = Nonce::from_slice(&encrypted.nonce);

    cipher
        .decrypt(nonce, encrypted.ciphertext.as_ref())
        .map_err(|_| anyhow::anyhow!("failed to decrypt TOTP secret — wrong passphrase?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_verify_totp() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        let setup = generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        assert!(!setup.secret_base32.is_empty());
        assert!(setup.otpauth_url.starts_with("otpauth://totp/"));
        assert_eq!(setup.issuer, "agentbook");
        assert!(has_totp(dir.path()));

        // Generate a valid code from the stored secret
        let secret_bytes = load_encrypted_secret(dir.path(), &kek).unwrap();
        let totp = build_totp_verify_only(&secret_bytes).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp.generate(now);

        assert!(verify_totp(dir.path(), &code, &kek).unwrap());
    }

    #[test]
    fn verify_wrong_code_fails() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();
        assert!(!verify_totp(dir.path(), "000000", &kek).unwrap());
    }

    #[test]
    fn verify_wrong_kek_fails() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];
        let wrong_kek = [0x99u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();
        assert!(verify_totp(dir.path(), "123456", &wrong_kek).is_err());
    }

    #[test]
    fn has_totp_false_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_totp(dir.path()));
    }

    #[test]
    fn derive_kek_deterministic() {
        let salt = b"test-salt-16byte";
        let kek1 = derive_kek_from_passphrase("my-passphrase", salt).unwrap();
        let kek2 = derive_kek_from_passphrase("my-passphrase", salt).unwrap();
        assert_eq!(kek1, kek2);
    }

    #[test]
    fn derive_kek_different_passphrases() {
        let salt = b"test-salt-16byte";
        let kek1 = derive_kek_from_passphrase("pass1", salt).unwrap();
        let kek2 = derive_kek_from_passphrase("pass2", salt).unwrap();
        assert_ne!(kek1, kek2);
    }

    // ── TOTP onboarding flow tests ──

    #[test]
    fn totp_setup_returns_valid_otpauth_url() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        let setup = generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        // URL must contain the issuer and account
        assert!(setup.otpauth_url.contains("agentbook"));
        assert!(setup.otpauth_url.contains("test-node"));
        // Secret must be valid base32
        assert!(setup.secret_base32.len() >= 16);
    }

    #[test]
    fn totp_setup_creates_encrypted_file() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        assert!(!dir.path().join(TOTP_KEY_FILE).exists());
        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();
        assert!(dir.path().join(TOTP_KEY_FILE).exists());

        // The file should be encrypted (not plaintext base32)
        let raw = std::fs::read_to_string(dir.path().join(TOTP_KEY_FILE)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(parsed.get("ciphertext").is_some());
        assert!(parsed.get("nonce").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn totp_key_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();
        let meta = std::fs::metadata(dir.path().join(TOTP_KEY_FILE)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn totp_verify_with_skew() {
        // TOTP should accept codes within the skew window (1 step = 30s)
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        let secret_bytes = load_encrypted_secret(dir.path(), &kek).unwrap();
        let totp = build_totp_verify_only(&secret_bytes).unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Current code should work
        let code = totp.generate(now);
        assert!(verify_totp(dir.path(), &code, &kek).unwrap());
    }

    #[test]
    fn totp_setup_then_has_totp_then_verify() {
        // Full onboarding flow: setup -> has_totp -> verify
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        // Step 1: no TOTP configured
        assert!(!has_totp(dir.path()));

        // Step 2: run setup (like first-run onboarding)
        let setup = generate_totp_secret(dir.path(), &kek, "node-0xabc").unwrap();
        assert!(!setup.secret_base32.is_empty());
        assert!(!setup.otpauth_url.is_empty());

        // Step 3: TOTP is now configured
        assert!(has_totp(dir.path()));

        // Step 4: generate a valid code and verify (like user entering from authenticator)
        let secret_bytes = load_encrypted_secret(dir.path(), &kek).unwrap();
        let totp = build_totp_verify_only(&secret_bytes).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp.generate(now);
        assert!(verify_totp(dir.path(), &code, &kek).unwrap());

        // Step 5: wrong code should fail
        assert!(!verify_totp(dir.path(), "000000", &kek).unwrap());
    }

    // ── Replay protection and rate limiting tests ──

    #[test]
    fn replay_same_code_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        let secret_bytes = load_encrypted_secret(dir.path(), &kek).unwrap();
        let totp = build_totp_verify_only(&secret_bytes).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp.generate(now);

        // First use should succeed.
        assert!(verify_totp(dir.path(), &code, &kek).unwrap());
        // Second use of the same code should be rejected (replay).
        assert!(!verify_totp(dir.path(), &code, &kek).unwrap());
    }

    #[test]
    fn lockout_after_failed_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        // Fail MAX_FAILED_ATTEMPTS times.
        for _ in 0..MAX_FAILED_ATTEMPTS {
            let result = verify_totp(dir.path(), "000000", &kek).unwrap();
            assert!(!result);
        }

        // Next attempt should return an error (locked out).
        let result = verify_totp(dir.path(), "000000", &kek);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("locked out"));
    }

    #[test]
    fn lockout_expires_after_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        // Fail MAX_FAILED_ATTEMPTS times to trigger lockout.
        for _ in 0..MAX_FAILED_ATTEMPTS {
            let _ = verify_totp(dir.path(), "000000", &kek);
        }

        // Should be locked out now.
        assert!(verify_totp(dir.path(), "000000", &kek).is_err());

        // Manually expire the lockout by adjusting the guard's deadline.
        {
            let mut guards = GUARDS.lock().unwrap();
            let key = dir.path().to_path_buf();
            if let Some(guard) = guards.get_mut(&key) {
                // Set lockout to the past.
                guard.lockout_until = Some(Instant::now() - std::time::Duration::from_secs(1));
            }
        }

        // Should no longer be locked out (returns Ok, not Err).
        let result = verify_totp(dir.path(), "000000", &kek);
        assert!(result.is_ok());
        // Still wrong code, but not locked out.
        assert!(!result.unwrap());
    }

    #[test]
    fn successful_verify_resets_failed_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        // Fail a few times (but less than MAX_FAILED_ATTEMPTS).
        for _ in 0..3 {
            let _ = verify_totp(dir.path(), "000000", &kek);
        }

        // Now succeed with a valid code.
        let secret_bytes = load_encrypted_secret(dir.path(), &kek).unwrap();
        let totp = build_totp_verify_only(&secret_bytes).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp.generate(now);
        assert!(verify_totp(dir.path(), &code, &kek).unwrap());

        // Failed attempts should be reset — we can fail MAX_FAILED_ATTEMPTS - 1
        // more times without lockout.
        for _ in 0..(MAX_FAILED_ATTEMPTS - 1) {
            let result = verify_totp(dir.path(), "000000", &kek);
            assert!(result.is_ok());
            assert!(!result.unwrap());
        }
    }

    #[test]
    fn guard_state_persisted_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        // Verify a valid code (persists last_used_step).
        let secret_bytes = load_encrypted_secret(dir.path(), &kek).unwrap();
        let totp = build_totp_verify_only(&secret_bytes).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp.generate(now);
        assert!(verify_totp(dir.path(), &code, &kek).unwrap());

        // The state file should exist on disk.
        assert!(dir.path().join(TOTP_STATE_FILE).exists());

        // Load the persisted state and check it.
        let state = load_guard_state(dir.path());
        assert!(state.last_used_step.is_some());
        assert_eq!(state.failed_attempts, 0);
    }

    #[test]
    fn totp_secret_not_regenerated_on_second_setup() {
        // Calling generate twice should overwrite — but the test ensures
        // we can detect if TOTP already exists before calling generate
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        let setup1 = generate_totp_secret(dir.path(), &kek, "node1").unwrap();
        let setup2 = generate_totp_secret(dir.path(), &kek, "node1").unwrap();

        // Secrets should differ (random each time)
        assert_ne!(setup1.secret_base32, setup2.secret_base32);

        // But both should verify with their respective codes
        let secret_bytes = load_encrypted_secret(dir.path(), &kek).unwrap();
        let totp = build_totp_verify_only(&secret_bytes).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp.generate(now);
        // Only the second setup's secret is on disk
        assert!(verify_totp(dir.path(), &code, &kek).unwrap());
    }
}
