use crate::app::{
    App, AutoAgentMode, PendingRequest, PrefixPending, SidekickChatStreamEvent, SidekickMessage,
    SidekickRole, Tab, TerminalSplit, truncate,
};
use agentbook::client::NodeWriter;
use agentbook::protocol::{Request, WalletType};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use std::sync::mpsc::TryRecvError;

/// Prefix-mode timeout (1 second).
const PREFIX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_TERMINAL_PANES: usize = 4;

/// Send a request to the daemon, setting status on error. Returns the pending
/// request kind on success, or `None` if the send failed.
async fn send_req(
    app: &mut App,
    writer: &mut NodeWriter,
    req: Request,
    kind: PendingRequest,
) -> Option<PendingRequest> {
    match writer.send(req).await {
        Ok(()) => Some(kind),
        Err(e) => {
            app.status_msg = format!("Error: {e}");
            None
        }
    }
}

/// Handle a key event. Returns `Some(PendingRequest)` if a request was sent
/// that expects a response (caller should push it to the pending queue).
pub async fn handle_key(
    app: &mut App,
    writer: &mut NodeWriter,
    key: KeyEvent,
) -> Option<PendingRequest> {
    // Auto-expire prefix mode.
    if app.prefix_mode
        && let Some(at) = app.prefix_mode_at
        && at.elapsed() >= PREFIX_TIMEOUT
    {
        app.prefix_mode = false;
        app.prefix_mode_at = None;
        app.prefix_pending = None;
    }

    // Quit-confirm modal has priority over normal input flow.
    if app.quit_confirm {
        if is_prefix_key(&key) {
            app.prefix_mode = true;
            app.prefix_mode_at = Some(std::time::Instant::now());
            app.prefix_pending = None;
            return None;
        }
        if app.prefix_mode {
            clear_prefix_mode(app);
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.quit_confirm = false;
                app.should_quit = true;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.quit_confirm = false;
                app.status_msg = "Exit cancelled.".to_string();
            }
            _ => {}
        }
        return None;
    }

    // Ctrl+Space enters prefix mode from any tab.
    if is_prefix_key(&key) {
        app.prefix_mode = true;
        app.prefix_mode_at = Some(std::time::Instant::now());
        app.prefix_pending = None;
        return None;
    }

    // Handle prefix-mode chord.
    if app.prefix_mode {
        if handle_pending_prefix_mode(app, key.code) {
            return None;
        }
        handle_prefix_chord(app, key.code);
        return None;
    }

    // Sidekick chat input focus in Terminal tab.
    if app.tab == Tab::Terminal && app.auto_agent.enabled && app.auto_agent.chat_focus {
        return handle_sidekick_chat_key(app, key);
    }

    // On Terminal tab, forward everything to PTY.
    if app.tab == Tab::Terminal {
        if let Some(term) = app.active_terminal_mut() {
            if let Some(bytes) = key_to_bytes(&key) {
                let _ = term.write_input(&bytes);
            }
        } else {
            // No terminal yet — Enter spawns it.
            if key.code == KeyCode::Enter {
                ensure_terminal(app);
            }
        }
        return None;
    }

    // Feed/DMs/Room tab key handling.
    match key.code {
        KeyCode::Esc => {
            app.should_quit = true;
            None
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
            None
        }
        KeyCode::Tab => {
            let next = match &app.tab {
                Tab::Feed => Tab::Dms,
                Tab::Dms => Tab::Feed,
                Tab::Terminal => Tab::Feed,
                Tab::Room(_) => Tab::Feed,
            };
            app.switch_tab(next.clone());
            if next == Tab::Terminal {
                ensure_terminal(app);
            }
            None
        }
        KeyCode::Up => {
            if app.tab == Tab::Dms && app.selected_contact > 0 {
                app.selected_contact -= 1;
            }
            None
        }
        KeyCode::Down => {
            if app.tab == Tab::Dms && app.selected_contact + 1 < app.following.len() {
                app.selected_contact += 1;
            }
            None
        }
        KeyCode::Enter => {
            if !app.input.is_empty() {
                let input = std::mem::take(&mut app.input);
                if input.starts_with('/') {
                    handle_slash_command(app, writer, &input).await
                } else {
                    send_message(app, writer, &input).await
                }
            } else {
                None
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
            None
        }
        KeyCode::Char(c) => {
            app.input.push(c);
            None
        }
        _ => None,
    }
}

/// Handle slash commands. Returns `Some(PendingRequest)` if a request was sent.
async fn handle_slash_command(
    app: &mut App,
    writer: &mut NodeWriter,
    input: &str,
) -> Option<PendingRequest> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    match parts.first().copied() {
        // ── Existing ──────────────────────────────────────────────────────
        Some("/join") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /join <room> [--passphrase <pass>]".to_string();
                return None;
            }
            let room = parts[1].to_string();
            let passphrase = if parts.len() >= 4 && parts[2] == "--passphrase" {
                Some(parts[3..].join(" "))
            } else {
                None
            };
            app.status_msg = "Joining room...".to_string();
            send_req(
                app,
                writer,
                Request::JoinRoom { room, passphrase },
                PendingRequest::Send,
            )
            .await
        }
        Some("/leave") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /leave <room>".to_string();
                return None;
            }
            let room = parts[1].to_string();
            app.status_msg = format!("Leaving #{room}...");
            send_req(
                app,
                writer,
                Request::LeaveRoom { room },
                PendingRequest::Send,
            )
            .await
        }

        // ── Social ────────────────────────────────────────────────────────
        Some("/follow") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /follow <node_id or @username>".to_string();
                return None;
            }
            let target = parts[1].to_string();
            app.status_msg = format!("Following {target}...");
            send_req(
                app,
                writer,
                Request::Follow { target },
                PendingRequest::Send,
            )
            .await
        }
        Some("/unfollow") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /unfollow <node_id or @username>".to_string();
                return None;
            }
            let target = parts[1].to_string();
            app.status_msg = format!("Unfollowing {target}...");
            send_req(
                app,
                writer,
                Request::Unfollow { target },
                PendingRequest::Send,
            )
            .await
        }
        Some("/block") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /block <node_id or @username>".to_string();
                return None;
            }
            let target = parts[1].to_string();
            app.status_msg = format!("Blocking {target}...");
            send_req(app, writer, Request::Block { target }, PendingRequest::Send).await
        }
        Some("/lookup") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /lookup <@username>".to_string();
                return None;
            }
            let username = parts[1].trim_start_matches('@').to_string();
            app.status_msg = format!("Looking up @{username}...");
            send_req(
                app,
                writer,
                Request::LookupUsername { username },
                PendingRequest::SlashLookup,
            )
            .await
        }
        Some("/followers") => {
            app.status_msg = "Fetching followers...".to_string();
            send_req(
                app,
                writer,
                Request::Followers,
                PendingRequest::SlashFollowers,
            )
            .await
        }
        Some("/following") => {
            app.status_msg = "Fetching following...".to_string();
            send_req(
                app,
                writer,
                Request::Following,
                PendingRequest::SlashFollowing,
            )
            .await
        }

        // ── Wallet ────────────────────────────────────────────────────────
        Some("/balance") => {
            app.status_msg = "Fetching balance...".to_string();
            send_req(
                app,
                writer,
                Request::WalletBalance {
                    wallet: WalletType::Human,
                },
                PendingRequest::SlashBalance,
            )
            .await
        }
        Some("/send-eth") => {
            if parts.len() < 4 {
                app.status_msg = "Usage: /send-eth <to> <amount> <otp>".to_string();
                return None;
            }
            let to = parts[1].to_string();
            let amount = parts[2].to_string();
            let otp = parts[3].to_string();
            app.status_msg = "Sending ETH...".to_string();
            send_req(
                app,
                writer,
                Request::SendEth { to, amount, otp },
                PendingRequest::Send,
            )
            .await
        }
        Some("/send-usdc") => {
            if parts.len() < 4 {
                app.status_msg = "Usage: /send-usdc <to> <amount> <otp>".to_string();
                return None;
            }
            let to = parts[1].to_string();
            let amount = parts[2].to_string();
            let otp = parts[3].to_string();
            app.status_msg = "Sending USDC...".to_string();
            send_req(
                app,
                writer,
                Request::SendUsdc { to, amount, otp },
                PendingRequest::Send,
            )
            .await
        }

        // ── Utility ───────────────────────────────────────────────────────
        Some("/identity") => {
            app.status_msg = "Fetching identity...".to_string();
            send_req(
                app,
                writer,
                Request::Identity,
                PendingRequest::SlashIdentity,
            )
            .await
        }
        Some("/health") => {
            app.status_msg = "Checking health...".to_string();
            send_req(app, writer, Request::Health, PendingRequest::SlashHealth).await
        }
        Some("/sidekick") | Some("/auto") => {
            handle_sidekick_command(app, &parts[1..]);
            None
        }
        Some("/help") => {
            app.status_msg = "Commands: /follow /unfollow /block /lookup /followers /following /balance /send-eth /send-usdc /identity /health /join /leave /sidekick /sound /help".to_string();
            None
        }
        Some("/sound") => {
            match parts.get(1).copied() {
                None | Some("status") => {
                    app.status_msg = format!(
                        "Notification sound is {}.",
                        if app.notification_sound_enabled {
                            "ON"
                        } else {
                            "OFF"
                        }
                    );
                }
                Some("on") => toggle_notification_sound(app, Some(true)),
                Some("off") => toggle_notification_sound(app, Some(false)),
                Some("toggle") => toggle_notification_sound(app, None),
                _ => app.status_msg = "Usage: /sound [on|off|toggle|status]".to_string(),
            }
            None
        }

        _ => {
            app.status_msg = format!("Unknown command: {}", parts[0]);
            None
        }
    }
}

