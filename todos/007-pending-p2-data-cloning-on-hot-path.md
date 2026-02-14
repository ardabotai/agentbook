---
status: pending
priority: p2
issue_id: "007"
tags: [code-review, performance]
dependencies: []
---

# Data Cloning on Hot Path (record_output)

## Problem Statement

Every output chunk is cloned: once for LiveBuffer, the Event gets cloned again per broadcast subscriber. `session_id.clone()` allocates a new String on every event. At 10MB/s aggregate output, this produces ~40MB/s of clone overhead.

**Flagged by:** performance-oracle

## Findings

- **File:** `crates/libtmax/src/session.rs` lines 243-265
- `data.clone()` on every output chunk for LiveBuffer
- `session_id.clone()` (36-byte String) on every event
- Broadcast channel clones Event per receiver

## Proposed Solutions

### Option A: Use `bytes::Bytes` and `Arc<str>` (Recommended)
Replace `Vec<u8>` with `Bytes` for zero-copy reference-counted sharing. Use `Arc<str>` for SessionId.

**Pros:** Reduces allocator pressure ~4x
**Cons:** Adds `bytes` crate dependency, API changes
**Effort:** Medium
**Risk:** Low

## Acceptance Criteria

- [ ] Output data uses `Bytes` instead of `Vec<u8>`
- [ ] SessionId uses `Arc<str>` or similar zero-copy type
- [ ] No unnecessary heap allocations on hot path

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
