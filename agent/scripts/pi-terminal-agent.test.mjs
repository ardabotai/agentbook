import { describe, it } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { realpathSync } from "node:fs";

import {
  extractJson,
  normalizeRequest,
  normalizeResult,
  sanitizeRequestedPath,
  extractChatAction,
  isJsonStart,
  grepTerminalLines,
  splitTerminalLines,
  sliceTerminalLines,
  parsePositiveInt,
  parseNonNegativeInt,
  clampToolText,
} from "./pi-terminal-agent.mjs";

// ── sanitizeRequestedPath ───────────────────────────────────────────────────

describe("sanitizeRequestedPath", () => {
  const fsRoot = realpathSync(process.cwd());

  it("accepts a simple relative path within workspace", () => {
    const result = sanitizeRequestedPath(fsRoot, "src/index.ts");
    assert.equal(result.ok, true);
    assert.equal(result.relPath, path.join("src", "index.ts"));
  });

  it("rejects a path that escapes the workspace via ..", () => {
    const result = sanitizeRequestedPath(fsRoot, "../../etc/passwd");
    assert.equal(result.ok, false);
    assert.match(result.error, /escapes workspace/);
  });

  it("rejects empty/missing path", () => {
    assert.equal(sanitizeRequestedPath(fsRoot, "").ok, false);
    assert.equal(sanitizeRequestedPath(fsRoot, "  ").ok, false);
    assert.equal(sanitizeRequestedPath(fsRoot, undefined).ok, false);
    assert.equal(sanitizeRequestedPath(fsRoot, null).ok, false);
  });
});

// ── extractChatAction ───────────────────────────────────────────────────────

describe("extractChatAction", () => {
  it("extracts a fenced action block from the end of text", () => {
    const text = `Here is my analysis.\n\n\`\`\`action\n{"action":"send_keys","target_window":0,"keys":"y\\n"}\n\`\`\``;
    const { action, replyText } = extractChatAction(text);
    assert.equal(action.action, "send_keys");
    assert.equal(action.keys, "y\n");
    assert.equal(replyText, "Here is my analysis.");
  });

  it("returns null action when there is no action block", () => {
    const text = "Just a plain response with no action.";
    const { action, replyText } = extractChatAction(text);
    assert.equal(action, null);
    assert.equal(replyText, "Just a plain response with no action.");
  });
});

// ── isJsonStart ─────────────────────────────────────────────────────────────

describe("isJsonStart", () => {
  it("returns true when text starts with {", () => {
    assert.equal(isJsonStart('{"action":"none"}'), true);
    assert.equal(isJsonStart("  { "), true);
  });

  it("returns true when text starts with triple backtick", () => {
    assert.equal(isJsonStart("```action\n{}```"), true);
  });

  it("returns false for plain text", () => {
    assert.equal(isJsonStart("Hello world"), false);
    assert.equal(isJsonStart(""), false);
    assert.equal(isJsonStart("  Let me explain..."), false);
  });
});

// ── normalizeResult ─────────────────────────────────────────────────────────

describe("normalizeResult", () => {
  const tabs = [{ index: 0, active: true, name: "shell" }];

  it("returns action=none for unknown action strings", () => {
    const result = normalizeResult({ action: "hack_system" }, "", tabs);
    assert.equal(result.action, "none");
  });

  it("preserves valid actions", () => {
    for (const action of ["enter", "yes", "send_keys", "none"]) {
      const result = normalizeResult({ action }, "", tabs);
      assert.equal(result.action, action);
    }
  });

  it("uses parsed summary with truncation to 220 chars", () => {
    const long = "x".repeat(300);
    const result = normalizeResult({ summary: long }, "", tabs);
    assert.equal(result.summary.length, 220);
  });

  it("falls back to fallbackText when parsed summary is empty", () => {
    const result = normalizeResult({}, "my fallback", tabs);
    assert.equal(result.summary, "my fallback");
  });

  it("in chat mode uses full reply without truncation", () => {
    const longReply = "word ".repeat(200);
    const result = normalizeResult(
      { reply: longReply },
      "",
      tabs,
      true
    );
    assert.equal(result.reply, longReply.trim());
  });

  it("in heartbeat mode truncates reply to 300 chars", () => {
    const longReply = "x".repeat(500);
    const result = normalizeResult(
      { reply: longReply },
      "",
      tabs,
      false
    );
    assert.equal(result.reply.length, 300);
  });
});

