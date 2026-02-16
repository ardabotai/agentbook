---
status: done
priority: p2
issue_id: "007"
tags: [code-review, performance]
dependencies: []
---

# Fix Unbounded Inbox Memory Growth and O(N) Ack Rewrite

## Problem Statement

`NodeInbox` stores ALL messages in a `Vec<InboxMessage>` in memory with no eviction or size limit. Every `ack` operation rewrites the entire JSONL file (O(N) for N total messages). At scale this causes unbounded memory growth and I/O.

## Findings

- **Performance Agent (CRITICAL-3):** 500 bytes/msg * 36.5K msgs/year = ~18 MB/year in RAM. Full file rewrite on every ack.

## Proposed Solutions

### Option A: Add Size Limit + Incremental Updates
- **Effort:** Medium
- Cap inbox at 10K messages, evict old acked messages
- Replace full JSONL rewrite with append-only + compaction
- Maintain running `unread_count` counter

### Option B: Replace with SQLite
- **Effort:** Medium-Large
- Indexed by message_id, efficient ack without full rewrite
- Better query patterns for filtering

## Acceptance Criteria

- [x] Inbox has configurable max size
- [x] Old acked messages are evicted when limit reached
- [x] Ack does not rewrite entire file
- [x] `unread_count()` is O(1)

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by performance review agent |
| 2026-02-16 | Implemented | Option A: size limit + ack journal + O(1) unread count. 7 tests passing. |
