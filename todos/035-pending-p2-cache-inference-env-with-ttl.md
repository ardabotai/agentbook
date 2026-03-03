---
status: pending
priority: p2
issue_id: "035"
tags: [code-review, performance]
dependencies: []
---

# Cache load_inference_env_vars() Instead of Reloading Every Tick

## Problem Statement

`load_inference_env_vars()` is called on every PI-mode heartbeat tick (~6 seconds) and every chat interaction. Each call performs up to 4 filesystem reads (`fs::read_to_string` for key files) plus env var lookups — synchronous blocking I/O on the main TUI event loop thread. The credentials change extremely rarely (only on login/logout).

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:103` — reloaded every tick
- **File:** `crates/agentbook-tui/src/automation.rs:137` — reloaded every chat
- **File:** `crates/agentbook-tui/src/input.rs:1011` — reloaded on prompt submit
- **Source:** performance-oracle, architecture-strategist
- **Note:** The `inference_env` field already exists on `AutoAgentState` but is overwritten every time instead of being used as a cache.

## Proposed Solutions

### Solution A: Load once and refresh only on login/logout events
- Stop calling `load_inference_env_vars()` on every tick/chat
- Only refresh when: Arda login detected (awaiting_api_key transition), new API key saved, or explicit logout
- Add `last_env_load: Option<Instant>` with 30-second TTL as safety net
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] `load_inference_env_vars()` is NOT called on every PI tick
- [ ] Credentials are refreshed on login/logout state transitions
- [ ] A TTL-based fallback prevents stale credentials

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review | Env loading added in fix cycle without caching |