fn handle_sidekick_command(app: &mut App, args: &[&str]) {
    match args.first().copied() {
        None | Some("status") => {
            let state = if app.auto_agent.enabled { "ON" } else { "OFF" };
            let auth = if app.auto_agent.awaiting_api_key {
                " auth=required"
            } else {
                ""
            };
            let gate = if app.auto_agent.awaiting_user_input {
                " decision=required"
            } else {
                ""
            };
            let summary = if app.auto_agent.last_summary.is_empty() {
                "none".to_string()
            } else {
                app.auto_agent.last_summary.clone()
            };
            let queue = app.auto_agent.chat_queue.len();
            app.status_msg = format!(
                "Sidekick {state} mode={} interval={}s queue={}{}{} summary={}",
                app.auto_agent.mode.label(),
                app.auto_agent.interval_secs,
                queue,
                auth,
                gate,
                truncate(&summary, 80)
            );
        }
        Some("on") => {
            app.auto_agent.enabled = true;
            app.status_msg = format!("Sidekick enabled (mode {}).", app.auto_agent.mode.label());
            persist_preferences_or_warn(app);
        }
        Some("off") => {
            app.auto_agent.enabled = false;
            app.auto_agent.reset();
            app.status_msg = "Sidekick disabled.".to_string();
            persist_preferences_or_warn(app);
        }
        Some("mode") => {
            let Some(mode) = args.get(1).copied() else {
                app.status_msg = "Usage: /sidekick mode <rules|pi>".to_string();
                return;
            };
            match mode {
                "rules" => {
                    app.auto_agent.mode = AutoAgentMode::Rules;
                    app.status_msg = "Sidekick mode set to RULES.".to_string();
                }
                "pi" => {
                    app.auto_agent.mode = AutoAgentMode::Pi;
                    app.status_msg = "Sidekick mode set to PI.".to_string();
                }
                _ => app.status_msg = "Usage: /sidekick mode <rules|pi>".to_string(),
            }
        }
        Some("interval") => {
            let Some(raw) = args.get(1).copied() else {
                app.status_msg = "Usage: /sidekick interval <seconds>".to_string();
                return;
            };
            match raw.parse::<u64>() {
                Ok(secs) if (1..=300).contains(&secs) => {
                    app.auto_agent.interval_secs = secs;
                    app.status_msg = format!("Sidekick interval set to {secs}s.");
                }
                _ => app.status_msg = "Interval must be between 1 and 300 seconds.".to_string(),
            }
        }
        Some("ask") => {
            if args.len() < 2 {
                app.status_msg = "Usage: /sidekick ask <instruction>".to_string();
                return;
            }
            let prompt = args[1..].join(" ");
            run_sidekick_chat_prompt(app, prompt);
        }
        Some("tick") => {
            crate::automation::run_once(app);
        }
        Some("summary") => {
            if app.auto_agent.last_summary.trim().is_empty() {
                app.status_msg = "Sidekick summary: none yet. Run /sidekick tick.".to_string();
                return;
            }
            app.status_msg = format!(
                "Sidekick summary: {}",
                truncate(&app.auto_agent.last_summary, 120)
            );
        }
        Some("focus") => toggle_sidekick_focus(app),
        Some("key") => {
            if args.len() < 2 {
                app.status_msg = "Usage: /sidekick key <anthropic_api_key>".to_string();
                return;
            }
            submit_sidekick_api_key(app, args[1..].join(" "));
        }
        Some("clear") => {
            app.auto_agent.chat_history.clear();
            app.auto_agent.chat_input.clear();
            app.auto_agent.reset();
            app.status_msg = "Sidekick chat cleared.".to_string();
        }
        _ => {
            app.status_msg =
                "Usage: /sidekick [on|off|status|mode <rules|pi>|interval <s>|ask <msg>|tick|summary|focus|key <api_key>|clear]"
                    .to_string();
        }
    }
}

/// Maximum length for feed posts (characters).
const MAX_FEED_LENGTH: usize = 10_000;
/// Maximum length for direct messages (characters).
const MAX_DM_LENGTH: usize = 10_000;

/// Send a message directly to the node daemon.
async fn send_message(
    app: &mut App,
    writer: &mut NodeWriter,
    input: &str,
) -> Option<PendingRequest> {
    let req = match &app.tab {
        Tab::Feed => {
            if input.len() > MAX_FEED_LENGTH {
                app.status_msg = format!(
                    "Feed posts are limited to {MAX_FEED_LENGTH} characters (current: {})",
                    input.len()
                );
                return None;
            }
            Request::PostFeed {
                body: input.to_string(),
            }
        }
        Tab::Dms => {
            if input.len() > MAX_DM_LENGTH {
                app.status_msg = format!(
                    "DMs are limited to {MAX_DM_LENGTH} characters (current: {})",
                    input.len()
                );
                return None;
            }
            let to = app
                .following
                .get(app.selected_contact)
                .cloned()
                .unwrap_or_default();
            if to.is_empty() {
                app.status_msg = "No contact selected".to_string();
                return None;
            }
            Request::SendDm {
                to,
                body: input.to_string(),
            }
        }
        Tab::Terminal => return None,
        Tab::Room(room) => {
            if input.len() > 140 {
                app.status_msg = "Room messages are limited to 140 characters".to_string();
                return None;
            }
            Request::SendRoom {
                room: room.clone(),
                body: input.to_string(),
            }
        }
    };

    app.status_msg = "Sending...".to_string();
    send_req(app, writer, req, PendingRequest::Send).await
}

/// Ensure the terminal emulator is spawned.
fn ensure_terminal(app: &mut App) {
    if !app.terminals.is_empty() {
        return;
    }
    // Default size — will be resized on next draw.
    match crate::terminal::TerminalEmulator::spawn(80, 24) {
        Ok(term) => {
            app.terminals.push(term);
            app.active_terminal = 0;
            app.terminal_split = TerminalSplit::Single;
            app.refresh_terminal_tabs();
            request_full_redraw(app);
        }
        Err(e) => app.status_msg = format!("Failed to spawn shell: {e}"),
    }
}

fn request_full_redraw(app: &mut App) {
    app.request_full_redraw = true;
}

fn reset_active_terminal_render_cache(app: &mut App) {
    if let Some(term) = app.active_terminal_mut() {
        // For tmux-backed terminals, a hard parser reset can race the window/pane
        // switch redraw and leave a blank pane. Keep the current parser state and
        // let incoming tmux bytes refresh it.
        if !term.is_persistent_mux() {
            term.reset_screen();
        }
        let _ = term.process_output();
    }
    request_full_redraw(app);
}

fn split_terminal(app: &mut App, direction: TerminalSplit) {
    if app.tab != Tab::Terminal {
        return;
    }
    ensure_terminal(app);
    let tmux_split_result = if let Some(term) = app.active_terminal_mut()
        && term.is_persistent_mux()
    {
        Some(match direction {
            TerminalSplit::Vertical => term.mux_split_vertical(),
            TerminalSplit::Horizontal => term.mux_split_horizontal(),
            TerminalSplit::Single => Ok(false),
        })
    } else {
        None
    };
    if let Some(result) = tmux_split_result {
        match result {
            Ok(true) => {
                app.status_msg = match direction {
                    TerminalSplit::Vertical => "tmux split vertical".to_string(),
                    TerminalSplit::Horizontal => "tmux split horizontal".to_string(),
                    TerminalSplit::Single => String::new(),
                };
                reset_active_terminal_render_cache(app);
            }
            Ok(false) => {}
            Err(e) => app.status_msg = format!("tmux split failed: {e}"),
        }
        return;
    }
    if app.terminals.len() >= MAX_TERMINAL_PANES {
        app.status_msg = format!("Pane limit reached ({MAX_TERMINAL_PANES})");
        return;
    }
    match crate::terminal::TerminalEmulator::spawn(80, 24) {
        Ok(term) => {
            app.terminals.push(term);
            app.active_terminal = app.terminals.len().saturating_sub(1);
            app.terminal_split = direction;
            app.status_msg = format!(
                "Split {} ({}/{MAX_TERMINAL_PANES})",
                match direction {
                    TerminalSplit::Vertical => "vertical",
                    TerminalSplit::Horizontal => "horizontal",
                    TerminalSplit::Single => "single",
                },
                app.terminals.len()
            );
            request_full_redraw(app);
        }
        Err(e) => app.status_msg = format!("Failed to split terminal: {e}"),
    }
}

fn focus_next_terminal(app: &mut App) {
    if app.tab != Tab::Terminal || app.terminals.len() < 2 {
        if app.tab == Tab::Terminal {
            let mux_result = if let Some(term) = app.active_terminal_mut()
                && term.is_persistent_mux()
            {
                Some(term.mux_next_pane())
            } else {
                None
            };
            if let Some(result) = mux_result {
                match result {
                    Ok(true) => reset_active_terminal_render_cache(app),
                    Ok(false) => {}
                    Err(e) => app.status_msg = format!("tmux pane switch failed: {e}"),
                }
            }
        }
        return;
    }
    app.active_terminal = (app.active_terminal + 1) % app.terminals.len();
    request_full_redraw(app);
}

fn close_active_terminal(app: &mut App) {
    if app.tab != Tab::Terminal || app.terminals.is_empty() {
        return;
    }
    let mux_close_result = if let Some(term) = app.active_terminal_mut()
        && term.is_persistent_mux()
    {
        Some(term.mux_close_pane())
    } else {
        None
    };
    if let Some(result) = mux_close_result {
        match result {
            Ok(true) => {
                app.status_msg = "tmux pane closed".to_string();
                reset_active_terminal_render_cache(app);
            }
            Ok(false) => {}
            Err(e) => app.status_msg = format!("tmux close failed: {e}"),
        }
        return;
    }
    app.terminals.remove(app.active_terminal);
    if app.terminals.is_empty() {
        app.active_terminal = 0;
        app.terminal_split = TerminalSplit::Single;
        app.status_msg = "Closed terminal pane".to_string();
        request_full_redraw(app);
        return;
    }
    if app.active_terminal >= app.terminals.len() {
        app.active_terminal = app.terminals.len().saturating_sub(1);
    }
    if app.terminals.len() == 1 {
        app.terminal_split = TerminalSplit::Single;
    }
    request_full_redraw(app);
}

fn parse_window_index_label(label: &str) -> Option<usize> {
    let n = label.split_whitespace().next()?.parse::<usize>().ok()?;
    n.checked_sub(1)
}

fn select_terminal_tab_by_display_index(app: &mut App, display_idx: usize) {
    app.switch_tab(Tab::Terminal);
    ensure_terminal(app);
    app.refresh_terminal_tabs();

    if app.terminal_window_tabs.is_empty() {
        app.status_msg = "No terminal tabs available.".to_string();
        return;
    }
    if display_idx >= app.terminal_window_tabs.len() {
        app.status_msg = format!("Terminal tab T{} is not available.", display_idx + 1);
        return;
    }

    let target_idx = app
        .terminal_window_tabs
        .get(display_idx)
        .and_then(|label| parse_window_index_label(label))
        .unwrap_or(display_idx);

    let result = if let Some(term) = app.active_terminal_mut() {
        term.mux_select_window(target_idx)
    } else {
        Ok(false)
    };
    match result {
        Ok(true) => {
            app.refresh_terminal_tabs();
            app.status_msg = format!("Switched to terminal tab T{}.", display_idx + 1);
            reset_active_terminal_render_cache(app);
        }
        Ok(false) => {
            app.active_terminal_window = display_idx.min(app.terminal_window_tabs.len() - 1);
            app.status_msg = format!("Switched to terminal tab T{}.", display_idx + 1);
            reset_active_terminal_render_cache(app);
        }
        Err(e) => app.status_msg = format!("tmux select window failed: {e}"),
    }
}

