---
status: complete
priority: p2
issue_id: "023"
tags: [code-review, security]
dependencies: []
---

# Gateway URL File Lacks Restrictive Permissions

## Problem Statement

`store_gateway_url()` uses `std::fs::write` (default umask ~0o644) while the API key file correctly uses `mode(0o600)`. A local attacker who can modify the gateway URL file could redirect inference traffic to a malicious endpoint, capturing the API key from request headers.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:393-397` — uses `std::fs::write` without permission setting
- **Source:** security-sentinel agent
- **Note:** The simplicity reviewer also found this file is unnecessary (writes a hardcoded constant). If todo #025 is resolved first (eliminate gateway URL file), this finding is moot.

## Proposed Solutions

### Solution A: Apply 0o600 permissions (if keeping file)
- Use `OpenOptions::new().mode(0o600)` like the API key file
- **Effort:** Small | **Risk:** Low

### Solution B: Eliminate the file entirely (see todo #025)
- The file always contains the same hardcoded constant
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Gateway URL file has 0o600 permissions OR is eliminated entirely

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |
