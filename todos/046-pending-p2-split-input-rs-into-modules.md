---
status: pending
priority: p2
issue_id: "046"
tags: [code-review, architecture, refactor]
dependencies: []
---

# Split input.rs Into Focused Modules

## Problem Statement

`input.rs` is 1,805 lines handling 7 distinct responsibilities: keyboard dispatch, slash command parsing (16 subcommands), sidekick command management (10 subcommands), terminal pane management, mouse input handling, sidekick chat input/streaming, and key-to-PTY byte encoding. This violates Single Responsibility and makes navigation difficult.

## Findings

- **File:** `input.rs:34-168` — handle_key (keyboard dispatch)
- **File:** `input.rs:170-387` — handle_slash_command (218-line match, 16 subcommands)
- **File:** `input.rs:389-511` — handle_sidekick_command (10 subcommands)
- **File:** `input.rs:556-833` — terminal pane management (ensure, split, focus, close, tabs)
- **File:** `input.rs:983-1232` — sidekick chat input and streaming
- **File:** `input.rs:1240-1516` — mouse input handling
- **File:** `input.rs:1569-1644` — key_to_bytes (pure PTY encoding)
- **Source:** architecture-strategist, code-simplicity-reviewer

## Proposed Solutions

### Solution A: Extract into input/ directory with focused modules
- `input/keys.rs` — keyboard dispatch and prefix chord handling
- `input/slash.rs` — slash command parsing and execution
- `input/mouse.rs` — mouse click, scroll, forwarding
- `input/sidekick_chat.rs` — sidekick chat input, streaming, API key submission
- `input/terminal_ops.rs` — terminal pane/tab management
- Move `key_to_bytes` into `terminal.rs` (purely PTY-related)
- **Effort:** Medium | **Risk:** Low (mechanical refactor)

## Acceptance Criteria

- [ ] input.rs core reduced to under 600 lines
- [ ] Each extracted module has a single clear responsibility
- [ ] All tests pass after refactor
- [ ] No behavior changes

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI architecture review | Highest-impact structural improvement |