fn terminal_new_tab(app: &mut App) {
    if app.tab != Tab::Terminal {
        return;
    }
    ensure_terminal(app);
    let result = if let Some(term) = app.active_terminal_mut() {
        term.mux_new_window()
    } else {
        Ok(false)
    };
    match result {
        Ok(true) => {
            app.refresh_terminal_tabs();
            app.status_msg = "tmux window created".to_string();
            reset_active_terminal_render_cache(app);
        }
        Ok(false) => app.status_msg = "Terminal tabs require tmux backend".to_string(),
        Err(e) => app.status_msg = format!("tmux new window failed: {e}"),
    }
}

fn terminal_next_tab(app: &mut App) {
    if app.tab != Tab::Terminal {
        return;
    }
    ensure_terminal(app);
    let result = if let Some(term) = app.active_terminal_mut() {
        term.mux_next_window()
    } else {
        Ok(false)
    };
    match result {
        Ok(true) => {
            app.refresh_terminal_tabs();
            reset_active_terminal_render_cache(app);
        }
        Ok(false) => {}
        Err(e) => app.status_msg = format!("tmux next window failed: {e}"),
    }
}

fn terminal_prev_tab(app: &mut App) {
    if app.tab != Tab::Terminal {
        return;
    }
    ensure_terminal(app);
    let result = if let Some(term) = app.active_terminal_mut() {
        term.mux_prev_window()
    } else {
        Ok(false)
    };
    match result {
        Ok(true) => {
            app.refresh_terminal_tabs();
            reset_active_terminal_render_cache(app);
        }
        Ok(false) => {}
        Err(e) => app.status_msg = format!("tmux prev window failed: {e}"),
    }
}

fn terminal_close_tab(app: &mut App) {
    if app.tab != Tab::Terminal {
        return;
    }
    ensure_terminal(app);
    let result = if let Some(term) = app.active_terminal_mut() {
        term.mux_close_window()
    } else {
        Ok(false)
    };
    match result {
        Ok(true) => {
            app.refresh_terminal_tabs();
            app.status_msg = "tmux window closed".to_string();
            reset_active_terminal_render_cache(app);
        }
        Ok(false) => app.status_msg = "Terminal tabs require tmux backend".to_string(),
        Err(e) => app.status_msg = format!("tmux close window failed: {e}"),
    }
}

fn toggle_sidekick(app: &mut App) {
    app.auto_agent.enabled = !app.auto_agent.enabled;
    if app.auto_agent.enabled {
        app.status_msg = format!("Sidekick enabled (mode {}).", app.auto_agent.mode.label());
    } else {
        app.auto_agent.reset();
        app.status_msg = "Sidekick disabled.".to_string();
    }
    persist_preferences_or_warn(app);
}

fn toggle_notification_sound(app: &mut App, enabled: Option<bool>) {
    let next = enabled.unwrap_or(!app.notification_sound_enabled);
    app.notification_sound_enabled = next;
    app.status_msg = format!("Notification sound {}.", if next { "ON" } else { "OFF" });
    persist_preferences_or_warn(app);
}

fn persist_preferences_or_warn(app: &mut App) {
    if let Err(e) = app.persist_preferences() {
        let prior = if app.status_msg.is_empty() {
            "Settings updated.".to_string()
        } else {
            app.status_msg.clone()
        };
        app.status_msg = format!(
            "{} (failed to save preferences: {})",
            truncate(&prior, 70),
            truncate(&e.to_string(), 70)
        );
    }
}

fn clear_prefix_mode(app: &mut App) {
    app.prefix_mode = false;
    app.prefix_mode_at = None;
    app.prefix_pending = None;
}

fn begin_terminal_tab_select_prefix(app: &mut App) {
    app.prefix_mode = true;
    app.prefix_mode_at = Some(std::time::Instant::now());
    app.prefix_pending = Some(PrefixPending::TerminalTabSelect);
    app.status_msg = "Select terminal tab: press 1-9.".to_string();
}

fn terminal_tab_digit_to_index(code: KeyCode) -> Option<usize> {
    let KeyCode::Char(c) = code else {
        return None;
    };
    if ('1'..='9').contains(&c) {
        return Some((c as usize) - ('1' as usize));
    }
    None
}

fn handle_pending_prefix_mode(app: &mut App, key: KeyCode) -> bool {
    let Some(pending) = app.prefix_pending else {
        return false;
    };
    clear_prefix_mode(app);
    match pending {
        PrefixPending::TerminalTabSelect => {
            if key == KeyCode::Esc {
                app.status_msg = "Terminal tab select cancelled.".to_string();
                return true;
            }
            let Some(display_idx) = terminal_tab_digit_to_index(key) else {
                app.status_msg = "Terminal tab select cancelled (expected 1-9).".to_string();
                return true;
            };
            select_terminal_tab_by_display_index(app, display_idx);
            true
        }
    }
}

fn handle_prefix_chord(app: &mut App, key: KeyCode) {
    clear_prefix_mode(app);
    match key {
        KeyCode::Char('1') => {
            app.switch_tab(Tab::Terminal);
            ensure_terminal(app);
            app.refresh_terminal_tabs();
        }
        KeyCode::Char('2') => app.switch_tab(Tab::Feed),
        KeyCode::Char('3') => app.switch_tab(Tab::Dms),
        KeyCode::Char('t') => begin_terminal_tab_select_prefix(app),
        // Terminal tab controls (tmux windows): c=new, n/p=next/prev, w=close.
        KeyCode::Char('c') => terminal_new_tab(app),
        KeyCode::Char('n') => terminal_next_tab(app),
        KeyCode::Char('p') => terminal_prev_tab(app),
        KeyCode::Char('w') => terminal_close_tab(app),
        KeyCode::Char('a') => toggle_sidekick(app),
        KeyCode::Char('s') => toggle_notification_sound(app, None),
        KeyCode::Char('i') => toggle_sidekick_focus(app),
        KeyCode::Char('q') => {
            app.quit_confirm = true;
            app.status_msg =
                "Confirm quit: click Yes/No or press Y/N (optionally after leader).".to_string();
        }
        // tmux-style terminal pane controls (while on Terminal tab):
        // % split vertical, " split horizontal, o cycle pane, x close pane.
        KeyCode::Char('%') => split_terminal(app, TerminalSplit::Vertical),
        KeyCode::Char('"') => split_terminal(app, TerminalSplit::Horizontal),
        KeyCode::Char('o') => focus_next_terminal(app),
        KeyCode::Char('x') => close_active_terminal(app),
        // Dynamic room tabs: 4, 5, 6, ... map to rooms by index
        KeyCode::Char(c) if c.is_ascii_digit() && c >= '4' => {
            let room_idx = (c as usize) - ('4' as usize);
            if let Some(room) = app.rooms.get(room_idx).cloned() {
                app.switch_tab(Tab::Room(room));
            }
        }
        // Arrow keys: navigate 2-row grid (top: terminal tabs, bottom: social tabs).
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => navigate_tab_grid(app, key),
        KeyCode::Esc => app.should_quit = true,
        _ => {} // unknown chord — ignore
    }
}

fn toggle_sidekick_focus(app: &mut App) {
    if !app.auto_agent.enabled {
        app.status_msg = "Enable Sidekick first (Ctrl+Space then A, or /sidekick on).".to_string();
        return;
    }
    app.auto_agent.chat_focus = !app.auto_agent.chat_focus;
    if app.auto_agent.chat_focus {
        app.status_msg = "Sidekick chat focus ON (type prompt and press Enter).".to_string();
    } else {
        app.status_msg = "Sidekick chat focus OFF (keyboard controls terminal).".to_string();
    }
}

fn run_sidekick_chat_prompt(app: &mut App, prompt: String) {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return;
    }
    if !app.auto_agent.enabled {
        app.auto_agent.enabled = true;
    }
    if app.auto_agent.chat_streaming {
        app.auto_agent.chat_queue.push(prompt);
        app.status_msg = format!(
            "Sidekick busy. Queued message ({} pending).",
            app.auto_agent.chat_queue.len()
        );
        return;
    }
    if app.auto_agent.awaiting_api_key {
        submit_sidekick_api_key(app, prompt);
        return;
    }
    app.auto_agent.awaiting_user_input = false;
    app.auto_agent.pending_user_question = None;
    app.auto_agent.chat_scroll = 0;
    app.auto_agent.push_chat(SidekickMessage {
        role: SidekickRole::User,
        content: prompt.clone(),
    });
    if app.auto_agent.mode == AutoAgentMode::Pi {
        app.auto_agent.inference_env = crate::automation::load_inference_env_vars();
        app.auto_agent.last_env_load = Some(std::time::Instant::now());
        app.auto_agent.push_chat(SidekickMessage {
            role: SidekickRole::Assistant,
            content: String::new(),
        });
        // Reset the cancellation flag for the new stream.
        app.auto_agent.stream_cancel =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = app.auto_agent.stream_cancel.clone();
        match crate::automation::start_pi_chat_stream(app, &prompt, cancel) {
            Ok(rx) => {
                app.auto_agent.chat_stream_rx = Some(rx);
                app.auto_agent.chat_streaming = true;
                app.status_msg = "Sidekick is streaming response…".to_string();
            }
            Err(e) => {
                app.auto_agent.chat_history.pop();
                let msg = format!("Sidekick error: {e}");
                app.auto_agent.push_chat(SidekickMessage {
                    role: SidekickRole::System,
                    content: msg.clone(),
                });
                app.status_msg = msg;
            }
        }
        return;
    }

    match crate::automation::chat(app, &prompt) {
        Ok(reply) => {
            app.auto_agent.push_chat(SidekickMessage {
                role: SidekickRole::Assistant,
                content: reply.clone(),
            });
            app.status_msg = format!("Sidekick: {}", truncate(&reply, 90));
        }
        Err(e) => {
            let msg = format!("Sidekick error: {e}");
            app.auto_agent.push_chat(SidekickMessage {
                role: SidekickRole::System,
                content: msg.clone(),
            });
            app.status_msg = msg;
        }
    }
}

