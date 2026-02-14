mod client;
mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tmax", about = "Programmable terminal multiplexer for AI workflows")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the tmax server daemon
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },

    /// Create a new session
    New {
        /// Command to execute
        #[arg(long)]
        exec: Option<String>,

        /// Use the default shell ($SHELL)
        #[arg(long)]
        shell: bool,

        /// Label for the session
        #[arg(long)]
        label: Option<String>,

        /// Writable paths for sandboxing (repeatable)
        #[arg(long = "sandbox-write")]
        sandbox_write: Vec<String>,

        /// Disable sandboxing
        #[arg(long)]
        no_sandbox: bool,

        /// Parent session ID for nesting
        #[arg(long)]
        parent: Option<String>,

        /// PTY columns
        #[arg(long, default_value = "80")]
        cols: u16,

        /// PTY rows
        #[arg(long, default_value = "24")]
        rows: u16,
    },

    /// List all sessions
    List {
        /// Show session hierarchy as a tree
        #[arg(long)]
        tree: bool,
    },

    /// Get session details
    Info {
        /// Session ID
        session: String,
    },

    /// Attach to a session
    Attach {
        /// Session ID
        session: String,

        /// View-only mode (no input)
        #[arg(long)]
        view: bool,
    },

    /// Send input to a session
    Send {
        /// Session ID
        session: String,

        /// Input text to send
        input: String,
    },

    /// Resize a session's PTY
    Resize {
        /// Session ID
        session: String,

        /// Columns
        cols: u16,

        /// Rows
        rows: u16,
    },

    /// Kill a session
    Kill {
        /// Session ID
        session: String,

        /// Also kill child sessions
        #[arg(long)]
        cascade: bool,
    },

    /// Insert a marker at current output position
    Marker {
        /// Session ID
        session: String,

        /// Marker name
        name: String,
    },

    /// List markers for a session
    Markers {
        /// Session ID
        session: String,
    },

    /// Stream raw output to stdout (for piping)
    Stream {
        /// Session ID
        session: String,
    },

    /// Stream JSON events to stdout
    Subscribe {
        /// Session ID
        session: String,

        /// Sequence ID to resume from
        #[arg(long)]
        since: Option<u64>,
    },
}

#[derive(Subcommand)]
enum ServerAction {
    /// Start the daemon
    Start {
        /// Run in foreground (don't daemonize)
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the daemon
    Stop,
    /// Check daemon status
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Server { action } => match action {
            ServerAction::Start { foreground } => commands::server_start(foreground).await,
            ServerAction::Stop => commands::server_stop().await,
            ServerAction::Status => commands::server_status().await,
        },
        Commands::New {
            exec,
            shell,
            label,
            sandbox_write,
            no_sandbox,
            parent,
            cols,
            rows,
        } => {
            let exec = if shell {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
            } else if let Some(e) = exec {
                e
            } else {
                anyhow::bail!("must specify --exec CMD or --shell");
            };

            let sandbox = if no_sandbox || sandbox_write.is_empty() {
                None
            } else {
                Some(tmax_protocol::SandboxConfig {
                    writable_paths: sandbox_write.into_iter().map(Into::into).collect(),
                    inherit_parent: true,
                })
            };

            commands::session_new(exec, label, sandbox, parent, cols, rows).await
        }
        Commands::List { tree } => commands::session_list(tree).await,
        Commands::Info { session } => commands::session_info(session).await,
        Commands::Attach { session, view } => commands::session_attach(session, view).await,
        Commands::Send { session, input } => commands::session_send(session, input).await,
        Commands::Resize {
            session,
            cols,
            rows,
        } => commands::session_resize(session, cols, rows).await,
        Commands::Kill { session, cascade } => commands::session_kill(session, cascade).await,
        Commands::Marker { session, name } => commands::session_marker(session, name).await,
        Commands::Markers { session } => commands::session_markers(session).await,
        Commands::Stream { session } => commands::session_stream(session).await,
        Commands::Subscribe { session, since } => {
            commands::session_subscribe(session, since).await
        }
    }
}
