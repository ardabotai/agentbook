---
status: complete
priority: p3
issue_id: "014"
tags: [code-review, quality]
dependencies: []
---

# Deduplicate CLI Error Handling Pattern

## Problem Statement

The same `match Response { Ok => ..., Error => eprintln + exit(1), _ => {} }` pattern appears 10 times in CLI commands. The `_ => {}` silently swallows unexpected `Event` responses.

**Flagged by:** code-simplicity-reviewer

## Findings

- **File:** `crates/tmax-cli/src/commands.rs` -- 10 occurrences
- ~40 lines of boilerplate

## Proposed Solutions

### Option A: Extract helper function (Recommended)
Create `unwrap_response(resp: Response) -> anyhow::Result<Option<serde_json::Value>>`.

**Pros:** -40 lines, explicit handling of unexpected variants
**Cons:** None
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] Helper function extracts common pattern
- [ ] All commands use the helper
- [ ] Unexpected response variants handled explicitly

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
