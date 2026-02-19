pub mod client;
pub mod node;
pub mod relay;

use agentbook::protocol::{InboxEntry, Response};
use std::time::Duration;

/// Poll inbox until it contains at least `count` messages, or timeout.
pub async fn poll_inbox_until(
    client: &mut client::TestClient,
    count: usize,
    timeout: Duration,
) -> Vec<InboxEntry> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let entries = client.inbox().await.unwrap_or_default();
        if entries.len() >= count {
            return entries;
        }
        if tokio::time::Instant::now() >= deadline {
            return entries;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Poll room inbox until it contains at least `count` messages, or timeout.
pub async fn poll_room_inbox_until(
    client: &mut client::TestClient,
    room: &str,
    count: usize,
    timeout: Duration,
) -> Vec<InboxEntry> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let entries = client.room_inbox(room).await.unwrap_or_default();
        if entries.len() >= count {
            return entries;
        }
        if tokio::time::Instant::now() >= deadline {
            return entries;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Initialize tracing for tests (only once per process).
pub fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter("agentbook=debug,agentbook_host=debug,agentbook_node=debug")
            .with_test_writer()
            .try_init()
            .ok();
    });
}

/// Extract data from an Ok response, or panic.
pub fn unwrap_ok_data(resp: Response) -> Option<serde_json::Value> {
    match resp {
        Response::Ok { data } => data,
        Response::Error { code, message } => panic!("expected Ok, got Error({code}): {message}"),
        other => panic!("expected Ok, got {other:?}"),
    }
}
