---
status: pending
priority: p1
issue_id: "020"
tags: [code-review, security]
dependencies: []
---

# HTML Injection in OAuth Callback Response

## Problem Statement

The `send_http_response` function in `login.rs` interpolates the `body` parameter directly into HTML without escaping. The `error_description` query parameter from the OAuth callback is URL-decoded and passed into this function. An attacker who can craft a request to the localhost callback port during the 120-second window could inject `<script>` tags, achieving reflected XSS.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:311-327` — `send_http_response` uses `format!("<h2>{body}</h2>")` with no HTML escaping
- **File:** `crates/agentbook-cli/src/login.rs:429` — `error_description` from query params passed directly to `send_http_response`
- **Source:** security-sentinel agent

## Proposed Solutions

### Solution A: Add html_escape() helper (Recommended)
- Add a simple `html_escape()` function that replaces `&`, `<`, `>`, `"`, `'`
- Apply to all `send_http_response` calls
- **Pros:** Simple, self-contained, no new dependency
- **Cons:** None
- **Effort:** Small
- **Risk:** Low

### Solution B: Use a templating crate
- Use `askama` or similar for HTML rendering
- **Pros:** More robust
- **Cons:** Overkill for 2 HTML responses
- **Effort:** Medium
- **Risk:** Low

## Recommended Action

Solution A — add `html_escape()` helper

## Technical Details

- **Affected files:** `crates/agentbook-cli/src/login.rs`
- **Components:** OAuth callback HTTP server

## Acceptance Criteria

- [ ] All user-controlled content passed to `send_http_response` is HTML-escaped
- [ ] `<script>` tags in error_description are rendered as text, not executed
- [ ] Test verifying HTML escaping

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |

## Resources

- PR branch: worktree-arda-gateway-sidekick
- OWASP XSS Prevention: https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Scripting_Prevention_Cheat_Sheet.html
