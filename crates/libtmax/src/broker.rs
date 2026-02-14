use std::collections::HashMap;
use tokio::sync::broadcast;
use tmax_protocol::{Event, SessionId};

use crate::output::{ClientCursor, OutputChunk};

const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Central event broker that manages per-session broadcast channels.
/// Each session gets its own independent broadcast channel to prevent
/// one noisy session from starving others.
pub struct EventBroker {
    channels: HashMap<SessionId, broadcast::Sender<Event>>,
    channel_capacity: usize,
}

impl EventBroker {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    pub fn with_capacity(channel_capacity: usize) -> Self {
        Self {
            channels: HashMap::new(),
            channel_capacity,
        }
    }

    /// Create a broadcast channel for a session.
    pub fn create_channel(&mut self, session_id: &SessionId) -> broadcast::Sender<Event> {
        let (tx, _) = broadcast::channel(self.channel_capacity);
        self.channels.insert(session_id.clone(), tx.clone());
        tx
    }

    /// Remove a session's broadcast channel.
    pub fn remove_channel(&mut self, session_id: &SessionId) {
        self.channels.remove(session_id);
    }

    /// Subscribe to a session's event stream.
    /// Returns a receiver for new events.
    pub fn subscribe(&self, session_id: &SessionId) -> Option<broadcast::Receiver<Event>> {
        self.channels.get(session_id).map(|tx| tx.subscribe())
    }

    /// Broadcast an event to all subscribers of a session.
    pub fn broadcast(&self, session_id: &SessionId, event: Event) -> Result<usize, BrokerError> {
        match self.channels.get(session_id) {
            Some(tx) => {
                // send returns Err only if there are no receivers, which is fine
                let count = tx.send(event).unwrap_or(0);
                Ok(count)
            }
            None => Err(BrokerError::NoChannel(session_id.clone())),
        }
    }

    pub fn has_channel(&self, session_id: &SessionId) -> bool {
        self.channels.contains_key(session_id)
    }
}

impl Default for EventBroker {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-client subscription state tracking cursors across sessions.
pub struct ClientSubscriptions {
    pub cursors: HashMap<SessionId, ClientCursor>,
}

impl ClientSubscriptions {
    pub fn new() -> Self {
        Self {
            cursors: HashMap::new(),
        }
    }

    pub fn add(&mut self, session_id: SessionId, last_seq: Option<u64>) {
        let mut cursor = ClientCursor::new();
        if let Some(seq) = last_seq {
            cursor.advance(seq);
        }
        self.cursors.insert(session_id, cursor);
    }

    pub fn remove(&mut self, session_id: &SessionId) {
        self.cursors.remove(session_id);
    }

    pub fn advance(&mut self, session_id: &SessionId, seq: u64) {
        if let Some(cursor) = self.cursors.get_mut(session_id) {
            cursor.advance(seq);
        }
    }

    pub fn is_subscribed(&self, session_id: &SessionId) -> bool {
        self.cursors.contains_key(session_id)
    }
}

impl Default for ClientSubscriptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("no channel for session: {0}")]
    NoChannel(SessionId),
}

/// Compute catch-up chunks for a reconnecting client.
/// Returns chunks the client missed, or None if the gap is too large
/// and a snapshot is needed.
pub fn compute_catchup(
    buffer: &crate::output::LiveBuffer,
    last_seq: Option<u64>,
) -> Option<Vec<OutputChunk>> {
    match last_seq {
        Some(seq) => buffer.replay_since(seq),
        None => Some(buffer.all_chunks()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmax_protocol::Event;

    #[tokio::test]
    async fn broker_create_subscribe_broadcast() {
        let mut broker = EventBroker::new();
        let session_id = "sess-1".to_string();

        broker.create_channel(&session_id);
        let mut rx = broker.subscribe(&session_id).unwrap();

        let event = Event::SessionCreated {
            session_id: session_id.clone(),
            label: Some("test".to_string()),
        };

        broker.broadcast(&session_id, event.clone()).unwrap();

        let received = rx.recv().await.unwrap();
        match received {
            Event::SessionCreated { label, .. } => {
                assert_eq!(label, Some("test".to_string()));
            }
            _ => panic!("wrong event"),
        }
    }

    #[tokio::test]
    async fn broker_multiple_subscribers() {
        let mut broker = EventBroker::new();
        let session_id = "sess-1".to_string();

        broker.create_channel(&session_id);
        let mut rx1 = broker.subscribe(&session_id).unwrap();
        let mut rx2 = broker.subscribe(&session_id).unwrap();

        let event = Event::SessionDestroyed {
            session_id: session_id.clone(),
        };

        let count = broker.broadcast(&session_id, event).unwrap();
        assert_eq!(count, 2);

        let _ = rx1.recv().await.unwrap();
        let _ = rx2.recv().await.unwrap();
    }

    #[test]
    fn broker_no_channel_error() {
        let broker = EventBroker::new();
        let result = broker.broadcast(
            &"nonexistent".to_string(),
            Event::SessionDestroyed {
                session_id: "nonexistent".to_string(),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn client_subscriptions_tracking() {
        let mut subs = ClientSubscriptions::new();
        let sid = "sess-1".to_string();

        subs.add(sid.clone(), Some(10));
        assert!(subs.is_subscribed(&sid));
        assert_eq!(subs.cursors[&sid].last_seq_seen, 10);

        subs.advance(&sid, 20);
        assert_eq!(subs.cursors[&sid].last_seq_seen, 20);

        subs.remove(&sid);
        assert!(!subs.is_subscribed(&sid));
    }
}
