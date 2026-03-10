---
status: done
priority: p2
issue_id: "063"
tags: [code-review, duplication, security, input-validation]
dependencies: []
---

# Extract Shared Rename Helper and Sanitize Rename Input

## Problem Statement

Two issues: (1) The tmux rename logic is duplicated between `/rename <name>` slash command and `handle_rename_key()` Enter handler — identical `mux_rename_window` + match arms. (2) The rename input has no length limit or character restrictions. Embedded control characters, ANSI escapes, or extremely long names could cause tmux rendering issues.

## Findings

- **File:** `crates/agentbook-tui/src/input.rs:402-424` — slash command rename path
- **File:** `crates/agentbook-tui/src/input.rs:1048-1075` — interactive rename path
- **Bug:** `begin_terminal_tab_rename` pre-fills with `"1 agentbook"` including numeric prefix from `terminal_window_tabs`
- **Source:** pattern-recognition, architecture-strategist, simplicity-reviewer, security-sentinel

## Proposed Solutions

### Solution A: Extract helper + validate (recommended)
```rust
fn apply_terminal_tab_rename(app: &mut App, name: &str) {
    // Shared logic: terminals.first(), terminal_window_indices.get(), mux_rename_window match
}
```
- Cap rename to 64 chars, strip control characters (0x00-0x1F, 0x7F)
- Fix pre-fill to strip the numeric prefix or start empty
- **Effort:** Small | **Risk:** None

## Acceptance Criteria

- [x] Single `apply_terminal_tab_rename` helper used by both code paths
- [x] Rename input capped at 64 characters
- [x] Control characters stripped or rejected
- [x] Pre-fill shows clean tab name (no numeric prefix)

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | 3 agents flagged the duplication independently |
| 2026-03-10 | Resolved: extracted helper, added sanitization, fixed pre-fill, added 8 unit tests | All acceptance criteria met |
