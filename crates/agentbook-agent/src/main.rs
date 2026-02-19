//! agentbook-agent — in-memory credential vault for the agentbook node daemon.
//!
//! Holds the recovery KEK in memory so the node daemon can restart without
//! prompting for a passphrase. Never writes credentials to disk.
//!
//! Security model:
//!   - Unix socket is `0600` in a `0700` directory — only the owning user's
//!     processes (including the node daemon) can connect.
//!   - Root (`sudo`) can always access the socket (expected Unix behaviour).
//!   - The KEK is stored in `Zeroizing` memory; it is wiped on `lock` and on exit.
//!   - The agent must be unlocked once after every login (interactive or 1Password).

use agentbook::agent_protocol::{AgentRequest, AgentResponse, default_agent_socket_path};
use agentbook_mesh::recovery;
use anyhow::{Context, Result};
use base64::Engine as _;
use clap::Parser;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(name = "agentbook-agent", about = "agentbook credential vault (in-memory KEK store)")]
struct Args {
    /// Path to the agent Unix socket.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// State directory (contains recovery.key).
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Unlock immediately on start: try 1Password first, then prompt interactively.
    #[arg(long)]
    unlock: bool,
}

struct AgentState {
    kek: Option<Zeroizing<[u8; 32]>>,
    state_dir: PathBuf,
}

impl Drop for AgentState {
    fn drop(&mut self) {
        // Ensure KEK is wiped when the agent exits.
        self.kek = None;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "agentbook_agent=info".to_string()),
        )
        .init();

    let args = Args::parse();

    let state_dir = args.state_dir.unwrap_or_else(|| {
        agentbook_mesh::state_dir::default_state_dir().expect("failed to determine state dir")
    });
    let recovery_key_path = state_dir.join("recovery.key");

    if !recovery::has_recovery_key(&recovery_key_path) {
        eprintln!("Node not set up. Run: agentbook-cli setup");
        std::process::exit(1);
    }

    let socket_path = args.socket.unwrap_or_else(default_agent_socket_path);

    // Remove stale socket if it exists.
    let _ = std::fs::remove_file(&socket_path);

    // Ensure socket directory exists with 0700 permissions.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }

    let state = Arc::new(Mutex::new(AgentState {
        kek: None,
        state_dir: state_dir.clone(),
    }));

    // Unlock on start if requested (1Password → interactive prompt).
    if args.unlock {
        let kek = unlock_interactively(&recovery_key_path)?;
        state.lock().unwrap().kek = Some(kek);
        eprintln!("  \x1b[1;32mAgent unlocked.\x1b[0m");
    } else {
        eprintln!("  Agent started (locked). Run: agentbook-cli agent unlock");
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind agent socket at {}", socket_path.display()))?;

    // Socket must be 0600 — only the owning user can connect.
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;

    tracing::info!(socket = %socket_path.display(), "agent listening");

    let shutdown = Arc::new(tokio::sync::Notify::new());

    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let shutdown = shutdown.clone();
                        let recovery_key_path = recovery_key_path.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, state, &recovery_key_path, shutdown).await {
                                tracing::debug!(err = %e, "connection error");
                            }
                        });
                    }
                    Err(e) => tracing::warn!(err = %e, "accept error"),
                }
            }
            _ = shutdown.notified() => {
                tracing::info!("shutdown requested");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT received, shutting down");
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

async fn handle_connection(
    stream: UnixStream,
    state: Arc<Mutex<AgentState>>,
    recovery_key_path: &Path,
    shutdown: Arc<tokio::sync::Notify>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let line = lines
        .next_line()
        .await?
        .context("connection closed without request")?;

    let request: AgentRequest =
        serde_json::from_str(&line).context("invalid request JSON")?;

    let response = process_request(request, state, recovery_key_path, shutdown).await;

    let mut json = serde_json::to_string(&response)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    Ok(())
}

async fn process_request(
    request: AgentRequest,
    state: Arc<Mutex<AgentState>>,
    recovery_key_path: &Path,
    shutdown: Arc<tokio::sync::Notify>,
) -> AgentResponse {
    match request {
        AgentRequest::Unlock { passphrase } => {
            let passphrase = Zeroizing::new(passphrase);
            match recovery::load_recovery_key(recovery_key_path, &passphrase) {
                Ok(kek) => {
                    state.lock().unwrap().kek = Some(kek);
                    tracing::info!("agent unlocked");
                    AgentResponse::Ok
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("wrong passphrase") {
                        AgentResponse::Error {
                            message: "wrong passphrase".to_string(),
                        }
                    } else {
                        AgentResponse::Error { message: msg }
                    }
                }
            }
        }

        AgentRequest::GetKek => {
            let guard = state.lock().unwrap();
            match &guard.kek {
                Some(kek) => {
                    let kek_b64 = base64::engine::general_purpose::STANDARD.encode(kek.as_ref());
                    AgentResponse::Kek { kek_b64 }
                }
                None => AgentResponse::Error {
                    message: "agent is locked — run: agentbook-cli agent unlock".to_string(),
                },
            }
        }

        AgentRequest::Lock => {
            state.lock().unwrap().kek = None;
            tracing::info!("agent locked");
            AgentResponse::Ok
        }

        AgentRequest::Status => {
            let locked = state.lock().unwrap().kek.is_none();
            AgentResponse::Status { locked }
        }

        AgentRequest::Stop => {
            tracing::info!("stop requested");
            shutdown.notify_one();
            AgentResponse::Ok
        }
    }
}

/// Try 1Password first, then fall back to interactive passphrase prompt.
fn unlock_interactively(recovery_key_path: &Path) -> Result<Zeroizing<[u8; 32]>> {
    let state_dir = recovery_key_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let op_title = agentbook_wallet::onepassword::item_title_from_state_dir(state_dir);

    if let Some(ref title) = op_title
        && agentbook_wallet::onepassword::has_op_cli()
        && agentbook_wallet::onepassword::has_agentbook_item(title)
    {
        eprintln!("  \x1b[1;36m1Password detected — unlocking via biometric...\x1b[0m");
        match agentbook_wallet::onepassword::read_passphrase(title) {
            Ok(passphrase) => {
                let passphrase = Zeroizing::new(passphrase);
                match recovery::load_recovery_key(recovery_key_path, &passphrase) {
                    Ok(kek) => {
                        eprintln!("  \x1b[1;32mUnlocked via 1Password.\x1b[0m");
                        return Ok(kek);
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("wrong passphrase") {
                            eprintln!(
                                "  \x1b[1;31m1Password passphrase didn't match. Falling back to manual entry.\x1b[0m"
                            );
                        } else {
                            return Err(e).context("failed to load recovery key");
                        }
                    }
                }
            }
            Err(_) => {
                eprintln!(
                    "  \x1b[1;33m1Password read failed. Falling back to manual entry.\x1b[0m"
                );
            }
        }
    }

    // Interactive prompt fallback.
    loop {
        let passphrase =
            Zeroizing::new(rpassword::prompt_password("  Enter passphrase to unlock agent: ")
                .context("failed to read passphrase")?);
        match recovery::load_recovery_key(recovery_key_path, &passphrase) {
            Ok(kek) => return Ok(kek),
            Err(e) => {
                if e.to_string().contains("wrong passphrase") {
                    eprintln!("  \x1b[1;31mWrong passphrase. Try again.\x1b[0m");
                } else {
                    return Err(e).context("failed to load recovery key");
                }
            }
        }
    }
}
