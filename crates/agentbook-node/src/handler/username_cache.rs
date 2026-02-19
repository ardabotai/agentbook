use std::collections::HashMap;
use std::path::Path;

/// Local cache of node_id → username, persisted across restarts.
///
/// Populated from:
/// - Follow store entries that already have usernames (seeded on startup)
/// - Relay lookups (forward and reverse) as new usernames are discovered
#[derive(Default)]
pub struct UsernameCache {
    map: HashMap<String, String>,
    state_dir: std::path::PathBuf,
}

impl UsernameCache {
    const FILE: &'static str = "username_cache.json";

    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join(Self::FILE);
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            map,
            state_dir: state_dir.to_path_buf(),
        }
    }

    /// Look up a cached username for a node_id.
    pub fn get(&self, node_id: &str) -> Option<&str> {
        self.map.get(node_id).map(String::as_str)
    }

    /// Insert a node_id → username mapping and persist to disk.
    pub fn insert(&mut self, node_id: String, username: String) {
        if self.map.get(&node_id).map(String::as_str) == Some(&username) {
            return; // already cached, skip the write
        }
        self.map.insert(node_id, username);
        self.save();
    }

    /// Seed from follow records (called once on startup).
    pub fn seed_from_follows<'a>(
        &mut self,
        follows: impl Iterator<Item = (&'a str, &'a str)>,
    ) {
        let mut changed = false;
        for (node_id, username) in follows {
            if !self.map.contains_key(node_id) {
                self.map.insert(node_id.to_string(), username.to_string());
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    fn save(&self) {
        let path = self.state_dir.join(Self::FILE);
        if let Ok(data) = serde_json::to_string(&self.map) {
            std::fs::write(path, data).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn empty_cache_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cache = UsernameCache::load(tmp.path());
        assert!(cache.get("unknown-node").is_none());
    }

    #[test]
    fn insert_and_get() {
        let tmp = TempDir::new().unwrap();
        let mut cache = UsernameCache::load(tmp.path());
        cache.insert("node-1".to_string(), "alice".to_string());
        assert_eq!(cache.get("node-1"), Some("alice"));
        assert!(cache.get("node-2").is_none());
    }

    #[test]
    fn insert_skips_duplicate_write() {
        let tmp = TempDir::new().unwrap();
        let mut cache = UsernameCache::load(tmp.path());
        cache.insert("node-1".to_string(), "alice".to_string());
        // Inserting the same mapping again should be a no-op (no panic, same value).
        cache.insert("node-1".to_string(), "alice".to_string());
        assert_eq!(cache.get("node-1"), Some("alice"));
    }

    #[test]
    fn insert_can_update_username() {
        let tmp = TempDir::new().unwrap();
        let mut cache = UsernameCache::load(tmp.path());
        cache.insert("node-1".to_string(), "alice".to_string());
        // Username can be updated in local cache (relay enforces permanence, not the cache).
        cache.insert("node-1".to_string(), "alice_v2".to_string());
        assert_eq!(cache.get("node-1"), Some("alice_v2"));
    }

    #[test]
    fn seed_from_follows_populates_cache() {
        let tmp = TempDir::new().unwrap();
        let mut cache = UsernameCache::load(tmp.path());
        let follows = vec![("node-a", "alice"), ("node-b", "bob")];
        cache.seed_from_follows(follows.into_iter());
        assert_eq!(cache.get("node-a"), Some("alice"));
        assert_eq!(cache.get("node-b"), Some("bob"));
    }

    #[test]
    fn seed_from_follows_does_not_overwrite_existing() {
        let tmp = TempDir::new().unwrap();
        let mut cache = UsernameCache::load(tmp.path());
        cache.insert("node-a".to_string(), "original".to_string());
        // seed_from_follows should not overwrite existing entries.
        cache.seed_from_follows(std::iter::once(("node-a", "overwritten")));
        assert_eq!(cache.get("node-a"), Some("original"));
    }

    #[test]
    fn persistence_across_loads() {
        let tmp = TempDir::new().unwrap();
        {
            let mut cache = UsernameCache::load(tmp.path());
            cache.insert("node-1".to_string(), "alice".to_string());
            cache.insert("node-2".to_string(), "bob".to_string());
        }
        // Load fresh — should see persisted data.
        let cache = UsernameCache::load(tmp.path());
        assert_eq!(cache.get("node-1"), Some("alice"));
        assert_eq!(cache.get("node-2"), Some("bob"));
        assert!(cache.get("node-3").is_none());
    }

    #[test]
    fn seed_from_follows_persists() {
        let tmp = TempDir::new().unwrap();
        {
            let mut cache = UsernameCache::load(tmp.path());
            let follows = vec![("node-a", "alice")];
            cache.seed_from_follows(follows.into_iter());
        }
        let cache = UsernameCache::load(tmp.path());
        assert_eq!(cache.get("node-a"), Some("alice"));
    }
}
