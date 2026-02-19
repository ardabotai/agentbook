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
