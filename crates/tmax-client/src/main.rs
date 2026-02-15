mod connection;
mod event_loop;
mod keybindings;
mod renderer;
mod status_bar;
mod terminal;

use clap::Parser;
use tmax_protocol::{AttachMode, ErrorCode, Request, Response};

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

/// Validate that a session ID is safe for use in protocol messages.
///
/// Rules:
/// - Must not be empty
/// - Maximum 256 characters
/// - Only alphanumeric, hyphen, underscore, and period characters
/// - No control characters or newlines
fn validate_session_id(session_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!session_id.is_empty(), "session_id must not be empty");
    anyhow::ensure!(
        session_id.len() <= 256,
        "session_id must be at most 256 characters (got {})",
        session_id.len()
    );
    anyhow::ensure!(
        session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'),
        "session_id contains invalid characters; only alphanumeric, hyphen, underscore, and period are allowed"
    );
    Ok(())
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
    validate_session_id(&cli.session_id)?;

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
        Response::Error { code: ErrorCode::AttachmentDenied, .. } => {
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
    let (cols, rows) = crossterm::terminal::size()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_session_ids() {
        assert!(validate_session_id("my-session").is_ok());
        assert!(validate_session_id("session_01").is_ok());
        assert!(validate_session_id("a.b.c").is_ok());
        assert!(validate_session_id("ABC-123_test.v2").is_ok());
        assert!(validate_session_id("x").is_ok());
    }

    #[test]
    fn empty_session_id_rejected() {
        let err = validate_session_id("").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn too_long_session_id_rejected() {
        let long = "a".repeat(257);
        let err = validate_session_id(&long).unwrap_err();
        assert!(err.to_string().contains("256"));
    }

    #[test]
    fn max_length_session_id_accepted() {
        let max = "a".repeat(256);
        assert!(validate_session_id(&max).is_ok());
    }

    #[test]
    fn newline_rejected() {
        assert!(validate_session_id("session\nid").is_err());
    }

    #[test]
    fn control_characters_rejected() {
        assert!(validate_session_id("session\x00id").is_err());
        assert!(validate_session_id("session\x1bid").is_err());
    }

    #[test]
    fn spaces_rejected() {
        assert!(validate_session_id("my session").is_err());
    }

    #[test]
    fn special_characters_rejected() {
        assert!(validate_session_id("session/id").is_err());
        assert!(validate_session_id("session@id").is_err());
        assert!(validate_session_id("{\"inject\":true}").is_err());
    }
}
