---
status: deferred
priority: p3
issue_id: "040"
tags: [code-review, architecture, agent-native]
dependencies: []
---

# Add Protocol-Level Login/Logout for Agent Self-Service

## Problem Statement

Login and Logout commands live exclusively in the CLI and are not exposed as `Request` variants in the Unix socket protocol. The standalone TypeScript agent and any future programmatic clients cannot trigger login or logout. This limits agent autonomy — an agent that discovers it has no API key cannot self-authenticate.

## Findings

- **File:** `crates/agentbook-cli/src/main.rs:206-211` — Login/Logout only in CLI dispatch
- **File:** `crates/agentbook/src/protocol.rs` — no Login/Logout request variants
- **Source:** agent-native-reviewer
- **Note:** The `--token` flag provides a headless path, but only from the CLI. Since login is a client-side filesystem operation (not daemon-dependent), it could be exposed as a shared library function.

## Proposed Solutions

### Solution A: Add Request::Login/Logout to socket protocol
- Node daemon handler delegates to `store_key`/`cmd_logout` logic
- For browser OAuth, return the auth URL to the caller
- **Effort:** Medium | **Risk:** Low

### Solution B: Expose login as a shared library function
- Move `store_key`/`cmd_logout` to `agentbook::gateway` crate
- Both CLI and agent can call directly without daemon involvement
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Agents can programmatically store/delete Arda credentials
- [ ] At least the `--token` equivalent is available via socket or library

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review, deferred | Requires protocol extension planning |
