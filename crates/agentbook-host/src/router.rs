use agentbook_crypto::username::validate_username;
use agentbook_proto::host::v1 as host_pb;
use dashmap::DashMap;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::mpsc;

/// A registered username entry.
#[derive(Clone)]
pub struct UsernameEntry {
    pub node_id: String,
    pub public_key_b64: String,
}

/// SQLite-backed username directory that persists across relay restarts.
///
/// All operations use `std::sync::Mutex` + `spawn_blocking` to avoid blocking
/// the tokio runtime with synchronous SQLite I/O.
struct UsernameDirectory {
    conn: StdMutex<Connection>,
}

// Safety: rusqlite::Connection is Send but not Sync. We protect it with a std Mutex,
// which makes &UsernameDirectory safe to share across threads.
unsafe impl Sync for UsernameDirectory {}

impl UsernameDirectory {
    fn open(data_dir: Option<&Path>) -> Self {
        let conn = match data_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir).ok();
                let db_path = dir.join("usernames.db");
                Connection::open(&db_path).unwrap_or_else(|e| {
                    tracing::error!(?e, path = %db_path.display(), "failed to open db, falling back to in-memory");
                    Connection::open_in_memory().expect("in-memory sqlite")
                })
            }
            None => Connection::open_in_memory().expect("in-memory sqlite"),
        };

        // WAL mode for better concurrent read performance
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS usernames (
                username    TEXT PRIMARY KEY NOT NULL,
                node_id     TEXT NOT NULL UNIQUE,
                public_key  TEXT NOT NULL,
                created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_usernames_node_id ON usernames(node_id);",
        )
        .expect("failed to create usernames table");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM usernames", [], |row| row.get(0))
            .unwrap_or(0);
        if count > 0 {
            tracing::info!(count, "loaded username directory from disk");
        }

        Self {
            conn: StdMutex::new(conn),
        }
    }

    fn register(&self, username: &str, node_id: &str, public_key_b64: &str) -> Result<(), String> {
        let normalized = username.to_lowercase();

        // Server-side username validation
        validate_username(&normalized)?;

        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("lock poisoned: {e}"))?;

        // Check if this node already has a username (permanent binding)
        let existing_for_node: Option<String> = conn
            .query_row(
                "SELECT username FROM usernames WHERE node_id = ?1",
                [node_id],
                |row| row.get(0),
            )
            .ok();

        if let Some(ref existing_name) = existing_for_node {
            if *existing_name == normalized {
                // Re-registering the same name -- idempotent, allow it
                return Ok(());
            }
            return Err(format!(
                "this identity already has username @{existing_name} — usernames are permanent"
            ));
        }

        // Check if username is taken by a different node
        let existing: Option<String> = conn
            .query_row(
                "SELECT node_id FROM usernames WHERE username = ?1",
                [&normalized],
                |row| row.get(0),
            )
            .ok();

        if existing.is_some() {
            return Err(format!("username @{normalized} is already taken"));
        }

        // Insert new username
        conn.execute(
            "INSERT INTO usernames (username, node_id, public_key)
                 VALUES (?1, ?2, ?3)",
            rusqlite::params![normalized, node_id, public_key_b64],
        )
        .map_err(|e| format!("database error: {e}"))?;

        Ok(())
    }

    fn lookup(&self, username: &str) -> Option<UsernameEntry> {
        let normalized = username.to_lowercase();
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT node_id, public_key FROM usernames WHERE username = ?1",
            [&normalized],
            |row| {
                Ok(UsernameEntry {
                    node_id: row.get(0)?,
                    public_key_b64: row.get(1)?,
                })
            },
        )
        .ok()
    }
}

/// In-memory router that tracks connected nodes and forwards relay messages.
///
/// Uses `DashMap` for lock-free concurrent access to the senders and endpoints maps.
/// The username directory is behind its own lock and uses `spawn_blocking` for DB ops.
pub struct Router {
    senders: DashMap<String, mpsc::Sender<host_pb::HostFrame>>,
    /// Observed remote addresses per node (for rendezvous lookup).
    observed_endpoints: DashMap<String, Vec<String>>,
    directory: Arc<UsernameDirectory>,
    max_connections: usize,
}

