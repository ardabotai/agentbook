---
status: done
priority: p2
issue_id: "006"
tags: [code-review, security]
dependencies: []
---

# Add TOTP Replay Protection and Rate Limiting

## Problem Statement

TOTP verification has no rate limiting and no replay protection. Each code is valid for ~90 seconds (skew=1). A local attacker with socket access can brute-force all 1M 6-digit codes, or replay an observed code for multiple transactions.

## Findings

- **Security Agent (HIGH-004):** No failed attempt tracking, no replay protection, no lockout.

## Proposed Solutions

### Track Last-Used Timestamp + Attempt Counter
- **Effort:** Small
- Track last-used TOTP time step; reject same or earlier steps
- Add failed attempt counter with lockout after 5 failures
- Consider reducing skew to 0

## Acceptance Criteria

- [x] Same TOTP code cannot be used twice within its window
- [x] After 5 failed attempts, lockout for cooldown period
- [x] Tests cover replay rejection and lockout behavior

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by security review agent |
| 2026-02-16 | Done | Already implemented: TotpGuard with replay protection, rate limiting (5 failures / 60s lockout), persistent state, and 7 dedicated tests |
