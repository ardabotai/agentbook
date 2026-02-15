use std::io::{self, Write};

use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::queue;
use futures::StreamExt;
use tokio::time::{self, Duration};
use tmax_protocol::{Request, Response, SessionInfo};

use crate::connection::ServerConnection;
use crate::keybindings::{Action, KeyHandler};
use crate::renderer;
use crate::status_bar;

/// Minimum terminal dimensions required for the client.
const MIN_COLS: u16 = 40;
const MIN_ROWS: u16 = 10;

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

    // Get terminal size — cached as mutable locals, updated only on Resize events
    let (mut cols, mut rows) = crossterm::terminal::size()?;

    // Wait for minimum terminal size before proceeding
    if cols < MIN_COLS || rows < MIN_ROWS {
        wait_for_minimum_size(&mut stdout, cols, rows, &mut EventStream::new()).await?;
        (cols, rows) = crossterm::terminal::size()?;
    }

    let mut content_rows = rows.saturating_sub(1); // Reserve 1 row for status bar

    // Create vt100 parser for this session
    let mut parser = vt100::Parser::new(content_rows, cols, 0);
    let mut prev_screen = parser.screen().clone();

    // Get session info for status bar
    let session_info = get_session_info(conn, &config.session_id).await?;
    let label = session_info.label.clone();
    // Set up key handler
    let mut key_handler = KeyHandler::new(config.view_mode);
    let mode_label = if config.view_mode { "VIEW" } else { "EDIT" };

    let mut show_help = false;

    // Initial clear and status bar render
    renderer::clear_screen(&mut stdout)?;
    status_bar::render_status_bar(
        &mut stdout,
        content_rows,
        cols,
        &config.session_id,
        label.as_deref(),
        mode_label,
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

                        let prev_mode = key_handler.mode();
                        let action = key_handler.handle_key(key_event);
                        match action {
                            Action::ShowHelp => {
                                show_help = !show_help;
                                if show_help {
                                    render_help_overlay(&mut stdout, cols, content_rows)?;
                                } else {
                                    // Redraw everything
                                    renderer::clear_screen(&mut stdout)?;
                                    renderer::render_full(
                                        &mut stdout,
                                        parser.screen(),
                                        cols, content_rows,
                                    )?;
                                    renderer::render_cursor(
                                        &mut stdout,
                                        parser.screen(),
                                        !config.view_mode,
                                    )?;
                                    status_bar::render_status_bar(
                                        &mut stdout,
                                        content_rows,
                                        cols,
                                        &config.session_id,
                                        label.as_deref(),
                                        mode_label,
                                        key_handler.mode(),
                                    )?;
                                }
                                stdout.flush()?;
                            }
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

                        // Re-render status bar only if mode changed (PREFIX indicator)
                        if key_handler.mode() != prev_mode {
                            status_bar::render_status_bar(
                                &mut stdout,
                                content_rows,
                                cols,
                                &config.session_id,
                                label.as_deref(),
                                mode_label,
                                key_handler.mode(),
                            )?;
                            stdout.flush()?;
                        }
                    }
                    Some(Ok(Event::Resize(new_cols, new_rows))) => {
                        // Check minimum terminal size
                        if new_cols < MIN_COLS || new_rows < MIN_ROWS {
                            render_too_small(&mut stdout, new_cols, new_rows)?;
                            stdout.flush()?;
                            continue;
                        }

                        // Update cached terminal dimensions
                        cols = new_cols;
                        content_rows = new_rows.saturating_sub(1);

                        // Recreate the vt100 parser with new dimensions
                        parser = vt100::Parser::new(content_rows, cols, 0);
                        prev_screen = parser.screen().clone();

                        // Tell the server about the new size
                        let req = Request::Resize {
                            session_id: config.session_id.clone(),
                            cols,
                            rows: content_rows,
                        };
                        conn.send_request(&req).await?;

                        // Full redraw
                        renderer::clear_screen(&mut stdout)?;
                        renderer::render_full(
                            &mut stdout,
                            parser.screen(),
                            cols, content_rows,
                        )?;
                        renderer::render_cursor(
                            &mut stdout,
                            parser.screen(),
                            !config.view_mode,
                        )?;
                        status_bar::render_status_bar(
                            &mut stdout,
                            content_rows,
                            cols,
                            &config.session_id,
                            label.as_deref(),
                            mode_label,
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

                        // Drain any immediately-available output events before
                        // rendering, so bulk output only triggers a single
                        // render pass (prevents frame drops).
                        loop {
                            match tokio::time::timeout(
                                std::time::Duration::ZERO,
                                conn.read_event(),
                            )
                            .await
                            {
                                Ok(Ok(Some(Response::Event(
                                    tmax_protocol::Event::Output { data, .. },
                                )))) => {
                                    parser.process(&data);
                                }
                                _ => break,
                            }
                        }

                        // Now render once for all coalesced output
                        renderer::render_diff(
                            &mut stdout,
                            &prev_screen,
                            parser.screen(),
                        )?;
                        renderer::render_cursor(
                            &mut stdout,
                            parser.screen(),
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
                    status_bar::render_status_bar(
                        &mut stdout,
                        content_rows,
                        cols,
                        &config.session_id,
                        label.as_deref(),
                        mode_label,
                        key_handler.mode(),
                    )?;
                    stdout.flush()?;
                }
            }
        }
    }

    Ok(())
}

