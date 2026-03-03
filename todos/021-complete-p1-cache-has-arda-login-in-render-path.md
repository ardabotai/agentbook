---
status: pending
priority: p1
issue_id: "021"
tags: [code-review, performance]
dependencies: []
---

# Cache has_arda_login() Result — Disk I/O on 60fps Render Path

## Problem Statement

`has_arda_login()` performs file I/O (`fs::read_to_string`) and is called from `draw_sidekick()` in `ui.rs` which runs at up to 60fps. During the `awaiting_api_key` state (first experience for new users), this causes ~240 syscalls/second (stat + open + read + close) for a single boolean check.

## Findings

- **File:** `crates/agentbook-tui/src/ui.rs:642-647` — `has_arda_login()` called every frame when `awaiting_api_key` is true
- **File:** `crates/agentbook-tui/src/automation.rs:769-777` — `has_arda_login()` does disk I/O
- **Source:** performance-oracle agent

## Proposed Solutions

### Solution A: Cache in AutoAgentState (Recommended)
- Add `cached_has_arda: Option<bool>` to `AutoAgentState`
- Set it when entering `awaiting_api_key` state in `apply_decision()`
- Refresh it in `submit_sidekick_api_key()` when user presses Enter
- Read cached value in `draw_sidekick()`
- **Pros:** Zero disk I/O in render path, simple
- **Cons:** Slightly stale (only updates on user action)
- **Effort:** Small
- **Risk:** Low

### Solution B: Timer-based refresh
- Re-check every 5 seconds via the tick handler
- **Pros:** Auto-detects external login
- **Cons:** More complex, still some disk I/O
- **Effort:** Medium
- **Risk:** Low

## Recommended Action

Solution A — cache in AutoAgentState

## Technical Details

- **Affected files:** `crates/agentbook-tui/src/automation.rs`, `crates/agentbook-tui/src/ui.rs`

## Acceptance Criteria

- [ ] `draw_sidekick()` does not call `has_arda_login()` directly
- [ ] Cached value is refreshed on user actions (Enter key, state transitions)
- [ ] No visible behavior change for users

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | Render functions should never perform I/O |

## Resources

- PR branch: worktree-arda-gateway-sidekick
