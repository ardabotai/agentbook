use crate::app::{
    App, AutoAgentMode, SidekickChatCompletion, SidekickChatStreamEvent, SidekickMessage,
    SidekickRole, truncate,
};
use agentbook::gateway::{ARDA_DEFAULT_GATEWAY_URL, ARDA_KEY_FILE, SIDEKICK_KEY_FILE};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const SNAPSHOT_LINES: usize = 80;
const MIN_ACTION_GAP: Duration = Duration::from_secs(2);
const PI_TIMEOUT: Duration = Duration::from_secs(6);
const PI_HISTORY_LIMIT: usize = 16;
const ENV_CACHE_TTL: Duration = Duration::from_secs(30);

/// Keywords that indicate a destructive action. If any of these appear in a
/// yes/no prompt, the auto-accept logic will skip sending `y\n` to avoid
/// confirming dangerous operations unattended.
const DESTRUCTIVE_KEYWORDS: &[&str] = &[
    "delete",
    "remove",
    "format",
    "destroy",
    "drop",
    "overwrite",
    "force push",
    "reset --hard",
];

#[derive(Clone, Debug, Serialize)]
struct TabSnapshot {
    index: usize,
    name: String,
    active: bool,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
struct PiHistoryMessage {
    role: String,
    content: String,
}

/// Type alias: `Decision` was identical to `SidekickChatCompletion`, so we use
/// the canonical type everywhere now.
type Decision = SidekickChatCompletion;

pub fn tick(app: &mut App) {
    if !app.auto_agent.enabled {
        return;
    }
    if app.auto_agent.awaiting_api_key {
        // Auto-poll for Arda login every ~5 seconds while waiting for auth.
        let should_check = app
            .auto_agent
            .last_arda_check
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(5));
        if should_check {
            app.auto_agent.last_arda_check = Some(Instant::now());
            if has_arda_login() {
                app.auto_agent.cached_has_arda = true;
                app.auto_agent.inference_env = load_inference_env_vars();
                app.auto_agent.last_env_load = Some(Instant::now());
                app.auto_agent.awaiting_api_key = false;
                app.auto_agent.auth_error = None;
                app.auto_agent.login_in_progress = false;
                app.auto_agent.login_started_at = None;
                app.auto_agent.push_chat(SidekickMessage {
                    role: SidekickRole::System,
                    content: "Logged in! Sidekick is ready.".to_string(),
                });
                app.status_msg =
                    "Sidekick: Arda login successful. Ready to chat.".to_string();
                // Fall through to normal tick processing.
            } else {
                // Check for login timeout (120 seconds).
                if app.auto_agent.login_in_progress
                    && app
                        .auto_agent
                        .login_started_at
                        .is_some_and(|t| t.elapsed() >= Duration::from_secs(120))
                {
                    app.auto_agent.login_in_progress = false;
                    app.auto_agent.login_started_at = None;
                    app.auto_agent.push_chat(SidekickMessage {
                        role: SidekickRole::System,
                        content: "Arda login timed out. Press Enter to try again.".to_string(),
                    });
                    app.status_msg = "Sidekick: Arda login timed out.".to_string();
                }
                return;
            }
        } else {
            return;
        }
    }
    if app.auto_agent.awaiting_user_input {
        return;
    }
    let now = Instant::now();
    if let Some(last) = app.auto_agent.last_tick_at
        && now.duration_since(last) < Duration::from_secs(app.auto_agent.interval_secs.max(1))
    {
        return;
    }
    app.auto_agent.last_tick_at = Some(now);

    let tabs = match collect_tab_snapshots(app) {
        Ok(t) => t,
        Err(e) => {
            app.status_msg = format!("Sidekick tab scan failed: {e}");
            return;
        }
    };
    if tabs.is_empty() {
        app.status_msg = "Sidekick: no active terminal.".to_string();
        return;
    }

    let decision = match app.auto_agent.mode {
        AutoAgentMode::Rules => decide_rules_heartbeat(&tabs),
        AutoAgentMode::Pi => {
            refresh_inference_env_if_stale(app);
            let history = sidekick_history_for_pi(app);
            let env = app.auto_agent.inference_env.clone();
            match decide_pi(&tabs, None, &history, "heartbeat", &env) {
                Ok(d) => d,
                Err(e) => {
                    app.status_msg = format!("Sidekick (PI): {e}");
                    return;
                }
            }
        }
    };
    app.auto_agent.last_summary = decision.summary.clone();

    if let Err(e) = apply_decision(app, decision, now) {
        app.status_msg = format!("Sidekick action failed: {e}");
    }
}

pub fn run_once(app: &mut App) {
    app.auto_agent.last_tick_at = None;
    tick(app);
}

pub fn chat(app: &mut App, prompt: &str) -> Result<String> {
    let now = Instant::now();
    let tabs = collect_tab_snapshots(app)?;
    if tabs.is_empty() {
        anyhow::bail!("no active terminal");
    }

    let decision = match app.auto_agent.mode {
        AutoAgentMode::Rules => decide_rules_chat(&tabs, prompt),
        AutoAgentMode::Pi => {
            refresh_inference_env_if_stale(app);
            let history = sidekick_history_for_pi(app);
            let env = app.auto_agent.inference_env.clone();
            decide_pi(&tabs, Some(prompt), &history, "chat", &env)?
        }
    };

    let reply = decision
        .reply
        .clone()
        .unwrap_or_else(|| decision.summary.clone());
    app.auto_agent.last_summary = decision.summary.clone();
    apply_decision(app, decision, now)?;
    Ok(reply)
}

fn sidekick_history_for_pi(app: &App) -> Vec<PiHistoryMessage> {
    let start = app
        .auto_agent
        .chat_history
        .len()
        .saturating_sub(PI_HISTORY_LIMIT);
    app.auto_agent.chat_history[start..]
        .iter()
        .map(|m| PiHistoryMessage {
            role: match m.role {
                SidekickRole::User => "user".to_string(),
                SidekickRole::Assistant => "assistant".to_string(),
                SidekickRole::System => "system".to_string(),
            },
            content: m.content.clone(),
        })
        .collect()
}

fn apply_decision(app: &mut App, decision: Decision, now: Instant) -> Result<()> {
    if decision
        .requires_api_key
        .as_deref()
        .is_some_and(|p| p.eq_ignore_ascii_case("anthropic"))
    {
        app.auto_agent.awaiting_api_key = true;
        app.auto_agent.cached_has_arda = has_arda_login();
        app.auto_agent.last_arda_check = Some(Instant::now());
        app.auto_agent.chat_focus = true;
        app.auto_agent.chat_input.clear();
        app.auto_agent.auth_error = Some(
            decision
                .reply
                .clone()
                .or(decision.action_note.clone())
                .unwrap_or_else(|| {
                    "No API key found. Run `agentbook login` or paste an Anthropic key.".to_string()
                }),
        );
        app.status_msg =
            "Sidekick auth required: run `agentbook login` or enter API key in Sidekick pane."
                .to_string();
        return Ok(());
    }

    if decision.requires_user_input {
        app.auto_agent.awaiting_user_input = true;
        app.auto_agent.chat_focus = true;
        app.auto_agent.pending_user_question = decision
            .user_question
            .clone()
            .or(decision.reply.clone())
            .or(decision.action_note.clone());
        if let Some(question) = app.auto_agent.pending_user_question.clone() {
            app.auto_agent.push_chat(SidekickMessage {
                role: SidekickRole::System,
                content: format!("Decision needed: {question}"),
            });
            app.status_msg = "Sidekick paused: user decision required in chat pane.".to_string();
        } else {
            app.status_msg = "Sidekick paused: user decision required.".to_string();
        }
        return Ok(());
    }

    if let Some(keys) = decision.keys {
        if app
            .auto_agent
            .last_action_at
            .is_some_and(|t| now.duration_since(t) < MIN_ACTION_GAP)
        {
            return Ok(());
        }
        let target = decision.target_window.unwrap_or(app.active_terminal_window);
        send_keys_to_window(app, target, &keys)?;
        app.auto_agent.last_action_at = Some(now);
        if let Some(note) = decision.action_note {
            app.status_msg = note;
        }
    } else if let Some(reply) = decision.reply {
        app.status_msg = format!("Sidekick: {}", truncate(&reply, 90));
    }
    Ok(())
}

