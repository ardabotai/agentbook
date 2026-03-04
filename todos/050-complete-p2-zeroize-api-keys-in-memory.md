---
status: pending
priority: p2
issue_id: "050"
tags: [code-review, security, credential-handling]
dependencies: []
---

# Zeroize API Keys in Memory

## Problem Statement

API keys (Anthropic, Arda Gateway) are stored as plain `String` in `inference_env: Vec<(String, String)>` and intermediate variables. When dropped, Rust's String deallocator does not zero memory — key material persists in heap until OS reclaims the page. The `zeroize` crate is already in the dependency tree.

## Findings

- **File:** `app.rs:94` — inference_env holds keys as plain String
- **File:** `automation.rs:894-958` — load_inference_env_vars reads keys into String
- **File:** `app.rs:85` — chat_input holds pasted API key in cleartext
- **Source:** security-sentinel
- **Known Pattern:** docs/solutions/security-issues/oauth-credential-handling-rust-tui.md

## Proposed Solutions

### Solution A: Use zeroize::Zeroizing<String> for credential fields
- Wrap inference_env values with Zeroizing
- Wrap chat_input when in awaiting_api_key mode
- Wrap intermediate String vars in load_inference_env_vars
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] API key strings are zeroized on drop
- [ ] chat_input zeroized when leaving API key entry mode
- [ ] Intermediate credential strings in load functions use Zeroizing

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI security review | zeroize crate already available |
