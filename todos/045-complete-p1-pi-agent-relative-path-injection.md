---
status: pending
priority: p1
issue_id: "045"
tags: [code-review, security, command-injection]
dependencies: []
---

# Anchor PI Agent Script Path to Prevent Injection

## Problem Statement

The PI automation command fallback uses a relative path (`agent/scripts/pi-terminal-agent.mjs`). If an attacker can control the current working directory and place a malicious file at that path, the TUI will auto-discover and execute it via `sh -lc`. Additionally, `sh -lc` loads the user's login profile which could introduce unexpected behavior.

## Findings

- **File:** `automation.rs:397-409` — Relative path fallback: `Path::new("agent/scripts/pi-terminal-agent.mjs")`
- **File:** `automation.rs:491-494, 701-704` — Command passed to `sh -lc` unsanitized
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Anchor to executable directory
- Resolve path relative to `std::env::current_exe()` parent
- **Effort:** Small | **Risk:** Low

### Solution B: Use sh -c instead of sh -lc
- Avoids loading shell profile; combine with absolute path
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] PI agent script path is resolved as absolute, not relative to CWD
- [ ] sh -lc replaced with sh -c to avoid profile loading
- [ ] AGENTBOOK_PI_AUTOMATION_CMD from env var still works as override

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI security review | Supply chain concern via CWD manipulation |
