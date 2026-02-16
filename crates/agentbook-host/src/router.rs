use agentbook_proto::host::v1 as host_pb;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

/// Server-side username validation. Mirrors the client-side rules as defense in depth.
fn validate_username(username: &str) -> Result<(), String> {
    if username.is_empty() {
        return Err("username cannot be empty".to_string());
    }
    if username.len() < 3 {
        return Err("username must be at least 3 characters".to_string());
    }
    if username.len() > 24 {
        return Err("username must be 24 characters or less".to_string());
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("username can only contain letters, numbers, and underscores".to_string());
    }
    Ok(())
}

/// A registered username entry.
#[derive(Clone)]
pub struct UsernameEntry {
    pub node_id: String,
    pub public_key_b64: String,
}

/// SQLite-backed username directory that persists across relay restarts.
struct UsernameDirectory {
    conn: Connection,
}

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

        Self { conn }
    }

    fn register(&self, username: &str, node_id: &str, public_key_b64: &str) -> Result<(), String> {
        let normalized = username.to_lowercase();

        // Server-side username validation
        validate_username(&normalized)?;

        // Check if this node already has a username (permanent binding)
        let existing_for_node: Option<String> = self
            .conn
            .query_row(
                "SELECT username FROM usernames WHERE node_id = ?1",
                [node_id],
                |row| row.get(0),
            )
            .ok();

        if let Some(ref existing_name) = existing_for_node {
            if *existing_name == normalized {
                // Re-registering the same name — idempotent, allow it
                return Ok(());
            }
            return Err(format!(
                "this identity already has username @{existing_name} — usernames are permanent"
            ));
        }

        // Check if username is taken by a different node
        let existing: Option<String> = self
            .conn
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
        self.conn
            .execute(
                "INSERT INTO usernames (username, node_id, public_key)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![normalized, node_id, public_key_b64],
            )
            .map_err(|e| format!("database error: {e}"))?;

        Ok(())
    }

    fn lookup(&self, username: &str) -> Option<UsernameEntry> {
        let normalized = username.to_lowercase();
        self.conn
            .query_row(
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

/// In-memory router that tracks connected nodes, forwards relay messages,
/// and maintains the persistent username directory.
pub struct Router {
    senders: HashMap<String, mpsc::Sender<host_pb::HostFrame>>,
    /// Observed remote addresses per node (for rendezvous lookup).
    pub observed_endpoints: HashMap<String, Vec<String>>,
    directory: UsernameDirectory,
    max_connections: usize,
}

impl Router {
    pub fn new(max_connections: usize, data_dir: Option<&Path>) -> Self {
        Self {
            senders: HashMap::new(),
            observed_endpoints: HashMap::new(),
            directory: UsernameDirectory::open(data_dir),
            max_connections,
        }
    }

    /// Register a node. Returns false if at capacity.
    pub fn register(
        &mut self,
        node_id: String,
        sender: mpsc::Sender<host_pb::HostFrame>,
        observed_addr: Option<String>,
    ) -> bool {
        if self.senders.len() >= self.max_connections && !self.senders.contains_key(&node_id) {
            return false;
        }
        self.senders.insert(node_id.clone(), sender);
        if let Some(addr) = observed_addr {
            let endpoints = self.observed_endpoints.entry(node_id).or_default();
            if !endpoints.contains(&addr) {
                endpoints.push(addr);
            }
        }
        true
    }

    /// Unregister a node (on disconnect).
    pub fn unregister(&mut self, node_id: &str) {
        self.senders.remove(node_id);
    }

    /// Relay an envelope to the target node. Returns None if the target is not connected.
    pub fn relay(&self, to_node_id: &str) -> Option<&mpsc::Sender<host_pb::HostFrame>> {
        self.senders.get(to_node_id)
    }

    /// Get observed endpoints for a node (for Lookup RPC).
    pub fn lookup(&self, node_id: &str) -> Vec<String> {
        self.observed_endpoints
            .get(node_id)
            .cloned()
            .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn connected_count(&self) -> usize {
        self.senders.len()
    }

    /// Register a username for a node. Persists to SQLite.
    pub fn register_username(
        &mut self,
        username: &str,
        node_id: &str,
        public_key_b64: &str,
    ) -> Result<(), String> {
        self.directory.register(username, node_id, public_key_b64)
    }

    /// Look up a username. Returns the entry if found.
    pub fn lookup_username(&self, username: &str) -> Option<UsernameEntry> {
        self.directory.lookup(username)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn register_and_relay() {
        let mut router = Router::new(10, None);
        let (tx, _rx) = mpsc::channel(1);
        assert!(router.register("a".to_string(), tx, Some("1.2.3.4:5000".to_string())));
        assert!(router.relay("a").is_some());
        assert!(router.relay("b").is_none());
        assert_eq!(router.lookup("a"), vec!["1.2.3.4:5000"]);
    }

    #[test]
    fn capacity_limit() {
        let mut router = Router::new(1, None);
        let (tx1, _) = mpsc::channel(1);
        let (tx2, _) = mpsc::channel(1);
        assert!(router.register("a".to_string(), tx1, None));
        assert!(!router.register("b".to_string(), tx2, None));
    }

    #[test]
    fn unregister() {
        let mut router = Router::new(10, None);
        let (tx, _) = mpsc::channel(1);
        router.register("a".to_string(), tx, None);
        router.unregister("a");
        assert!(router.relay("a").is_none());
    }

    #[test]
    fn username_persistence() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();

        // Register a username
        {
            let mut router = Router::new(10, Some(data_dir));
            router
                .register_username("alice", "node-1", "pubkey-1")
                .unwrap();
            let entry = router.lookup_username("alice").unwrap();
            assert_eq!(entry.node_id, "node-1");
            assert_eq!(entry.public_key_b64, "pubkey-1");
        }

        // Load from a fresh Router — should still have alice
        {
            let router = Router::new(10, Some(data_dir));
            let entry = router.lookup_username("alice").unwrap();
            assert_eq!(entry.node_id, "node-1");
            assert_eq!(entry.public_key_b64, "pubkey-1");
        }
    }

    #[test]
    fn username_taken_by_other_node() {
        let mut router = Router::new(10, None);
        router
            .register_username("alice", "node-1", "pubkey-1")
            .unwrap();
        let err = router
            .register_username("alice", "node-2", "pubkey-2")
            .unwrap_err();
        assert!(err.contains("already taken"));
    }

    #[test]
    fn username_re_register_same_name_idempotent() {
        let mut router = Router::new(10, None);
        router
            .register_username("alice", "node-1", "pubkey-1")
            .unwrap();
        // Re-registering the same name for the same node should succeed
        router
            .register_username("alice", "node-1", "pubkey-1")
            .unwrap();
        let entry = router.lookup_username("alice").unwrap();
        assert_eq!(entry.node_id, "node-1");
    }

    #[test]
    fn username_case_insensitive() {
        let mut router = Router::new(10, None);
        router
            .register_username("Alice", "node-1", "pubkey-1")
            .unwrap();
        let entry = router.lookup_username("ALICE").unwrap();
        assert_eq!(entry.node_id, "node-1");
    }

    #[test]
    fn username_permanent_binding() {
        let mut router = Router::new(10, None);
        router
            .register_username("alice", "node-1", "pubkey-1")
            .unwrap();
        // Trying to change username should fail
        let err = router
            .register_username("bob", "node-1", "pubkey-1")
            .unwrap_err();
        assert!(err.contains("permanent"));
        // Original username should still work
        assert!(router.lookup_username("alice").is_some());
    }

    #[test]
    fn username_server_side_validation() {
        let mut router = Router::new(10, None);

        // Too short
        let err = router
            .register_username("ab", "node-1", "pubkey-1")
            .unwrap_err();
        assert!(err.contains("at least 3"));

        // Too long
        let err = router
            .register_username("a".repeat(25).as_str(), "node-2", "pubkey-2")
            .unwrap_err();
        assert!(err.contains("24 characters"));

        // Invalid characters
        let err = router
            .register_username("al!ce", "node-3", "pubkey-3")
            .unwrap_err();
        assert!(err.contains("letters, numbers, and underscores"));

        // Valid
        router
            .register_username("valid_user_123", "node-4", "pubkey-4")
            .unwrap();
    }
}