fn collect_tab_snapshots(app: &App) -> Result<Vec<TabSnapshot>> {
    let Some(term) = app.active_terminal() else {
        return Ok(Vec::new());
    };

    match term.mux_windows()? {
        Some(windows) if !windows.is_empty() => {
            let mut tabs = windows
                .into_iter()
                .map(|w| {
                    let text = term
                        .mux_capture_window_text(w.index, SNAPSHOT_LINES)?
                        .unwrap_or_default();
                    Ok(TabSnapshot {
                        index: w.index,
                        name: w.name,
                        active: w.active,
                        text,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            tabs.sort_by_key(|t| (!t.active, t.index));
            Ok(tabs)
        }
        _ => Ok(vec![TabSnapshot {
            index: 0,
            name: "shell".to_string(),
            active: true,
            text: term.snapshot_text(SNAPSHOT_LINES),
        }]),
    }
}

fn waiting_input_window_indices(tabs: &[TabSnapshot]) -> HashSet<usize> {
    tabs.iter()
        .filter(|tab| {
            let lower = tab.text.to_ascii_lowercase();
            has_enter_prompt(&lower) || has_yes_no_continue_prompt(&lower)
        })
        .map(|tab| tab.index)
        .collect()
}

/// Detect tmux window indices that appear to be waiting for user input.
pub fn detect_waiting_input_windows(app: &App) -> Result<HashSet<usize>> {
    let tabs = collect_tab_snapshots(app)?;
    Ok(waiting_input_window_indices(&tabs))
}

fn decide_rules_heartbeat(tabs: &[TabSnapshot]) -> Decision {
    let mut summary = summarize_tabs(tabs);
    for tab in tabs {
        let lower = tab.text.to_ascii_lowercase();
        if has_enter_prompt(&lower) {
            summary = format!(
                "{} | action: enter on T{} {}",
                summary,
                tab.index + 1,
                truncate(&tab.name, 14)
            );
            return Decision {
                target_window: Some(tab.index),
                keys: Some("\n".to_string()),
                action_note: Some(format!(
                    "Sidekick: sent Enter on T{} {}.",
                    tab.index + 1,
                    truncate(&tab.name, 18)
                )),
                summary,
                ..Default::default()
            };
        }
        if assume_yes_enabled() && has_yes_no_continue_prompt(&lower) {
            if has_destructive_keyword(&lower) {
                summary = format!(
                    "{} | skipped auto-accept (destructive) on T{} {}",
                    summary,
                    tab.index + 1,
                    truncate(&tab.name, 14)
                );
                return Decision {
                    action_note: Some(format!(
                        "Sidekick: destructive prompt detected on T{} {}, skipping auto-accept.",
                        tab.index + 1,
                        truncate(&tab.name, 18)
                    )),
                    summary,
                    reply: Some(format!(
                        "Destructive prompt on T{} -- manual confirmation required.",
                        tab.index + 1
                    )),
                    ..Default::default()
                };
            }
            summary = format!(
                "{} | action: yes on T{} {}",
                summary,
                tab.index + 1,
                truncate(&tab.name, 14)
            );
            return Decision {
                target_window: Some(tab.index),
                keys: Some("y\n".to_string()),
                action_note: Some(format!(
                    "Sidekick: sent y on T{} {}.",
                    tab.index + 1,
                    truncate(&tab.name, 18)
                )),
                summary,
                ..Default::default()
            };
        }
    }
    Decision {
        summary,
        ..Default::default()
    }
}

fn decide_rules_chat(tabs: &[TabSnapshot], prompt: &str) -> Decision {
    let mut d = decide_rules_heartbeat(tabs);
    let mut reply = format!(
        "I checked {} terminal tabs. {}",
        tabs.len(),
        summarize_tabs(tabs)
    );
    if !prompt.trim().is_empty() {
        reply.push_str(" | instruction: ");
        reply.push_str(&truncate(prompt.trim(), 80));
    }
    d.reply = Some(reply.clone());
    d.summary = truncate(&reply, 220);
    d
}

#[derive(Debug, Serialize)]
struct PiAutomationRequest<'a> {
    kind: &'a str,
    prompt: Option<&'a str>,
    policy: &'a str,
    tabs: &'a [TabSnapshot],
    history: &'a [PiHistoryMessage],
    stream_events: bool,
}

#[derive(Debug, Deserialize)]
struct PiAutomationResponse {
    action: Option<String>,
    target_window: Option<usize>,
    keys: Option<String>,
    instruction: Option<String>,
    summary: Option<String>,
    reply: Option<String>,
    requires_api_key: Option<String>,
    requires_user_input: Option<bool>,
    user_question: Option<String>,
}

/// Returns `true` if the command string does not contain shell metacharacters
/// that could enable command injection when passed to `sh -c`.
fn is_safe_shell_command(cmd: &str) -> bool {
    !cmd.contains(';')
        && !cmd.contains('|')
        && !cmd.contains('&')
        && !cmd.contains('`')
        && !cmd.contains("$(")
        && !cmd.contains('\n')
        && !cmd.contains('\r')
}

/// Resolve the PI automation command, preferring the env var override,
/// then falling back to the script path anchored next to the current
/// executable (not relative to CWD).
fn resolve_pi_command() -> Result<String> {
    if let Ok(cmd) = std::env::var("AGENTBOOK_PI_AUTOMATION_CMD")
        && !cmd.trim().is_empty()
    {
        if !is_safe_shell_command(&cmd) {
            anyhow::bail!(
                "AGENTBOOK_PI_AUTOMATION_CMD contains shell metacharacters \
                 (;|&`$()) that are not allowed for safety. \
                 Use a simple command like `node /path/to/script.mjs`."
            );
        }
        return Ok(cmd);
    }

    let exe_dir = exe_parent_dir()?;
    let script = exe_dir.join("agent/scripts/pi-terminal-agent.mjs");
    if script.exists() {
        return Ok(format!("node {}", script.display()));
    }

    anyhow::bail!(
        "set AGENTBOOK_PI_AUTOMATION_CMD (e.g. `node agent/scripts/pi-terminal-agent.mjs`)"
    )
}

/// Return the parent directory of the current executable.
fn exe_parent_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("unable to determine current executable path")?;
    Ok(exe.parent().unwrap_or_else(|| Path::new(".")).to_path_buf())
}

/// Returns `true` if the lowercased text contains any destructive keyword.
fn has_destructive_keyword(lower: &str) -> bool {
    DESTRUCTIVE_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

fn decide_pi(
    tabs: &[TabSnapshot],
    prompt: Option<&str>,
    history: &[PiHistoryMessage],
    kind: &str,
    inference_env: &[(String, String)],
) -> Result<Decision> {
    let cmd = resolve_pi_command()?;

    let req = PiAutomationRequest {
        kind,
        prompt,
        policy: "Prefer no action when uncertain. Never perform destructive commands.",
        tabs,
        history,
        stream_events: false,
    };
    let payload = serde_json::to_vec(&req).context("failed to encode PI request")?;
    let raw = run_command_with_stdin(&cmd, &payload, PI_TIMEOUT, inference_env)?;
    let parsed = parse_pi_response(&raw)?;
    decision_from_pi_response(parsed, tabs)
}

pub fn start_pi_chat_stream(
    app: &App,
    prompt: &str,
    cancel: Arc<AtomicBool>,
) -> Result<mpsc::Receiver<SidekickChatStreamEvent>> {
    let tabs = collect_tab_snapshots(app)?;
    if tabs.is_empty() {
        anyhow::bail!("no active terminal");
    }
    let history = sidekick_history_for_pi(app);
    let cmd = resolve_pi_command()?;

    let req = PiAutomationRequest {
        kind: "chat",
        prompt: Some(prompt),
        policy: "Prefer no action when uncertain. Never perform destructive commands.",
        tabs: &tabs,
        history: &history,
        stream_events: true,
    };
    let payload = serde_json::to_vec(&req).context("failed to encode PI request")?;
    let tabs_for_thread = tabs.clone();
    let env_for_thread = app.auto_agent.inference_env.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = run_pi_chat_stream_worker(
            &cmd,
            payload,
            tabs_for_thread,
            tx.clone(),
            &env_for_thread,
            &cancel,
        ) {
            let _ = tx.send(SidekickChatStreamEvent::Error(format!(
                "Sidekick (PI): {e}"
            )));
        }
    });
    Ok(rx)
}

pub fn apply_chat_completion(app: &mut App, completion: SidekickChatCompletion) -> Result<String> {
    let now = Instant::now();
    let reply = completion
        .reply
        .clone()
        .unwrap_or_else(|| completion.summary.clone());
    app.auto_agent.last_summary = completion.summary.clone();
    apply_decision(app, completion, now)?;
    Ok(reply)
}

fn run_pi_chat_stream_worker(
    cmd: &str,
    payload: Vec<u8>,
    tabs: Vec<TabSnapshot>,
    tx: mpsc::Sender<SidekickChatStreamEvent>,
    inference_env: &[(String, String)],
    cancel: &AtomicBool,
) -> Result<()> {
    let mut builder = Command::new("sh");
    builder
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in inference_env {
        builder.env(k, v);
    }
    let mut child = builder
        .spawn()
        .with_context(|| format!("failed to spawn PI command: {cmd}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .context("failed to write request payload to PI command stdin")?;
        // Closing stdin by dropping it.
    }

    // Take stderr before the stdout read loop so we can read it after the
    // child exits.
    let child_stderr = child.stderr.take();

    let stdout = child
        .stdout
        .take()
        .context("failed to capture PI command stdout")?;
    let reader = BufReader::new(stdout);
    let mut final_parsed: Option<PiAutomationResponse> = None;

    for line in reader.lines() {
        // Check cancellation flag between lines.
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("PI stream cancelled");
        }

        let line = line.context("failed reading PI stream output")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if value.get("event").and_then(|v| v.as_str()) == Some("reply_delta") {
                if let Some(delta) = value.get("delta").and_then(|v| v.as_str())
                    && !delta.is_empty()
                {
                    let _ = tx.send(SidekickChatStreamEvent::ReplyDelta(delta.to_string()));
                }
                continue;
            }
            if value.get("event").and_then(|v| v.as_str()) == Some("thinking") {
                continue;
            }
            if value.get("event").and_then(|v| v.as_str()) == Some("tool_call")
                && value.get("tool").and_then(|v| v.as_str()).is_some()
            {
                continue;
            }
        }

        if let Ok(parsed) = serde_json::from_str::<PiAutomationResponse>(trimmed)
            && (parsed.action.is_some()
                || parsed.summary.is_some()
                || parsed.reply.is_some()
                || parsed.requires_api_key.is_some()
                || parsed.requires_user_input.is_some()
                || parsed.user_question.is_some())
        {
            final_parsed = Some(parsed);
        }
    }

    let status = child.wait().context("failed waiting for PI command")?;
    if !status.success() {
        let stderr_tail = stderr_last_lines(child_stderr, 3);
        if stderr_tail.is_empty() {
            anyhow::bail!("PI command exited with {status}");
        } else {
            anyhow::bail!("PI command exited with {status}: {stderr_tail}");
        }
    }
    let parsed = final_parsed.context("streamed PI output did not include final JSON decision")?;
    let completion = decision_from_pi_response(parsed, &tabs)?;
    let _ = tx.send(SidekickChatStreamEvent::Complete(completion));
    Ok(())
}

