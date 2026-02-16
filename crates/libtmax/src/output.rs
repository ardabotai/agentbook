use anyhow::Result;
use base64::Engine;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub seq: u64,
    pub data: Vec<u8>,
    pub timestamp: SystemTime,
}

#[derive(Debug)]
pub struct LiveBuffer {
    chunks: VecDeque<OutputChunk>,
    max_chunks: usize,
    next_seq: u64,
}

#[derive(Debug)]
pub struct HistoryLog {
    file: File,
    pub path: PathBuf,
    pub write_pos: u64,
}

impl LiveBuffer {
    pub fn new(max_chunks: usize) -> Self {
        Self {
            chunks: VecDeque::with_capacity(max_chunks.max(1)),
            max_chunks: max_chunks.max(1),
            next_seq: 1,
        }
    }

    pub fn push(&mut self, data: Vec<u8>) -> OutputChunk {
        let chunk = OutputChunk {
            seq: self.next_seq,
            data,
            timestamp: SystemTime::now(),
        };
        self.next_seq = self.next_seq.saturating_add(1);
        self.chunks.push_back(chunk.clone());
        while self.chunks.len() > self.max_chunks {
            let _ = self.chunks.pop_front();
        }
        chunk
    }

    pub fn replay_from(&self, last_seq_seen: Option<u64>) -> Vec<OutputChunk> {
        let start_after = last_seq_seen.unwrap_or(0);
        self.chunks
            .iter()
            .filter(|chunk| chunk.seq > start_after)
            .cloned()
            .collect()
    }

    pub fn oldest_seq(&self) -> Option<u64> {
        self.chunks.front().map(|c| c.seq)
    }

    pub fn newest_seq(&self) -> Option<u64> {
        self.chunks.back().map(|c| c.seq)
    }
}

impl HistoryLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let write_pos = file.metadata()?.len();
        Ok(Self {
            file,
            path,
            write_pos,
        })
    }

    pub fn append_chunk(&mut self, chunk: &OutputChunk) -> Result<()> {
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&chunk.data);
        let line = serde_json::json!({
            "seq": chunk.seq,
            "timestamp_ms": chunk
                .timestamp
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            "data_b64": data_b64,
        })
        .to_string();
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.write_pos = self
            .write_pos
            .saturating_add(u64::try_from(line.len() + 1).unwrap_or(0));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{HistoryLog, LiveBuffer};
    use base64::Engine;
    use std::fs;

    #[test]
    fn replay_is_bounded_and_ordered() {
        let mut lb = LiveBuffer::new(2);
        let _ = lb.push(vec![1]);
        let _ = lb.push(vec![2]);
        let _ = lb.push(vec![3]);

        let all = lb.replay_from(None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].data, vec![2]);
        assert_eq!(all[1].data, vec![3]);

        let since_second = lb.replay_from(Some(2));
        assert_eq!(since_second.len(), 1);
        assert_eq!(since_second[0].seq, 3);
    }

    #[test]
    fn history_log_appends_json_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.log");
        let mut log = HistoryLog::open(&path).expect("open history log");

        let mut lb = LiveBuffer::new(8);
        let chunk = lb.push(b"hello".to_vec());
        log.append_chunk(&chunk).expect("append");

        let content = fs::read_to_string(&path).expect("read");
        let line = content.lines().next().expect("line");
        let value: serde_json::Value = serde_json::from_str(line).expect("json");
        assert_eq!(value["seq"].as_u64(), Some(chunk.seq));
        let encoded = value["data_b64"].as_str().expect("data_b64");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("decode");
        assert_eq!(decoded, b"hello");
    }
}
