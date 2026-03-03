---
status: pending
priority: p2
issue_id: "025"
tags: [code-review, quality]
dependencies: []
---

# Delete Dead Code and Unnecessary Gateway URL File

## Problem Statement

Several public APIs have zero callers (YAGNI), and the gateway URL file mechanism always writes a hardcoded constant that is also the fallback default — making the file redundant.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:401-451` — `InferenceConfig` enum + `load_inference_config()` — zero callers (~50 lines)
- **File:** `crates/agentbook-tui/src/automation.rs:779-808` — `has_any_inference_key()` — zero callers (~29 lines)
- **File:** `crates/agentbook-cli/src/login.rs:393-397` — `store_gateway_url()` writes hardcoded constant
- **File:** `crates/agentbook-cli/src/login.rs:128-130, 192-193` — redundant `set_nonblocking(false)` calls
- **Source:** code-simplicity-reviewer agent
- **Estimated LOC reduction:** ~114 lines

## Proposed Solutions

### Solution A: Delete all dead code in one pass (Recommended)
1. Delete `InferenceConfig`, `load_inference_config()` from login.rs
2. Delete `has_any_inference_key()` from automation.rs
3. Delete `store_gateway_url()`, `ARDA_GATEWAY_URL_FILE` from login.rs
4. Remove `store_gateway_url(&state_dir)` call and URL file cleanup in `cmd_logout()`
5. Remove `ARDA_GATEWAY_URL_FILE` from automation.rs, always use `ARDA_DEFAULT_GATEWAY_URL`
6. Remove redundant `set_nonblocking(false)` calls
7. Remove belt-and-suspenders `ANTHROPIC_BASE_URL` env var from pi-terminal-agent.mjs
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] No dead `pub` functions with zero callers
- [ ] Gateway URL always uses hardcoded constant
- [ ] All tests pass after cleanup
- [ ] ~100+ lines removed

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |
