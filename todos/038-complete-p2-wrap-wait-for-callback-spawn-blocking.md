---
status: pending
priority: p2
issue_id: "038"
tags: [code-review, architecture]
dependencies: []
---

# Wrap wait_for_callback in spawn_blocking

## Problem Statement

`cmd_login` is `async` and calls `wait_for_callback` which uses `std::thread::sleep(200ms)` in a blocking poll loop. This blocks the Tokio runtime thread during the entire OAuth wait (up to 120 seconds). Also, `set_nonblocking(true)` is called inside the loop on every iteration when it only needs to be called once.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:198-268` — blocking sleep in async context
- **File:** `crates/agentbook-cli/src/login.rs:211` — `set_nonblocking(true)` called every iteration
- **Source:** architecture-strategist, performance-oracle

## Proposed Solutions

### Solution A: Wrap in tokio::task::spawn_blocking
- Move `wait_for_callback` call into `spawn_blocking` to avoid blocking the Tokio runtime
- Move `set_nonblocking(true)` before the loop
- **Effort:** Small | **Risk:** Low

### Solution B: Make wait_for_callback async
- Use `tokio::net::TcpListener` with `select!` instead of blocking poll
- **Effort:** Medium | **Risk:** Low

## Acceptance Criteria

- [ ] `wait_for_callback` does not block the Tokio runtime thread
- [ ] `set_nonblocking` is called once, not in the loop

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review | - |
