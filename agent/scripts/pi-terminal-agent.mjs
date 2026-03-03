#!/usr/bin/env node

import { getModel, stream } from "@mariozechner/pi-ai";
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";

const DEFAULT_MODEL_SPEC = "anthropic:claude-sonnet-4-6";
const DEFAULT_TMUX_SOCKET = "agentbook";
const DEFAULT_TMUX_SESSION = "main";
const MAX_FILE_READ_REQUESTS = 6;
const MAX_TERMINAL_READ_REQUESTS = 8;
const MAX_MODEL_STEPS = 8;
const MAX_FILE_BYTES = 64 * 1024;
const MAX_GREP_MATCHES = 80;
const MAX_TMUX_CAPTURE_BYTES = 8 * 1024 * 1024;
const MAX_TOOL_RESULT_CHARS = 120_000;

const execFileAsync = promisify(execFile);

async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf8");
}

function modelFromEnv() {
  const spec = process.env.AGENTBOOK_MODEL ?? DEFAULT_MODEL_SPEC;
  const [provider, modelName] = spec.split(":", 2);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return { provider, model: getModel(provider, modelName) };
}

/**
 * Resolve inference credentials. Prefers Arda Gateway, falls back to direct
 * Anthropic key.
 *
 * Returns { apiKey, baseURL? } where baseURL is only set for Arda Gateway.
 */
function resolveInferenceConfig(provider) {
  // Arda Gateway takes priority (set by TUI's maybe_load_inference_env).
  const gatewayKey = process.env.AGENTBOOK_GATEWAY_API_KEY;
  if (gatewayKey) {
    const gatewayUrl =
      process.env.AGENTBOOK_GATEWAY_URL ?? "https://bot.ardabot.ai";
    // Validate gateway URL: must be HTTPS with no whitespace
    if (!gatewayUrl.startsWith("https://") || /[\n\r ]/.test(gatewayUrl)) {
      console.error(`[pi] Invalid gateway URL (must be HTTPS, no whitespace): ${gatewayUrl}`);
      return {};
    }
    return { apiKey: gatewayKey, baseURL: `${gatewayUrl}/v1` };
  }

  // Legacy direct Anthropic key.
  if (provider === "anthropic") {
    const key =
      process.env.AGENTBOOK_ANTHROPIC_API_KEY ?? process.env.ANTHROPIC_API_KEY;
    return key ? { apiKey: key } : {};
  }

  return {};
}

function extractJson(text) {
  if (!text) return null;
  const trimmed = text.trim();
  try {
    return JSON.parse(trimmed);
  } catch {}
  const start = trimmed.indexOf("{");
  const end = trimmed.lastIndexOf("}");
  if (start >= 0 && end > start) {
    try {
      return JSON.parse(trimmed.slice(start, end + 1));
    } catch {}
  }
  return null;
}

function normalizeRequest(req) {
  const tabs = Array.isArray(req.tabs)
    ? req.tabs
    : [
        {
          index: Number(req.window_index ?? 0),
          name: String(req.window_name ?? "shell"),
          active: true,
          text: String(req.snapshot ?? ""),
        },
      ];
  const history = Array.isArray(req.history) ? req.history : [];
  return {
    kind: req.kind === "chat" ? "chat" : "heartbeat",
    prompt: typeof req.prompt === "string" ? req.prompt : "",
    policy: String(
      req.policy ??
        "Prefer no action when uncertain. Never perform destructive commands."
    ),
    tabs,
    history,
    stream_events: req.stream_events === true,
    tmux_socket:
      typeof req.tmux_socket === "string" && req.tmux_socket.trim()
        ? req.tmux_socket.trim()
        : undefined,
    tmux_session:
      typeof req.tmux_session === "string" && req.tmux_session.trim()
        ? req.tmux_session.trim()
        : undefined,
  };
}

