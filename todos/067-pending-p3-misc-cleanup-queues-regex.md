---
status: done
priority: p3
issue_id: "067"
tags: [code-review, performance, cleanup, data-structures]
dependencies: []
---

# Misc Cleanup: VecDeque for Queues, Cap chat_queue, ReDoS Guard

## Problem Statement

Several minor cleanup items from the review: (1) `chat_queue` and `pending` use `Vec::remove(0)` which is O(n) — should use VecDeque. (2) `chat_queue` has no size cap, allowing unbounded growth. (3) `grep_terminal` tool accepts arbitrary regex, risking ReDoS from catastrophic backtracking. (4) Legacy single-tab request format in `normalizeRequest` and request-level tmux_socket/tmux_session plumbing are unused by the TUI caller.

## Findings

- **File:** `input.rs:1377` — `chat_queue.remove(0)` O(n)
- **File:** `main.rs:302` — `pending.remove(0)` O(n)
- **File:** `input.rs:1213-1219` — chat_queue unbounded
- **File:** `agent/scripts/pi-terminal-agent.mjs:584-597` — unguarded regex compilation
- **File:** `agent/scripts/pi-terminal-agent.mjs:83-113` — legacy request format + unused tmux fields
- **Source:** performance-oracle, security-sentinel, simplicity-reviewer

## Proposed Solutions

- Replace `Vec` with `VecDeque` for chat_queue and pending
- Cap chat_queue at 10 entries
- Add regex pattern length limit (e.g., 200 chars) or timeout wrapper
- Remove legacy single-tab format and request-level tmux_socket/tmux_session
- **Effort:** Small | **Risk:** None

## Acceptance Criteria

- [x] FIFO queues use VecDeque
- [x] chat_queue capped at reasonable size
- [x] grep_terminal rejects excessively long regex patterns

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | Minor cleanup items grouped together |
| 2026-03-10 | Implemented all three changes | VecDeque for chat_queue and pending, cap at 10, ReDoS guard at 200 chars |