// ── grepTerminalLines regex guard ───────────────────────────────────────────

describe("grepTerminalLines", () => {
  const okResult = { ok: true, source: "snapshot", text: "line1\nline2 hello\nline3" };

  it("returns error when pattern is missing", () => {
    const output = grepTerminalLines(okResult, {
      target_window: 0,
      pattern: "",
    });
    assert.match(output, /missing pattern/);
  });

  it("returns error when pattern exceeds 200 chars", () => {
    const output = grepTerminalLines(okResult, {
      target_window: 0,
      pattern: "a".repeat(201),
    });
    assert.match(output, /too long/);
  });

  it("returns error for invalid regex", () => {
    const output = grepTerminalLines(okResult, {
      target_window: 0,
      pattern: "[invalid",
    });
    assert.match(output, /invalid regex/);
  });

  it("returns matches for valid regex", () => {
    const output = grepTerminalLines(okResult, {
      target_window: 0,
      pattern: "hello",
    });
    assert.match(output, /status=ok/);
    assert.match(output, /matches=1/);
    assert.match(output, /hello/);
  });
});

// ── extractJson ─────────────────────────────────────────────────────────────

describe("extractJson", () => {
  it("parses clean JSON", () => {
    const result = extractJson('{"action":"none"}');
    assert.deepEqual(result, { action: "none" });
  });

  it("extracts JSON embedded in other text", () => {
    const result = extractJson('Some preamble {"key":"val"} trailing');
    assert.deepEqual(result, { key: "val" });
  });

  it("returns null for non-JSON text", () => {
    assert.equal(extractJson("just plain text"), null);
    assert.equal(extractJson(""), null);
    assert.equal(extractJson(null), null);
  });
});

// ── normalizeRequest ────────────────────────────────────────────────────────

describe("normalizeRequest", () => {
  it("normalizes a minimal request with defaults", () => {
    const req = normalizeRequest({});
    assert.equal(req.kind, "heartbeat");
    assert.equal(req.prompt, "");
    assert.equal(Array.isArray(req.tabs), true);
    assert.equal(Array.isArray(req.history), true);
    assert.equal(req.stream_events, false);
  });

  it("preserves chat kind and prompt", () => {
    const req = normalizeRequest({ kind: "chat", prompt: "hello" });
    assert.equal(req.kind, "chat");
    assert.equal(req.prompt, "hello");
  });
});

// ── parsePositiveInt / parseNonNegativeInt ───────────────────────────────────

describe("parsePositiveInt", () => {
  it("parses positive integers", () => {
    assert.equal(parsePositiveInt(5), 5);
    assert.equal(parsePositiveInt(1), 1);
  });

  it("returns undefined for zero and negatives", () => {
    assert.equal(parsePositiveInt(0), undefined);
    assert.equal(parsePositiveInt(-3), undefined);
    assert.equal(parsePositiveInt(1.5), undefined);
    assert.equal(parsePositiveInt("abc"), undefined);
  });
});

describe("parseNonNegativeInt", () => {
  it("parses zero and positive integers", () => {
    assert.equal(parseNonNegativeInt(0), 0);
    assert.equal(parseNonNegativeInt(10), 10);
  });

  it("returns fallback for invalid values", () => {
    assert.equal(parseNonNegativeInt(-1, 42), 42);
    assert.equal(parseNonNegativeInt("abc", 0), 0);
  });
});

