---
status: done
priority: p2
issue_id: "009"
tags: [code-review, architecture, quality]
dependencies: []
---

# Split handler.rs and Deduplicate Wallet Handlers

## Problem Statement

`handler.rs` at 933 lines is the largest file, containing 25+ handler functions with significant duplication. The 4 send handlers, 2 contract write handlers, and 2 sign handlers share >80% of their logic. The wallet `Mutex<Option<BaseWallet>>` lazy-init pattern is a manual reimplementation of `OnceCell`.

## Findings

- **Simplicity Agent:** Wallet handler deduplication would save ~120 lines.
- **Architecture Agent:** Recommends splitting into messaging/wallet/social sub-handlers.
- **Pattern Agent:** Grades handler anti-patterns as B+. Identifies god-module risk.
- **Architecture Agent:** Wallet mutexes held across async blockchain RPC calls (seconds).

## Proposed Solutions

### Deduplicate + Split
- **Effort:** Medium
- Extract `with_wallet(state, wallet_type, otp)` helper to eliminate 8 near-identical functions
- Replace `Mutex<Option<BaseWallet>>` with `tokio::sync::OnceCell<BaseWallet>`
- Release wallet lock before RPC calls
- Split into `handler/mod.rs`, `handler/wallet.rs`, `handler/messaging.rs`, `handler/social.rs`

## Acceptance Criteria

- [x] Wallet handler duplication eliminated via shared helper
- [x] Wallet locks not held across async RPC calls
- [x] handler.rs split into focused sub-modules
- [x] All existing tests pass

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by simplicity, architecture, and pattern review agents |
| 2026-02-16 | Done | Split handler.rs into handler/{mod,wallet,messaging,social}.rs; replaced Mutex<Option<BaseWallet>> with OnceLock<BaseWallet>; extracted with_human_wallet/verify_totp/send_eth/send_usdc/write_contract/sign_message helpers; all 158 tests pass, clippy clean |
