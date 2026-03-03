---
status: pending
priority: p2
issue_id: "027"
tags: [code-review, security]
dependencies: ["020"]
---

# Add Security Headers to OAuth Callback HTTP Response

## Problem Statement

The localhost callback server's HTTP responses lack security headers (CSP, X-Frame-Options, X-Content-Type-Options, Cache-Control). While the server is short-lived and localhost-bound, these headers provide defense-in-depth against Finding #020 (XSS) and prevent browser caching of the auth page.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:316-325` — raw HTTP response without security headers
- **Source:** security-sentinel agent

## Proposed Solutions

### Solution A: Add headers to send_http_response (Recommended)
- Add `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Content-Security-Policy: default-src 'none'`, `Cache-Control: no-store`
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] All 4 security headers present in HTTP responses
- [ ] CSP blocks inline script execution

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |
