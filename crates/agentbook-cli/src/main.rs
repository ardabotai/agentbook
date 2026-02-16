use agentbook::client::{NodeClient, default_socket_path};
use agentbook::protocol::Request;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "agentbook", about = "agentbook CLI")]
struct Cli {
    /// Path to the node daemon's Unix socket.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    match cli.command {
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
            print_json(&data);
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
            let wallet_type = if yolo { "yolo" } else { "human" };
            let data = client
                .request(Request::WalletBalance {
                    wallet: wallet_type.to_string(),
                })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::SendEth { to, amount } => {
            let mut client = connect(&socket_path).await?;
            eprintln!("Send {amount} ETH to {to}");
            let otp = rpassword::prompt_password("Enter authenticator code: ")
                .context("failed to read OTP")?;
            let data = client
                .request(Request::SendEth {
                    to,
                    amount,
                    otp: otp.trim().to_string(),
                })
                .await?;
            print_json(&data);
            Ok(())
        }
        Command::SendUsdc { to, amount } => {
            let mut client = connect(&socket_path).await?;
            eprintln!("Send {amount} USDC to {to}");
            let otp = rpassword::prompt_password("Enter authenticator code: ")
                .context("failed to read OTP")?;
            let data = client
                .request(Request::SendUsdc {
                    to,
                    amount,
                    otp: otp.trim().to_string(),
                })
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
                let otp = rpassword::prompt_password("Enter authenticator code: ")
                    .context("failed to read OTP")?;
                let data = client
                    .request(Request::WriteContract {
                        contract,
                        abi: abi_json,
                        function,
                        args: parsed_args,
                        value,
                        otp: otp.trim().to_string(),
                    })
                    .await?;
                print_json(&data);
            }
            Ok(())
        }
        Command::SignMessage { message, yolo } => {
            let mut client = connect(&socket_path).await?;
            if yolo {
                let data = client
                    .request(Request::YoloSignMessage { message })
                    .await?;
                print_json(&data);
            } else {
                let otp = rpassword::prompt_password("Enter authenticator code: ")
                    .context("failed to read OTP")?;
                let data = client
                    .request(Request::SignMessage {
                        message,
                        otp: otp.trim().to_string(),
                    })
                    .await?;
                print_json(&data);
            }
            Ok(())
        }
    }
}

async fn connect(socket_path: &std::path::Path) -> Result<NodeClient> {
    NodeClient::connect(socket_path).await.with_context(|| {
        format!(
            "failed to connect to node at {}. Is the daemon running? Try: agentbook up",
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
    // Find the agentbook-node binary
    let node_bin = find_node_binary()?;

    // The node requires interactive input (TOTP auth on every start, plus
    // first-run setup). Always run in foreground unless --yolo skips auth.
    let needs_interactive = !yolo;

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
        // Node needs TOTP auth — run in foreground so user can enter the code
        let status = cmd
            .status()
            .with_context(|| format!("failed to run {}", node_bin.display()))?;
        if !status.success() {
            anyhow::bail!("node exited with status {status}");
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
        std::fs::read_to_string(path)
            .with_context(|| format!("failed to read ABI file: {path}"))
    } else {
        Ok(s.to_string())
    }
}

fn print_json(data: &Option<serde_json::Value>) {
    if let Some(v) = data {
        println!("{}", serde_json::to_string_pretty(v).unwrap());
    }
}
