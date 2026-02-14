---
status: complete
priority: p3
issue_id: "026"
tags: [code-review, quality, sandbox]
dependencies: []
---

# Consolidate Integration Test Boilerplate

## Problem Statement

The `sandboxed_session_config` helper exists but is only used for 2 of 5 tests. The nesting tests manually construct `SessionCreateConfig` with 8 fields, repeating the same `/bin/sh`, `-c`, `cols: 80, rows: 24` boilerplate (~80 LOC of repetition).

Also, `wait_for_exit` uses a thread spawn + busy-poll pattern that could be simplified to a direct `child.wait()` call, and should return a boolean indicating timeout vs success.

## Findings

- **Source**: Code Simplicity Reviewer, Performance Oracle
- **File**: `crates/libtmax/tests/sandbox_integration.rs`

## Proposed Solutions

Extend the helper to accept optional sandbox and parent_id. Simplify `wait_for_exit`.

- Effort: Small
- Risk: None

## Acceptance Criteria

- [ ] Single helper function used by all 5 tests
- [ ] ~80 LOC reduction
- [ ] `wait_for_exit` returns timeout status

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | |
