---
status: pending
priority: p2
issue_id: "024"
tags: [code-review, security]
dependencies: []
---

# Gateway URL Not Validated After Reading from Disk

## Problem Statement

The gateway URL is read from a file and used to construct API endpoints. No validation checks that it uses HTTPS, points to a trusted domain, or lacks control characters. If the file is tampered with, the API key would be sent to an attacker's server.

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:881-882` — reads gateway URL from disk, no validation
- **File:** `agent/scripts/pi-terminal-agent.mjs:48` — same pattern in JS
- **Source:** security-sentinel agent

## Proposed Solutions

### Solution A: Add validate_gateway_url() (Recommended)
- Validate HTTPS scheme, no control characters, optional domain allowlist
- Apply in both Rust (automation.rs) and JS (pi-terminal-agent.mjs)
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Gateway URL validated for HTTPS scheme
- [ ] Control characters rejected
- [ ] Test for validation logic

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |
