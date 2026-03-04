---
status: pending
priority: p3
issue_id: "056"
tags: [code-review, security, filesystem-permissions]
dependencies: []
---

# Fix Preferences File Permissions

## Problem Statement

`persist_preferences_to_path` uses `create_dir_all` (respects umask, typically 0o755) and `fs::write` (default permissions) instead of `ensure_state_dir()` and explicit 0o600 mode. Inconsistent with the project's security conventions for state directories.

## Findings

- **File:** `app.rs:468-480` — create_dir_all + fs::write with default perms
- **Known Pattern:** docs/solutions/security-issues/oauth-credential-handling-rust-tui.md Problem 5
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Use ensure_state_dir and set 0o600
- Replace create_dir_all with ensure_state_dir
- Use OpenOptions with mode(0o600) for the file write
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] State directory created with 0o700
- [ ] Preferences file written with 0o600

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI security review | Follow established pattern |