function extractReplyPrefix(jsonText) {
  const marker = /"reply"\s*:\s*"/g;
  const match = marker.exec(jsonText);
  if (!match) {
    return { value: "", closed: false };
  }
  let i = match.index + match[0].length;
  let value = "";
  let escaped = false;
  while (i < jsonText.length) {
    const ch = jsonText[i++];
    if (escaped) {
      escaped = false;
      switch (ch) {
        case "n":
          value += "\n";
          break;
        case "r":
          value += "\r";
          break;
        case "t":
          value += "\t";
          break;
        case '"':
          value += '"';
          break;
        case "\\":
          value += "\\";
          break;
        case "u": {
          if (i + 3 >= jsonText.length) {
            return { value, closed: false };
          }
          const hex = jsonText.slice(i, i + 4);
          if (!/^[0-9a-fA-F]{4}$/.test(hex)) {
            return { value, closed: false };
          }
          value += String.fromCharCode(parseInt(hex, 16));
          i += 4;
          break;
        }
        default:
          value += ch;
      }
      continue;
    }
    if (ch === "\\") {
      escaped = true;
      continue;
    }
    if (ch === '"') {
      return { value, closed: true };
    }
    value += ch;
  }
  return { value, closed: false };
}

function emitStreamEvent(event, payload = {}) {
  process.stdout.write(`${JSON.stringify({ event, ...payload })}\n`);
}

function normalizeResult(parsed, fallbackText, tabs) {
  const action = String(parsed?.action ?? "none").toLowerCase();
  const targetWindow =
    Number.isInteger(parsed?.target_window) && parsed.target_window >= 0
      ? parsed.target_window
      : tabs.find((t) => t.active)?.index ?? tabs[0]?.index ?? 0;
  const summary =
    typeof parsed?.summary === "string" && parsed.summary.trim()
      ? parsed.summary.trim().slice(0, 220)
      : String(fallbackText || "No summary provided.")
          .trim()
          .slice(0, 220);
  const reply =
    typeof parsed?.reply === "string" && parsed.reply.trim()
      ? parsed.reply.trim().slice(0, 300)
      : undefined;
  return {
    action:
      action === "enter" ||
      action === "yes" ||
      action === "send_instruction" ||
      action === "send_keys" ||
      action === "none"
        ? action
        : "none",
    target_window: targetWindow,
    keys: typeof parsed?.keys === "string" ? parsed.keys : undefined,
    instruction:
      typeof parsed?.instruction === "string" ? parsed.instruction : undefined,
    summary,
    reply,
    requires_api_key:
      typeof parsed?.requires_api_key === "string"
        ? parsed.requires_api_key
        : undefined,
    requires_user_input:
      typeof parsed?.requires_user_input === "boolean"
        ? parsed.requires_user_input
        : undefined,
    user_question:
      typeof parsed?.user_question === "string"
        ? parsed.user_question
        : undefined,
    path:
      typeof parsed?.path === "string"
        ? parsed.path
        : typeof parsed?.file_path === "string"
        ? parsed.file_path
        : undefined,
  };
}

function buildPrompt(req) {
  const tabBlock = req.tabs
    .slice(0, 6)
    .map((t) => {
      const header = `T${Number(t.index) + 1} ${t.name} ${
        t.active ? "(active)" : ""
      }`;
      const body = String(t.text ?? "").slice(0, 4000);
      return `${header}\n${body}`;
    })
    .join("\n\n---\n\n");

  const historyBlock = req.history
    .slice(-16)
    .map((m) => `${m.role}: ${String(m.content ?? "").slice(0, 500)}`)
    .join("\n");

  return `Mode: ${req.kind}
Policy: ${req.policy}
User prompt: ${req.prompt || "(none)"}

Chat history:
${historyBlock || "(empty)"}

Terminal tabs:
${tabBlock || "(no tabs)"}

If you need more terminal history than shown, use read_terminal with:
- target_window
- optional tail_lines (omit for full output)
- optional line_offset (for paging back)

If you need to search terminal history efficiently, use grep_terminal with:
- target_window
- pattern
- optional tail_lines
- optional line_offset`;
}

