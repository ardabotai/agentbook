---
status: pending
priority: p1
issue_id: "041"
tags: [code-review, architecture, race-condition, robustness]
dependencies: []
---

# Fix Pending Request Queue FIFO Desync

## Problem Statement

The `pending: Vec<PendingRequest>` in main.rs assumes strict FIFO ordering — the daemon answers requests in exactly the order they were sent. Events, auto-ack batches, and cascading requests (e.g., ListRooms triggers N RoomInbox requests) can desynchronize the queue, causing responses to be matched against the wrong PendingRequest. Silent data drops occur because `serde_json::from_value` returns Err and the data is discarded.

## Findings

- **File:** `main.rs:131-136, 300-303` — Vec used as FIFO, `pending.remove(0)` to dequeue
- **File:** `main.rs:281-297` — Event handler sends new requests mid-queue
- **File:** `main.rs:156-171` — Auto-ack pushes N entries per draw cycle
- **File:** `main.rs:500-509` — ListRooms cascade pushes N RoomInbox entries
- **Source:** race-conditions-reviewer, architecture-strategist

## Proposed Solutions

### Solution A: Add request_id to protocol
- Add monotonic counter, daemon echoes it back, match by ID
- **Effort:** Medium (protocol change) | **Risk:** Low

### Solution B: HashMap<u64, PendingRequest> with sequence numbers
- TUI-side only, no protocol change, just tag each request with a sequence
- **Effort:** Small | **Risk:** Low (but still relies on daemon ordering for correctness)

## Acceptance Criteria

- [ ] Responses matched to correct request type even under concurrent sends
- [ ] Event interleaving does not cause queue drift
- [ ] Auto-ack batches do not misalign subsequent responses

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI robustness review | Most dangerous structural flaw |