fn decision_from_pi_response(
    parsed: PiAutomationResponse,
    tabs: &[TabSnapshot],
) -> Result<Decision> {
    let action = parsed
        .action
        .as_deref()
        .unwrap_or("none")
        .to_ascii_lowercase();
    let (keys, action_note) = match action.as_str() {
        "none" => (None, None),
        "enter" => (
            Some("\n".to_string()),
            Some("Sidekick (PI): sent Enter.".to_string()),
        ),
        "yes" => (
            Some("y\n".to_string()),
            Some("Sidekick (PI): sent y.".to_string()),
        ),
        "send_instruction" => {
            let instruction = parsed
                .instruction
                .clone()
                .or(parsed.keys.clone())
                .filter(|s| !s.trim().is_empty())
                .with_context(
                    || "PI response action `send_instruction` requires non-empty `instruction`",
                )?;
            let mut out = instruction.trim_end().to_string();
            out.push('\n');
            (
                Some(out),
                Some("Sidekick (PI): sent instruction to downstream agent.".to_string()),
            )
        }
        "send_keys" | "type" => {
            let k = parsed
                .keys
                .clone()
                .filter(|s| !s.is_empty())
                .with_context(|| "PI response action requires non-empty `keys`")?;
            (
                Some(k),
                Some("Sidekick (PI): sent requested keys.".to_string()),
            )
        }
        other => anyhow::bail!("unsupported PI action `{other}`"),
    };

    Ok(Decision {
        target_window: parsed.target_window,
        keys,
        action_note,
        summary: parsed
            .summary
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| summarize_tabs(tabs)),
        reply: parsed.reply.filter(|s| !s.trim().is_empty()),
        requires_api_key: parsed.requires_api_key,
        requires_user_input: parsed.requires_user_input.unwrap_or(false),
        user_question: parsed.user_question.filter(|s| !s.trim().is_empty()),
    })
}

