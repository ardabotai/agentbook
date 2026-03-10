---
status: done
priority: p3
issue_id: "066"
tags: [code-review, observability, automation, diagnostics]
dependencies: []
---

# Capture Child Process stderr for Diagnostics

## Problem Statement

Both `run_command_with_stdin` and `run_pi_chat_stream_worker` set `.stderr(Stdio::null())`, silently discarding all error output from the pi-terminal-agent child process. Gateway URL validation warnings, Node.js crashes, and security-relevant errors are invisible. The login subprocess also discards its exit status.

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:529,712` — `.stderr(Stdio::null())`
- **File:** `crates/agentbook-tui/src/automation.rs:877` — login exit status discarded with `let _`
- **Source:** security-sentinel, architecture-strategist

## Proposed Solutions

### Solution A: Capture and log last N lines of stderr
- Capture stderr via `Stdio::piped()`, read last 5 lines, include in error messages
- For login subprocess, check exit status and push error chat message on failure
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [x] PI command stderr captured and included in error messages on failure
- [x] Login subprocess exit failure produces a user-visible error message

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | Silent stderr loss hides security-relevant diagnostics |
| 2026-03-10 | Implemented Solution A | Changed all 3 callsites from Stdio::null() to Stdio::piped(); last 3 stderr lines included in error messages |
