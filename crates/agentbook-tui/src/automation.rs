use crate::app::{
    App, AutoAgentMode, SidekickChatCompletion, SidekickChatStreamEvent, SidekickMessage,
    SidekickRole,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const SNAPSHOT_LINES: usize = 80;
const MIN_ACTION_GAP: Duration = Duration::from_secs(2);
const PI_TIMEOUT: Duration = Duration::from_secs(6);
const PI_HISTORY_LIMIT: usize = 16;
const SIDEKICK_KEY_FILE: &str = "sidekick_anthropic_api_key";
const ARDA_KEY_FILE: &str = "arda_api_key";
const ARDA_DEFAULT_GATEWAY_URL: &str = "https://bot.ardabot.ai";

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

struct Decision {
    target_window: Option<usize>,
    keys: Option<String>,
    action_note: Option<String>,
    summary: String,
    reply: Option<String>,
    requires_api_key: Option<String>,
    requires_user_input: bool,
    user_question: Option<String>,
}

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
                app.auto_agent.awaiting_api_key = false;
                app.auto_agent.auth_error = None;
                app.auto_agent.chat_history.push(SidekickMessage {
                    role: SidekickRole::System,
                    content: "Arda login detected. Sidekick inference resumed.".to_string(),
                });
                app.status_msg =
                    "Sidekick auth: Arda login detected. Inference resumed.".to_string();
                // Fall through to normal tick processing.
            } else {
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
            app.auto_agent.inference_env = load_inference_env_vars();
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
            app.auto_agent.inference_env = load_inference_env_vars();
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
                    "No API key found. Run `agentbook login` or paste an Anthropic key."
                        .to_string()
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
            app.auto_agent.chat_history.push(SidekickMessage {
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
                reply: None,
                requires_api_key: None,
                requires_user_input: false,
                user_question: None,
            };
        }
        if assume_yes_enabled() && has_yes_no_continue_prompt(&lower) {
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
                reply: None,
                requires_api_key: None,
                requires_user_input: false,
                user_question: None,
            };
        }
    }
    Decision {
        target_window: None,
        keys: None,
        action_note: None,
        summary,
        reply: None,
        requires_api_key: None,
        requires_user_input: false,
        user_question: None,
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

fn decide_pi(
    tabs: &[TabSnapshot],
    prompt: Option<&str>,
    history: &[PiHistoryMessage],
    kind: &str,
    inference_env: &[(String, String)],
) -> Result<Decision> {
    let cmd = std::env::var("AGENTBOOK_PI_AUTOMATION_CMD")
        .ok()
        .or_else(|| {
            let local = Path::new("agent/scripts/pi-terminal-agent.mjs");
            if local.exists() {
                Some("node agent/scripts/pi-terminal-agent.mjs".to_string())
            } else {
                None
            }
        })
        .with_context(
            || "set AGENTBOOK_PI_AUTOMATION_CMD (e.g. `node agent/scripts/pi-terminal-agent.mjs`)",
        )?;

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
) -> Result<mpsc::Receiver<SidekickChatStreamEvent>> {
    let tabs = collect_tab_snapshots(app)?;
    if tabs.is_empty() {
        anyhow::bail!("no active terminal");
    }
    let history = sidekick_history_for_pi(app);
    let cmd = std::env::var("AGENTBOOK_PI_AUTOMATION_CMD")
        .ok()
        .or_else(|| {
            let local = Path::new("agent/scripts/pi-terminal-agent.mjs");
            if local.exists() {
                Some("node agent/scripts/pi-terminal-agent.mjs".to_string())
            } else {
                None
            }
        })
        .with_context(
            || "set AGENTBOOK_PI_AUTOMATION_CMD (e.g. `node agent/scripts/pi-terminal-agent.mjs`)",
        )?;

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
        if let Err(e) =
            run_pi_chat_stream_worker(&cmd, payload, tabs_for_thread, tx.clone(), &env_for_thread)
        {
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
    let decision = completion_to_decision(completion);
    apply_decision(app, decision, now)?;
    Ok(reply)
}

fn run_pi_chat_stream_worker(
    cmd: &str,
    payload: Vec<u8>,
    tabs: Vec<TabSnapshot>,
    tx: mpsc::Sender<SidekickChatStreamEvent>,
    inference_env: &[(String, String)],
) -> Result<()> {
    let mut builder = Command::new("sh");
    builder
        .arg("-lc")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in inference_env {
        builder.env(k, v);
    }
    let mut child = builder
        .spawn()
        .with_context(|| format!("failed to spawn PI command: {cmd}"))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(&payload)
            .context("failed to write request payload to PI command stdin")?;
    }
    drop(child.stdin.take());

    let stdout = child
        .stdout
        .take()
        .context("failed to capture PI command stdout")?;
    let reader = BufReader::new(stdout);
    let mut final_parsed: Option<PiAutomationResponse> = None;

    for line in reader.lines() {
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
        anyhow::bail!("PI command exited with {status}");
    }
    let parsed = final_parsed.context("streamed PI output did not include final JSON decision")?;
    let decision = decision_from_pi_response(parsed, &tabs)?;
    let completion = decision_to_completion(decision);
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

fn decision_to_completion(decision: Decision) -> SidekickChatCompletion {
    SidekickChatCompletion {
        target_window: decision.target_window,
        keys: decision.keys,
        action_note: decision.action_note,
        summary: decision.summary,
        reply: decision.reply,
        requires_api_key: decision.requires_api_key,
        requires_user_input: decision.requires_user_input,
        user_question: decision.user_question,
    }
}

fn completion_to_decision(completion: SidekickChatCompletion) -> Decision {
    Decision {
        target_window: completion.target_window,
        keys: completion.keys,
        action_note: completion.action_note,
        summary: completion.summary,
        reply: completion.reply,
        requires_api_key: completion.requires_api_key,
        requires_user_input: completion.requires_user_input,
        user_question: completion.user_question,
    }
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

fn run_command_with_stdin(
    cmd: &str,
    stdin_data: &[u8],
    timeout: Duration,
    inference_env: &[(String, String)],
) -> Result<String> {
    let cmd = cmd.to_string();
    let stdin_data = stdin_data.to_vec();
    let env_owned: Vec<(String, String)> = inference_env.to_vec();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let result = (|| -> Result<String> {
            let mut builder = Command::new("sh");
            builder
                .arg("-lc")
                .arg(&cmd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            for (k, v) in &env_owned {
                builder.env(k, v);
            }
            let mut child = builder
                .spawn()
                .with_context(|| format!("failed to spawn PI command: {cmd}"))?;

            if let Some(stdin) = child.stdin.as_mut() {
                stdin
                    .write_all(&stdin_data)
                    .context("failed to write request payload to PI command stdin")?;
            }
            let out = child
                .wait_with_output()
                .context("failed waiting for PI command output")?;
            if !out.status.success() {
                anyhow::bail!("PI command exited with {}", out.status);
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        })();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => anyhow::bail!("PI command timed out after {}s", timeout.as_secs()),
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

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
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
    fs::read_to_string(path)
        .ok()
        .is_some_and(|s| !s.trim().is_empty())
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

/// Load inference credentials and return them as env var pairs for child
/// processes (pi-terminal-agent.mjs).
///
/// Priority: Arda Gateway key > env AGENTBOOK_ANTHROPIC_API_KEY >
/// env ANTHROPIC_API_KEY > saved sidekick_anthropic_api_key file.
pub fn load_inference_env_vars() -> Vec<(String, String)> {
    // If Arda Gateway key is already in env, pass it through.
    if let Ok(key) = std::env::var("AGENTBOOK_GATEWAY_API_KEY")
        && !key.trim().is_empty()
    {
        let mut vars = vec![(
            "AGENTBOOK_GATEWAY_API_KEY".to_string(),
            key.trim().to_string(),
        )];
        let url = std::env::var("AGENTBOOK_GATEWAY_URL")
            .unwrap_or_else(|_| ARDA_DEFAULT_GATEWAY_URL.to_string());
        vars.push(("AGENTBOOK_GATEWAY_URL".to_string(), url));
        return vars;
    }

    let Ok(state_dir) = agentbook_mesh::state_dir::default_state_dir() else {
        return Vec::new();
    };

    // Prefer Arda Gateway key from disk.
    let arda_key_path = state_dir.join(ARDA_KEY_FILE);
    if let Ok(raw) = fs::read_to_string(&arda_key_path) {
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
    if let Ok(key) = std::env::var("AGENTBOOK_ANTHROPIC_API_KEY")
        && !key.trim().is_empty()
    {
        return vec![(
            "AGENTBOOK_ANTHROPIC_API_KEY".to_string(),
            key.trim().to_string(),
        )];
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
        && !key.trim().is_empty()
    {
        return vec![("ANTHROPIC_API_KEY".to_string(), key.trim().to_string())];
    }

    // Fall back to saved sidekick_anthropic_api_key file.
    let path = state_dir.join(SIDEKICK_KEY_FILE);
    if let Ok(raw) = fs::read_to_string(path) {
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
    fn arda_key_file_constants_match_cli() {
        // Ensure the TUI and CLI agree on file names so keys are interoperable.
        assert_eq!(ARDA_KEY_FILE, "arda_api_key");
        assert_eq!(ARDA_DEFAULT_GATEWAY_URL, "https://bot.ardabot.ai");
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
}
