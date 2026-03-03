---
status: pending
priority: p1
issue_id: "032"
tags: [code-review, security]
dependencies: []
---

# Validate Gateway URL from Environment Variable

## Problem Statement

When `AGENTBOOK_GATEWAY_API_KEY` is set in the environment, `load_inference_env_vars()` reads `AGENTBOOK_GATEWAY_URL` and passes it directly to child processes without calling `is_valid_gateway_url()`. The validation function exists and is correctly used for the disk-based path, but the env-var path bypasses it entirely. A malicious or misconfigured env var could send the API key over HTTP or to an attacker-controlled endpoint.

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:880-891` — env-var path has no URL validation
- **Source:** security-sentinel agent
- **Note:** The disk-based path at line 903 correctly validates. Only the env-var fast-path is missing validation.

## Proposed Solutions

### Solution A: Apply is_valid_gateway_url() to env-sourced URL
- Add validation before returning env vars: if invalid, log warning and fall through to next priority tier
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] `is_valid_gateway_url()` is called on env-sourced `AGENTBOOK_GATEWAY_URL`
- [ ] Invalid env URL logs a warning and falls through gracefully

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from second code review | Env-var path added during fix cycle missed validation |
