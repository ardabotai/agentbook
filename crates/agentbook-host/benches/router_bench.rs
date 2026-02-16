use std::time::Instant;

// Inline minimal types to avoid needing the full proto dependency in a simple bench
#[allow(dead_code)]
mod bench_router {
    use rusqlite::Connection;
    use std::path::Path;

    #[derive(Clone)]
    pub struct UsernameEntry {
        pub node_id: String,
        pub public_key_b64: String,
    }

    pub struct UsernameDirectory {
        conn: Connection,
    }

    impl UsernameDirectory {
        pub fn open(data_dir: Option<&Path>) -> Self {
            let conn = match data_dir {
                Some(dir) => {
                    std::fs::create_dir_all(dir).ok();
                    let db_path = dir.join("usernames.db");
                    Connection::open(&db_path).expect("open db")
                }
                None => Connection::open_in_memory().expect("in-memory sqlite"),
            };

            conn.pragma_update(None, "journal_mode", "WAL").ok();
            conn.pragma_update(None, "synchronous", "NORMAL").ok();

            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS usernames (
                    username    TEXT PRIMARY KEY NOT NULL,
                    node_id     TEXT NOT NULL UNIQUE,
                    public_key  TEXT NOT NULL,
                    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE INDEX IF NOT EXISTS idx_usernames_node_id ON usernames(node_id);",
            )
            .expect("create table");

            Self { conn }
        }

        pub fn register(
            &self,
            username: &str,
            node_id: &str,
            public_key_b64: &str,
        ) -> Result<(), String> {
            let normalized = username.to_lowercase();

            let existing: Option<String> = self
                .conn
                .query_row(
                    "SELECT node_id FROM usernames WHERE username = ?1",
                    [&normalized],
                    |row| row.get(0),
                )
                .ok();

            if let Some(ref existing_node) = existing
                && existing_node != node_id
            {
                return Err(format!("username @{normalized} is already taken"));
            }

            self.conn
                .execute("DELETE FROM usernames WHERE node_id = ?1", [node_id])
                .ok();

            self.conn
                .execute(
                    "INSERT OR REPLACE INTO usernames (username, node_id, public_key, updated_at)
                     VALUES (?1, ?2, ?3, datetime('now'))",
                    rusqlite::params![normalized, node_id, public_key_b64],
                )
                .map_err(|e| format!("database error: {e}"))?;

            Ok(())
        }

        pub fn lookup(&self, username: &str) -> Option<UsernameEntry> {
            let normalized = username.to_lowercase();
            self.conn
                .query_row(
                    "SELECT node_id, public_key FROM usernames WHERE username = ?1",
                    [&normalized],
                    |row| {
                        Ok(UsernameEntry {
                            node_id: row.get(0)?,
                            public_key_b64: row.get(1)?,
                        })
                    },
                )
                .ok()
        }
    }
}

fn main() {
    let tmp = tempfile::TempDir::new().unwrap();

    println!("=== agentbook-host Router Benchmark ===\n");

    // --- Username Registration Benchmark ---
    {
        let dir = bench_router::UsernameDirectory::open(Some(tmp.path()));
        let n = 10_000;

        let start = Instant::now();
        for i in 0..n {
            dir.register(
                &format!("user{i}"),
                &format!("node-{i}"),
                &format!("pubkey-{i}"),
            )
            .unwrap();
        }
        let elapsed = start.elapsed();
        let per_op = elapsed / n;
        println!(
            "Register {n} usernames (on-disk SQLite):\n  Total: {elapsed:?}\n  Per op: {per_op:?}\n  Throughput: {:.0} ops/sec\n",
            n as f64 / elapsed.as_secs_f64()
        );
    }

    // --- Username Lookup Benchmark (with populated DB) ---
    {
        let dir = bench_router::UsernameDirectory::open(Some(tmp.path()));
        let n = 100_000u32;

        // Lookups of existing usernames
        let start = Instant::now();
        for i in 0..n {
            let idx = i % 10_000;
            let _ = dir.lookup(&format!("user{idx}"));
        }
        let elapsed = start.elapsed();
        let per_op = elapsed / n;
        println!(
            "Lookup {n} usernames (10k entries, on-disk SQLite):\n  Total: {elapsed:?}\n  Per op: {per_op:?}\n  Throughput: {:.0} ops/sec\n",
            n as f64 / elapsed.as_secs_f64()
        );

        // Lookups of non-existent usernames
        let start = Instant::now();
        for i in 0..n {
            let _ = dir.lookup(&format!("nonexistent{i}"));
        }
        let elapsed = start.elapsed();
        let per_op = elapsed / n;
        println!(
            "Lookup {n} non-existent usernames:\n  Total: {elapsed:?}\n  Per op: {per_op:?}\n  Throughput: {:.0} ops/sec\n",
            n as f64 / elapsed.as_secs_f64()
        );
    }

    // --- In-Memory Benchmark (baseline) ---
    {
        let dir = bench_router::UsernameDirectory::open(None);
        let n = 10_000;

        let start = Instant::now();
        for i in 0..n {
            dir.register(
                &format!("user{i}"),
                &format!("node-{i}"),
                &format!("pubkey-{i}"),
            )
            .unwrap();
        }
        let elapsed = start.elapsed();
        println!(
            "Register {n} usernames (in-memory SQLite):\n  Total: {elapsed:?}\n  Throughput: {:.0} ops/sec\n",
            n as f64 / elapsed.as_secs_f64()
        );

        let start = Instant::now();
        for i in 0..100_000u32 {
            let idx = i % 10_000;
            let _ = dir.lookup(&format!("user{idx}"));
        }
        let elapsed = start.elapsed();
        println!(
            "Lookup 100k usernames (in-memory SQLite):\n  Total: {elapsed:?}\n  Throughput: {:.0} ops/sec\n",
            100_000.0 / elapsed.as_secs_f64()
        );
    }

    // --- Large-scale test ---
    {
        let tmp2 = tempfile::TempDir::new().unwrap();
        let dir = bench_router::UsernameDirectory::open(Some(tmp2.path()));
        let n = 100_000;

        println!("--- Large scale: {n} usernames ---\n");

        let start = Instant::now();
        for i in 0..n {
            dir.register(
                &format!("biguser{i}"),
                &format!("bignode-{i}"),
                &format!("bigpubkey-{i}"),
            )
            .unwrap();
        }
        let elapsed = start.elapsed();
        println!(
            "Register {n} usernames (on-disk):\n  Total: {elapsed:?}\n  Throughput: {:.0} ops/sec\n",
            n as f64 / elapsed.as_secs_f64()
        );

        let start = Instant::now();
        for i in 0..n {
            let _ = dir.lookup(&format!("biguser{i}"));
        }
        let elapsed = start.elapsed();
        println!(
            "Lookup {n} usernames ({n} entries, on-disk):\n  Total: {elapsed:?}\n  Throughput: {:.0} ops/sec\n",
            n as f64 / elapsed.as_secs_f64()
        );

        // DB file size
        let db_path = tmp2.path().join("usernames.db");
        if let Ok(meta) = std::fs::metadata(&db_path) {
            println!("DB file size: {:.1} MB", meta.len() as f64 / 1_048_576.0);
        }
    }
}
