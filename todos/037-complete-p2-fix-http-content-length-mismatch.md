---
status: pending
priority: p2
issue_id: "037"
tags: [code-review, security]
dependencies: []
---

# Fix HTTP Content-Length Mismatch in Callback Response

## Problem Statement

The `send_http_response` format string in `login.rs` has extra whitespace between the headers and body due to line continuation indentation. `Content-Length` is calculated from `html.len()` but the actual body sent includes leading spaces from the `format!` indentation.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:323-334` — format string with `\` continuations adds whitespace before `{html}`
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Fix the format string to eliminate extra whitespace
- Ensure `\r\n\r\n` is immediately followed by `{html}` with no extra spaces
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Content-Length header matches actual body length
- [ ] No extra whitespace between HTTP headers and body

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review | - |