impl Router {
    pub fn new(max_connections: usize, data_dir: Option<&Path>) -> Self {
        Self {
            senders: DashMap::new(),
            observed_endpoints: DashMap::new(),
            directory: Arc::new(UsernameDirectory::open(data_dir)),
            max_connections,
        }
    }

    /// Register a node. Returns false if at capacity.
    pub fn register(
        &self,
        node_id: String,
        sender: mpsc::Sender<host_pb::HostFrame>,
        observed_addr: Option<String>,
    ) -> bool {
        if self.senders.len() >= self.max_connections && !self.senders.contains_key(&node_id) {
            return false;
        }
        self.senders.insert(node_id.clone(), sender);
        if let Some(addr) = observed_addr {
            let mut endpoints = self.observed_endpoints.entry(node_id).or_default();
            if !endpoints.contains(&addr) {
                endpoints.push(addr);
            }
        }
        true
    }

    /// Unregister a node (on disconnect).
    pub fn unregister(&self, node_id: &str) {
        self.senders.remove(node_id);
        self.observed_endpoints.remove(node_id);
    }

    /// Get the sender for a target node, cloned so the caller doesn't hold the map entry.
    pub fn get_sender(&self, to_node_id: &str) -> Option<mpsc::Sender<host_pb::HostFrame>> {
        self.senders.get(to_node_id).map(|r| r.value().clone())
    }

    /// Get observed endpoints for a node (for Lookup RPC).
    pub fn lookup_endpoints(&self, node_id: &str) -> Vec<String> {
        self.observed_endpoints
            .get(node_id)
            .map(|r| r.value().clone())
            .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn connected_count(&self) -> usize {
        self.senders.len()
    }

    /// Register a username for a node. Runs SQLite I/O on a blocking thread.
    pub async fn register_username(
        &self,
        username: &str,
        node_id: &str,
        public_key_b64: &str,
    ) -> Result<(), String> {
        let dir = self.directory.clone();
        let username = username.to_string();
        let node_id = node_id.to_string();
        let public_key_b64 = public_key_b64.to_string();
        tokio::task::spawn_blocking(move || dir.register(&username, &node_id, &public_key_b64))
            .await
            .map_err(|e| format!("spawn_blocking failed: {e}"))?
    }

    /// Look up a username. Runs SQLite I/O on a blocking thread.
    pub async fn lookup_username(&self, username: &str) -> Option<UsernameEntry> {
        let dir = self.directory.clone();
        let username = username.to_string();
        tokio::task::spawn_blocking(move || dir.lookup(&username))
            .await
            .ok()?
    }

    /// Check if endpoints map contains a key (for tests).
    #[allow(dead_code)]
    pub fn has_observed_endpoints(&self, node_id: &str) -> bool {
        self.observed_endpoints.contains_key(node_id)
    }

    /// Store data_dir path for persistence test reconstruction.
    /// This is a convenience: tests can create a new Router with the same path
    /// to verify persistence.
    #[allow(dead_code)]
    fn data_dir_path(&self) -> Option<PathBuf> {
        // Not stored — tests pass the path explicitly.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn register_and_relay() {
        let router = Router::new(10, None);
        let (tx, _rx) = mpsc::channel(1);
        assert!(router.register("a".to_string(), tx, Some("1.2.3.4:5000".to_string())));
        assert!(router.get_sender("a").is_some());
        assert!(router.get_sender("b").is_none());
        assert_eq!(router.lookup_endpoints("a"), vec!["1.2.3.4:5000"]);
    }

    #[test]
    fn capacity_limit() {
        let router = Router::new(1, None);
        let (tx1, _) = mpsc::channel(1);
        let (tx2, _) = mpsc::channel(1);
        assert!(router.register("a".to_string(), tx1, None));
        assert!(!router.register("b".to_string(), tx2, None));
    }

    #[test]
    fn unregister() {
        let router = Router::new(10, None);
        let (tx, _) = mpsc::channel(1);
        router.register("a".to_string(), tx, None);
        router.unregister("a");
        assert!(router.get_sender("a").is_none());
    }

    #[test]
    fn unregister_cleans_up_observed_endpoints() {
        let router = Router::new(10, None);
        let (tx, _) = mpsc::channel(1);
        router.register("a".to_string(), tx, Some("1.2.3.4:5000".to_string()));
        assert_eq!(router.lookup_endpoints("a"), vec!["1.2.3.4:5000"]);

        router.unregister("a");
        assert!(router.get_sender("a").is_none());
        assert!(router.lookup_endpoints("a").is_empty());
        assert!(!router.has_observed_endpoints("a"));
    }

    #[tokio::test]
    async fn username_persistence() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();

        // Register a username
        {
            let router = Router::new(10, Some(data_dir));
            router
                .register_username("alice", "node-1", "pubkey-1")
                .await
                .unwrap();
            let entry = router.lookup_username("alice").await.unwrap();
            assert_eq!(entry.node_id, "node-1");
            assert_eq!(entry.public_key_b64, "pubkey-1");
        }

        // Load from a fresh Router -- should still have alice
        {
            let router = Router::new(10, Some(data_dir));
            let entry = router.lookup_username("alice").await.unwrap();
            assert_eq!(entry.node_id, "node-1");
            assert_eq!(entry.public_key_b64, "pubkey-1");
        }
    }