function parsePositiveInt(value) {
  const n = Number(value);
  if (!Number.isInteger(n) || n <= 0) return undefined;
  return n;
}

function parseNonNegativeInt(value, fallback = 0) {
  const n = Number(value);
  if (!Number.isInteger(n) || n < 0) return fallback;
  return n;
}

function resolveTargetWindow(value, tabs) {
  if (Number.isInteger(value) && value >= 0) return value;
  return tabs.find((t) => t.active)?.index ?? tabs[0]?.index ?? 0;
}

function resolveTmuxSocket(req) {
  return (
    req.tmux_socket ??
    process.env.AGENTBOOK_TMUX_SOCKET ??
    DEFAULT_TMUX_SOCKET
  );
}

function resolveTmuxSession(req) {
  return (
    req.tmux_session ??
    process.env.AGENTBOOK_TMUX_SESSION ??
    DEFAULT_TMUX_SESSION
  );
}

function splitTerminalLines(text) {
  const lines = String(text ?? "").split(/\r?\n/);
  if (lines.length > 0 && lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines;
}

function sliceTerminalLines(text, tailLines, lineOffset) {
  const lines = splitTerminalLines(text);
  const offset = parseNonNegativeInt(lineOffset, 0);
  const end = Math.max(0, lines.length - offset);
  const limit = parsePositiveInt(tailLines);
  const start = limit ? Math.max(0, end - limit) : 0;
  const selected = lines.slice(start, end);
  return {
    lines,
    selected,
    text: selected.join("\n"),
    total_lines: lines.length,
    returned_lines: selected.length,
    start_line: selected.length > 0 ? start + 1 : 0,
    end_line: selected.length > 0 ? end : 0,
    line_offset: offset,
    tail_lines: limit,
  };
}

function clampToolText(text) {
  const str = String(text ?? "");
  if (str.length <= MAX_TOOL_RESULT_CHARS) {
    return { text: str, truncated: false };
  }
  return {
    text: str.slice(0, MAX_TOOL_RESULT_CHARS),
    truncated: true,
  };
}

async function captureTmuxWindow(req, targetWindow) {
  const socket = resolveTmuxSocket(req);
  const session = resolveTmuxSession(req);
  try {
    const { stdout } = await execFileAsync(
      "tmux",
      [
        "-L",
        socket,
        "capture-pane",
        "-p",
        "-t",
        `${session}:${targetWindow}`,
        "-S",
        "-",
      ],
      { maxBuffer: MAX_TMUX_CAPTURE_BYTES }
    );
    return {
      ok: true,
      source: "tmux",
      text: String(stdout ?? ""),
      tmux_socket: socket,
      tmux_session: session,
    };
  } catch (err) {
    return {
      ok: false,
      source: "tmux",
      error: String(err?.message ?? err),
      tmux_socket: socket,
      tmux_session: session,
    };
  }
}

function tabSnapshotText(req, targetWindow) {
  const tab = req.tabs.find((t) => Number(t.index) === Number(targetWindow));
  if (tab) return String(tab.text ?? "");
  return String(req.tabs.find((t) => t.active)?.text ?? req.tabs[0]?.text ?? "");
}

async function readTerminalHistory(req, targetWindow) {
  const tmux = await captureTmuxWindow(req, targetWindow);
  if (tmux.ok) return tmux;

  const fallback = tabSnapshotText(req, targetWindow);
  if (fallback.trim()) {
    return {
      ok: true,
      source: "snapshot",
      text: fallback,
      warning: `tmux capture failed: ${tmux.error}`,
      tmux_socket: tmux.tmux_socket,
      tmux_session: tmux.tmux_session,
    };
  }

  return {
    ok: false,
    source: "snapshot",
    error: tmux.error ?? "terminal history unavailable",
    tmux_socket: tmux.tmux_socket,
    tmux_session: tmux.tmux_session,
  };
}

function formatTerminalReadResult(result, opts) {
  if (!result.ok) {
    return [
      "TERMINAL_READ_RESULT",
      "status=error",
      `target_window=${opts.target_window}`,
      `line_offset=${opts.line_offset}`,
      `tail_lines=${opts.tail_lines ?? "full"}`,
      `error=${result.error ?? "unknown error"}`,
      "",
      "Continue and return final JSON.",
    ].join("\n");
  }

  const sliced = sliceTerminalLines(result.text, opts.tail_lines, opts.line_offset);
  const clipped = clampToolText(sliced.text);
  return [
    "TERMINAL_READ_RESULT",
    "status=ok",
    `source=${result.source}`,
    `target_window=${opts.target_window}`,
    `line_offset=${sliced.line_offset}`,
    `tail_lines=${sliced.tail_lines ?? "full"}`,
    `total_lines=${sliced.total_lines}`,
    `returned_lines=${sliced.returned_lines}`,
    `start_line=${sliced.start_line}`,
    `end_line=${sliced.end_line}`,
    `truncated=${clipped.truncated ? "true" : "false"}`,
    `tmux_socket=${result.tmux_socket ?? ""}`,
    `tmux_session=${result.tmux_session ?? ""}`,
    result.warning ? `warning=${result.warning}` : "",
    "",
    clipped.text,
    "",
    "Continue and return final JSON.",
  ]
    .filter(Boolean)
    .join("\n");
}

function grepTerminalLines(result, opts) {
  if (!result.ok) {
    return [
      "TERMINAL_GREP_RESULT",
      "status=error",
      `target_window=${opts.target_window}`,
      `pattern=${opts.pattern ?? ""}`,
      `error=${result.error ?? "unknown error"}`,
      "",
      "Continue and return final JSON.",
    ].join("\n");
  }

  const pattern = String(opts.pattern ?? "").trim();
  if (!pattern) {
    return [
      "TERMINAL_GREP_RESULT",
      "status=error",
      `target_window=${opts.target_window}`,
      "error=missing pattern",
      "",
      "Continue and return final JSON.",
    ].join("\n");
  }

  const sliced = sliceTerminalLines(result.text, opts.tail_lines, opts.line_offset);
  const maxMatches = Math.min(
    parsePositiveInt(opts.max_matches) ?? 40,
    MAX_GREP_MATCHES
  );
  const caseSensitive = opts.case_sensitive === true;
  let regex;
  try {
    regex = new RegExp(pattern, caseSensitive ? "" : "i");
  } catch (err) {
    return [
      "TERMINAL_GREP_RESULT",
      "status=error",
      `target_window=${opts.target_window}`,
      `pattern=${pattern}`,
      `error=invalid regex: ${String(err?.message ?? err)}`,
      "",
      "Continue and return final JSON.",
    ].join("\n");
  }

  const out = [];
  let totalMatches = 0;
  for (let i = 0; i < sliced.selected.length; i++) {
    const line = sliced.selected[i];
    regex.lastIndex = 0;
    if (!regex.test(line)) continue;
    totalMatches += 1;
    if (out.length >= maxMatches) continue;
    const lineNo = sliced.start_line + i;
    out.push(`${lineNo}\t${line}`);
  }

  const clipped = clampToolText(out.join("\n"));
  return [
    "TERMINAL_GREP_RESULT",
    "status=ok",
    `source=${result.source}`,
    `target_window=${opts.target_window}`,
    `pattern=${pattern}`,
    `case_sensitive=${caseSensitive ? "true" : "false"}`,
    `line_offset=${sliced.line_offset}`,
    `tail_lines=${sliced.tail_lines ?? "full"}`,
    `total_lines=${sliced.total_lines}`,
    `searched_lines=${sliced.returned_lines}`,
    `matches=${totalMatches}`,
    `returned_matches=${out.length}`,
    `truncated=${clipped.truncated ? "true" : "false"}`,
    result.warning ? `warning=${result.warning}` : "",
    "",
    clipped.text || "(no matches)",
    "",
    "Continue and return final JSON.",
  ]
    .filter(Boolean)
    .join("\n");
}

function resolveFsRoot() {
  const root = process.env.AGENTBOOK_SIDEKICK_FS_ROOT ?? process.cwd();
  return path.resolve(root);
}

function sanitizeRequestedPath(fsRoot, requestedPath) {
  if (typeof requestedPath !== "string" || !requestedPath.trim()) {
    return { ok: false, error: "missing file path" };
  }
  const rel = requestedPath.trim();
  const abs = path.resolve(fsRoot, rel);
  const relFromRoot = path.relative(fsRoot, abs);
  if (relFromRoot.startsWith("..") || path.isAbsolute(relFromRoot)) {
    return { ok: false, error: "path escapes workspace root" };
  }
  return { ok: true, absPath: abs, relPath: relFromRoot || "." };
}

async function readFileForModel(fsRoot, requestedPath) {
  const pathCheck = sanitizeRequestedPath(fsRoot, requestedPath);
  if (!pathCheck.ok) {
    return { ok: false, requestedPath, error: pathCheck.error };
  }
  try {
    const buf = await readFile(pathCheck.absPath);
    const truncated = buf.byteLength > MAX_FILE_BYTES;
    const slice = truncated ? buf.subarray(0, MAX_FILE_BYTES) : buf;
    const content = slice.toString("utf8");
    return {
      ok: true,
      requestedPath,
      relPath: pathCheck.relPath,
      truncated,
      content,
      bytes: slice.byteLength,
    };
  } catch (err) {
    return {
      ok: false,
      requestedPath,
      relPath: pathCheck.relPath,
      error: String(err?.message ?? err),
    };
  }
}

function formatFileReadResult(result, fsRoot) {
  if (!result.ok) {
    return [
      "FILE_READ_RESULT",
      `workspace_root=${fsRoot}`,
      `status=error`,
      `requested_path=${result.requestedPath ?? ""}`,
      `resolved_path=${result.relPath ?? ""}`,
      `error=${result.error ?? "unknown error"}`,
      "",
      "Continue and return final JSON.",
    ].join("\n");
  }
  return [
    "FILE_READ_RESULT",
    `workspace_root=${fsRoot}`,
    "status=ok",
    `requested_path=${result.requestedPath}`,
    `resolved_path=${result.relPath}`,
    `bytes=${result.bytes}`,
    `truncated=${result.truncated ? "true" : "false"}`,
    "",
    result.content,
    "",
    "Continue and return final JSON.",
  ].join("\n");
}

async function inferWithFilesystem(model, inferenceOpts, req) {
  const fsRoot = resolveFsRoot();
  const messages = [
    {
      role: "user",
      content: buildPrompt(req),
      timestamp: Date.now(),
    },
  ];
  let fileReads = 0;
  let terminalReads = 0;
  let lastText = "";
  let lastParsed = null;

  for (let step = 0; step < MAX_MODEL_STEPS; step++) {
    const context = {
      systemPrompt: `You are Sidekick, an AI coding assistant inside a terminal multiplexer.
You can inspect multiple terminal tabs and optionally send safe key input.
You can also request local filesystem reads inside the current workspace.
You can page through full terminal history and grep it when needed.

Output ONLY strict JSON:
{"action":"none|enter|yes|send_keys|send_instruction|read_file|read_terminal|grep_terminal","target_window":0,"keys":"optional","instruction":"optional full instruction text","path":"relative/path","pattern":"optional regex for grep_terminal","tail_lines":120,"line_offset":0,"max_matches":40,"summary":"short status","reply":"chat reply","requires_user_input":false,"user_question":"optional","requires_api_key":"optional"}

Rules:
- Prefer action=none unless confidence is high.
- Use enter only for explicit Enter/Return continue prompts.
- Use yes only for explicit continue/proceed yes/no prompts.
- Use read_file when you need source context to make a high-quality decision.
- Use read_terminal when you need more terminal history.
- read_terminal supports optional tail_lines and optional line_offset for paging.
- Omit tail_lines to request full terminal history for the target window.
- Use grep_terminal to search terminal history by regex pattern.
- Use send_instruction to pass a clear instruction to a downstream coding agent tab.
- For send_instruction, set target_window and instruction; the system will submit it.
- For read_file, provide path relative to workspace root when possible.
- Never run destructive actions.
- You are a Sidekick agent whose job is to help manage multiple terminal tabs and coding agents.
- In auto/heartbeat mode, advance coding agents when safe and useful.
- If a major architectural decision is needed, STOP automation and set:
  requires_user_input=true and a clear user_question.
- Always optimize for secure, high-quality, well-tested, well-architected code following best practices.
- Keep summary concise (<220 chars), reply concise (<300 chars).`,
      messages,
      tools: [],
    };

    let text = "";
    let replyCharsSent = 0;
    const streamOpts = {};
    if (inferenceOpts.apiKey) streamOpts.apiKey = inferenceOpts.apiKey;
    if (inferenceOpts.baseURL) streamOpts.baseURL = inferenceOpts.baseURL;
    const s = stream(model, context, Object.keys(streamOpts).length ? streamOpts : undefined);
    for await (const event of s) {
      if (event.type === "text_delta") {
        text += event.delta;
        if (req.stream_events) {
          const { value } = extractReplyPrefix(text);
          if (value.length > replyCharsSent) {
            const delta = value.slice(replyCharsSent);
            if (delta) {
              emitStreamEvent("reply_delta", { delta });
              replyCharsSent = value.length;
            }
          }
        }
      }
    }
    await s.result();

    lastText = text;
    lastParsed = extractJson(text);
    const action = String(lastParsed?.action ?? "").toLowerCase();
    if (
      action !== "read_file" &&
      action !== "read_terminal" &&
      action !== "grep_terminal"
    ) {
      break;
    }

    messages.push({ role: "assistant", content: text, timestamp: Date.now() });

    if (action === "read_file") {
      const requestedPath =
        typeof lastParsed?.path === "string"
          ? lastParsed.path
          : typeof lastParsed?.file_path === "string"
          ? lastParsed.file_path
          : "";

      if (fileReads >= MAX_FILE_READ_REQUESTS) {
        messages.push({
          role: "user",
          content: `FILE_READ_RESULT\nworkspace_root=${fsRoot}\nstatus=error\nerror=read limit reached (${MAX_FILE_READ_REQUESTS})\n\nContinue and return final JSON.`,
          timestamp: Date.now(),
        });
        continue;
      }

      const result = await readFileForModel(fsRoot, requestedPath);
      fileReads += 1;
      messages.push({
        role: "user",
        content: formatFileReadResult(result, fsRoot),
        timestamp: Date.now(),
      });
      continue;
    }

    if (terminalReads >= MAX_TERMINAL_READ_REQUESTS) {
      const kind = action === "grep_terminal" ? "TERMINAL_GREP_RESULT" : "TERMINAL_READ_RESULT";
      messages.push({
        role: "user",
        content: `${kind}\nstatus=error\nerror=terminal read limit reached (${MAX_TERMINAL_READ_REQUESTS})\n\nContinue and return final JSON.`,
        timestamp: Date.now(),
      });
      continue;
    }

    const targetWindow = resolveTargetWindow(lastParsed?.target_window, req.tabs);
    const tailLines = parsePositiveInt(lastParsed?.tail_lines);
    const lineOffset = parseNonNegativeInt(lastParsed?.line_offset, 0);
    const terminalResult = await readTerminalHistory(req, targetWindow);
    terminalReads += 1;

    if (action === "read_terminal") {
      messages.push({
        role: "user",
        content: formatTerminalReadResult(terminalResult, {
          target_window: targetWindow,
          tail_lines: tailLines,
          line_offset: lineOffset,
        }),
        timestamp: Date.now(),
      });
      continue;
    }

    const pattern =
      typeof lastParsed?.pattern === "string"
        ? lastParsed.pattern
        : typeof lastParsed?.grep === "string"
        ? lastParsed.grep
        : "";
    messages.push({
      role: "user",
      content: grepTerminalLines(terminalResult, {
        target_window: targetWindow,
        tail_lines: tailLines,
        line_offset: lineOffset,
        pattern,
        max_matches: lastParsed?.max_matches,
        case_sensitive: lastParsed?.case_sensitive,
      }),
      timestamp: Date.now(),
    });
  }

  return { text: lastText, parsed: lastParsed };
}

async function main() {
  const raw = await readStdin();
  let reqRaw = {};
  try {
    reqRaw = JSON.parse(raw || "{}");
  } catch {
    process.stdout.write(
      JSON.stringify({
        action: "none",
        target_window: 0,
        summary: "Invalid request payload.",
        reply: "I could not parse the Sidekick request payload.",
      })
    );
    return;
  }

  const req = normalizeRequest(reqRaw);
  const { provider, model } = modelFromEnv();
  const inferenceConfig = resolveInferenceConfig(provider);

  if (!inferenceConfig.apiKey) {
    const isArda = !!process.env.AGENTBOOK_GATEWAY_API_KEY;
    process.stdout.write(
      JSON.stringify({
        action: "none",
        target_window: req.tabs.find((t) => t.active)?.index ?? 0,
        requires_api_key: "anthropic",
        summary: isArda
          ? "Arda Gateway key present but empty. Run `agentbook login` to re-authenticate."
          : "No API key found. Run `agentbook login` to authenticate with Arda, or set ANTHROPIC_API_KEY.",
        reply: isArda
          ? "Your Arda session appears invalid. Run `agentbook login` to re-authenticate."
          : "I need an API key. Run `agentbook login` to authenticate with Arda Gateway, or export ANTHROPIC_API_KEY.",
      })
    );
    return;
  }

  try {
    const { text, parsed } = await inferWithFilesystem(model, inferenceConfig, req);
    const result = normalizeResult(parsed, text, req.tabs);
    process.stdout.write(JSON.stringify(result));
  } catch (err) {
    const msg = String(err?.message ?? err);
    const isGateway = !!inferenceConfig.baseURL;

    // Detect auth / billing / rate-limit errors.
    const authLike =
      /(api key|authentication|unauthorized|forbidden|invalid x-api-key|401|403)/i.test(msg);
    const billingLike = /(insufficient.*balance|payment|402)/i.test(msg);
    const rateLimited = /(rate.?limit|too many requests|429)/i.test(msg);

    let summary, reply;
    if (billingLike) {
      summary = "Arda Gateway: insufficient balance. Add credits at bot.ardabot.ai.";
      reply = "Your Arda balance is too low. Visit bot.ardabot.ai to add credits.";
    } else if (rateLimited) {
      summary = "Rate limited. Wait a moment and try again.";
      reply = "You've been rate-limited. Please wait a moment before trying again.";
    } else if (authLike) {
      summary = isGateway
        ? "Arda Gateway auth failed. Run `agentbook login` to re-authenticate."
        : "Anthropic authentication failed. Enter a valid API key.";
      reply = isGateway
        ? "Arda auth failed. Run `agentbook login` to re-authenticate."
        : "Anthropic auth failed. Enter a valid API key in Sidekick and press Enter.";
    } else {
      summary = `Inference error: ${msg}`.slice(0, 180);
      reply = "Inference failed. Check Sidekick configuration.";
    }

    process.stdout.write(
      JSON.stringify({
        action: "none",
        target_window: req.tabs.find((t) => t.active)?.index ?? 0,
        requires_api_key: authLike && !isGateway ? "anthropic" : undefined,
        summary,
        reply,
      })
    );
    return;
  }
}

main().catch((err) => {
  process.stdout.write(
    JSON.stringify({
      action: "none",
      target_window: 0,
      summary: `PI adapter error: ${String(err?.message ?? err)}`.slice(0, 180),
      reply: "I hit an internal Sidekick adapter error.",
    })
  );
});
