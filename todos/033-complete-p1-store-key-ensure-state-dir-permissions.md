---
status: pending
priority: p1
issue_id: "033"
tags: [code-review, security]
dependencies: []
---

# store_key Should Use ensure_state_dir for Directory Permissions

## Problem Statement

`store_key()` in `login.rs` creates the state directory with `std::fs::create_dir_all` but does not call `ensure_state_dir()` which sets `0o700` permissions on the parent directory. If the directory was freshly created by this flow (no prior `agentbook setup`), the parent directory might have default umask permissions (e.g., `0o755`), making the key file discoverable by other users even though the file itself is `0o600`.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:378` — uses `create_dir_all` instead of `ensure_state_dir`
- **Source:** security-sentinel agent
- **Note:** The secure version exists at `agentbook_mesh::state_dir::ensure_state_dir()` and is used elsewhere.

## Proposed Solutions

### Solution A: Replace create_dir_all with ensure_state_dir
- Replace `std::fs::create_dir_all(state_dir)` with `agentbook_mesh::state_dir::ensure_state_dir(state_dir)?`
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] `store_key()` uses `ensure_state_dir()` instead of `create_dir_all`
- [ ] State directory has `0o700` permissions when created by login flow

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review | - |
