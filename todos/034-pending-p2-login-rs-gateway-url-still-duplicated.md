---
status: pending
priority: p2
issue_id: "034"
tags: [code-review, architecture]
dependencies: ["026"]
---

# login.rs Still Has Duplicate ARDA_GATEWAY_URL Constant

## Problem Statement

The centralization commit (9a7392d) moved `ARDA_KEY_FILE` to the shared `agentbook::gateway` module, but `login.rs` still defines its own `ARDA_GATEWAY_URL` constant at line 25, duplicating the value from `ARDA_DEFAULT_GATEWAY_URL` in `gateway.rs`. This contradicts the centralization goal and creates drift risk.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:25` — `const ARDA_GATEWAY_URL: &str = "https://bot.ardabot.ai";`
- **File:** `crates/agentbook/src/gateway.rs:5` — `pub const ARDA_DEFAULT_GATEWAY_URL: &str = "https://bot.ardabot.ai";`
- **Source:** architecture-strategist, code-simplicity-reviewer

## Proposed Solutions

### Solution A: Import from shared crate and derive auth URL
- Replace `ARDA_GATEWAY_URL` with `use agentbook::gateway::ARDA_DEFAULT_GATEWAY_URL`
- Derive `ARDA_AUTH_PAGE_URL` at runtime: `format!("{ARDA_DEFAULT_GATEWAY_URL}/connect")`
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] `login.rs` imports gateway URL from `agentbook::gateway` instead of defining its own
- [ ] No duplicate gateway URL constants remain across CLI and shared crate

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review | Centralization commit missed this constant |
