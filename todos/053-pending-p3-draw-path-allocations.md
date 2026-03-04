---
status: pending
priority: p3
issue_id: "053"
tags: [code-review, performance, tui-rendering]
dependencies: []
---

# Reduce Draw Path Allocations

## Problem Statement

Several draw functions perform unnecessary allocations per frame at 60fps: visible_messages() called 2x per frame (O(n) scan each), display_name() clones strings, scroll_key() allocates String for HashMap lookup, unread count O(n) scan in status bar, draw_input allocates title from static literals.

## Findings

- **File:** `ui.rs:482,537,806` + `main.rs:157` — visible_messages called 2x per frame
- **File:** `ui.rs:944-951` — display_name clones String
- **File:** `app.rs:328-341` — scroll_key allocates String every query
- **File:** `ui.rs:861` — unread count iterates all messages per frame
- **File:** `ui.rs:840-858` — draw_input allocates title String from static literals
- **Source:** performance-oracle

## Proposed Solutions

### Solution A: Cache and use Cow<str>
- Cache visible_messages result, compute once before draw+ack
- Use Cow<str> for display_name, truncate return types
- Use enum ScrollKey instead of String
- Maintain cached unread_count, update only on message changes
- Use Cow::Borrowed for static title strings
- **Effort:** Medium | **Risk:** Low

## Acceptance Criteria

- [ ] visible_messages computed once per frame cycle
- [ ] String allocations eliminated where Cow or caching works
- [ ] Unread count cached, not recomputed every frame

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI performance review | Incremental improvements |
