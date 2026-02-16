use std::collections::HashMap;
use tmax_mesh_proto::host::v1 as host_pb;
use tokio::sync::mpsc;

/// In-memory router that tracks connected nodes and forwards relay messages.
pub struct Router {
    senders: HashMap<String, mpsc::Sender<host_pb::HostFrame>>,
    /// Observed remote addresses per node (for rendezvous lookup).
    pub observed_endpoints: HashMap<String, Vec<String>>,
    max_connections: usize,
}

impl Router {
    pub fn new(max_connections: usize) -> Self {
        Self {
            senders: HashMap::new(),
            observed_endpoints: HashMap::new(),
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

    #[allow(dead_code)] // useful for monitoring/tests
    pub fn connected_count(&self) -> usize {
        self.senders.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_relay() {
        let mut router = Router::new(10);
        let (tx, _rx) = mpsc::channel(1);
        assert!(router.register("a".to_string(), tx, Some("1.2.3.4:5000".to_string())));
        assert!(router.relay("a").is_some());
        assert!(router.relay("b").is_none());
        assert_eq!(router.lookup("a"), vec!["1.2.3.4:5000"]);
    }

    #[test]
    fn capacity_limit() {
        let mut router = Router::new(1);
        let (tx1, _) = mpsc::channel(1);
        let (tx2, _) = mpsc::channel(1);
        assert!(router.register("a".to_string(), tx1, None));
        assert!(!router.register("b".to_string(), tx2, None));
    }

    #[test]
    fn unregister() {
        let mut router = Router::new(10);
        let (tx, _) = mpsc::channel(1);
        router.register("a".to_string(), tx, None);
        router.unregister("a");
        assert!(router.relay("a").is_none());
    }
}
