---
status: pending
priority: p2
issue_id: "009"
tags: [code-review, bug, architecture]
dependencies: []
---

# Unsubscribe Does Not Cancel forward_events Task

## Problem Statement

`Unsubscribe` removes the session from `client_state.subscriptions` but does NOT stop the spawned `forward_events` task. No `JoinHandle` is stored and no cancellation token exists. Unsubscribe is effectively a no-op at runtime. Additionally, subscribing twice spawns duplicate tasks causing duplicate events.

**Flagged by:** architecture-strategist

## Findings

- **File:** `crates/tmax-server/src/connection.rs` lines 341-357
- `forward_events` task spawned with no stored handle
- No `CancellationToken` passed to the task
- Subscribing twice to same session spawns two forwarding tasks

## Proposed Solutions

### Option A: CancellationToken per subscription (Recommended)
Store a `tokio_util::sync::CancellationToken` per subscription, cancel it on unsubscribe.

**Pros:** Clean lifecycle management, prevents duplicates
**Cons:** Adds `tokio-util` dependency (already in tmax-server)
**Effort:** Medium
**Risk:** Low

## Acceptance Criteria

- [ ] Unsubscribe actually stops the forwarding task
- [ ] Cannot create duplicate subscriptions for same session
- [ ] Clean shutdown on client disconnect

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
