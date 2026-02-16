---
status: done
priority: p2
issue_id: "011"
tags: [code-review, security]
dependencies: []
---

# Add Yolo Wallet Spending Limits

## Problem Statement

The yolo wallet has no per-transaction or daily spending limits. An LLM prompt injection or jailbreak could cause the agent to drain the entire wallet in a single action with no human approval.

## Findings

- **Security Agent (MEDIUM-004):** Agent tools `yolo_send_eth`, `yolo_send_usdc`, `yolo_write_contract` have no amount checks or daily limits.

## Proposed Solutions

### Configurable Limits
- **Effort:** Small
- Add `--max-yolo-tx` (e.g., 0.01 ETH / 10 USDC per transaction)
- Add `--max-yolo-daily` with rolling window
- Enforce in handler before sending

## Acceptance Criteria

- [x] Per-transaction limit configurable via CLI arg
- [x] Daily spending limit with rolling window
- [x] Transactions exceeding limits return clear error
- [x] Tests cover limit enforcement

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by security review agent |
| 2026-02-16 | Completed | Implemented SpendingLimiter with per-tx and daily rolling window limits, CLI args, 11 unit tests |