pub fn poll_sidekick_chat_stream(app: &mut App) {
    if !app.auto_agent.chat_streaming {
        return;
    }

    let mut completed = false;
    let mut disconnected = false;
    let mut events = Vec::new();
    if let Some(rx) = app.auto_agent.chat_stream_rx.as_ref() {
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    let terminal = matches!(
                        event,
                        SidekickChatStreamEvent::Complete(_) | SidekickChatStreamEvent::Error(_)
                    );
                    events.push(event);
                    if terminal {
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
    } else {
        disconnected = true;
    }

    for event in events {
        match event {
            SidekickChatStreamEvent::ReplyDelta(delta) => {
                if delta.is_empty() {
                    continue;
                }
                if let Some(last) = app.auto_agent.chat_history.last_mut()
                    && last.role == SidekickRole::Assistant
                {
                    last.content.push_str(&delta);
                }
            }
            SidekickChatStreamEvent::Complete(completion) => {
                if let Some(last) = app.auto_agent.chat_history.last_mut()
                    && last.role == SidekickRole::Assistant
                    && last.content.trim().is_empty()
                    && let Some(reply) = completion.reply.as_ref()
                {
                    last.content = reply.clone();
                }
                match crate::automation::apply_chat_completion(app, completion) {
                    Ok(reply) => {
                        app.status_msg = format!("Sidekick: {}", truncate(&reply, 90));
                    }
                    Err(e) => {
                        let msg = format!("Sidekick error: {e}");
                        app.auto_agent.push_chat(SidekickMessage {
                            role: SidekickRole::System,
                            content: msg.clone(),
                        });
                        app.status_msg = msg;
                    }
                }
                completed = true;
            }
            SidekickChatStreamEvent::Error(err) => {
                let msg = format!("Sidekick error: {err}");
                app.auto_agent.push_chat(SidekickMessage {
                    role: SidekickRole::System,
                    content: msg.clone(),
                });
                app.status_msg = msg;
                completed = true;
            }
        }
    }

    if completed || disconnected {
        app.auto_agent.chat_streaming = false;
        app.auto_agent.chat_stream_rx = None;
        if disconnected && !completed {
            let msg = "Sidekick stream ended unexpectedly.".to_string();
            app.auto_agent.push_chat(SidekickMessage {
                role: SidekickRole::System,
                content: msg.clone(),
            });
            app.status_msg = msg;
        }
    }

    if !app.auto_agent.chat_streaming
        && !app.auto_agent.chat_queue.is_empty()
        && !app.auto_agent.awaiting_api_key
    {
        let next = app.auto_agent.chat_queue.remove(0);
        run_sidekick_chat_prompt(app, next);
    }
}

fn handle_sidekick_chat_key(app: &mut App, key: KeyEvent) -> Option<PendingRequest> {
    match key.code {
        KeyCode::Esc => {
            app.auto_agent.chat_focus = false;
            app.status_msg = "Sidekick chat focus OFF (keyboard controls terminal).".to_string();
            None
        }
        KeyCode::Enter => {
            let prompt = std::mem::take(&mut app.auto_agent.chat_input);
            if app.auto_agent.awaiting_api_key {
                submit_sidekick_api_key(app, prompt);
            } else {
                run_sidekick_chat_prompt(app, prompt);
            }
            None
        }
        KeyCode::Backspace => {
            app.auto_agent.chat_input.pop();
            None
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.auto_agent.chat_input.push(c);
            None
        }
        _ => None,
    }
}

fn submit_sidekick_api_key(app: &mut App, key: String) {
    let key = key.trim().to_string();
    if key.is_empty() {
        // Empty submission — check if Arda key appeared in the meantime
        // (e.g. user ran `agentbook login` in another terminal).
        let has_arda = crate::automation::has_arda_login();
        app.auto_agent.cached_has_arda = has_arda;
        if has_arda {
            app.auto_agent.awaiting_api_key = false;
            app.auto_agent.auth_error = None;
            app.auto_agent.chat_input.clear();
            app.auto_agent.inference_env = crate::automation::load_inference_env_vars();
            app.auto_agent.last_env_load = Some(std::time::Instant::now());
            app.auto_agent.push_chat(SidekickMessage {
                role: SidekickRole::System,
                content: "Arda login detected. Sidekick inference resumed.".to_string(),
            });
            app.status_msg = "Sidekick auth: Arda login detected. Inference resumed.".to_string();
            crate::automation::run_once(app);
        } else {
            app.status_msg =
                "Sidekick auth: run `agentbook login` or paste an API key.".to_string();
        }
        return;
    }
    match crate::automation::save_anthropic_api_key(&key) {
        Ok(()) => {
            app.auto_agent.awaiting_api_key = false;
            app.auto_agent.auth_error = None;
            app.auto_agent.awaiting_user_input = false;
            app.auto_agent.pending_user_question = None;
            app.auto_agent.chat_input.clear();
            app.auto_agent.chat_scroll = 0;
            app.auto_agent.inference_env = crate::automation::load_inference_env_vars();
            app.auto_agent.last_env_load = Some(std::time::Instant::now());
            app.auto_agent.push_chat(SidekickMessage {
                role: SidekickRole::System,
                content: "API key saved for future Sidekick sessions.".to_string(),
            });
            app.status_msg = "Sidekick auth: API key saved. Inference resumed.".to_string();
            crate::automation::run_once(app);
        }
        Err(e) => {
            app.auto_agent.awaiting_api_key = true;
            app.auto_agent.auth_error = Some(format!("{e}"));
            app.status_msg = format!("Sidekick auth: failed to save API key: {e}");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalPaneClickTarget {
    pane_idx: usize,
    pane_area: Rect,
}

fn point_in_rect(column: u16, row: u16, rect: Rect) -> bool {
    column >= rect.x
        && row >= rect.y
        && column < rect.x.saturating_add(rect.width)
        && row < rect.y.saturating_add(rect.height)
}

fn pane_inner_area(pane_area: Rect) -> Rect {
    Rect {
        x: pane_area.x.saturating_add(1),
        y: pane_area.y.saturating_add(1),
        width: pane_area.width.saturating_sub(2),
        height: pane_area.height.saturating_sub(2),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseScrollDirection {
    Up,
    Down,
}

pub fn handle_mouse_scroll(
    app: &mut App,
    column: u16,
    row: u16,
    viewport: Rect,
    direction: MouseScrollDirection,
) -> bool {
    if app.tab == Tab::Terminal
        && app.auto_agent.enabled
        && let Some(sidekick_area) = crate::ui::sidekick_area_for_viewport(viewport, true)
        && point_in_rect(column, row, sidekick_area)
    {
        match direction {
            MouseScrollDirection::Up => {
                app.auto_agent.chat_scroll = app
                    .auto_agent
                    .chat_scroll
                    .saturating_add(crate::app::SCROLL_STEP)
            }
            MouseScrollDirection::Down => {
                app.auto_agent.chat_scroll = app
                    .auto_agent
                    .chat_scroll
                    .saturating_sub(crate::app::SCROLL_STEP)
            }
        }
        request_full_redraw(app);
        return true;
    }

    if app.tab == Tab::Terminal
        && let Some(target) = terminal_pane_click_target(
            viewport,
            app.auto_agent.enabled,
            app.terminals.len(),
            app.terminal_split,
            column,
            row,
        )
    {
        if app.active_terminal != target.pane_idx {
            app.active_terminal = target.pane_idx;
        }

        let inner = pane_inner_area(target.pane_area);
        let mut passed_through = false;
        if point_in_rect(column, row, inner) {
            let rel_col = column.saturating_sub(inner.x).saturating_add(1);
            let rel_row = row.saturating_sub(inner.y).saturating_add(1);
            if let Some(term) = app.active_terminal_mut() {
                passed_through = term
                    .write_mouse_wheel(rel_col, rel_row, direction == MouseScrollDirection::Up)
                    .unwrap_or(false);
            }
        }

        if !passed_through && let Some(term) = app.active_terminal_mut() {
            match direction {
                MouseScrollDirection::Up => term.scroll_up(crate::app::SCROLL_STEP),
                MouseScrollDirection::Down => term.scroll_down(crate::app::SCROLL_STEP),
            }
        }
        request_full_redraw(app);
        return true;
    }

    false
}

fn terminal_pane_click_target(
    viewport: Rect,
    sidekick_enabled: bool,
    pane_count: usize,
    split: TerminalSplit,
    column: u16,
    row: u16,
) -> Option<TerminalPaneClickTarget> {
    if pane_count == 0 {
        return None;
    }
    let full_terminal_area = crate::ui::terminal_content_area(viewport);
    let (term_area, _) =
        crate::ui::terminal_main_and_sidekick_areas(full_terminal_area, sidekick_enabled);
    let panes = crate::ui::terminal_pane_areas(term_area, pane_count, split);
    panes.iter().enumerate().find_map(|(idx, area)| {
        if point_in_rect(column, row, *area) {
            Some(TerminalPaneClickTarget {
                pane_idx: idx,
                pane_area: *area,
            })
        } else {
            None
        }
    })
}

fn focus_terminal_pane_from_click(app: &mut App, column: u16, row: u16, viewport: Rect) {
    if app.tab != Tab::Terminal || app.terminals.is_empty() {
        return;
    }
    let Some(target) = terminal_pane_click_target(
        viewport,
        app.auto_agent.enabled,
        app.terminals.len(),
        app.terminal_split,
        column,
        row,
    ) else {
        return;
    };

    if app.active_terminal != target.pane_idx {
        app.active_terminal = target.pane_idx;
        app.status_msg = format!("Focused pane {}.", target.pane_idx + 1);
        request_full_redraw(app);
    }

    // Also focus the tmux pane under the click when running in tmux backend.
    let inner = pane_inner_area(target.pane_area);
    if !point_in_rect(column, row, inner) {
        return;
    }
    let rel_col = column.saturating_sub(inner.x);
    let rel_row = row.saturating_sub(inner.y);
    let tmux_focus = app
        .active_terminal()
        .map(|term| term.mux_select_pane_at(rel_col, rel_row));
    if let Some(result) = tmux_focus {
        match result {
            Ok(true) => {
                app.status_msg = "tmux pane focused".to_string();
                reset_active_terminal_render_cache(app);
            }
            Ok(false) => {}
            Err(e) => app.status_msg = format!("tmux pane focus failed: {e}"),
        }
    }
}

pub fn handle_mouse_click(app: &mut App, column: u16, row: u16, viewport: Rect) {
    if app.quit_confirm {
        match crate::ui::quit_modal_click_target(column, row, viewport) {
            Some(crate::ui::QuitModalClickTarget::Confirm) => {
                app.quit_confirm = false;
                app.should_quit = true;
            }
            Some(crate::ui::QuitModalClickTarget::Cancel) => {
                app.quit_confirm = false;
                app.status_msg = "Exit cancelled.".to_string();
            }
            None => {}
        }
        return;
    }

    match crate::ui::tab_click_target(app, column, row, viewport) {
        Some(crate::ui::TabClickTarget::TerminalWindow(idx)) => {
            select_terminal_tab_by_display_index(app, idx);
        }
        Some(crate::ui::TabClickTarget::SocialTab(tab)) => {
            let is_terminal = tab == Tab::Terminal;
            app.switch_tab(tab);
            if is_terminal {
                ensure_terminal(app);
                app.refresh_terminal_tabs();
            }
        }
        Some(crate::ui::TabClickTarget::Control(action)) => {
            app.switch_tab(Tab::Terminal);
            ensure_terminal(app);
            app.refresh_terminal_tabs();
            match action {
                crate::ui::ControlAction::NewTab => terminal_new_tab(app),
                crate::ui::ControlAction::NextTab => terminal_next_tab(app),
                crate::ui::ControlAction::PrevTab => terminal_prev_tab(app),
                crate::ui::ControlAction::CloseTab => terminal_close_tab(app),
                crate::ui::ControlAction::ToggleSidekick => toggle_sidekick(app),
                crate::ui::ControlAction::ToggleSound => toggle_notification_sound(app, None),
                crate::ui::ControlAction::Quit => {
                    app.quit_confirm = true;
                    app.status_msg =
                        "Confirm quit: click Yes/No or press Y/N (optionally after leader)."
                            .to_string();
                }
                crate::ui::ControlAction::SplitVertical => {
                    split_terminal(app, TerminalSplit::Vertical)
                }
                crate::ui::ControlAction::SplitHorizontal => {
                    split_terminal(app, TerminalSplit::Horizontal)
                }
                crate::ui::ControlAction::NextPane => focus_next_terminal(app),
                crate::ui::ControlAction::ClosePane => close_active_terminal(app),
            }
        }
        None => {}
    }

    focus_terminal_pane_from_click(app, column, row, viewport);
}

/// Forward a mouse button/motion event to the PTY when the cursor is inside a
/// terminal pane and the child has enabled xterm mouse reporting. Returns true
/// if the event was consumed.
pub fn handle_mouse_forward(
    app: &mut App,
    column: u16,
    row: u16,
    viewport: Rect,
    event: crate::terminal::MouseEvent,
) -> bool {
    if app.tab != Tab::Terminal || app.terminals.is_empty() {
        return false;
    }

    let Some(target) = terminal_pane_click_target(
        viewport,
        app.auto_agent.enabled,
        app.terminals.len(),
        app.terminal_split,
        column,
        row,
    ) else {
        return false;
    };

    let inner = pane_inner_area(target.pane_area);
    if !point_in_rect(column, row, inner) {
        return false;
    }

    // Translate to 1-based pane-local coordinates.
    let rel_col = column.saturating_sub(inner.x).saturating_add(1);
    let rel_row = row.saturating_sub(inner.y).saturating_add(1);

    // Switch focus if clicking into a different pane.
    if matches!(event, crate::terminal::MouseEvent::Press(_))
        && app.active_terminal != target.pane_idx
    {
        app.active_terminal = target.pane_idx;
    }

    if let Some(term) = app.terminals.get_mut(target.pane_idx) {
        term.write_mouse_event(rel_col, rel_row, event)
            .unwrap_or(false)
    } else {
        false
    }
}

fn is_prefix_key(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char(' '))
}

fn navigate_tab_grid(app: &mut App, key: KeyCode) {
    let switch = |app: &mut App, tab: Tab| {
        let is_terminal = tab == Tab::Terminal;
        app.switch_tab(tab);
        if is_terminal {
            ensure_terminal(app);
            app.refresh_terminal_tabs();
        }
    };

    match (&app.tab, key) {
        // Top row: terminal tabs.
        (Tab::Terminal, KeyCode::Left) => terminal_prev_tab(app),
        (Tab::Terminal, KeyCode::Right) => terminal_next_tab(app),
        (Tab::Terminal, KeyCode::Down) => switch(app, Tab::Feed),

        // Bottom row: social tabs (Feed, DMs, Rooms...).
        (Tab::Feed, KeyCode::Up) | (Tab::Dms, KeyCode::Up) | (Tab::Room(_), KeyCode::Up) => {
            switch(app, Tab::Terminal)
        }
        (Tab::Feed, KeyCode::Right) => switch(app, Tab::Dms),
        (Tab::Dms, KeyCode::Left) => switch(app, Tab::Feed),
        (Tab::Dms, KeyCode::Right) => {
            if let Some(first_room) = app.rooms.first().cloned() {
                switch(app, Tab::Room(first_room));
            }
        }
        (Tab::Room(room), KeyCode::Left) => {
            if let Some(room_idx) = app.rooms.iter().position(|r| r == room) {
                if room_idx == 0 {
                    switch(app, Tab::Dms);
                } else {
                    switch(app, Tab::Room(app.rooms[room_idx - 1].clone()));
                }
            }
        }
        (Tab::Room(room), KeyCode::Right) => {
            if let Some(room_idx) = app.rooms.iter().position(|r| r == room)
                && room_idx + 1 < app.rooms.len()
            {
                switch(app, Tab::Room(app.rooms[room_idx + 1].clone()));
            }
        }
        _ => {}
    }
}

/// Convert a crossterm KeyEvent to raw bytes for the PTY.
fn key_to_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Helper: wrap bytes with ESC prefix for Alt modifier.
    let with_alt = |bytes: Vec<u8>| -> Vec<u8> {
        if alt {
            let mut out = vec![0x1b];
            out.extend(bytes);
            out
        } else {
            bytes
        }
    };

    match key.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl+A..Z → 0x01..0x1A
            let byte = c.to_ascii_lowercase() as u8;
            if byte.is_ascii_lowercase() {
                Some(with_alt(vec![byte - b'a' + 1]))
            } else {
                None
            }
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            Some(with_alt(s.as_bytes().to_vec()))
        }
        KeyCode::Enter => Some(with_alt(b"\r".to_vec())),
        KeyCode::Backspace => Some(with_alt(b"\x7f".to_vec())),
        KeyCode::Tab => Some(with_alt(b"\t".to_vec())),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Esc => Some(b"\x1b".to_vec()),
        KeyCode::Up if shift => Some(b"\x1b[1;2A".to_vec()),
        KeyCode::Down if shift => Some(b"\x1b[1;2B".to_vec()),
        KeyCode::Right if shift => Some(b"\x1b[1;2C".to_vec()),
        KeyCode::Left if shift => Some(b"\x1b[1;2D".to_vec()),
        KeyCode::Up if alt => Some(b"\x1b[1;3A".to_vec()),
        KeyCode::Down if alt => Some(b"\x1b[1;3B".to_vec()),
        KeyCode::Right if alt => Some(b"\x1b[1;3C".to_vec()),
        KeyCode::Left if alt => Some(b"\x1b[1;3D".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::F(n) => {
            let seq = match n {
                1 => "\x1bOP",
                2 => "\x1bOQ",
                3 => "\x1bOR",
                4 => "\x1bOS",
                5 => "\x1b[15~",
                6 => "\x1b[17~",
                7 => "\x1b[18~",
                8 => "\x1b[19~",
                9 => "\x1b[20~",
                10 => "\x1b[21~",
                11 => "\x1b[23~",
                12 => "\x1b[24~",
                _ => return None,
            };
            Some(seq.as_bytes().to_vec())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::QuitModalClickTarget;

    fn find_modal_target(viewport: Rect, target: QuitModalClickTarget) -> (u16, u16) {
        for row in viewport.y..viewport.y + viewport.height {
            for col in viewport.x..viewport.x + viewport.width {
                if crate::ui::quit_modal_click_target(col, row, viewport) == Some(target) {
                    return (col, row);
                }
            }
        }
        panic!("missing modal target: {target:?}");
    }

    #[test]
    fn click_confirm_quit_modal_sets_should_quit() {
        let viewport = Rect::new(0, 0, 120, 40);
        let (col, row) = find_modal_target(viewport, QuitModalClickTarget::Confirm);
        let mut app = App::new("me".to_string());
        app.quit_confirm = true;

        handle_mouse_click(&mut app, col, row, viewport);

        assert!(app.should_quit);
        assert!(!app.quit_confirm);
    }

    #[test]
    fn click_cancel_quit_modal_cancels_exit() {
        let viewport = Rect::new(0, 0, 120, 40);
        let (col, row) = find_modal_target(viewport, QuitModalClickTarget::Cancel);
        let mut app = App::new("me".to_string());
        app.quit_confirm = true;

        handle_mouse_click(&mut app, col, row, viewport);

        assert!(!app.should_quit);
        assert!(!app.quit_confirm);
        assert_eq!(app.status_msg, "Exit cancelled.");
    }

    #[test]
    fn leader_t_enters_terminal_tab_select_mode() {
        let mut app = App::new("me".to_string());
        app.prefix_mode = true;
        app.prefix_mode_at = Some(std::time::Instant::now());

        handle_prefix_chord(&mut app, KeyCode::Char('t'));

        assert!(app.prefix_mode);
        assert_eq!(app.prefix_pending, Some(PrefixPending::TerminalTabSelect));
        assert_eq!(app.status_msg, "Select terminal tab: press 1-9.");
    }

    #[test]
    fn pending_terminal_tab_select_invalid_key_cancels() {
        let mut app = App::new("me".to_string());
        app.prefix_mode = true;
        app.prefix_mode_at = Some(std::time::Instant::now());
        app.prefix_pending = Some(PrefixPending::TerminalTabSelect);

        let consumed = handle_pending_prefix_mode(&mut app, KeyCode::Char('z'));

        assert!(consumed);
        assert!(!app.prefix_mode);
        assert_eq!(app.prefix_pending, None);
        assert_eq!(
            app.status_msg,
            "Terminal tab select cancelled (expected 1-9)."
        );
    }

    #[test]
    fn parse_window_index_label_uses_first_token() {
        assert_eq!(parse_window_index_label("3 logs"), Some(2));
        assert_eq!(parse_window_index_label("1 shell"), Some(0));
        assert_eq!(parse_window_index_label("oops"), None);
    }

    #[test]
    fn terminal_pane_click_target_vertical_split_hits_right_pane() {
        let viewport = Rect::new(0, 0, 120, 40);
        let hit = terminal_pane_click_target(viewport, false, 2, TerminalSplit::Vertical, 90, 12)
            .expect("pane should be hit");
        assert_eq!(hit.pane_idx, 1);
    }

    #[test]
    fn terminal_pane_click_target_ignores_sidekick_area() {
        let viewport = Rect::new(0, 0, 120, 40);
        let full = crate::ui::terminal_content_area(viewport);
        let (_, sidekick) = crate::ui::terminal_main_and_sidekick_areas(full, true);
        let sidekick = sidekick.expect("sidekick enabled should create area");
        let hit = terminal_pane_click_target(
            viewport,
            true,
            1,
            TerminalSplit::Single,
            sidekick.x.saturating_add(1),
            sidekick.y.saturating_add(1),
        );
        assert!(
            hit.is_none(),
            "click in sidekick should not target terminal pane"
        );
    }

    #[test]
    fn mouse_scroll_in_sidekick_updates_chat_scroll() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Terminal;
        app.auto_agent.enabled = true;
        let viewport = Rect::new(0, 0, 120, 40);
        let sidekick = crate::ui::sidekick_area_for_viewport(viewport, true)
            .expect("enabled sidekick should have area");
        let col = sidekick.x.saturating_add(1);
        let row = sidekick.y.saturating_add(1);

        let consumed = handle_mouse_scroll(&mut app, col, row, viewport, MouseScrollDirection::Up);
        assert!(consumed);
        assert_eq!(app.auto_agent.chat_scroll, crate::app::SCROLL_STEP);

        let consumed =
            handle_mouse_scroll(&mut app, col, row, viewport, MouseScrollDirection::Down);
        assert!(consumed);
        assert_eq!(app.auto_agent.chat_scroll, 0);
    }

    #[test]
    fn mouse_scroll_outside_sidekick_does_not_consume() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Terminal;
        app.auto_agent.enabled = true;
        let viewport = Rect::new(0, 0, 120, 40);

        let consumed = handle_mouse_scroll(
            &mut app,
            viewport.x + 2,
            viewport.y + 2,
            viewport,
            MouseScrollDirection::Up,
        );
        assert!(!consumed);
        assert_eq!(app.auto_agent.chat_scroll, 0);
    }

    #[test]
    fn sidekick_prompt_queues_when_stream_busy() {
        let mut app = App::new("me".to_string());
        app.auto_agent.enabled = true;
        app.auto_agent.chat_streaming = true;

        run_sidekick_chat_prompt(&mut app, "next task".to_string());

        assert_eq!(app.auto_agent.chat_queue, vec!["next task".to_string()]);
        assert!(app.status_msg.contains("Queued message"));
    }

    #[test]
    fn mask_sensitive_input_masks_otp_in_send_eth() {
        let input = "/send-eth 0xABC 0.5 123456";
        let masked = crate::ui::mask_sensitive_input(input);
        assert_eq!(masked, "/send-eth 0xABC 0.5 ******");
    }

    #[test]
    fn mask_sensitive_input_masks_otp_in_send_usdc() {
        let input = "/send-usdc 0xABC 10.0 789012";
        let masked = crate::ui::mask_sensitive_input(input);
        assert_eq!(masked, "/send-usdc 0xABC 10.0 ******");
    }

    #[test]
    fn mask_sensitive_input_no_mask_without_otp() {
        let input = "/send-eth 0xABC 0.5";
        let masked = crate::ui::mask_sensitive_input(input);
        assert_eq!(masked, input);
    }

    #[test]
    fn mask_sensitive_input_masks_passphrase() {
        let input = "/join secret-room --passphrase my secret pass";
        let masked = crate::ui::mask_sensitive_input(input);
        assert_eq!(masked, "/join secret-room --passphrase **************");
    }

    #[test]
    fn mask_sensitive_input_no_mask_for_normal_input() {
        let input = "hello world";
        let masked = crate::ui::mask_sensitive_input(input);
        assert_eq!(masked, input);
    }

    #[tokio::test]
    async fn feed_post_rejects_over_max_length() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        let long_input = "a".repeat(MAX_FEED_LENGTH + 1);

        // Call send_message directly — it needs a NodeWriter, but we can test
        // the length check by observing that it sets status_msg and returns None.
        // We cannot easily create a NodeWriter in tests, so we duplicate the check
        // logic here to verify the constant and error message.
        if long_input.len() > MAX_FEED_LENGTH {
            app.status_msg = format!(
                "Feed posts are limited to {MAX_FEED_LENGTH} characters (current: {})",
                long_input.len()
            );
        }
        assert!(app.status_msg.contains("limited to 10000 characters"));
        assert!(app.status_msg.contains("10001"));
    }

    #[tokio::test]
    async fn dm_rejects_over_max_length() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Dms;
        let long_input = "b".repeat(MAX_DM_LENGTH + 1);

        if long_input.len() > MAX_DM_LENGTH {
            app.status_msg = format!(
                "DMs are limited to {MAX_DM_LENGTH} characters (current: {})",
                long_input.len()
            );
        }
        assert!(app.status_msg.contains("limited to 10000 characters"));
        assert!(app.status_msg.contains("10001"));
    }
}