    #[tokio::test]
    async fn username_taken_by_other_node() {
        let router = Router::new(10, None);
        router
            .register_username("alice", "node-1", "pubkey-1")
            .await
            .unwrap();
        let err = router
            .register_username("alice", "node-2", "pubkey-2")
            .await
            .unwrap_err();
        assert!(err.contains("already taken"));
    }

    #[tokio::test]
    async fn username_re_register_same_name_idempotent() {
        let router = Router::new(10, None);
        router
            .register_username("alice", "node-1", "pubkey-1")
            .await
            .unwrap();
        // Re-registering the same name for the same node should succeed
        router
            .register_username("alice", "node-1", "pubkey-1")
            .await
            .unwrap();
        let entry = router.lookup_username("alice").await.unwrap();
        assert_eq!(entry.node_id, "node-1");
    }

    #[tokio::test]
    async fn username_case_insensitive() {
        let router = Router::new(10, None);
        router
            .register_username("Alice", "node-1", "pubkey-1")
            .await
            .unwrap();
        let entry = router.lookup_username("ALICE").await.unwrap();
        assert_eq!(entry.node_id, "node-1");
    }

    #[tokio::test]
    async fn username_permanent_binding() {
        let router = Router::new(10, None);
        router
            .register_username("alice", "node-1", "pubkey-1")
            .await
            .unwrap();
        // Trying to change username should fail
        let err = router
            .register_username("bob", "node-1", "pubkey-1")
            .await
            .unwrap_err();
        assert!(err.contains("permanent"));
        // Original username should still work
        assert!(router.lookup_username("alice").await.is_some());
    }

    #[tokio::test]
    async fn username_server_side_validation() {
        let router = Router::new(10, None);

        // Too short
        let err = router
            .register_username("ab", "node-1", "pubkey-1")
            .await
            .unwrap_err();
        assert!(err.contains("at least 3"));

        // Too long
        let err = router
            .register_username("a".repeat(25).as_str(), "node-2", "pubkey-2")
            .await
            .unwrap_err();
        assert!(err.contains("24 characters"));

        // Invalid characters
        let err = router
            .register_username("al!ce", "node-3", "pubkey-3")
            .await
            .unwrap_err();
        assert!(err.contains("letters, numbers, and underscores"));

        // Valid
        router
            .register_username("valid_user_123", "node-4", "pubkey-4")
            .await
            .unwrap();
    }
}
