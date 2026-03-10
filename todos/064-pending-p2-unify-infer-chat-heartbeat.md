---
status: done
priority: p2
issue_id: "064"
tags: [code-review, duplication, pi-terminal-agent, architecture]
dependencies: []
---

# Unify inferChat/inferHeartbeat Tool Execution Loop

## Problem Statement

`inferChat()` and `inferHeartbeat()` in `pi-terminal-agent.mjs` share ~80% of their logic (message building, tool loop up to MAX_MODEL_STEPS, read_file/read_terminal/grep_terminal execution, limit guards). The differences are: system prompt, prompt builder, output parsing, streaming heuristic, and continuation message. This duplication means tool-handling bugs must be fixed in two places.

## Findings

- **File:** `agent/scripts/pi-terminal-agent.mjs:753-884` — inferHeartbeat tool loop
- **File:** `agent/scripts/pi-terminal-agent.mjs:891-1053` — inferChat tool loop (near-identical)
- **Also:** `extractReplyPrefix` (56 lines) exists solely for low-value heartbeat streaming
- **Also:** `.replace()` on continuation footer is brittle (lines 973, 1006, 1031)
- **Source:** pattern-recognition, simplicity-reviewer, architecture-strategist

## Proposed Solutions

### Solution A: Parameterized mode config (recommended)
```javascript
const MODE_CONFIG = {
  heartbeat: { systemPrompt, buildPrompt, parseOutput, toolContinue, streamFilter },
  chat: { systemPrompt, buildPrompt, parseOutput, toolContinue, streamFilter },
};
```
- Single `infer(model, modeConfig, req)` function with shared tool loop
- Accept `continuation` parameter in format functions instead of `.replace()`
- Remove `extractReplyPrefix` (heartbeat doesn't need incremental streaming)
- **Effort:** Medium | **Risk:** Low | **LOC saved:** ~135

## Acceptance Criteria

- [x] Single tool-execution loop shared by both modes
- [x] Behavioral differences expressed via config, not code duplication
- [x] extractReplyPrefix removed (heartbeat responses buffered fully)
- [x] Continuation footer parameterized, not string-replaced

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | Largest DRY opportunity in the changeset (~20% of additions) |
| 2026-03-10 | Implemented Solution A | Unified into `infer()` with HEARTBEAT_MODE/CHAT_MODE configs, removed extractReplyPrefix, parameterized continuation footer. -127 lines |
