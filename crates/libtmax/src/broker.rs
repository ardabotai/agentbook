use anyhow::{Result, anyhow};
use std::collections::HashMap;
use tokio::sync::{RwLock, broadcast};

use tmax_protocol::{Event, SessionId};

pub struct EventBroker {
    channels: RwLock<HashMap<SessionId, broadcast::Sender<Event>>>,
}

impl EventBroker {
    pub fn new() -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(
        &self,
        session_id: &str,
        capacity: usize,
    ) -> Result<broadcast::Sender<Event>> {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        self.channels
            .write()
            .await
            .insert(session_id.to_string(), tx.clone());
        Ok(tx)
    }

    pub async fn remove(&self, session_id: &str) {
        self.channels.write().await.remove(session_id);
    }

    pub async fn subscribe(&self, session_id: &str) -> Result<broadcast::Receiver<Event>> {
        let tx = self
            .channels
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("session channel not found"))?;
        Ok(tx.subscribe())
    }
}

impl Default for EventBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::EventBroker;
    use tmax_protocol::Event;

    #[tokio::test]
    async fn register_subscribe_remove_cycle() {
        let broker = EventBroker::new();
        let tx = broker.register("s1", 8).await.expect("register channel");
        let mut rx = broker.subscribe("s1").await.expect("subscribe");

        tx.send(Event::SessionCreated {
            session_id: "s1".to_string(),
            label: None,
        })
        .expect("send");

        let evt = rx.recv().await.expect("recv");
        assert!(matches!(evt, Event::SessionCreated { .. }));

        broker.remove("s1").await;
        assert!(broker.subscribe("s1").await.is_err());
    }
}
