---
status: pending
priority: p3
issue_id: "057"
tags: [code-review, security, ui]
dependencies: []
---

# Mask OTP and Passphrase in Input Bar

## Problem Statement

When users type `/send-eth <to> <amount> <otp>` or `/join room --passphrase secret`, the OTP and passphrase are visible in cleartext in the input bar. The Sidekick API key input already masks with `*` characters.

## Findings

- **File:** `input.rs:304-337` — OTP parsed from plaintext input
- **File:** `input.rs:185-188` — passphrase parsed from plaintext input
- **File:** `ui.rs:717` — API key input already masked with *
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Detect sensitive commands and mask portions
- When input starts with `/send-eth` or `/send-usdc`, mask the OTP portion
- When input contains `--passphrase`, mask the passphrase value
- **Effort:** Medium | **Risk:** Low

## Acceptance Criteria

- [ ] OTP portion masked while typing /send-* commands
- [ ] Passphrase masked while typing /join --passphrase

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI security review | Shoulder-surfing prevention |
