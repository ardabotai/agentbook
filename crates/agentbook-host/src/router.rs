use agentbook_crypto::username::validate_username;
use agentbook_proto::host::v1 as host_pb;
use dashmap::DashMap;
use rusqlite::Connection;
use std::collections::HashSet;
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
            CREATE INDEX IF NOT EXISTS idx_usernames_node_id ON usernames(node_id);
            CREATE TABLE IF NOT EXISTS follows (
                follower_node_id  TEXT NOT NULL,
                followed_node_id  TEXT NOT NULL,
                created_at        TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (follower_node_id, followed_node_id)
            );
            CREATE INDEX IF NOT EXISTS idx_follows_followed ON follows(followed_node_id);",
        )
        .expect("failed to create tables");

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

    fn notify_follow(&self, follower_node_id: &str, followed_node_id: &str) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("lock poisoned: {e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO follows (follower_node_id, followed_node_id) VALUES (?1, ?2)",
            rusqlite::params![follower_node_id, followed_node_id],
        )
        .map_err(|e| format!("database error: {e}"))?;
        Ok(())
    }

    fn notify_unfollow(
        &self,
        follower_node_id: &str,
        followed_node_id: &str,
    ) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("lock poisoned: {e}"))?;
        conn.execute(
            "DELETE FROM follows WHERE follower_node_id = ?1 AND followed_node_id = ?2",
            rusqlite::params![follower_node_id, followed_node_id],
        )
        .map_err(|e| format!("database error: {e}"))?;
        Ok(())
    }

    /// Get nodes that a given node follows, joined with the usernames table for pubkey + username.
    fn get_following(&self, node_id: &str) -> Vec<FollowEntryRow> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare(
            "SELECT f.followed_node_id, COALESCE(u.public_key, ''), COALESCE(u.username, '')
             FROM follows f
             LEFT JOIN usernames u ON u.node_id = f.followed_node_id
             WHERE f.follower_node_id = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map([node_id], |row| {
            Ok(FollowEntryRow {
                node_id: row.get(0)?,
                public_key_b64: row.get(1)?,
                username: row.get(2)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// Get followers of a node, joined with the usernames table for pubkey + username.
    fn get_followers(&self, node_id: &str) -> Vec<FollowEntryRow> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare(
            "SELECT f.follower_node_id, COALESCE(u.public_key, ''), COALESCE(u.username, '')
             FROM follows f
             LEFT JOIN usernames u ON u.node_id = f.follower_node_id
             WHERE f.followed_node_id = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map([node_id], |row| {
            Ok(FollowEntryRow {
                node_id: row.get(0)?,
                public_key_b64: row.get(1)?,
                username: row.get(2)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }
}

/// A row from the followers query (joined with usernames).
#[derive(Clone)]
pub struct FollowEntryRow {
    pub node_id: String,
    pub public_key_b64: String,
    pub username: String,
}

/// In-memory router that tracks connected nodes and forwards relay messages.
///
/// Uses `DashMap` for lock-free concurrent access to the senders and endpoints maps.
/// The username directory is behind its own lock and uses `spawn_blocking` for DB ops.
pub struct Router {
    senders: DashMap<String, mpsc::Sender<host_pb::HostFrame>>,
    /// Observed remote addresses per node (for rendezvous lookup).
    observed_endpoints: DashMap<String, Vec<String>>,
    /// Room subscribers: room_id → set of node_ids.
    room_subscribers: DashMap<String, HashSet<String>>,
    directory: Arc<UsernameDirectory>,
    max_connections: usize,
}

impl Router {
    pub fn new(max_connections: usize, data_dir: Option<&Path>) -> Self {
        Self {
            senders: DashMap::new(),
            observed_endpoints: DashMap::new(),
            room_subscribers: DashMap::new(),
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
        self.unsubscribe_all_rooms(node_id);
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

    /// Subscribe a node to a room.
    pub fn subscribe_room(&self, room_id: &str, node_id: &str) {
        self.room_subscribers
            .entry(room_id.to_string())
            .or_default()
            .insert(node_id.to_string());
    }

    /// Unsubscribe a node from a room. Cleans up empty rooms.
    pub fn unsubscribe_room(&self, room_id: &str, node_id: &str) {
        if let Some(mut subscribers) = self.room_subscribers.get_mut(room_id) {
            subscribers.remove(node_id);
            if subscribers.is_empty() {
                drop(subscribers);
                self.room_subscribers.remove(room_id);
            }
        }
    }

    /// Unsubscribe a node from all rooms (called on disconnect).
    pub fn unsubscribe_all_rooms(&self, node_id: &str) {
        let mut empty_rooms = Vec::new();
        for mut entry in self.room_subscribers.iter_mut() {
            entry.value_mut().remove(node_id);
            if entry.value().is_empty() {
                empty_rooms.push(entry.key().clone());
            }
        }
        for room_id in empty_rooms {
            self.room_subscribers.remove(&room_id);
        }
    }

    /// Get senders for all room subscribers except the given node.
    pub fn get_room_subscribers(
        &self,
        room_id: &str,
        exclude_node_id: &str,
    ) -> Vec<mpsc::Sender<host_pb::HostFrame>> {
        let Some(subscribers) = self.room_subscribers.get(room_id) else {
            return vec![];
        };
        subscribers
            .iter()
            .filter(|id| id.as_str() != exclude_node_id)
            .filter_map(|id| self.senders.get(id.as_str()).map(|r| r.value().clone()))
            .collect()
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

    /// Record a follow relationship. Runs SQLite I/O on a blocking thread.
    pub async fn notify_follow(
        &self,
        follower_node_id: &str,
        followed_node_id: &str,
    ) -> Result<(), String> {
        let dir = self.directory.clone();
        let follower = follower_node_id.to_string();
        let followed = followed_node_id.to_string();
        tokio::task::spawn_blocking(move || dir.notify_follow(&follower, &followed))
            .await
            .map_err(|e| format!("spawn_blocking failed: {e}"))?
    }

    /// Remove a follow relationship. Runs SQLite I/O on a blocking thread.
    pub async fn notify_unfollow(
        &self,
        follower_node_id: &str,
        followed_node_id: &str,
    ) -> Result<(), String> {
        let dir = self.directory.clone();
        let follower = follower_node_id.to_string();
        let followed = followed_node_id.to_string();
        tokio::task::spawn_blocking(move || dir.notify_unfollow(&follower, &followed))
            .await
            .map_err(|e| format!("spawn_blocking failed: {e}"))?
    }

    /// Get nodes that a given node follows. Runs SQLite I/O on a blocking thread.
    pub async fn get_following(&self, node_id: &str) -> Vec<FollowEntryRow> {
        let dir = self.directory.clone();
        let node_id = node_id.to_string();
        tokio::task::spawn_blocking(move || dir.get_following(&node_id))
            .await
            .unwrap_or_default()
    }

    /// Get followers of a node. Runs SQLite I/O on a blocking thread.
    pub async fn get_followers(&self, node_id: &str) -> Vec<FollowEntryRow> {
        let dir = self.directory.clone();
        let node_id = node_id.to_string();
        tokio::task::spawn_blocking(move || dir.get_followers(&node_id))
            .await
            .unwrap_or_default()
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

    #[tokio::test]
    async fn follow_and_get_followers() {
        let router = Router::new(10, None);
        // Register usernames so the join returns data
        router
            .register_username("alice", "node-a", "pubkey-a")
            .await
            .unwrap();
        router
            .register_username("bob", "node-b", "pubkey-b")
            .await
            .unwrap();

        // alice follows bob
        router.notify_follow("node-a", "node-b").await.unwrap();

        // bob's followers should include alice
        let followers = router.get_followers("node-b").await;
        assert_eq!(followers.len(), 1);
        assert_eq!(followers[0].node_id, "node-a");
        assert_eq!(followers[0].public_key_b64, "pubkey-a");
        assert_eq!(followers[0].username, "alice");

        // alice has no followers
        let followers = router.get_followers("node-a").await;
        assert!(followers.is_empty());
    }

    #[tokio::test]
    async fn unfollow_removes_relationship() {
        let router = Router::new(10, None);
        router.notify_follow("node-a", "node-b").await.unwrap();
        assert_eq!(router.get_followers("node-b").await.len(), 1);

        router.notify_unfollow("node-a", "node-b").await.unwrap();
        assert!(router.get_followers("node-b").await.is_empty());
    }

    #[tokio::test]
    async fn follow_idempotent() {
        let router = Router::new(10, None);
        router.notify_follow("node-a", "node-b").await.unwrap();
        router.notify_follow("node-a", "node-b").await.unwrap();
        assert_eq!(router.get_followers("node-b").await.len(), 1);
    }

    #[tokio::test]
    async fn follow_persistence() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();

        {
            let router = Router::new(10, Some(data_dir));
            router.notify_follow("node-a", "node-b").await.unwrap();
            assert_eq!(router.get_followers("node-b").await.len(), 1);
        }

        // Reload from disk
        let router = Router::new(10, Some(data_dir));
        let followers = router.get_followers("node-b").await;
        assert_eq!(followers.len(), 1);
        assert_eq!(followers[0].node_id, "node-a");
    }

    #[tokio::test]
    async fn get_following_returns_correct_data() {
        let router = Router::new(10, None);
        router
            .register_username("alice", "node-a", "pubkey-a")
            .await
            .unwrap();
        router
            .register_username("bob", "node-b", "pubkey-b")
            .await
            .unwrap();
        router
            .register_username("carol", "node-c", "pubkey-c")
            .await
            .unwrap();

        // alice follows bob and carol
        router.notify_follow("node-a", "node-b").await.unwrap();
        router.notify_follow("node-a", "node-c").await.unwrap();

        // alice's following list should include bob and carol
        let following = router.get_following("node-a").await;
        assert_eq!(following.len(), 2);
        let ids: Vec<&str> = following.iter().map(|f| f.node_id.as_str()).collect();
        assert!(ids.contains(&"node-b"));
        assert!(ids.contains(&"node-c"));

        // bob follows nobody
        let following = router.get_following("node-b").await;
        assert!(following.is_empty());
    }

    #[tokio::test]
    async fn multiple_followers() {
        let router = Router::new(10, None);
        router.notify_follow("node-a", "node-c").await.unwrap();
        router.notify_follow("node-b", "node-c").await.unwrap();
        let followers = router.get_followers("node-c").await;
        assert_eq!(followers.len(), 2);
    }

    #[tokio::test]
    async fn room_subscribe_and_unsubscribe() {
        let router = Router::new(10, None);
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        router.register("node-a".to_string(), tx, None);

        router.subscribe_room("test-room", "node-a");
        let subs = router.get_room_subscribers("test-room", "");
        assert_eq!(subs.len(), 1);

        router.unsubscribe_room("test-room", "node-a");
        let subs = router.get_room_subscribers("test-room", "");
        assert!(subs.is_empty());
    }

    #[tokio::test]
    async fn room_broadcast_excludes_sender() {
        let router = Router::new(10, None);
        let (tx_a, _rx_a) = tokio::sync::mpsc::channel(16);
        let (tx_b, _rx_b) = tokio::sync::mpsc::channel(16);
        router.register("node-a".to_string(), tx_a, None);
        router.register("node-b".to_string(), tx_b, None);

        router.subscribe_room("chat", "node-a");
        router.subscribe_room("chat", "node-b");

        // Excluding node-a should return only node-b's sender.
        let subs = router.get_room_subscribers("chat", "node-a");
        assert_eq!(subs.len(), 1);
    }

    #[tokio::test]
    async fn unregister_cleans_room_subscriptions() {
        let router = Router::new(10, None);
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        router.register("node-a".to_string(), tx, None);
        router.subscribe_room("room1", "node-a");
        router.subscribe_room("room2", "node-a");

        router.unregister("node-a");

        let subs1 = router.get_room_subscribers("room1", "");
        let subs2 = router.get_room_subscribers("room2", "");
        assert!(subs1.is_empty());
        assert!(subs2.is_empty());
    }
}
