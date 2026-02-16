use crate::agent_config::{self, AgentConfig, OAuthCredentials};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// Messages from the agent process to the TUI.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    TextDelta { delta: String },
    ToolCall { name: String },
    ToolResult { name: String, result: String },
    ApprovalRequest { action: String, details: String },
    NodeEvent { event: serde_json::Value },
    CredentialsUpdated { credentials: OAuthCredentials },
    Done,
}

/// Messages from the TUI to the agent process.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TuiToAgent {
    UserMessage { content: String },
    ApprovalResponse { approved: bool },
}

/// Messages from the agent login process to the TUI.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)] // fields used by serde deserialization
pub enum LoginMessage {
    AuthUrl {
        url: String,
        instructions: Option<String>,
    },
    Prompt {
        message: String,
    },
    AuthResult {
        credentials: OAuthCredentials,
    },
    AuthError {
        error: String,
    },
}

/// Messages from the TUI to the agent login process.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TuiToLogin {
    AuthCode { code: String },
}

/// Manages the agent OAuth login process.
pub struct LoginProcess {
    child: Child,
    stdin_tx: mpsc::Sender<String>,
    pub message_rx: mpsc::Receiver<LoginMessage>,
}

/// Manages the agent child process lifecycle.
pub struct AgentProcess {
    child: Child,
    stdin_tx: mpsc::Sender<String>,
    pub message_rx: mpsc::Receiver<AgentMessage>,
}

impl AgentProcess {
    /// Send a user message to the agent.
    pub async fn send_message(&self, content: &str) -> Result<()> {
        let msg = TuiToAgent::UserMessage {
            content: content.to_string(),
        };
        let line = serde_json::to_string(&msg)?;
        self.stdin_tx
            .send(line)
            .await
            .map_err(|_| anyhow::anyhow!("agent stdin closed"))?;
        Ok(())
    }

    /// Send an approval response to the agent.
    pub async fn send_approval(&self, approved: bool) -> Result<()> {
        let msg = TuiToAgent::ApprovalResponse { approved };
        let line = serde_json::to_string(&msg)?;
        self.stdin_tx
            .send(line)
            .await
            .map_err(|_| anyhow::anyhow!("agent stdin closed"))?;
        Ok(())
    }

    /// Spawn the agent process with a specific config, setting the right env vars.
    pub async fn spawn_with_config(socket_path: &str, config: &AgentConfig) -> Result<Self> {
        let agent_path = find_agent_script()?;
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(32);
        let (msg_tx, message_rx) = mpsc::channel::<AgentMessage>(64);

        let mut cmd = Command::new("node");
        cmd.arg("--import=tsx")
            .arg(&agent_path)
            .arg("--stdio")
            .env("AGENTBOOK_SOCKET", socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        // Set provider-specific env vars
        for (key, value) in agent_config::provider_env_vars(config) {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn agent at {}", agent_path.display()))?;

        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        // Writer task
        tokio::spawn(async move {
            while let Some(line) = stdin_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Reader task
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(msg) = serde_json::from_str::<AgentMessage>(&line)
                    && msg_tx.send(msg).await.is_err()
                {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            stdin_tx,
            message_rx,
        })
    }

    /// Kill the agent process.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

impl LoginProcess {
    /// Spawn the agent in --login mode for OAuth.
    pub async fn spawn(provider: &str) -> Result<Self> {
        let agent_path = find_agent_script()?;
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(32);
        let (msg_tx, message_rx) = mpsc::channel::<LoginMessage>(64);

        let mut child = Command::new("node")
            .arg("--import=tsx")
            .arg(&agent_path)
            .arg("--login")
            .arg(provider)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn agent login at {}", agent_path.display()))?;

        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        // Writer task
        tokio::spawn(async move {
            while let Some(line) = stdin_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Reader task
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(msg) = serde_json::from_str::<LoginMessage>(&line)
                    && msg_tx.send(msg).await.is_err()
                {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            stdin_tx,
            message_rx,
        })
    }

    /// Send the user's authorization code to the login process.
    pub async fn send_code(&self, code: &str) -> Result<()> {
        let msg = TuiToLogin::AuthCode {
            code: code.to_string(),
        };
        let line = serde_json::to_string(&msg)?;
        self.stdin_tx
            .send(line)
            .await
            .map_err(|_| anyhow::anyhow!("login process stdin closed"))?;
        Ok(())
    }

    /// Kill the login process.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

/// Find the agent TypeScript entry point.
fn find_agent_script() -> Result<PathBuf> {
    // Check AGENTBOOK_AGENT_PATH env var
    if let Ok(path) = std::env::var("AGENTBOOK_AGENT_PATH") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    // Check relative to current binary
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        // Development: binary is in target/debug/, agent is at repo root
        for ancestor in dir.ancestors() {
            let candidate = ancestor.join("agent/src/index.ts");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // Check current directory
    let cwd_candidate = PathBuf::from("agent/src/index.ts");
    if cwd_candidate.exists() {
        return Ok(cwd_candidate);
    }

    anyhow::bail!(
        "Could not find agent script. Set AGENTBOOK_AGENT_PATH or run from the repo root."
    )
}
