---
status: pending
priority: p1
issue_id: "001"
tags: [code-review, performance, architecture]
dependencies: []
---

# Blocking PTY Read on Tokio Runtime

## Problem Statement

The `pty_io_loop` function performs synchronous blocking `std::io::Read::read()` calls inside a `tokio::spawn`ed async task. This blocks Tokio worker threads. With N active sessions on an N-core machine, all Tokio workers are blocked, causing the entire server to stall -- no new connections accepted, no requests processed.

The `WouldBlock` fallback with a 10ms sleep is a busy-wait that compounds the problem.

**Flagged by:** security-sentinel, performance-oracle, architecture-strategist

## Findings

- **File:** `crates/tmax-server/src/connection.rs` lines 389-423
- `reader.read(&mut buf)` is a blocking syscall on an async worker thread
- Each active PTY session permanently parks one Tokio worker thread
- At 4 sessions on a 4-core machine, the entire server freezes

## Proposed Solutions

### Option A: `spawn_blocking` with mpsc channel (Recommended)
Use `tokio::task::spawn_blocking` for the read loop, sending data back via `tokio::sync::mpsc::channel`.

**Pros:** Idiomatic Tokio pattern, uses the blocking thread pool correctly
**Cons:** Adds a channel hop per output chunk
**Effort:** Small
**Risk:** Low

### Option B: Dedicated thread pool
Create a separate thread pool for PTY readers, independent from Tokio's runtime.

**Pros:** Full control over thread count
**Cons:** More complex, manual thread management
**Effort:** Medium
**Risk:** Low

## Acceptance Criteria

- [ ] PTY reads do not block Tokio worker threads
- [ ] Server remains responsive with 10+ active sessions
- [ ] All existing tests pass

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
