---
status: pending
priority: p3
issue_id: "039"
tags: [code-review, security]
dependencies: []
---

# Document --token Flag Visibility in Process Listing

## Problem Statement

`agentbook login --token gw_sk_SECRET` exposes the API key in process listings (`ps aux`) and shell history. This is a standard trade-off for CLI tools but should be documented.

## Findings

- **File:** `crates/agentbook-cli/src/main.rs:837` — `--token` as command-line argument
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Add warning to help text
- Add `(visible in process listing and shell history)` to the `--arg` help string
- **Effort:** Small | **Risk:** Low

### Solution B: Add --token-stdin option (future)
- Read token from stdin: `echo gw_sk_xxx | agentbook login --token-stdin`
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Help text warns about process listing visibility

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review | Standard concern for CLI tools with secret args |
