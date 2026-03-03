---
status: pending
priority: p2
issue_id: "036"
tags: [code-review, performance]
dependencies: []
---

# has_arda_login() Should Use fs::metadata() Instead of read_to_string()

## Problem Statement

`has_arda_login()` reads the entire file content into a `String` just to check if the key exists and is non-empty. This is called during the `awaiting_api_key` poll (every 5 seconds) and on state transitions.

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:814-822` — uses `fs::read_to_string` for an existence check
- **Source:** performance-oracle

## Proposed Solutions

### Solution A: Use fs::metadata() for existence + non-zero size
- Replace `fs::read_to_string(path).ok().is_some_and(|s| !s.trim().is_empty())` with `fs::metadata(path).ok().is_some_and(|m| m.len() > 0)`
- Eliminates heap allocation and read syscall, uses single `stat()` syscall instead
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] `has_arda_login()` uses `fs::metadata()` instead of `fs::read_to_string()`

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review | - |
