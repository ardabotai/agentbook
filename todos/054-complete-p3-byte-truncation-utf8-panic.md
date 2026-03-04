---
status: pending
priority: p3
issue_id: "054"
tags: [code-review, bug, tui-rendering]
dependencies: ["048"]
---

# Fix Byte-Based Truncation UTF-8 Panic in ui.rs

## Problem Statement

The truncate function in ui.rs uses `s.len()` (byte count) and byte slicing `&s[..max]`. If `s` contains multi-byte UTF-8 characters (e.g., emoji lock icons used in room tabs), this will panic at runtime by slicing mid-codepoint.

## Findings

- **File:** `ui.rs:953-959` — uses s.len() and &s[..max.saturating_sub(3)]
- **Note:** automation.rs version at line 802 correctly uses char counting
- **Source:** architecture-strategist

## Proposed Solutions

### Solution A: Replace with char-safe version (part of #048 consolidation)
- Use chars().count() and chars().take(max) pattern
- Or consolidate all truncate variants into one shared function
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] truncate in ui.rs handles multi-byte UTF-8 without panic
- [ ] Lock icon emoji in room tab names display correctly when truncated

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI architecture review | Latent panic on emoji/Unicode input |
