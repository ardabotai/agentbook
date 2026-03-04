---
status: pending
priority: p3
issue_id: "058"
tags: [code-review, security, robustness]
dependencies: []
---

# Add Input Length Limits for Feed Posts and DMs

## Problem Statement

Room messages enforce a 140-character limit, but feed posts and DMs have no length limit in the TUI. Users could paste very large strings causing memory growth and expensive encryption operations (feed posts are encrypted per-follower).

## Findings

- **File:** `input.rs:514-553` — Room has 140-char check; Feed and DMs have none
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Add reasonable max length
- Add MAX_FEED_LENGTH and MAX_DM_LENGTH constants (e.g., 10,000 chars)
- Check before sending, show error in status bar
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Feed posts and DMs reject input over max length
- [ ] User sees clear error message when limit exceeded

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI security review | Bounded by 64 KiB protocol max but TUI should enforce earlier |
