---
status: pending
priority: p2
issue_id: "004"
tags: [code-review, performance, architecture]
dependencies: []
---

# Global Mutex Contention -- All Operations Serialize Through One Lock

## Problem Statement

`SharedState = Arc<Mutex<SessionManager>>` serializes ALL operations (PTY output, client requests, broadcasts) through a single lock. Every 4KB PTY read acquires this lock. At 10+ sessions, this becomes the throughput ceiling.

**Flagged by:** performance-oracle, architecture-strategist

## Findings

- **File:** `crates/tmax-server/src/server.rs` line 12
- **File:** `crates/tmax-server/src/connection.rs` lines 396-409
- Read-only queries (list, info, tree) contend with write operations
- Lock held during broadcast, blocking all other operations
- ABBA lock ordering risk between `client_state` and `state` in Attach vs Detach handlers

## Proposed Solutions

### Option A: Per-session locks with DashMap (Recommended)
Replace global mutex with `DashMap<SessionId, Arc<Mutex<Session>>>` or similar per-session locking.

**Pros:** Unlocks parallelism, different sessions don't contend
**Cons:** More complex, requires decomposing SessionManager
**Effort:** Large
**Risk:** Medium

### Option B: RwLock for read-heavy operations
Replace `Mutex` with `RwLock` to allow concurrent readers.

**Pros:** Simple change, helps read-heavy workloads
**Cons:** Doesn't solve write contention on hot path
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] Operations on different sessions don't contend
- [ ] Read-only queries don't block write operations
- [ ] No ABBA lock ordering risk

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
