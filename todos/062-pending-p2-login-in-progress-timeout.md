---
status: done
priority: p2
issue_id: "062"
tags: [code-review, robustness, sidekick, arda-login]
dependencies: []
---

# Add Timeout for login_in_progress and Fix reset() Cleanup

## Problem Statement

`start_arda_login()` sets `login_in_progress = true` and spawns a background thread, but there is no timeout. If the OAuth flow fails (user closes browser, network error, process hangs), `login_in_progress` stays `true` indefinitely, leaving the Sidekick stuck in "Waiting for Arda login..." with no escape hatch. Additionally, `AutoAgentState::reset()` does not clear `login_in_progress`, so toggling Sidekick off/on doesn't fix it either.

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:846-882` — no timeout, exit status discarded
- **File:** `crates/agentbook-tui/src/automation.rs:56-88` — tick() only clears on key detection
- **File:** `crates/agentbook-tui/src/app.rs:112-123` — reset() missing `login_in_progress = false`
- **Source:** security-sentinel, architecture-strategist, pattern-recognition, simplicity-reviewer (unanimous)

## Proposed Solutions

### Solution A: Wall-clock timeout in tick() (recommended)
- Store `login_started_at: Option<Instant>` when login begins
- In tick(), if login_in_progress and elapsed > 120s, reset flag and show error
- Add `self.login_in_progress = false` to `AutoAgentState::reset()`
- **Effort:** Small | **Risk:** None

### Solution B: Background thread signals completion via channel
- Spawn thread returns result via `Arc<Mutex<Option<Result>>>>`
- tick() checks for completion and handles success/failure
- **Effort:** Medium | **Risk:** Low

## Acceptance Criteria

- [x] login_in_progress resets after timeout (e.g., 120s) with user-visible error
- [x] AutoAgentState::reset() clears login_in_progress
- [x] User can retry login after failure

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | All 5 review agents flagged this independently |
| 2026-03-10 | Implemented Solution A | Added login_started_at field, 120s timeout in tick(), reset() cleanup, and test |
