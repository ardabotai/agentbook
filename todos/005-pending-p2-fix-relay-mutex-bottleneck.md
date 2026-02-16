---
status: done
priority: p2
issue_id: "005"
tags: [code-review, performance]
dependencies: []
---

# Fix Single Global Mutex Bottleneck on Relay Host

## Problem Statement

The entire `Router` (HashMap of senders + SQLite username directory) is behind a single `Arc<Mutex<Router>>`. Every relay message, lookup, and registration must acquire the same lock. At 1000+ nodes this serializes all operations. SQLite writes inside the async mutex block tokio worker threads.

## Findings

- **Performance Agent (CRITICAL-1):** At 1000 nodes with 10 msg/sec each, 10K lock acquisitions/sec on one mutex.
- **Performance Agent (CRITICAL-2):** Synchronous SQLite inside async mutex blocks tokio workers.

## Proposed Solutions

### Option A: Split into Separate Locks
- **Effort:** Medium
- Use `DashMap` or `RwLock<HashMap>` for senders (relay only needs read access)
- Move `UsernameDirectory` to its own Mutex with `spawn_blocking` for SQLite ops
- Keep rate limiters separate (already are for register/lookup)

## Acceptance Criteria

- [x] Senders map uses concurrent data structure (DashMap or RwLock)
- [x] SQLite operations wrapped in `spawn_blocking`
- [x] Relay message forwarding doesn't block on username operations
- [x] Existing tests pass

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by performance review agent |
| 2026-02-16 | Completed | Replaced global Mutex with DashMap + spawn_blocking for SQLite |
