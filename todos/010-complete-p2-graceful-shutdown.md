---
status: pending
priority: p2
issue_id: "010"
tags: [code-review, architecture]
dependencies: []
---

# Server Shutdown via process::exit Skips Cleanup

## Problem Statement

The Ctrl+C handler calls `std::process::exit(0)`, skipping destructors, orphaning child processes, and dropping buffered output. No graceful drain of connections or SessionExited events.

**Flagged by:** security-sentinel

## Findings

- **File:** `crates/tmax-server/src/server.rs` lines 40-47
- `std::process::exit(0)` called directly
- Active PTY sessions become orphaned
- Clients receive abrupt disconnections

## Proposed Solutions

### Option A: Cancellation token for graceful shutdown (Recommended)
Use a `CancellationToken` to signal the accept loop to stop, drain active connections, and send exit events before terminating.

**Pros:** Clean shutdown, no orphaned processes
**Cons:** More complex shutdown logic
**Effort:** Medium
**Risk:** Low

## Acceptance Criteria

- [ ] Server drains connections on shutdown
- [ ] Child processes terminated gracefully
- [ ] Socket and PID files cleaned up

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
