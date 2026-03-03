---
status: pending
priority: p1
issue_id: "022"
tags: [code-review, agent-native]
dependencies: []
---

# Add Non-Interactive Login Path (--token flag)

## Problem Statement

The `agentbook login` command requires a browser for the OAuth flow. Agents, CI systems, and headless environments cannot authenticate. This violates agent-native parity — the user can authenticate but an agent cannot. Same pattern as `gh auth login --with-token`.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:43-98` — `cmd_login()` binds TCP listener and opens browser, both fail in headless
- **Source:** agent-native-reviewer agent

## Proposed Solutions

### Solution A: Add --token flag (Recommended)
- Add `--token <KEY>` flag to the Login command
- When provided, validate key format (`gw_sk_` prefix), write to state_dir, skip browser flow
- Example: `agentbook login --token gw_sk_abc123`
- **Pros:** Simple, follows `gh` convention, enables CI/agent auth
- **Cons:** None
- **Effort:** Small
- **Risk:** Low

### Solution B: Also accept stdin pipe
- `echo "gw_sk_abc123" | agentbook login --with-token`
- **Pros:** Works with secret managers
- **Cons:** Slightly more complex
- **Effort:** Small
- **Risk:** Low

## Recommended Action

Solution A, optionally with Solution B as well

## Technical Details

- **Affected files:** `crates/agentbook-cli/src/login.rs`, `crates/agentbook-cli/src/main.rs`

## Acceptance Criteria

- [ ] `agentbook login --token gw_sk_xxx` writes key to state_dir without browser
- [ ] Key format validated (gw_sk_ prefix)
- [ ] Error message if key format invalid
- [ ] Works in headless/CI environments

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |

## Resources

- PR branch: worktree-arda-gateway-sidekick
- `gh auth login --with-token` pattern
