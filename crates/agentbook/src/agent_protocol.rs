//! Wire protocol for the agentbook-agent credential vault.
//!
//! The agent listens on a Unix socket (default: same dir as the node socket,
//! named `agent.sock`). Each connection carries exactly one request → response.
//! The socket is `0600`, so only the owning user's processes can connect.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default path for the agent socket.
///
/// Checks `$AGENTBOOK_AGENT_SOCK`, then the same dir as the node socket.
pub fn default_agent_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("AGENTBOOK_AGENT_SOCK") {
        return PathBuf::from(p);
    }
    // Mirror the node socket path but use `agent.sock`.
    let node = crate::client::default_socket_path();
    node.with_file_name("agent.sock")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    /// Verify the passphrase against the recovery key and store the KEK in memory.
    Unlock { passphrase: String },
    /// Return the stored KEK (base64-encoded). Fails if locked.
    GetKek,
    /// Clear the KEK from memory.
    Lock,
    /// Return whether the agent is locked.
    Status,
    /// Shut down the agent process.
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentResponse {
    /// Generic success (unlock / lock / stop).
    Ok,
    /// Error with human-readable message.
    Error { message: String },
    /// KEK as base64. Only returned for `GetKek`.
    Kek { kek_b64: String },
    /// Locked state. Only returned for `Status`.
    Status { locked: bool },
}
