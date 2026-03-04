---
status: pending
priority: p2
issue_id: "047"
tags: [code-review, architecture, refactor]
dependencies: []
---

# Extract Sub-State Structs From App God Struct

## Problem Statement

The `App` struct has 36 public fields spanning 5 concerns: tab/navigation, messaging, terminal/pane, room, and sidekick/automation state. All fields are `pub`, allowing any module to mutate any field without invariant checks. AutoAgentState (18 fields) partially groups sidekick state but still mixes chat UI, credentials, and timing.

## Findings

- **File:** `app.rs:146-206` — App struct with 36 pub fields
- **File:** `app.rs:73-98` — AutoAgentState with 18 fields mixing concerns
- **Source:** architecture-strategist

## Proposed Solutions

### Solution A: Extract sub-state structs with encapsulated methods
- `NavigationState` (tab, prefix mode, activity indicators)
- `MessagingState` (messages, following, contacts, acked IDs)
- `TerminalState` (terminals, split, window tabs, waiting inputs)
- `RoomState` (rooms, room_messages, secure_rooms)
- Split AutoAgentState further into SidekickAuth, SidekickChat, SidekickTiming
- Add methods like `AutoAgentState::reset()`, `resume_after_auth()`
- **Effort:** Large | **Risk:** Medium (wide-reaching refactor)

## Acceptance Criteria

- [ ] App fields organized into sub-state structs
- [ ] Key invariants enforced via methods (not raw field access)
- [ ] AutoAgentState::reset() consolidates the 3 duplicated reset blocks
- [ ] All tests pass

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI architecture review | Foundation for maintainability |
