use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FRIENDS_FILE: &str = "friends.json";

/// Trust level assigned to a friend, controlling what message types they can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    /// Can receive broadcasts only.
    Public = 0,
    /// Can receive DMs + broadcasts. Default for new friends.
    #[default]
    Follower = 1,
    /// Can receive DMs + broadcasts + task updates.
    Trusted = 2,
    /// Full trust — can send commands.
    Operator = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FriendRecord {
    pub node_id: String,
    pub public_key_b64: String,
    pub alias: Option<String>,
    pub relay_hosts: Vec<String>,
    pub blocked: bool,
    pub added_at_ms: u64,
    #[serde(default)]
    pub trust_tier: TrustTier,
}

/// Persistent friends store backed by a JSON file.
pub struct FriendsStore {
    path: PathBuf,
    friends: Vec<FriendRecord>,
}

impl FriendsStore {
    /// Load from disk, or create empty.
    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(FRIENDS_FILE);
        let friends = if path.exists() {
            let data = std::fs::read_to_string(&path).context("failed to read friends.json")?;
            serde_json::from_str(&data).context("invalid friends.json")?
        } else {
            Vec::new()
        };
        Ok(Self { path, friends })
    }

    fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.friends)?;
        std::fs::write(&self.path, data)
            .with_context(|| format!("failed to write {}", self.path.display()))
    }

    /// Add a friend. Deduplicates by `node_id` — if already present, updates fields.
    pub fn add(&mut self, record: FriendRecord) -> Result<()> {
        if let Some(existing) = self
            .friends
            .iter_mut()
            .find(|f| f.node_id == record.node_id)
        {
            existing.public_key_b64 = record.public_key_b64;
            existing.alias = record.alias.or(existing.alias.take());
            existing.relay_hosts = record.relay_hosts;
            existing.blocked = false;
        } else {
            self.friends.push(record);
        }
        self.save()
    }

    /// Remove a friend by node_id.
    pub fn remove(&mut self, node_id: &str) -> Result<()> {
        let before = self.friends.len();
        self.friends.retain(|f| f.node_id != node_id);
        if self.friends.len() == before {
            bail!("friend not found: {node_id}");
        }
        self.save()
    }

    /// Block a friend.
    pub fn block(&mut self, node_id: &str) -> Result<()> {
        let friend = self
            .friends
            .iter_mut()
            .find(|f| f.node_id == node_id)
            .ok_or_else(|| anyhow::anyhow!("friend not found: {node_id}"))?;
        friend.blocked = true;
        self.save()
    }

    /// Unblock a friend.
    pub fn unblock(&mut self, node_id: &str) -> Result<()> {
        let friend = self
            .friends
            .iter_mut()
            .find(|f| f.node_id == node_id)
            .ok_or_else(|| anyhow::anyhow!("friend not found: {node_id}"))?;
        friend.blocked = false;
        self.save()
    }

    /// Set the trust tier for a friend.
    pub fn set_trust(&mut self, node_id: &str, tier: TrustTier) -> Result<()> {
        let friend = self
            .friends
            .iter_mut()
            .find(|f| f.node_id == node_id)
            .ok_or_else(|| anyhow::anyhow!("friend not found: {node_id}"))?;
        friend.trust_tier = tier;
        self.save()
    }

    /// Get a friend by node_id.
    pub fn get(&self, node_id: &str) -> Option<&FriendRecord> {
        self.friends.iter().find(|f| f.node_id == node_id)
    }

    /// Check if a node_id is a known (non-blocked) friend.
    pub fn is_friend(&self, node_id: &str) -> bool {
        self.friends
            .iter()
            .any(|f| f.node_id == node_id && !f.blocked)
    }

    /// List all friends.
    pub fn list(&self) -> &[FriendRecord] {
        &self.friends
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

    fn make_friend(id: &str) -> FriendRecord {
        FriendRecord {
            node_id: id.to_string(),
            public_key_b64: format!("pub_{id}"),
            alias: None,
            relay_hosts: vec![],
            blocked: false,
            added_at_ms: now_ms(),
            trust_tier: TrustTier::default(),
        }
    }

    #[test]
    fn crud_operations() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FriendsStore::load(dir.path()).unwrap();
        assert!(store.list().is_empty());

        store.add(make_friend("a")).unwrap();
        store.add(make_friend("b")).unwrap();
        assert_eq!(store.list().len(), 2);
        assert!(store.is_friend("a"));

        // Dedup
        store.add(make_friend("a")).unwrap();
        assert_eq!(store.list().len(), 2);

        store.remove("a").unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(!store.is_friend("a"));
    }

    #[test]
    fn block_unblock() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FriendsStore::load(dir.path()).unwrap();
        store.add(make_friend("a")).unwrap();

        store.block("a").unwrap();
        assert!(!store.is_friend("a"));
        assert!(store.get("a").unwrap().blocked);

        store.unblock("a").unwrap();
        assert!(store.is_friend("a"));
    }

    #[test]
    fn persistence() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = FriendsStore::load(dir.path()).unwrap();
            store.add(make_friend("x")).unwrap();
        }
        let store = FriendsStore::load(dir.path()).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].node_id, "x");
    }

    #[test]
    fn remove_nonexistent_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = FriendsStore::load(dir.path()).unwrap();
        assert!(store.remove("nope").is_err());
    }

    #[test]
    fn trust_tier_default_is_follower() {
        assert_eq!(TrustTier::default(), TrustTier::Follower);
    }
}
