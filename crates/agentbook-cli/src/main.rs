mod service;
mod setup;
mod update;

use agentbook::client::{NodeClient, default_socket_path};
use agentbook::protocol::{Request, WalletType};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "agentbook-cli", about = "agentbook CLI")]
struct Cli {
    /// Path to the node daemon's Unix socket.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One-time interactive setup: creates identity, recovery key, TOTP, and registers username.
    Setup {
        /// Also create the yolo wallet during setup.
        #[arg(long)]
        yolo: bool,
        /// Custom state directory.
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Start the node daemon.
    Up {
        /// Run in the foreground (default: background).
        #[arg(long)]
        foreground: bool,
        /// State directory.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Relay host address(es). Defaults to agentbook.ardabot.ai.
        #[arg(long)]
        relay_host: Vec<String>,
        /// Disable connecting to any relay host.
        #[arg(long)]
        no_relay: bool,
        /// Base chain RPC URL.
        #[arg(long)]
        rpc_url: Option<String>,
        /// Enable yolo wallet for autonomous agent transactions.
        #[arg(long)]
        yolo: bool,
    },
    /// Stop the node daemon.
    Down,
    /// Show node identity.
    Identity,
    /// Register a username on the relay host.
    Register {
        /// Username to register.
        username: String,
    },
    /// Look up a username on the relay host.
    Lookup {
        /// Username to look up.
        username: String,
    },
    /// Follow a node.
    Follow {
        /// Node ID or @username.
        target: String,
    },
    /// Unfollow a node.
    Unfollow {
        /// Node ID or @username.
        target: String,
    },
    /// Block a node.
    Block {
        /// Node ID or @username.
        target: String,
    },
    /// List nodes you follow.
    Following,
    /// List your followers.
    Followers,
    /// Push local follow data to relay (reconciliation).
    SyncPush {
        /// Confirm the push operation.
        #[arg(long)]
        confirm: bool,
    },
    /// Pull follow data from relay to local store (reconciliation/recovery).
    SyncPull {
        /// Confirm the pull operation.
        #[arg(long)]
        confirm: bool,
    },
    /// Send a DM (requires mutual follow).
    Send {
        /// Recipient node ID or @username.
        to: String,
        /// Message body.
        message: String,
    },
    /// Post to your feed.
    Post {
        /// Message body.
        message: String,
    },
    /// List inbox messages.
    Inbox {
        /// Show only unread messages.
        #[arg(long)]
        unread: bool,
        /// Limit number of messages.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Acknowledge a message.
    Ack {
        /// Message ID to acknowledge.
        message_id: String,
    },
    /// Health check.
    Health,

    // -- Wallet commands --
    /// Show wallet address and balances.
    Wallet {
        /// Show yolo wallet instead of human wallet.
        #[arg(long)]
        yolo: bool,
    },
    /// Send ETH on Base from human wallet (prompts for authenticator code).
    SendEth {
        /// Recipient address (0x...) or @username.
        to: String,
        /// Amount in ETH (e.g. "0.01").
        amount: String,
    },
    /// Send USDC on Base from human wallet (prompts for authenticator code).
    SendUsdc {
        /// Recipient address (0x...) or @username.
        to: String,
        /// Amount in USDC (e.g. "10.00").
        amount: String,
    },
    /// Set up TOTP authenticator (shows QR code and secret).
    SetupTotp,

    // -- Contract & signing commands --
    /// Call a view/pure function on any contract.
    ReadContract {
        /// Contract address (0x...).
        contract: String,
        /// Function name to call.
        function: String,
        /// ABI JSON (inline or @path/to/abi.json).
        #[arg(long)]
        abi: String,
        /// Arguments as a JSON array (default: []).
        #[arg(long, default_value = "[]")]
        args: String,
    },
    /// Send a state-changing transaction to a contract.
    WriteContract {
        /// Contract address (0x...).
        contract: String,
        /// Function name to call.
        function: String,
        /// ABI JSON (inline or @path/to/abi.json).
        #[arg(long)]
        abi: String,
        /// Arguments as a JSON array (default: []).
        #[arg(long, default_value = "[]")]
        args: String,
        /// ETH value to send with the call (e.g. "0.01").
        #[arg(long)]
        value: Option<String>,
        /// Use yolo wallet (no OTP).
        #[arg(long)]
        yolo: bool,
    },
    /// EIP-191 sign a message.
    SignMessage {
        /// Message to sign (hex string 0x... or UTF-8 text).
        message: String,
        /// Use yolo wallet (no OTP).
        #[arg(long)]
        yolo: bool,
    },

    /// Update agentbook to the latest release from GitHub.
    Update {
        /// Skip confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },

    /// Manage the node daemon as a background service (launchd on macOS, systemd on Linux).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Control the in-memory credential agent (agentbook-agent).
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },

    // -- Room commands --
    /// Join a chat room.
    Join {
        /// Room name to join.
        room: String,
        /// Passphrase for secure (encrypted) rooms.
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Leave a chat room.
    Leave {
        /// Room name to leave.
        room: String,
    },
    /// List joined rooms.
    Rooms,
    /// Send a message to a room (140 char limit).
    RoomSend {
        /// Room name.
        room: String,
        /// Message body.
        message: String,
    },
    /// Read messages from a room.
    RoomInbox {
        /// Room name.
        room: String,
        /// Limit number of messages.
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Install and start the node daemon service (starts at login, restarts on crash).
    Install {
        /// State directory.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Relay host address(es).
        #[arg(long)]
        relay_host: Vec<String>,
        /// Disable relay connection.
        #[arg(long)]
        no_relay: bool,
        /// Base chain RPC URL.
        #[arg(long)]
        rpc_url: Option<String>,
        /// Enable yolo wallet mode (skips TOTP, not recommended).
        #[arg(long)]
        yolo: bool,
    },
    /// Stop and remove the node daemon service.
    Uninstall,
    /// Show current service status.
    Status,
}

#[derive(Subcommand)]
enum AgentAction {
    /// Start the agent daemon (prompts for passphrase once via 1Password or interactively).
    Start {
        /// State directory.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Agent socket path.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Run in the foreground (default: background).
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running agent.
    Stop,
    /// Unlock the agent (load KEK into memory). Prompts for passphrase.
    Unlock {
        /// State directory.
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Lock the agent (wipe KEK from memory).
    Lock,
    /// Show whether the agent is locked or unlocked.
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    match cli.command {
        Command::Setup { yolo, state_dir } => setup::cmd_setup(yolo, state_dir).await,
        Command::Up {
            foreground,
            state_dir,
            relay_host,
            no_relay,
            rpc_url,
            yolo,
        } => {
            cmd_up(
                &socket_path,
                foreground,
                state_dir,
                relay_host,
                no_relay,
                rpc_url,
                yolo,
            )
            .await
        }
        Command::Down => {
            let mut client = connect(&socket_path).await?;
            client.request(Request::Shutdown).await?;
            println!("Node shutting down.");
            Ok(())
        }
        Command::Identity => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::Identity).await?;
            print_json(&data);
            Ok(())
        }
        Command::Register { username } => {
            let mut client = connect(&socket_path).await?;
            let data = client
                .request(Request::RegisterUsername { username })
                .await?;
            if let Some(obj) = &data {
                let name = obj["username"].as_str().unwrap_or("unknown");
                println!("Successfully registered username @{name}");
            }
            Ok(())
        }
        Command::Lookup { username } => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::LookupUsername { username }).await?;
            print_json(&data);
            Ok(())
        }
        Command::Follow { target } => {
            let mut client = connect(&socket_path).await?;
            client.request(Request::Follow { target }).await?;
            println!("Followed.");
            Ok(())
        }
        Command::Unfollow { target } => {
            let mut client = connect(&socket_path).await?;
            client.request(Request::Unfollow { target }).await?;
            println!("Unfollowed.");
            Ok(())
        }
        Command::Block { target } => {
            let mut client = connect(&socket_path).await?;
            client.request(Request::Block { target }).await?;
            println!("Blocked.");
            Ok(())
        }
        Command::Following => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::Following).await?;
            print_json(&data);
            Ok(())
        }
        Command::Followers => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::Followers).await?;
            print_json(&data);
            Ok(())
        }
        Command::SyncPush { confirm } => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::SyncPush { confirm }).await?;
            print_json(&data);
            Ok(())
        }
        Command::SyncPull { confirm } => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::SyncPull { confirm }).await?;
            print_json(&data);
            Ok(())
        }
        Command::Send { to, message } => {
            let mut client = connect(&socket_path).await?;
            let data = client
                .request(Request::SendDm { to, body: message })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::Post { message } => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::PostFeed { body: message }).await?;
            print_json(&data);
            Ok(())
        }
        Command::Inbox { unread, limit } => {
            let mut client = connect(&socket_path).await?;
            let data = client
                .request(Request::Inbox {
                    unread_only: unread,
                    limit,
                })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::Ack { message_id } => {
            let mut client = connect(&socket_path).await?;
            client.request(Request::InboxAck { message_id }).await?;
            println!("Acknowledged.");
            Ok(())
        }
        Command::Health => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::Health).await?;
            print_json(&data);
            Ok(())
        }

        // -- Wallet commands --
        Command::Wallet { yolo } => {
            let mut client = connect(&socket_path).await?;
            let wallet_type = if yolo {
                WalletType::Yolo
            } else {
                WalletType::Human
            };
            let data = client
                .request(Request::WalletBalance {
                    wallet: wallet_type,
                })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::SendEth { to, amount } => {
            let mut client = connect(&socket_path).await?;
            eprintln!("Send {amount} ETH to {to}");
            let otp = read_otp_auto_or_prompt()?;
            let data = client.request(Request::SendEth { to, amount, otp }).await?;
            print_json(&data);
            Ok(())
        }
        Command::SendUsdc { to, amount } => {
            let mut client = connect(&socket_path).await?;
            eprintln!("Send {amount} USDC to {to}");
            let otp = read_otp_auto_or_prompt()?;
            let data = client
                .request(Request::SendUsdc { to, amount, otp })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::SetupTotp => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::SetupTotp).await?;
            print_json(&data);
            Ok(())
        }

        // -- Contract & signing commands --
        Command::ReadContract {
            contract,
            function,
            abi,
            args,
        } => {
            let mut client = connect(&socket_path).await?;
            let abi_json = load_abi(&abi)?;
            let parsed_args: Vec<serde_json::Value> =
                serde_json::from_str(&args).context("invalid JSON args array")?;
            let data = client
                .request(Request::ReadContract {
                    contract,
                    abi: abi_json,
                    function,
                    args: parsed_args,
                })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::WriteContract {
            contract,
            function,
            abi,
            args,
            value,
            yolo,
        } => {
            let mut client = connect(&socket_path).await?;
            let abi_json = load_abi(&abi)?;
            let parsed_args: Vec<serde_json::Value> =
                serde_json::from_str(&args).context("invalid JSON args array")?;
            if yolo {
                let data = client
                    .request(Request::YoloWriteContract {
                        contract,
                        abi: abi_json,
                        function,
                        args: parsed_args,
                        value,
                    })
                    .await?;
                print_json(&data);
            } else {
                let otp = read_otp_auto_or_prompt()?;
                let data = client
                    .request(Request::WriteContract {
                        contract,
                        abi: abi_json,
                        function,
                        args: parsed_args,
                        value,
                        otp,
                    })
                    .await?;
                print_json(&data);
            }
            Ok(())
        }
        Command::SignMessage { message, yolo } => {
            let mut client = connect(&socket_path).await?;
            if yolo {
                let data = client.request(Request::YoloSignMessage { message }).await?;
                print_json(&data);
            } else {
                let otp = read_otp_auto_or_prompt()?;
                let data = client
                    .request(Request::SignMessage { message, otp })
                    .await?;
                print_json(&data);
            }
            Ok(())
        }

        // -- Room commands --
        Command::Join { room, passphrase } => {
            let mut client = connect(&socket_path).await?;
            let data = client
                .request(Request::JoinRoom { room, passphrase })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::Leave { room } => {
            let mut client = connect(&socket_path).await?;
            client.request(Request::LeaveRoom { room }).await?;
            println!("Left room.");
            Ok(())
        }
        Command::Rooms => {
            let mut client = connect(&socket_path).await?;
            let data = client.request(Request::ListRooms).await?;
            print_json(&data);
            Ok(())
        }
        Command::RoomSend { room, message } => {
            let mut client = connect(&socket_path).await?;
            let data = client
                .request(Request::SendRoom {
                    room,
                    body: message,
                })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::RoomInbox { room, limit } => {
            let mut client = connect(&socket_path).await?;
            let data = client
                .request(Request::RoomInbox { room, limit })
                .await?;
            print_json(&data);
            Ok(())
        }

        Command::Update { yes } => update::cmd_update(yes).await,

        Command::Service { action } => match action {
            ServiceAction::Install {
                state_dir,
                relay_host,
                no_relay,
                rpc_url,
                yolo,
            } => service::cmd_service_install(state_dir, relay_host, no_relay, rpc_url, yolo),
            ServiceAction::Uninstall => service::cmd_service_uninstall(),
            ServiceAction::Status => service::cmd_service_status(),
        },

        Command::Agent { action } => match action {
            AgentAction::Start { state_dir, socket, foreground } => {
                cmd_agent_start(state_dir, socket, foreground).await
            }
            AgentAction::Stop => cmd_agent_request(AgentCmd::Stop).await,
            AgentAction::Unlock { state_dir } => cmd_agent_unlock(state_dir).await,
            AgentAction::Lock => cmd_agent_request(AgentCmd::Lock).await,
            AgentAction::Status => cmd_agent_request(AgentCmd::Status).await,
        },
    }
}

async fn connect(socket_path: &std::path::Path) -> Result<NodeClient> {
    NodeClient::connect(socket_path).await.with_context(|| {
        format!(
            "failed to connect to node at {}. Is the daemon running? Try: agentbook-cli up",
            socket_path.display()
        )
    })
}

async fn cmd_up(
    socket_path: &std::path::Path,
    foreground: bool,
    state_dir: Option<PathBuf>,
    relay_host: Vec<String>,
    no_relay: bool,
    rpc_url: Option<String>,
    yolo: bool,
) -> Result<()> {
    // Check that setup has been run
    let resolved_state_dir = state_dir.clone().unwrap_or_else(|| {
        agentbook_mesh::state_dir::default_state_dir().expect("failed to determine state dir")
    });
    if !agentbook_mesh::recovery::has_recovery_key(&resolved_state_dir.join("recovery.key")) {
        eprintln!();
        eprintln!("  \x1b[1;31mError: Node not set up. Run: agentbook-cli setup\x1b[0m");
        eprintln!();
        std::process::exit(1);
    }

    // Find the agentbook-node binary
    let node_bin = find_node_binary()?;

    // The node requires interactive input (TOTP auth on every start)
    // unless 1Password can auto-fill everything.
    let op_title = agentbook_wallet::onepassword::item_title_from_state_dir(&resolved_state_dir);
    let has_op = agentbook_wallet::onepassword::has_op_cli()
        && op_title
            .as_ref()
            .map(|t| agentbook_wallet::onepassword::has_agentbook_item(t))
            .unwrap_or(false);
    let needs_interactive = !yolo && !has_op;

    let mut cmd = std::process::Command::new(&node_bin);
    cmd.arg("--socket").arg(socket_path);
    if let Some(ref dir) = state_dir {
        cmd.arg("--state-dir").arg(dir);
    }
    if no_relay {
        cmd.arg("--no-relay");
    } else if !relay_host.is_empty() {
        for host in &relay_host {
            cmd.arg("--relay-host").arg(host);
        }
    }
    if let Some(ref url) = rpc_url {
        cmd.arg("--rpc-url").arg(url);
    }
    if yolo {
        cmd.arg("--yolo");
    }

    if needs_interactive && !foreground {
        // Node needs interactive auth, then backgrounds after auth completes.
        // We pipe stdout to catch the READY signal, but inherit stderr (for prompts)
        // and stdin (rpassword reads from /dev/tty directly).
        cmd.arg("--notify-ready");
        cmd.stdout(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {}", node_bin.display()))?;

        // Wait for READY on stdout (auth completed) or process exit (auth failed)
        let stdout = child.stdout.take().expect("piped stdout");
        let reader = std::io::BufReader::new(stdout);
        use std::io::BufRead;
        let mut got_ready = false;
        for line in reader.lines() {
            match line {
                Ok(l) if l.trim() == "READY" => {
                    got_ready = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }

        if got_ready {
            println!("Node daemon started (pid {}).", child.id());
            // Detach — let the node keep running
            std::mem::forget(child);
        } else {
            let status = child.wait()?;
            anyhow::bail!("node exited during auth with status {status}");
        }
    } else if foreground {
        let status = cmd
            .status()
            .with_context(|| format!("failed to run {}", node_bin.display()))?;
        if !status.success() {
            anyhow::bail!("node exited with status {status}");
        }
    } else {
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {}", node_bin.display()))?;
        println!("Node daemon started (pid {}).", child.id());
    }
    Ok(())
}

// ── Agent control helpers ─────────────────────────────────────────────────────

enum AgentCmd {
    Stop,
    Lock,
    Status,
}

/// Start the agentbook-agent daemon (foreground or background).
async fn cmd_agent_start(
    state_dir: Option<PathBuf>,
    socket: Option<PathBuf>,
    foreground: bool,
) -> Result<()> {
    let agent_bin = find_agent_binary()?;
    let mut cmd = std::process::Command::new(&agent_bin);
    cmd.arg("--unlock"); // always unlock on start
    if let Some(ref dir) = state_dir {
        cmd.arg("--state-dir").arg(dir);
    }
    if let Some(ref sock) = socket {
        cmd.arg("--socket").arg(sock);
    }

    if foreground {
        cmd.status()
            .with_context(|| format!("failed to run {}", agent_bin.display()))?;
        return Ok(());
    }

    // Background: inherit stderr (for prompts/output), pipe stdout to catch ready.
    cmd.stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::null());

    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {}", agent_bin.display()))?;

    println!("Agent started (pid {}).", child.id());
    println!("  Status: agentbook-cli agent status");
    Ok(())
}

/// Send a simple command to the running agent (stop / lock / status).
async fn cmd_agent_request(cmd: AgentCmd) -> Result<()> {
    use agentbook::agent_protocol::{AgentRequest, AgentResponse, default_agent_socket_path};
    use agentbook::client::AgentClient;

    let socket = default_agent_socket_path();
    let mut client = AgentClient::connect(&socket)
        .await
        .context("agent not running — start it with: agentbook-cli agent start")?;

    let req = match cmd {
        AgentCmd::Stop => AgentRequest::Stop,
        AgentCmd::Lock => AgentRequest::Lock,
        AgentCmd::Status => AgentRequest::Status,
    };

    // For Status we want the response body; for others just ok/err.
    if matches!(req, AgentRequest::Status) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(&socket).await?;
        stream
            .write_all(
                format!("{}\n", serde_json::to_string(&AgentRequest::Status)?).as_bytes(),
            )
            .await?;
        let (read, _) = stream.split();
        let mut lines = BufReader::new(read).lines();
        if let Some(line) = lines.next_line().await? {
            let resp: AgentResponse = serde_json::from_str(&line)?;
            match resp {
                AgentResponse::Status { locked } => {
                    if locked {
                        println!("Agent status: \x1b[1;33mlocked\x1b[0m (run: agentbook-cli agent unlock)");
                    } else {
                        println!("Agent status: \x1b[1;32munlocked\x1b[0m");
                    }
                }
                AgentResponse::Error { message } => eprintln!("Error: {message}"),
                _ => {}
            }
        }
        return Ok(());
    }

    client.request_ok(&req).await?;
    match cmd {
        AgentCmd::Stop => println!("Agent stopped."),
        AgentCmd::Lock => println!("Agent locked."),
        AgentCmd::Status => {}
    }
    Ok(())
}

/// Unlock the agent: prompt passphrase interactively (or via 1Password) and send to agent.
async fn cmd_agent_unlock(state_dir: Option<PathBuf>) -> Result<()> {
    use agentbook::agent_protocol::{AgentRequest, AgentResponse, default_agent_socket_path};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let resolved_state_dir = state_dir.unwrap_or_else(|| {
        agentbook_mesh::state_dir::default_state_dir().expect("failed to determine state dir")
    });
    let recovery_key_path = resolved_state_dir.join("recovery.key");

    // Try 1Password first.
    let op_title = agentbook_wallet::onepassword::item_title_from_state_dir(&resolved_state_dir);
    let passphrase = if let Some(ref title) = op_title
        && agentbook_wallet::onepassword::has_op_cli()
        && agentbook_wallet::onepassword::has_agentbook_item(title)
    {
        eprintln!("  \x1b[1;36m1Password detected — reading passphrase...\x1b[0m");
        match agentbook_wallet::onepassword::read_passphrase(title) {
            Ok(p) => {
                eprintln!("  \x1b[1;32mGot passphrase from 1Password.\x1b[0m");
                p
            }
            Err(_) => {
                eprintln!("  \x1b[1;33m1Password read failed. Falling back to manual entry.\x1b[0m");
                rpassword::prompt_password("  Enter passphrase: ")?
            }
        }
    } else {
        rpassword::prompt_password("  Enter passphrase: ")?
    };

    // Verify passphrase locally before sending to agent.
    agentbook_mesh::recovery::load_recovery_key(&recovery_key_path, &passphrase)
        .context("wrong passphrase")?;

    let socket = default_agent_socket_path();
    let mut stream = UnixStream::connect(&socket)
        .await
        .context("agent not running — start it with: agentbook-cli agent start")?;

    let req = serde_json::to_string(&AgentRequest::Unlock { passphrase })?;
    stream.write_all(format!("{req}\n").as_bytes()).await?;

    let (read, _) = stream.split();
    let mut lines = BufReader::new(read).lines();
    if let Some(line) = lines.next_line().await? {
        let resp: AgentResponse = serde_json::from_str(&line)?;
        match resp {
            AgentResponse::Ok => println!("  \x1b[1;32mAgent unlocked.\x1b[0m"),
            AgentResponse::Error { message } => anyhow::bail!("{message}"),
            _ => {}
        }
    }
    Ok(())
}

fn find_agent_binary() -> Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("agentbook-agent");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Ok(PathBuf::from("agentbook-agent"))
}

fn find_node_binary() -> Result<PathBuf> {
    // Check next to this binary
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().unwrap();
        let candidate = dir.join("agentbook-node");
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    // Fall back to PATH
    Ok(PathBuf::from("agentbook-node"))
}

/// Load ABI JSON: if prefixed with `@`, read from file; otherwise return as-is.
fn load_abi(s: &str) -> Result<String> {
    if let Some(path) = s.strip_prefix('@') {
        std::fs::read_to_string(path).with_context(|| format!("failed to read ABI file: {path}"))
    } else {
        Ok(s.to_string())
    }
}

/// Read a TOTP code: try 1Password first, then fall back to manual prompt.
fn read_otp_auto_or_prompt() -> Result<String> {
    use agentbook_wallet::onepassword;

    let state_dir =
        agentbook_mesh::state_dir::default_state_dir().unwrap_or_else(|_| PathBuf::from("."));
    let op_title = onepassword::item_title_from_state_dir(&state_dir);

    if let Some(ref title) = op_title
        && onepassword::has_op_cli()
        && onepassword::has_agentbook_item(title)
    {
        eprintln!("Reading TOTP from 1Password...");
        match onepassword::read_otp(title) {
            Ok(code) => {
                eprintln!("Authenticator code filled via 1Password.");
                return Ok(code);
            }
            Err(_) => {
                eprintln!("1Password OTP read failed. Falling back to manual entry.");
            }
        }
    }

    let otp =
        rpassword::prompt_password("Enter authenticator code: ").context("failed to read OTP")?;
    Ok(otp.trim().to_string())
}

fn print_json(data: &Option<serde_json::Value>) {
    if let Some(v) = data {
        println!("{}", serde_json::to_string_pretty(v).unwrap());
    }
}
