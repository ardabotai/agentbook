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
        } => cmd_up(&socket_path, foreground, state_dir, relay_host, no_relay).await,
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
) -> Result<()> {
    // Find the agentbook-node binary
    let node_bin = find_node_binary()?;

    let mut cmd = std::process::Command::new(&node_bin);
    cmd.arg("--socket").arg(socket_path);
    if let Some(dir) = state_dir {
        cmd.arg("--state-dir").arg(dir);
    }
    if no_relay {
        cmd.arg("--no-relay");
    } else if relay_host.is_empty() {
        // Default relay is handled by agentbook-node, no need to pass it
    } else {
        for host in &relay_host {
            cmd.arg("--relay-host").arg(host);
        }
    }

    if foreground {
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

fn print_json(data: &Option<serde_json::Value>) {
    if let Some(v) = data {
        println!("{}", serde_json::to_string_pretty(v).unwrap());
    }
}
