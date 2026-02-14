---
status: pending
priority: p3
issue_id: "015"
tags: [code-review, security, performance]
dependencies: []
---

# No Limits on Sessions, Connections, or Message Size

## Problem Statement

No upper bound on sessions, connections, or subscriptions. Server reads lines with no size limit -- a single client can exhaust memory with an extremely long line. No backpressure on PTY output.

**Flagged by:** security-sentinel, performance-oracle

## Findings

- `crates/tmax-server/src/server.rs` lines 49-56 -- unbounded accept loop
- `crates/tmax-server/src/connection.rs` line 34 -- `reader.lines()` with no size limit
- `crates/libtmax/src/session.rs` -- HashMap with no capacity check

## Proposed Solutions

### Option A: Add configurable limits (Recommended)
Add `max_sessions`, `max_connections`, max line length to `ServerConfig`.

**Pros:** Prevents resource exhaustion
**Cons:** Configuration complexity
**Effort:** Medium
**Risk:** Low

## Acceptance Criteria

- [ ] Configurable session and connection limits
- [ ] Maximum message size enforced
- [ ] Limits documented in config

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