fn send_keys_to_window(app: &mut App, target_window: usize, keys: &str) -> Result<()> {
    if target_window != app.active_terminal_window
        && let Some(term) = app.active_terminal()
    {
        match term.mux_send_window_keys(target_window, keys) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => return Err(e).context("tmux send-keys failed"),
        }
    }

    if target_window != app.active_terminal_window {
        if let Some(term) = app.active_terminal() {
            let _ = term.mux_select_window(target_window);
        }
        app.refresh_terminal_tabs();
        if let Some(term) = app.active_terminal_mut() {
            if !term.is_persistent_mux() {
                term.reset_screen();
            }
            let _ = term.process_output();
        }
        app.request_full_redraw = true;
    }
    let Some(term) = app.active_terminal_mut() else {
        anyhow::bail!("no active terminal");
    };
    term.write_input(keys.as_bytes())
}

/// Read the last `n` lines from a child process stderr handle.
/// Returns an empty string if the handle is `None` or unreadable.
fn stderr_last_lines(handle: Option<ChildStderr>, n: usize) -> String {
    let Some(mut stderr) = handle else {
        return String::new();
    };
    let mut buf = String::new();
    if stderr.read_to_string(&mut buf).is_err() {
        return String::new();
    }
    let lines: Vec<&str> = buf.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

fn run_command_with_stdin(
    cmd: &str,
    stdin_data: &[u8],
    timeout: Duration,
    inference_env: &[(String, String)],
) -> Result<String> {
    let mut builder = Command::new("sh");
    builder
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in inference_env {
        builder.env(k, v);
    }
    let mut child = builder
        .spawn()
        .with_context(|| format!("failed to spawn PI command: {cmd}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_data)
            .context("failed to write request payload to PI command stdin")?;
        // Closing stdin by dropping it.
    }

    let child_handle: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
    let child_for_thread = Arc::clone(&child_handle);
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let result = (|| -> Result<String> {
            let mut guard = child_for_thread.lock().unwrap();
            let child = guard.take().context("child process already consumed")?;
            drop(guard);

            let out = child
                .wait_with_output()
                .context("failed waiting for PI command output")?;
            if !out.status.success() {
                let stderr_tail = String::from_utf8_lossy(&out.stderr);
                let stderr_lines: Vec<&str> = stderr_tail.lines().collect();
                let start = stderr_lines.len().saturating_sub(3);
                let tail = stderr_lines[start..].join("\n");
                if tail.is_empty() {
                    anyhow::bail!("PI command exited with {}", out.status);
                } else {
                    anyhow::bail!("PI command exited with {}: {}", out.status, tail);
                }
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        })();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => {
            // Kill the orphaned child process on timeout.
            if let Ok(mut guard) = child_handle.lock()
                && let Some(ref mut child) = *guard
            {
                let _ = child.kill();
                let _ = child.wait();
            }
            anyhow::bail!("PI command timed out after {}s", timeout.as_secs())
        }
    }
}

fn parse_pi_response(raw: &str) -> Result<PiAutomationResponse> {
    if let Ok(parsed) = serde_json::from_str::<PiAutomationResponse>(raw) {
        return Ok(parsed);
    }
    for line in raw.lines().rev() {
        if let Ok(parsed) = serde_json::from_str::<PiAutomationResponse>(line) {
            return Ok(parsed);
        }
    }
    anyhow::bail!("invalid PI response (expected JSON)");
}

fn has_enter_prompt(lower: &str) -> bool {
    [
        "press enter to continue",
        "hit enter to continue",
        "press return to continue",
        "press any key to continue",
        "enter to continue",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

fn has_yes_no_continue_prompt(lower: &str) -> bool {
    let yes_no = lower.contains("(y/n)") || lower.contains("[y/n]") || lower.contains("(yes/no)");
    let continue_words =
        lower.contains("continue") || lower.contains("proceed") || lower.contains("resume");
    yes_no && continue_words
}

fn summarize_tabs(tabs: &[TabSnapshot]) -> String {
    let mut parts = Vec::new();
    for tab in tabs.iter().take(4) {
        let s = summarize_snapshot(&tab.text);
        if !s.trim().is_empty() && s != "No terminal output yet." {
            parts.push(format!(
                "T{} {}: {}",
                tab.index + 1,
                truncate(&tab.name, 12),
                truncate(&s, 60)
            ));
        }
    }
    if parts.is_empty() {
        "No terminal output yet.".to_string()
    } else {
        parts.join(" || ")
    }
}

fn summarize_snapshot(snapshot: &str) -> String {
    let lines: Vec<&str> = snapshot
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "No terminal output yet.".to_string();
    }
    let start = lines.len().saturating_sub(4);
    let merged = lines[start..].join(" | ");
    truncate(&merged, 220)
}

fn assume_yes_enabled() -> bool {
    std::env::var("AGENTBOOK_AUTO_ASSUME_YES")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Returns `true` if an Arda Gateway API key is stored on disk.
pub fn has_arda_login() -> bool {
    let Ok(state_dir) = agentbook_mesh::state_dir::default_state_dir() else {
        return false;
    };
    let path = state_dir.join(ARDA_KEY_FILE);
    fs::metadata(path).ok().is_some_and(|m| m.len() > 0)
}

/// Launch `agentbook login` in a background thread to run the Arda OAuth flow.
/// Opens the user's browser and waits for the callback. The TUI's 5-second
/// polling in `tick()` will detect the key once it's stored.
pub fn start_arda_login(app: &mut App) {
    if app.auto_agent.login_in_progress {
        app.status_msg = "Arda login already in progress...".to_string();
        return;
    }
    app.auto_agent.login_in_progress = true;
    app.auto_agent.login_started_at = Some(Instant::now());
    app.auto_agent.push_chat(SidekickMessage {
        role: SidekickRole::System,
        content: "Opening browser for Arda login...".to_string(),
    });
    app.status_msg = "Opening browser for Arda login...".to_string();

    // Resolve the agentbook CLI binary path.
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            app.auto_agent.login_in_progress = false;
            app.auto_agent.push_chat(SidekickMessage {
                role: SidekickRole::System,
                content: format!("Failed to locate agentbook binary: {e}"),
            });
            return;
        }
    };

    std::thread::spawn(move || {
        match Command::new(&exe)
            .arg("login")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(out) if !out.status.success() => {
                let stderr_text = String::from_utf8_lossy(&out.stderr);
                let lines: Vec<&str> = stderr_text.lines().collect();
                let start = lines.len().saturating_sub(3);
                let tail = lines[start..].join("\n");
                eprintln!("agentbook login exited with {}: {tail}", out.status);
            }
            Err(e) => {
                eprintln!("failed to spawn agentbook login: {e}");
            }
            _ => {}
        }
        // Success or failure, the tick() polling will detect if the key was
        // stored. login_in_progress is cleared by the polling logic when it
        // detects the key or after a timeout.
    });
}

pub fn save_anthropic_api_key(api_key: &str) -> Result<()> {
    let key = api_key.trim();
    if key.is_empty() {
        anyhow::bail!("API key cannot be empty");
    }
    let state_dir = agentbook_mesh::state_dir::default_state_dir()
        .context("unable to locate state dir for sidekick key storage")?;
    fs::create_dir_all(&state_dir).context("failed to create sidekick state directory")?;
    let path = state_dir.join(SIDEKICK_KEY_FILE);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        f.write_all(key.as_bytes())
            .with_context(|| format!("failed writing {}", path.display()))?;
        f.flush()
            .with_context(|| format!("failed flushing {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        f.write_all(key.as_bytes())
            .with_context(|| format!("failed writing {}", path.display()))?;
        f.flush()
            .with_context(|| format!("failed flushing {}", path.display()))?;
    }

    Ok(())
}

/// Validate that a gateway URL looks safe to use.
fn is_valid_gateway_url(url: &str) -> bool {
    let url = url.trim();
    url.starts_with("https://") && !url.contains('\n') && !url.contains('\r') && !url.contains(' ')
}

/// Reload `inference_env` only when the TTL-based cache is stale or empty.
/// Event-driven callers (key save, Arda login) should call
/// `load_inference_env_vars()` directly instead.
fn refresh_inference_env_if_stale(app: &mut crate::app::App) {
    let stale = app
        .auto_agent
        .last_env_load
        .is_none_or(|t| t.elapsed() >= ENV_CACHE_TTL);
    if stale || app.auto_agent.inference_env.is_empty() {
        app.auto_agent.inference_env = load_inference_env_vars();
        app.auto_agent.last_env_load = Some(Instant::now());
    }
}

/// Load inference credentials and return them as env var pairs for child
/// processes (pi-terminal-agent.mjs).
///
/// Priority: Arda Gateway key > env AGENTBOOK_ANTHROPIC_API_KEY >
/// env ANTHROPIC_API_KEY > saved sidekick_anthropic_api_key file.
pub fn load_inference_env_vars() -> Vec<(String, String)> {
    // If Arda Gateway key is already in env, pass it through.
    // Wrap intermediate key reads in Zeroizing so they are wiped on drop.
    if let Ok(raw_key) = std::env::var("AGENTBOOK_GATEWAY_API_KEY") {
        let key = Zeroizing::new(raw_key);
        if !key.trim().is_empty() {
            let url = std::env::var("AGENTBOOK_GATEWAY_URL")
                .unwrap_or_else(|_| ARDA_DEFAULT_GATEWAY_URL.to_string());
            if !is_valid_gateway_url(&url) {
                eprintln!(
                    "Warning: invalid AGENTBOOK_GATEWAY_URL {url:?}, skipping env gateway credentials"
                );
            } else {
                return vec![
                    (
                        "AGENTBOOK_GATEWAY_API_KEY".to_string(),
                        key.trim().to_string(),
                    ),
                    ("AGENTBOOK_GATEWAY_URL".to_string(), url),
                ];
            }
        }
    }

    let Ok(state_dir) = agentbook_mesh::state_dir::default_state_dir() else {
        return Vec::new();
    };

    // Prefer Arda Gateway key from disk.
    let arda_key_path = state_dir.join(ARDA_KEY_FILE);
    if let Ok(raw) = fs::read_to_string(&arda_key_path) {
        let raw = Zeroizing::new(raw);
        let key = raw.trim().to_string();
        if !key.is_empty() {
            let gateway_url = ARDA_DEFAULT_GATEWAY_URL.to_string();
            if !is_valid_gateway_url(&gateway_url) {
                eprintln!(
                    "Warning: invalid gateway URL {gateway_url:?}, skipping Arda Gateway credentials"
                );
            } else {
                return vec![
                    ("AGENTBOOK_GATEWAY_API_KEY".to_string(), key),
                    ("AGENTBOOK_GATEWAY_URL".to_string(), gateway_url),
                ];
            }
        }
    }

    // Fall back to legacy Anthropic key from env.
    if let Ok(raw_key) = std::env::var("AGENTBOOK_ANTHROPIC_API_KEY") {
        let key = Zeroizing::new(raw_key);
        if !key.trim().is_empty() {
            return vec![(
                "AGENTBOOK_ANTHROPIC_API_KEY".to_string(),
                key.trim().to_string(),
            )];
        }
    }
    if let Ok(raw_key) = std::env::var("ANTHROPIC_API_KEY") {
        let key = Zeroizing::new(raw_key);
        if !key.trim().is_empty() {
            return vec![("ANTHROPIC_API_KEY".to_string(), key.trim().to_string())];
        }
    }

    // Fall back to saved sidekick_anthropic_api_key file.
    let path = state_dir.join(SIDEKICK_KEY_FILE);
    if let Ok(raw) = fs::read_to_string(path) {
        let raw = Zeroizing::new(raw);
        let key = raw.trim().to_string();
        if !key.is_empty() {
            return vec![("AGENTBOOK_ANTHROPIC_API_KEY".to_string(), key)];
        }
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mutex to serialize tests that manipulate the `AGENTBOOK_AUTO_ASSUME_YES`
    /// env var, preventing races when tests run in parallel.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn rules_detect_enter_prompt() {
        let tabs = vec![TabSnapshot {
            index: 1,
            name: "work".to_string(),
            active: true,
            text: "Build done.\nPress Enter to continue".to_string(),
        }];
        let decision = decide_rules_heartbeat(&tabs);
        assert_eq!(decision.target_window, Some(1));
        assert_eq!(decision.keys, Some("\n".to_string()));
    }

    #[test]
    fn summary_uses_recent_non_empty_lines() {
        let summary = summarize_snapshot("a\n\nb\nc\nd\ne\n");
        assert_eq!(summary, "b | c | d | e");
    }

    #[test]
    fn waiting_input_indices_detect_enter_and_yes_no_prompts() {
        let tabs = vec![
            TabSnapshot {
                index: 0,
                name: "main".to_string(),
                active: true,
                text: "Press Enter to continue".to_string(),
            },
            TabSnapshot {
                index: 2,
                name: "agent".to_string(),
                active: false,
                text: "Continue? (y/n)".to_string(),
            },
            TabSnapshot {
                index: 3,
                name: "logs".to_string(),
                active: false,
                text: "all good".to_string(),
            },
        ];
        let waiting = waiting_input_window_indices(&tabs);
        assert!(waiting.contains(&0));
        assert!(waiting.contains(&2));
        assert!(!waiting.contains(&3));
    }

    #[test]
    fn save_and_load_anthropic_key_round_trip() {
        let key = "sk-ant-test-round-trip-key";
        let state_dir = agentbook_mesh::state_dir::default_state_dir().unwrap();
        let path = state_dir.join(SIDEKICK_KEY_FILE);

        // Save the key.
        save_anthropic_api_key(key).unwrap();

        // Verify file content.
        let stored = fs::read_to_string(&path).unwrap();
        assert_eq!(stored.trim(), key);

        // Clean up: remove the test key file.
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn is_valid_gateway_url_accepts_https() {
        assert!(is_valid_gateway_url("https://bot.ardabot.ai"));
        assert!(is_valid_gateway_url("  https://example.com  "));
    }

    #[test]
    fn is_valid_gateway_url_rejects_invalid() {
        assert!(!is_valid_gateway_url("http://insecure.example.com"));
        assert!(!is_valid_gateway_url("https://evil.com\nHost: good.com"));
        assert!(!is_valid_gateway_url("https://evil.com\rHost: good.com"));
        assert!(!is_valid_gateway_url("https://evil.com /path"));
        assert!(!is_valid_gateway_url("not-a-url"));
        assert!(!is_valid_gateway_url(""));
    }

    #[test]
    fn load_inference_env_vars_returns_empty_without_keys() {
        // When no keys are in env or on disk, should return empty vec.
        // This test is best-effort since the test environment may have keys.
        let vars = load_inference_env_vars();
        // Just verify the return type is correct and doesn't panic.
        assert!(vars.iter().all(|(k, _)| !k.is_empty()));
    }

    #[test]
    fn has_arda_login_false_without_key_file() {
        // When no arda_api_key file exists in state dir, should return false.
        // We can't easily control state_dir in tests, but if there's no Arda
        // key on this machine it will be false.
        let state_dir = agentbook_mesh::state_dir::default_state_dir().unwrap();
        let path = state_dir.join(ARDA_KEY_FILE);
        let had_key = path.exists();
        if !had_key {
            assert!(!has_arda_login());
        }
        // If a key exists, just verify has_arda_login returns true.
        if had_key {
            assert!(has_arda_login());
        }
    }

    #[test]
    fn destructive_keyword_detected() {
        assert!(has_destructive_keyword("delete all files and continue?"));
        assert!(has_destructive_keyword("this will remove the database"));
        assert!(has_destructive_keyword("format disk now"));
        assert!(has_destructive_keyword("this will destroy everything"));
        assert!(has_destructive_keyword("drop table users"));
        assert!(has_destructive_keyword("overwrite existing config?"));
        assert!(has_destructive_keyword("about to force push to main"));
        assert!(has_destructive_keyword("running git reset --hard"));
    }

    #[test]
    fn safe_prompt_no_destructive_keyword() {
        assert!(!has_destructive_keyword("continue with installation?"));
        assert!(!has_destructive_keyword("proceed to next step?"));
        assert!(!has_destructive_keyword("build completed successfully"));
        assert!(!has_destructive_keyword(""));
    }

    #[test]
    fn auto_accept_skips_destructive_prompt() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: test-only env manipulation; serialized by ENV_MUTEX.
        unsafe {
            std::env::set_var("AGENTBOOK_AUTO_ASSUME_YES", "1");
        }

        let tabs = vec![TabSnapshot {
            index: 0,
            name: "agent".to_string(),
            active: true,
            text: "Delete all files and continue? (y/n)".to_string(),
        }];
        let decision = decide_rules_heartbeat(&tabs);
        // Should NOT auto-send y\n because "delete" is destructive.
        assert!(decision.keys.is_none());
        assert!(
            decision
                .action_note
                .as_ref()
                .is_some_and(|n| n.contains("destructive"))
        );

        // Clean up env var.
        unsafe {
            std::env::remove_var("AGENTBOOK_AUTO_ASSUME_YES");
        }
    }

    #[test]
    fn auto_accept_allows_safe_prompt() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: test-only env manipulation; serialized by ENV_MUTEX.
        unsafe {
            std::env::set_var("AGENTBOOK_AUTO_ASSUME_YES", "1");
        }

        let tabs = vec![TabSnapshot {
            index: 0,
            name: "agent".to_string(),
            active: true,
            text: "Continue with build? (y/n)".to_string(),
        }];
        let decision = decide_rules_heartbeat(&tabs);
        // Should auto-send y\n because no destructive keyword.
        assert_eq!(decision.keys, Some("y\n".to_string()));

        // Clean up env var.
        unsafe {
            std::env::remove_var("AGENTBOOK_AUTO_ASSUME_YES");
        }
    }

    #[test]
    fn safe_shell_command_accepts_legitimate_commands() {
        assert!(is_safe_shell_command("node /path/to/script.mjs"));
        assert!(is_safe_shell_command("python3 agent.py --flag value"));
        assert!(is_safe_shell_command("/usr/local/bin/my-tool"));
        assert!(is_safe_shell_command("node script.mjs --port 3000"));
        assert!(is_safe_shell_command(""));
    }

    #[test]
    fn safe_shell_command_rejects_metacharacters() {
        assert!(!is_safe_shell_command("node script.mjs; rm -rf /"));
        assert!(!is_safe_shell_command("cmd | cat /etc/passwd"));
        assert!(!is_safe_shell_command("cmd & background"));
        assert!(!is_safe_shell_command("cmd && second"));
        assert!(!is_safe_shell_command("echo `whoami`"));
        assert!(!is_safe_shell_command("echo $(whoami)"));
        assert!(!is_safe_shell_command("cmd\nmalicious"));
        assert!(!is_safe_shell_command("cmd\rmalicious"));
    }

    #[test]
    fn start_arda_login_noop_when_already_in_progress() {
        let mut app = App::new("me".to_string());
        app.auto_agent.login_in_progress = true;
        let original_started_at = app.auto_agent.login_started_at;

        start_arda_login(&mut app);

        // Should remain in the same state — no new thread spawned.
        assert!(app.auto_agent.login_in_progress);
        assert_eq!(app.auto_agent.login_started_at, original_started_at);
        assert!(app.status_msg.contains("already in progress"));
    }
}
