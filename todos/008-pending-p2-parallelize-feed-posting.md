---
status: done
priority: p2
issue_id: "008"
tags: [code-review, performance]
dependencies: []
---

# Fix O(N) Sequential Feed Post Broadcasting

## Problem Statement

`handle_post_feed` holds the `follow_store` mutex for the entire duration of sending to all followers sequentially. At 1000 followers, this is 1000 sequential sends while blocking all follow/unfollow operations.

## Findings

- **Performance Agent (CRITICAL-4):** Lock held across all sends. If any relay channel is full (256 buffer), the entire operation stalls.

## Proposed Solutions

### Clone Follower List, Release Lock, Send in Parallel
- **Effort:** Small
- Clone the follower list, drop the mutex, then use `futures::future::join_all` for concurrent sends.

## Acceptance Criteria

- [x] Follow store lock released before any network I/O
- [x] Sends happen concurrently
- [x] Failures for individual followers don't block others

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by performance review agent |
| 2026-02-16 | Resolved | Parallelized feed posting in handler/messaging.rs using futures_util::future::join_all |
