use std::collections::VecDeque;
use std::time::SystemTime;

/// A sequenced chunk of PTY output.
#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub seq: u64,
    pub data: Vec<u8>,
}

/// In-memory circular buffer of sequenced output chunks.
/// This is the live streaming layer - fast hot path for real-time
/// streaming and reconnect catch-up.
pub struct LiveBuffer {
    chunks: VecDeque<OutputChunk>,
    max_chunks: usize,
    next_seq: u64,
}

impl LiveBuffer {
    pub fn new(max_chunks: usize) -> Self {
        Self {
            chunks: VecDeque::with_capacity(max_chunks),
            max_chunks,
            next_seq: 0,
        }
    }

    /// Append data to the buffer, returning the assigned sequence ID.
    pub fn push(&mut self, data: Vec<u8>) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;

        self.chunks.push_back(OutputChunk {
            seq,
            data,
        });

        // Evict oldest if over capacity
        while self.chunks.len() > self.max_chunks {
            self.chunks.pop_front();
        }

        seq
    }

    /// Get the current next sequence number (i.e., the seq that will be
    /// assigned to the next push).
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Get the oldest sequence number still in the buffer, or None if empty.
    pub fn oldest_seq(&self) -> Option<u64> {
        self.chunks.front().map(|c| c.seq)
    }

    /// Replay chunks starting from `since_seq` (exclusive).
    /// Returns None if the requested seq is too old (evicted from buffer).
    pub fn replay_since(&self, since_seq: u64) -> Option<Vec<OutputChunk>> {
        if self.chunks.is_empty() {
            return Some(vec![]);
        }

        let oldest = self.chunks.front().unwrap().seq;
        if since_seq < oldest.saturating_sub(1) {
            // Gap: client is too far behind, needs snapshot recovery
            return None;
        }

        let result: Vec<OutputChunk> = self
            .chunks
            .iter()
            .filter(|c| c.seq > since_seq)
            .cloned()
            .collect();

        Some(result)
    }

    /// Get all chunks currently in the buffer.
    pub fn all_chunks(&self) -> Vec<OutputChunk> {
        self.chunks.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

/// Marker stored per session, indexed by sequence ID.
#[derive(Debug, Clone)]
pub struct Marker {
    pub name: String,
    pub seq: u64,
    pub timestamp: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_buffer_push_and_replay() {
        let mut buf = LiveBuffer::new(100);
        let s0 = buf.push(b"hello".to_vec());
        let s1 = buf.push(b" world".to_vec());
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.next_seq(), 2);

        // Replay all
        let chunks = buf.replay_since(0).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].data, b" world");

        // Replay from before start
        let all = buf.all_chunks();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn live_buffer_eviction() {
        let mut buf = LiveBuffer::new(3);
        for i in 0..5 {
            buf.push(format!("chunk-{i}").into_bytes());
        }
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.oldest_seq(), Some(2));
        assert_eq!(buf.next_seq(), 5);

        // Can replay from seq 2
        let chunks = buf.replay_since(2).unwrap();
        assert_eq!(chunks.len(), 2); // seq 3, 4

        // Cannot replay from seq 0 (too old)
        assert!(buf.replay_since(0).is_none());
    }

}
