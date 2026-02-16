---
status: done
priority: p3
issue_id: "017"
tags: [code-review, performance]
dependencies: []
---

# Fix Memory Leaks in Relay Host

## Problem Statement

Two slow memory leaks in the relay host:
1. `observed_endpoints` HashMap never cleaned up on `unregister()` -- disconnected nodes leave stale endpoint data.
2. Mesh rate limiter (`agentbook-mesh/src/rate_limit.rs`) has no cleanup -- buckets accumulate forever.

## Findings

- **Performance Agent:** `unregister` removes from `senders` but not `observed_endpoints`. Mesh rate limiter has no periodic cleanup (host version does).

## Proposed Solutions

### Clean Up on Disconnect + Add Periodic Cleanup
- **Effort:** Small
- Remove from `observed_endpoints` in `unregister()`
- Add `cleanup()` method to mesh rate limiter (or consolidate with host version per todo 015)

## Acceptance Criteria

- [x] `unregister()` cleans up observed_endpoints
- [x] Rate limiter buckets cleaned periodically
- [x] Tests verify cleanup behavior

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by performance review agent |
| 2026-02-16 | Completed | Fixed both memory leaks, added tests |
