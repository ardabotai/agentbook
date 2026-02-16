use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

const INBOX_FILE: &str = "inbox.jsonl";

/// Type of message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    #[default]
    Unspecified,
    DmText,
    FeedPost,
}

/// A message record stored in the node inbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxMessage {
    pub message_id: String,
    pub from_node_id: String,
    pub from_public_key_b64: String,
    pub topic: Option<String>,
    pub body: String,
    pub timestamp_ms: u64,
    pub acked: bool,
    #[serde(default)]
    pub message_type: MessageType,
}

/// Append-only node-level inbox persisted as JSONL.
pub struct NodeInbox {
    path: PathBuf,
    messages: Vec<InboxMessage>,
}

impl NodeInbox {
    /// Load existing messages from disk, or start empty.
    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(INBOX_FILE);
        let messages = if path.exists() {
            let data = std::fs::read_to_string(&path).context("failed to read inbox.jsonl")?;
            data.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).context("invalid inbox entry"))
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(Self { path, messages })
    }

    /// Push a new message and append to disk.
    pub fn push(&mut self, msg: InboxMessage) -> Result<()> {
        let line = serde_json::to_string(&msg)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        writeln!(file, "{line}")?;
        self.messages.push(msg);
        Ok(())
    }

    /// List messages, optionally filtering to unread only.
    pub fn list(&self, unread_only: bool, limit: Option<usize>) -> Vec<&InboxMessage> {
        let iter = self.messages.iter().filter(|m| !unread_only || !m.acked);
        match limit {
            Some(n) => iter.take(n).collect(),
            None => iter.collect(),
        }
    }

    /// Mark a message as acknowledged.
    pub fn ack(&mut self, message_id: &str) -> Result<bool> {
        if let Some(msg) = self
            .messages
            .iter_mut()
            .find(|m| m.message_id == message_id)
        {
            msg.acked = true;
            self.rewrite()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn rewrite(&self) -> Result<()> {
        let mut file = std::fs::File::create(&self.path)
            .with_context(|| format!("failed to rewrite {}", self.path.display()))?;
        for msg in &self.messages {
            let line = serde_json::to_string(msg)?;
            writeln!(file, "{line}")?;
        }
        Ok(())
    }

    /// Get unread count.
    pub fn unread_count(&self) -> usize {
        self.messages.iter().filter(|m| !m.acked).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(id: &str) -> InboxMessage {
        InboxMessage {
            message_id: id.to_string(),
            from_node_id: "node-a".to_string(),
            from_public_key_b64: "pub".to_string(),
            topic: None,
            body: "hello".to_string(),
            timestamp_ms: 1000,
            acked: false,
            message_type: MessageType::default(),
        }
    }

    #[test]
    fn push_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = NodeInbox::load(dir.path()).unwrap();
        inbox.push(make_msg("1")).unwrap();
        inbox.push(make_msg("2")).unwrap();
        assert_eq!(inbox.list(false, None).len(), 2);
        assert_eq!(inbox.unread_count(), 2);
    }

    #[test]
    fn ack_message() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = NodeInbox::load(dir.path()).unwrap();
        inbox.push(make_msg("1")).unwrap();
        inbox.ack("1").unwrap();
        assert_eq!(inbox.unread_count(), 0);
        assert_eq!(inbox.list(true, None).len(), 0);
        assert_eq!(inbox.list(false, None).len(), 1);
    }

    #[test]
    fn persistence() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut inbox = NodeInbox::load(dir.path()).unwrap();
            inbox.push(make_msg("1")).unwrap();
            inbox.push(make_msg("2")).unwrap();
            inbox.ack("1").unwrap();
        }
        let inbox = NodeInbox::load(dir.path()).unwrap();
        assert_eq!(inbox.list(false, None).len(), 2);
        assert_eq!(inbox.unread_count(), 1);
    }
}
