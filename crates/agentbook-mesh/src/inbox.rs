use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

const INBOX_FILE: &str = "inbox.jsonl";
const ACKED_FILE: &str = "inbox_acked.jsonl";

/// Default maximum number of messages kept in the inbox.
pub const DEFAULT_MAX_INBOX_SIZE: usize = 10_000;

/// Type of message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    #[default]
    Unspecified,
    DmText,
    FeedPost,
    RoomMessage,
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
///
/// Persistence strategy:
/// - New messages are appended to `inbox.jsonl`.
/// - Acks are appended to `inbox_acked.jsonl` (just the message_id).
/// - On load, acked IDs are merged into the message list.
/// - A full rewrite (compaction) only happens when evicting old messages.
///
/// This avoids the O(N) rewrite on every ack while keeping the on-disk
/// format simple.
pub struct NodeInbox {
    path: PathBuf,
    acked_path: PathBuf,
    messages: Vec<InboxMessage>,
    /// Running count of unread (un-acked) messages for O(1) access.
    unread_count: usize,
    /// Maximum number of messages to keep in the inbox.
    max_size: usize,
}

impl NodeInbox {
    /// Load existing messages from disk, or start empty.
    pub fn load(state_dir: &Path) -> Result<Self> {
        Self::load_with_capacity(state_dir, DEFAULT_MAX_INBOX_SIZE)
    }

    /// Load with a custom max inbox size.
    pub fn load_with_capacity(state_dir: &Path, max_size: usize) -> Result<Self> {
        let path = state_dir.join(INBOX_FILE);
        let acked_path = state_dir.join(ACKED_FILE);

        // Load acked IDs from the ack journal.
        let acked_ids: HashSet<String> = if acked_path.exists() {
            let data =
                std::fs::read_to_string(&acked_path).context("failed to read inbox_acked.jsonl")?;
            data.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .collect()
        } else {
            HashSet::new()
        };

        // Load messages and merge ack state.
        let mut messages: Vec<InboxMessage> = if path.exists() {
            let data = std::fs::read_to_string(&path).context("failed to read inbox.jsonl")?;
            data.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let mut msg: InboxMessage =
                        serde_json::from_str(l).context("invalid inbox entry")?;
                    if acked_ids.contains(&msg.message_id) {
                        msg.acked = true;
                    }
                    Ok(msg)
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        // If we loaded more than max_size, compact immediately.
        if messages.len() > max_size {
            evict_to_capacity(&mut messages, max_size);
        }

        let unread_count = messages.iter().filter(|m| !m.acked).count();

        let inbox = Self {
            path,
            acked_path,
            messages,
            unread_count,
            max_size,
        };

        // If we had acked IDs to merge, compact the files so next load is clean.
        if !acked_ids.is_empty() {
            inbox.compact()?;
        }

        Ok(inbox)
    }

    /// Push a new message, evicting old acked messages if at capacity.
    pub fn push(&mut self, msg: InboxMessage) -> Result<()> {
        let is_unread = !msg.acked;

        // Evict if at capacity before pushing.
        if self.messages.len() >= self.max_size {
            let evicted = evict_to_capacity(&mut self.messages, self.max_size.saturating_sub(1));
            self.unread_count = self.unread_count.saturating_sub(evicted);
            self.compact()?;
        }

        // Append to disk.
        let line = serde_json::to_string(&msg)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        writeln!(file, "{line}")?;

        self.messages.push(msg);
        if is_unread {
            self.unread_count += 1;
        }
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

    /// List messages filtered by topic (room name), with optional limit.
    pub fn list_by_topic(&self, topic: &str, limit: Option<usize>) -> Vec<&InboxMessage> {
        let iter = self
            .messages
            .iter()
            .filter(|m| m.topic.as_deref() == Some(topic));
        match limit {
            Some(n) => iter.take(n).collect(),
            None => iter.collect(),
        }
    }

    /// Mark a message as acknowledged.
    ///
    /// Instead of rewriting the entire inbox file, we append the acked
    /// message ID to a separate journal file. The journal is merged on
    /// load and cleared during compaction.
    pub fn ack(&mut self, message_id: &str) -> Result<bool> {
        if let Some(msg) = self
            .messages
            .iter_mut()
            .find(|m| m.message_id == message_id)
        {
            if !msg.acked {
                msg.acked = true;
                self.unread_count = self.unread_count.saturating_sub(1);
            }
            // Append to ack journal instead of rewriting the whole file.
            self.append_ack(message_id)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Get unread count in O(1).
    pub fn unread_count(&self) -> usize {
        self.unread_count
    }

    /// Current total message count.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the inbox is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Append a single acked message ID to the journal file.
    fn append_ack(&self, message_id: &str) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.acked_path)
            .with_context(|| format!("failed to open {}", self.acked_path.display()))?;
        writeln!(file, "{message_id}")?;
        Ok(())
    }

    /// Compact: rewrite inbox.jsonl with current state and clear the ack journal.
    fn compact(&self) -> Result<()> {
        let mut file = std::fs::File::create(&self.path)
            .with_context(|| format!("failed to rewrite {}", self.path.display()))?;
        for msg in &self.messages {
            let line = serde_json::to_string(msg)?;
            writeln!(file, "{line}")?;
        }
        // Clear ack journal since all ack state is now in the main file.
        if self.acked_path.exists() {
            std::fs::File::create(&self.acked_path)
                .with_context(|| format!("failed to clear {}", self.acked_path.display()))?;
        }
        Ok(())
    }
}

/// Evict oldest acked messages until `messages.len() <= target_size`.
/// Returns the number of unread messages that were evicted (should be 0
/// unless all acked messages are already gone).
fn evict_to_capacity(messages: &mut Vec<InboxMessage>, target_size: usize) -> usize {
    if messages.len() <= target_size {
        return 0;
    }

    let to_remove = messages.len() - target_size;

    // First pass: remove oldest acked messages (they appear earliest in the vec).
    let mut removed = 0;
    let mut unread_evicted = 0;
    messages.retain(|msg| {
        if removed >= to_remove {
            return true;
        }
        if msg.acked {
            removed += 1;
            false
        } else {
            true
        }
    });

    // If we still need to evict more (not enough acked messages), remove oldest unread.
    if messages.len() > target_size {
        let still_over = messages.len() - target_size;
        unread_evicted = still_over;
        // Remove from the front (oldest first).
        messages.drain(..still_over);
    }

    unread_evicted
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

    #[test]
    fn ack_does_not_rewrite_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = NodeInbox::load(dir.path()).unwrap();
        inbox.push(make_msg("1")).unwrap();
        inbox.push(make_msg("2")).unwrap();

        // Record main file content before ack.
        let before = std::fs::read_to_string(dir.path().join(INBOX_FILE)).unwrap();
        inbox.ack("1").unwrap();
        let after = std::fs::read_to_string(dir.path().join(INBOX_FILE)).unwrap();

        // Main file should NOT have been rewritten (still contains unacked version).
        assert_eq!(before, after);

        // But the ack journal should exist.
        let acked = std::fs::read_to_string(dir.path().join(ACKED_FILE)).unwrap();
        assert!(acked.contains("1"));
    }

    #[test]
    fn max_size_evicts_acked_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = NodeInbox::load_with_capacity(dir.path(), 5).unwrap();

        // Push 5 messages and ack the first 3.
        for i in 1..=5 {
            inbox.push(make_msg(&i.to_string())).unwrap();
        }
        for i in 1..=3 {
            inbox.ack(&i.to_string()).unwrap();
        }

        assert_eq!(inbox.len(), 5);
        assert_eq!(inbox.unread_count(), 2);

        // Push one more — should evict an acked message.
        inbox.push(make_msg("6")).unwrap();
        assert_eq!(inbox.len(), 5);
        assert_eq!(inbox.unread_count(), 3); // 4, 5, 6 are unread

        // Verify acked messages were evicted, not unread ones.
        let ids: Vec<&str> = inbox
            .list(false, None)
            .iter()
            .map(|m| m.message_id.as_str())
            .collect();
        // Should not contain any of the first 3 acked messages (or at most some).
        // The 3 unread (4, 5) + new (6) must be present.
        assert!(ids.contains(&"4"));
        assert!(ids.contains(&"5"));
        assert!(ids.contains(&"6"));
    }