/// Render the help overlay showing keybindings.
fn render_help_overlay(stdout: &mut impl Write, cols: u16, rows: u16) -> anyhow::Result<()> {
    use crossterm::{cursor, style};

    let lines = [
        "tmax keybindings",
        "",
        "  Ctrl+Space, d        Detach from session",
        "  Ctrl+Space, ?        Toggle this help",
        "  Ctrl+Space, Ctrl+Space  Send literal Ctrl+Space",
        "",
        "Press ? to close",
    ];

    // Center the overlay
    let box_width = 50u16.min(cols);
    let box_height = lines.len() as u16;
    let start_row = rows.saturating_sub(box_height) / 2;
    let start_col = cols.saturating_sub(box_width) / 2;

    for (i, line) in lines.iter().enumerate() {
        let row = start_row + i as u16;
        queue!(stdout, cursor::MoveTo(start_col, row))?;
        queue!(
            stdout,
            style::SetAttribute(style::Attribute::Reset),
            style::SetAttribute(style::Attribute::Reverse),
        )?;
        let display: String = line.chars().take(box_width as usize).collect();
        let padding = box_width as usize - display.len().min(box_width as usize);
        queue!(stdout, style::Print(format!("{display}{:padding$}", "")))?;
        queue!(stdout, style::SetAttribute(style::Attribute::Reset))?;
    }

    Ok(())
}

/// Render "terminal too small" message.
fn render_too_small(stdout: &mut impl Write, cols: u16, rows: u16) -> anyhow::Result<()> {
    renderer::clear_screen(stdout)?;
    let msg = format!("Terminal too small: {}x{} (need {}x{})", cols, rows, MIN_COLS, MIN_ROWS);
    queue!(stdout, crossterm::cursor::MoveTo(0, 0))?;
    queue!(stdout, crossterm::style::Print(msg))?;
    Ok(())
}

/// Wait for the terminal to reach minimum size before starting.
async fn wait_for_minimum_size(
    stdout: &mut impl Write,
    mut cols: u16,
    mut rows: u16,
    input_stream: &mut EventStream,
) -> anyhow::Result<()> {
    render_too_small(stdout, cols, rows)?;
    stdout.flush()?;

    loop {
        if let Some(Ok(Event::Resize(new_cols, new_rows))) = input_stream.next().await {
            cols = new_cols;
            rows = new_rows;
            if cols >= MIN_COLS && rows >= MIN_ROWS {
                renderer::clear_screen(stdout)?;
                stdout.flush()?;
                return Ok(());
            }
            render_too_small(stdout, cols, rows)?;
            stdout.flush()?;
        }
    }
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
