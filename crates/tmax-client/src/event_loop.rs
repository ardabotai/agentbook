use std::io::{self, Write};

use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::time::{self, Duration};
use tmax_protocol::{Request, Response, SessionInfo};

use crate::connection::ServerConnection;
use crate::keybindings::{Action, KeyHandler};
use crate::renderer;
use crate::status_bar;

/// Configuration for the event loop.
pub struct EventLoopConfig {
    pub session_id: String,
    pub view_mode: bool,
}

/// Run the main event loop.
///
/// This concurrently handles:
/// - Terminal input events (crossterm EventStream)
/// - Server messages (socket read)
/// - Prefix key timeout checks
pub async fn run(
    conn: &mut ServerConnection,
    config: EventLoopConfig,
) -> anyhow::Result<()> {
    let mut stdout = io::stdout();

    // Get terminal size
    let (cols, rows) = crossterm::terminal::size()?;
    let content_rows = rows.saturating_sub(1); // Reserve 1 row for status bar

    // Create vt100 parser for this session
    let mut parser = vt100::Parser::new(content_rows, cols, 0);
    let mut prev_screen = parser.screen().clone();

    // Get session info for status bar
    let session_info = get_session_info(conn, &config.session_id).await?;
    let label = session_info.label.clone();
    // git_info is not available on this branch version of the protocol
    let git_branch: Option<String> = None;

    // Set up key handler
    let mut key_handler = KeyHandler::new(config.view_mode);
    let mode_label = if config.view_mode { "VIEW" } else { "EDIT" };

    // Initial clear and status bar render
    renderer::clear_screen(&mut stdout)?;
    status_bar::render_status_bar(
        &mut stdout,
        content_rows,
        cols,
        &config.session_id,
        label.as_deref(),
        mode_label,
        git_branch.as_deref(),
        key_handler.mode(),
    )?;
    stdout.flush()?;

    // Set up input stream
    let mut input_stream = EventStream::new();

    // Timeout check interval
    let mut timeout_interval = time::interval(Duration::from_millis(200));

    loop {
        tokio::select! {
            // Terminal input events
            event = input_stream.next() => {
                match event {
                    Some(Ok(Event::Key(key_event))) => {
                        // Only handle Press events (not Release/Repeat)
                        if key_event.kind != KeyEventKind::Press {
                            continue;
                        }

                        let action = key_handler.handle_key(key_event);
                        match action {
                            Action::ForwardInput(bytes) => {
                                if !bytes.is_empty() {
                                    let req = Request::SendInput {
                                        session_id: config.session_id.clone(),
                                        data: bytes,
                                    };
                                    conn.send_request(&req).await?;
                                }
                            }
                            Action::Detach => {
                                let req = Request::Detach {
                                    session_id: config.session_id.clone(),
                                };
                                let _ = conn.send_request(&req).await;
                                return Ok(());
                            }
                            Action::None => {}
                        }

                        // Re-render status bar if mode changed (PREFIX indicator)
                        status_bar::render_status_bar(
                            &mut stdout,
                            content_rows,
                            cols,
                            &config.session_id,
                            label.as_deref(),
                            mode_label,
                            git_branch.as_deref(),
                            key_handler.mode(),
                        )?;
                        stdout.flush()?;
                    }
                    Some(Ok(Event::Resize(new_cols, new_rows))) => {
                        let new_content_rows = new_rows.saturating_sub(1);

                        // Recreate the vt100 parser with new dimensions
                        parser = vt100::Parser::new(new_content_rows, new_cols, 0);
                        prev_screen = parser.screen().clone();

                        // Tell the server about the new size
                        let req = Request::Resize {
                            session_id: config.session_id.clone(),
                            cols: new_cols,
                            rows: new_content_rows,
                        };
                        conn.send_request(&req).await?;

                        // Full redraw
                        renderer::clear_screen(&mut stdout)?;
                        renderer::render_full(
                            &mut stdout,
                            parser.screen(),
                            0, 0,
                            new_cols, new_content_rows,
                        )?;
                        renderer::render_cursor(
                            &mut stdout,
                            parser.screen(),
                            0, 0,
                            !config.view_mode,
                        )?;
                        status_bar::render_status_bar(
                            &mut stdout,
                            new_content_rows,
                            new_cols,
                            &config.session_id,
                            label.as_deref(),
                            mode_label,
                            git_branch.as_deref(),
                            key_handler.mode(),
                        )?;
                        stdout.flush()?;
                    }
                    Some(Ok(_)) => {} // Mouse events, paste events, etc.
                    Some(Err(e)) => {
                        tracing::error!("terminal event error: {e}");
                        break;
                    }
                    None => break,
                }
            }

            // Server messages
            msg = conn.read_event() => {
                match msg {
                    Ok(Some(Response::Event(tmax_protocol::Event::Output { data, .. }))) => {
                        // Feed output to vt100 parser
                        parser.process(&data);

                        // Render diff
                        let (cols, _) = crossterm::terminal::size()?;
                        let content_rows = crossterm::terminal::size()?.1.saturating_sub(1);

                        renderer::render_diff(
                            &mut stdout,
                            &prev_screen,
                            parser.screen(),
                            0, 0,
                        )?;
                        renderer::render_cursor(
                            &mut stdout,
                            parser.screen(),
                            0, 0,
                            !config.view_mode,
                        )?;
                        // Re-render status bar (output may have scrolled over it)
                        status_bar::render_status_bar(
                            &mut stdout,
                            content_rows,
                            cols,
                            &config.session_id,
                            label.as_deref(),
                            mode_label,
                            git_branch.as_deref(),
                            key_handler.mode(),
                        )?;
                        stdout.flush()?;

                        prev_screen = parser.screen().clone();
                    }
                    Ok(Some(Response::Event(tmax_protocol::Event::SessionExited { exit_code, .. }))) => {
                        eprintln!("\r\n[session exited with code {exit_code:?}]\r");
                        return Ok(());
                    }
                    Ok(Some(Response::Event(tmax_protocol::Event::SessionDestroyed { .. }))) => {
                        eprintln!("\r\n[session destroyed]\r");
                        return Ok(());
                    }
                    Ok(None) => {
                        eprintln!("\r\n[server disconnected]\r");
                        return Ok(());
                    }
                    Ok(Some(_)) => {} // Other responses
                    Err(e) => {
                        tracing::error!("server message error: {e}");
                        return Err(e);
                    }
                }
            }

            // Periodic prefix timeout check
            _ = timeout_interval.tick() => {
                if key_handler.check_timeout() {
                    let (cols, rows) = crossterm::terminal::size()?;
                    let content_rows = rows.saturating_sub(1);
                    status_bar::render_status_bar(
                        &mut stdout,
                        content_rows,
                        cols,
                        &config.session_id,
                        label.as_deref(),
                        mode_label,
                        git_branch.as_deref(),
                        key_handler.mode(),
                    )?;
                    stdout.flush()?;
                }
            }
        }
    }

    Ok(())
}

/// Get session info from the server.
async fn get_session_info(
    conn: &mut ServerConnection,
    session_id: &str,
) -> anyhow::Result<SessionInfo> {
    let req = Request::SessionInfo {
        session_id: session_id.to_string(),
    };
    let resp = conn.send_request(&req).await?;
    match resp {
        Response::Ok { data: Some(data) } => {
            let info: SessionInfo = serde_json::from_value(data)?;
            Ok(info)
        }
        Response::Error { message, .. } => {
            anyhow::bail!("failed to get session info: {message}");
        }
        _ => anyhow::bail!("unexpected response for session info"),
    }
}
