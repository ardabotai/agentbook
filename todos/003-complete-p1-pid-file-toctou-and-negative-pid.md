---
status: pending
priority: p1
issue_id: "003"
tags: [code-review, security]
dependencies: []
---

# PID File TOCTOU Race and Unvalidated Kill

## Problem Statement

`server_stop` reads a PID from a file and sends SIGTERM without verifying the process is tmax. A negative PID sends the signal to an entire process group. PID 0 signals the caller's process group. Combined with PID recycling, this can kill arbitrary processes.

**Flagged by:** security-sentinel

## Findings

- **File:** `crates/tmax-cli/src/commands.rs` lines 28-43
- PID parsed as `i32` with no validation (negative values, zero)
- No verification that the PID belongs to a tmax process
- PID file in user-writable directories

## Proposed Solutions

### Option A: Validate PID and send graceful shutdown via socket (Recommended)
Validate PID > 1, and prefer sending a shutdown command over the Unix socket instead of raw `kill`.

**Pros:** Eliminates entire class of TOCTOU bugs
**Cons:** Requires adding a Shutdown request to the protocol
**Effort:** Medium
**Risk:** Low

### Option B: Validate PID and verify process name
Check PID > 1 and verify via `/proc/{pid}/cmdline` or equivalent.

**Pros:** Quick fix
**Cons:** Platform-specific, still has TOCTOU window
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] PID validated as positive integer > 1
- [ ] Process identity verified or shutdown sent via socket
- [ ] Cannot kill arbitrary processes via tampered PID file

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
