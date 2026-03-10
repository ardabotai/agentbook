---
status: done
priority: p2
issue_id: "065"
tags: [code-review, testing, rename, login, pi-terminal-agent]
dependencies: []
---

# Add Test Coverage for Rename, Login, Tab Labels, and PI Agent Helpers

## Problem Statement

Scroll mode has 7 tests (good), but the other new features have zero test coverage: rename key handler, /rename slash command, terminal_tab_label(), start_arda_login state transitions, and all pi-terminal-agent.mjs helper functions (extractChatAction, sanitizeRequestedPath, normalizeResult, isJsonStart, grepTerminalLines, etc.).

## Findings

- **File:** `input.rs` — no tests for handle_rename_key, begin_terminal_tab_rename, /rename
- **File:** `app.rs` — no tests for terminal_tab_label()
- **File:** `automation.rs` — no tests for start_arda_login state transitions
- **File:** `agent/scripts/pi-terminal-agent.mjs` — no test suite exists at all
- **Source:** pattern-recognition

## Proposed Solutions

### Rust tests to add:
- `terminal_tab_label("zsh", Some("/home/user/project"))` → `"project"`
- `terminal_tab_label("vim", Some("/path"))` → `"vim"` (non-default shell)
- `terminal_tab_label("zsh", None)` → `"zsh"` (no pane_path)
- Rename: Enter commits, Esc cancels, Backspace edits, empty cancels, non-Terminal tab rejects
- Login: guard prevents double-launch, sets login_in_progress

### JS tests to add:
- `sanitizeRequestedPath` with traversal attempts, symlinks, normal paths
- `extractChatAction` with and without fenced action blocks
- `isJsonStart` heuristic edge cases
- `normalizeResult` for chat vs heartbeat modes
- **Effort:** Medium | **Risk:** None

## Acceptance Criteria

- [x] terminal_tab_label has at least 3 unit tests
- [x] Rename feature has at least 5 unit tests
- [x] pi-terminal-agent.mjs has a test file with at least 8 tests for pure helpers

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | Scroll mode well tested; other features need parity |
| 2026-03-10 | Implemented all tests | 4 tab_label tests, 5 rename tests, 1 login guard test in Rust; 27 JS tests for pure helpers. 382 Rust + 27 JS all passing |
