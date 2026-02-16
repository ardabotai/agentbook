use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FOLLOWING_FILE: &str = "following.json";
const BLOCKED_FILE: &str = "blocked.json";

/// A node you follow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FollowRecord {
    pub node_id: String,
    pub public_key_b64: String,
    pub username: Option<String>,
    pub relay_hints: Vec<String>,
    pub followed_at_ms: u64,
}

/// A blocked node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockRecord {
    pub node_id: String,
    pub blocked_at_ms: u64,
}

/// Persistent follow graph backed by JSON files.
pub struct FollowStore {
    following_path: PathBuf,
    blocked_path: PathBuf,
    following: Vec<FollowRecord>,
    blocked: Vec<BlockRecord>,
}

impl FollowStore {
    /// Load from disk, or create empty.
    pub fn load(state_dir: &Path) -> Result<Self> {
        let following_path = state_dir.join(FOLLOWING_FILE);
        let blocked_path = state_dir.join(BLOCKED_FILE);

        let following = if following_path.exists() {
            let data = std::fs::read_to_string(&following_path)
                .context("failed to read following.json")?;
            serde_json::from_str(&data).context("invalid following.json")?
        } else {
            Vec::new()
        };

        let blocked = if blocked_path.exists() {
            let data =
                std::fs::read_to_string(&blocked_path).context("failed to read blocked.json")?;
            serde_json::from_str(&data).context("invalid blocked.json")?
        } else {
            Vec::new()
        };

        Ok(Self {
            following_path,
            blocked_path,
            following,
            blocked,
        })
    }

    fn save_following(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.following)?;
        std::fs::write(&self.following_path, data)
            .with_context(|| format!("failed to write {}", self.following_path.display()))
    }

    fn save_blocked(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.blocked)?;
        std::fs::write(&self.blocked_path, data)
            .with_context(|| format!("failed to write {}", self.blocked_path.display()))
    }

    /// Follow a node. Deduplicates by node_id.
    pub fn follow(&mut self, record: FollowRecord) -> Result<()> {
        // Remove from blocked if present
        self.blocked.retain(|b| b.node_id != record.node_id);

        if let Some(existing) = self
            .following
            .iter_mut()
            .find(|f| f.node_id == record.node_id)
        {
            existing.public_key_b64 = record.public_key_b64;
            existing.username = record.username.or(existing.username.take());
            existing.relay_hints = record.relay_hints;
        } else {
            self.following.push(record);
        }
        self.save_following()?;
        self.save_blocked()
    }

    /// Unfollow a node.
    pub fn unfollow(&mut self, node_id: &str) -> Result<()> {
        let before = self.following.len();
        self.following.retain(|f| f.node_id != node_id);
        if self.following.len() == before {
            bail!("not following: {node_id}");
        }
        self.save_following()
    }

    /// Block a node. Removes from following if present.
    pub fn block(&mut self, node_id: &str) -> Result<()> {
        self.following.retain(|f| f.node_id != node_id);
        if !self.blocked.iter().any(|b| b.node_id == node_id) {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            self.blocked.push(BlockRecord {
                node_id: node_id.to_string(),
                blocked_at_ms: now_ms,
            });
        }
        self.save_following()?;
        self.save_blocked()
    }

    /// Check if we follow a given node.
    pub fn is_following(&self, node_id: &str) -> bool {
        self.following.iter().any(|f| f.node_id == node_id)
    }

    /// Check if a node is blocked.
    pub fn is_blocked(&self, node_id: &str) -> bool {
        self.blocked.iter().any(|b| b.node_id == node_id)
    }

    /// Get a follow record by node_id.
    pub fn get(&self, node_id: &str) -> Option<&FollowRecord> {
        self.following.iter().find(|f| f.node_id == node_id)
    }

    /// List all nodes we follow.
    pub fn following(&self) -> &[FollowRecord] {
        &self.following
    }

    /// List all blocked nodes.
    pub fn blocked(&self) -> &[BlockRecord] {
        &self.blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    fn make_follow(id: &str) -> FollowRecord {
        FollowRecord {
            node_id: id.to_string(),
            public_key_b64: format!("pub_{id}"),
            username: None,
            relay_hints: vec![],
            followed_at_ms: now_ms(),
        }
    }

    #[test]
    fn follow_and_unfollow() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FollowStore::load(dir.path()).unwrap();
        assert!(store.following().is_empty());

        store.follow(make_follow("a")).unwrap();
        store.follow(make_follow("b")).unwrap();
        assert_eq!(store.following().len(), 2);
        assert!(store.is_following("a"));

        // Dedup
        store.follow(make_follow("a")).unwrap();
        assert_eq!(store.following().len(), 2);

        store.unfollow("a").unwrap();
        assert_eq!(store.following().len(), 1);
        assert!(!store.is_following("a"));
    }

    #[test]
    fn block_removes_follow() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FollowStore::load(dir.path()).unwrap();
        store.follow(make_follow("a")).unwrap();

        store.block("a").unwrap();
        assert!(!store.is_following("a"));
        assert!(store.is_blocked("a"));
    }

    #[test]
    fn follow_removes_block() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FollowStore::load(dir.path()).unwrap();
        store.block("a").unwrap();
        assert!(store.is_blocked("a"));

        store.follow(make_follow("a")).unwrap();
        assert!(store.is_following("a"));
        assert!(!store.is_blocked("a"));
    }

    #[test]
    fn persistence() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = FollowStore::load(dir.path()).unwrap();
            store.follow(make_follow("x")).unwrap();
            store.block("y").unwrap();
        }
        let store = FollowStore::load(dir.path()).unwrap();
        assert_eq!(store.following().len(), 1);
        assert_eq!(store.following()[0].node_id, "x");
        assert_eq!(store.blocked().len(), 1);
        assert_eq!(store.blocked()[0].node_id, "y");
    }

    #[test]
    fn unfollow_nonexistent_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FollowStore::load(dir.path()).unwrap();
        assert!(store.unfollow("nope").is_err());
    }
}
