---
status: done
priority: p2
issue_id: "061"
tags: [code-review, security, command-injection, automation]
dependencies: []
---

# Validate AGENTBOOK_PI_AUTOMATION_CMD Against Shell Metacharacters

## Problem Statement

`resolve_pi_command()` reads `AGENTBOOK_PI_AUTOMATION_CMD` from the environment and passes it directly to `sh -c` with zero validation. While this requires env var control (local attack surface), combined with inference env vars (API keys) being passed to the child, it presents a command injection vector.

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:411-427` — unvalidated env var used as shell command
- **File:** `crates/agentbook-tui/src/automation.rs:706-711` — passed to `sh -c`
- **Related:** todo #045 (complete) addressed relative path anchoring but not env var validation
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Validate command path exists as a file
- Check that the resolved command path points to an existing file
- Reject commands containing shell metacharacters (`;`, `|`, `&`, backticks, `$(`)
- **Effort:** Small | **Risk:** Low

### Solution B: Use execFile instead of sh -c
- Split the command into program + args and use `Command::new(program).args(args)` directly
- Avoids shell interpretation entirely
- **Effort:** Medium | **Risk:** Low (may break legitimate space-in-path cases)

## Acceptance Criteria

- [x] Shell metacharacters in AGENTBOOK_PI_AUTOMATION_CMD are rejected
- [x] Legitimate commands (e.g., `node /path/to/script.mjs`) still work
- [x] Error message explains why command was rejected

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | Defense-in-depth for env-controlled command execution |
| 2026-03-10 | Implemented Solution A | Added `is_safe_shell_command()` validator + tests in `automation.rs` |
