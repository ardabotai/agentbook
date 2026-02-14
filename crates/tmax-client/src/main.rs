mod connection;
mod event_loop;
mod keybindings;
mod renderer;
mod status_bar;
mod terminal;

use clap::Parser;
use tmax_protocol::{AttachMode, Request, Response};

use connection::ServerConnection;
use terminal::TerminalGuard;

#[derive(Parser)]
#[command(
    name = "tmax-attach",
    about = "Attach to a tmax session with full terminal UI"
)]
struct Cli {
    /// Session ID to attach to
    session_id: String,

    /// Attach in view-only mode (no input forwarding)
    #[arg(long)]
    view: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing (only to file/stderr, not stdout - we own stdout for rendering)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = Cli::parse();

    // Connect to server
    let mut conn = ServerConnection::connect().await?;

    // Attach to the session
    let mode = if cli.view {
        AttachMode::View
    } else {
        AttachMode::Edit
    };

    let attach_resp = conn
        .send_request(&Request::Attach {
            session_id: cli.session_id.clone(),
            mode: mode.clone(),
        })
        .await?;

    // Handle attach rejection (e.g., edit already taken)
    let view_mode = match &attach_resp {
        Response::Error { message, .. } if message.contains("edit") => {
            eprintln!("warning: edit attachment denied, falling back to view mode");
            // Retry as view
            conn.send_request(&Request::Attach {
                session_id: cli.session_id.clone(),
                mode: AttachMode::View,
            })
            .await?;
            true
        }
        Response::Error { message, .. } => {
            anyhow::bail!("failed to attach: {message}");
        }
        _ => cli.view,
    };

    // Subscribe to session events (get all buffered output)
    let subscribe_resp = conn
        .send_request(&Request::Subscribe {
            session_id: cli.session_id.clone(),
            last_seq: None,
        })
        .await?;

    if let Response::Error { message, .. } = subscribe_resp {
        anyhow::bail!("failed to subscribe: {message}");
    }

    // Set up terminal (alternate screen, raw mode)
    let _guard = TerminalGuard::setup()?;

    // Send initial resize to match our terminal
    let (cols, rows) = TerminalGuard::size()?;
    let content_rows = rows.saturating_sub(1); // Reserve 1 for status bar

    if !view_mode {
        let _ = conn
            .send_request(&Request::Resize {
                session_id: cli.session_id.clone(),
                cols,
                rows: content_rows,
            })
            .await;
    }

    // Run the event loop
    let config = event_loop::EventLoopConfig {
        session_id: cli.session_id.clone(),
        view_mode,
    };

    let result = event_loop::run(&mut conn, config).await;

    // TerminalGuard Drop restores terminal state
    // Detach request already sent by event loop on Ctrl+Space,d

    if let Err(e) = &result {
        eprintln!("error: {e}");
    }

    result
}
