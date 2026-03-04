---
status: pending
priority: p2
issue_id: "051"
tags: [code-review, performance, robustness]
dependencies: []
---

# Cap Unbounded Memory Growth in TUI State

## Problem Statement

Several data structures grow without bound over long-running sessions:
- `acked_ids: HashSet<String>` — never trimmed, grows monotonically
- `chat_history: Vec<SidekickMessage>` — never capped
- `room_messages: HashMap<String, Vec<InboxEntry>>` — 200 msgs per room, unlimited rooms

## Findings

- **File:** `main.rs:163` — acked_ids.insert() never pruned
- **File:** `app.rs:86` — chat_history grows without limit
- **File:** `main.rs:287` — room_messages 200 per room, no room count limit
- **Source:** performance-oracle, race-conditions-reviewer

## Proposed Solutions

### Solution A: Add caps and pruning
- Cap chat_history at 200, trim from front
- Prune acked_ids on inbox refresh (remove IDs not in current messages)
- Cap total room message memory or limit room count
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] chat_history capped at MAX_CHAT_HISTORY
- [ ] acked_ids pruned on inbox refresh
- [ ] No unbounded growth over multi-hour sessions

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI review | Prevents memory leak in long sessions |