    #[test]
    fn max_size_evicts_oldest_unread_when_no_acked() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = NodeInbox::load_with_capacity(dir.path(), 3).unwrap();

        for i in 1..=3 {
            inbox.push(make_msg(&i.to_string())).unwrap();
        }
        assert_eq!(inbox.len(), 3);

        // Push one more with no acked messages — oldest unread is evicted.
        inbox.push(make_msg("4")).unwrap();
        assert_eq!(inbox.len(), 3);
        assert_eq!(inbox.unread_count(), 3);

        let ids: Vec<&str> = inbox
            .list(false, None)
            .iter()
            .map(|m| m.message_id.as_str())
            .collect();
        assert!(!ids.contains(&"1")); // oldest was evicted
        assert!(ids.contains(&"2"));
        assert!(ids.contains(&"3"));
        assert!(ids.contains(&"4"));
    }

    #[test]
    fn unread_count_is_accurate_after_operations() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = NodeInbox::load(dir.path()).unwrap();

        assert_eq!(inbox.unread_count(), 0);

        inbox.push(make_msg("1")).unwrap();
        assert_eq!(inbox.unread_count(), 1);

        inbox.push(make_msg("2")).unwrap();
        assert_eq!(inbox.unread_count(), 2);

        inbox.ack("1").unwrap();
        assert_eq!(inbox.unread_count(), 1);

        // Double-ack should not underflow.
        inbox.ack("1").unwrap();
        assert_eq!(inbox.unread_count(), 1);

        inbox.ack("2").unwrap();
        assert_eq!(inbox.unread_count(), 0);

        // Ack non-existent message.
        let found = inbox.ack("999").unwrap();
        assert!(!found);
        assert_eq!(inbox.unread_count(), 0);
    }

    #[test]
    fn persistence_with_eviction() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut inbox = NodeInbox::load_with_capacity(dir.path(), 3).unwrap();
            for i in 1..=3 {
                inbox.push(make_msg(&i.to_string())).unwrap();
            }
            inbox.ack("1").unwrap();
            inbox.ack("2").unwrap();
            // Push triggers eviction of acked messages.
            inbox.push(make_msg("4")).unwrap();
        }
        // Reload and verify state is consistent.
        let inbox = NodeInbox::load_with_capacity(dir.path(), 3).unwrap();
        assert_eq!(inbox.len(), 3);
        assert_eq!(inbox.unread_count(), 2); // 3 and 4 are unread

        let ids: Vec<&str> = inbox
            .list(false, None)
            .iter()
            .map(|m| m.message_id.as_str())
            .collect();
        assert!(ids.contains(&"3"));
        assert!(ids.contains(&"4"));
    }

    #[test]
    fn list_by_topic_filters_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = NodeInbox::load(dir.path()).unwrap();

        // Push messages with different topics.
        let mut msg1 = make_msg("1");
        msg1.topic = Some("room-a".to_string());
        msg1.message_type = MessageType::RoomMessage;
        inbox.push(msg1).unwrap();

        let mut msg2 = make_msg("2");
        msg2.topic = Some("room-b".to_string());
        msg2.message_type = MessageType::RoomMessage;
        inbox.push(msg2).unwrap();

        let mut msg3 = make_msg("3");
        msg3.topic = Some("room-a".to_string());
        msg3.message_type = MessageType::RoomMessage;
        inbox.push(msg3).unwrap();

        // No topic (regular message).
        inbox.push(make_msg("4")).unwrap();

        let room_a = inbox.list_by_topic("room-a", None);
        assert_eq!(room_a.len(), 2);
        assert_eq!(room_a[0].message_id, "1");
        assert_eq!(room_a[1].message_id, "3");

        let room_b = inbox.list_by_topic("room-b", None);
        assert_eq!(room_b.len(), 1);

        // With limit.
        let limited = inbox.list_by_topic("room-a", Some(1));
        assert_eq!(limited.len(), 1);

        // Non-existent topic.
        let empty = inbox.list_by_topic("room-c", None);
        assert!(empty.is_empty());
    }
}
