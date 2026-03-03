---
status: pending
priority: p2
issue_id: "028"
tags: [code-review, security]
dependencies: []
---

# Unsafe set_var in Multi-Threaded TUI Context

## Problem Statement

`maybe_load_inference_env()` uses `unsafe { std::env::set_var(...) }` to mutate the process environment, which is not thread-safe. The TUI uses multiple threads (crossterm events, sidekick automation, PTY). While the SAFETY comments exist, the invariant (no concurrent reads) is not enforced.

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:884-895` — unsafe set_var calls
- **Source:** security-sentinel agent

## Proposed Solutions

### Solution A: Pass env vars via Command::env() at spawn time (Recommended)
- Store credentials in a struct/field instead of process env
- Pass them to child process via `Command::env("KEY", value)` when spawning pi-terminal-agent
- **Pros:** No unsafe, no global mutation, clean separation
- **Cons:** Requires plumbing credentials through to spawn callsite
- **Effort:** Medium | **Risk:** Low

### Solution B: Use a Mutex<HashMap> for credential storage
- Thread-safe storage, read at spawn time
- **Effort:** Medium | **Risk:** Low

## Acceptance Criteria

- [ ] No `unsafe { std::env::set_var(...) }` calls in automation.rs
- [ ] Credentials passed via child process builder
- [ ] All tests pass

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |
