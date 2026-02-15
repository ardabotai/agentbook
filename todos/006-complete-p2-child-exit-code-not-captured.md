---
status: complete
priority: p2
issue_id: "006"
tags: [code-review, bug, architecture]
dependencies: []
---

# Child Process Exit Code Never Captured

## Problem Statement

The `_child` handle from `pty_pair.slave.spawn_command(cmd)` is immediately discarded. The PTY I/O loop hardcodes exit code 0 on EOF. tmax always reports exit code 0 regardless of how the process actually exited.

**Flagged by:** architecture-strategist

## Findings

- **File:** `crates/libtmax/src/session.rs` line 159 -- `let _child = ...`
- **File:** `crates/tmax-server/src/connection.rs` line 401 -- `record_exit(&session_id, Some(0), None)`
- Child handle must be retained and awaited to capture the real exit code

## Proposed Solutions

### Option A: Retain Child handle and await exit (Recommended)
Store the `Child` handle in the `Session` struct, await it in the PTY I/O loop after EOF.

**Pros:** Correct exit code reporting
**Cons:** Requires storing Child in Session
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] Child handle retained in Session
- [ ] Real exit code captured on process exit
- [ ] Exit code available via SessionInfo

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
