---
status: done
priority: p2
issue_id: "012"
tags: [code-review, architecture]
dependencies: []
---

# Fix Agent Stdio Approval Flow and Human Wallet Tools

## Problem Statement

Two issues in the TypeScript agent:
1. Agent's `send_eth`/`send_usdc` tools for human wallet send empty OTP (""), which always fails TOTP verification. These tools are non-functional.
2. In stdio mode, `approval_response` messages from the TUI are never processed -- the handler only processes `user_message`. Approvals are silently dropped.

## Findings

- **Architecture Agent (HIGH):** Agent sends empty OTP at `agent/src/tools/index.ts:267`. Approval flow in `runStdioMode` doesn't handle `approval_response`.
- **Security Agent (MEDIUM-005):** Empty OTP means human wallet tools via agent never work.

## Proposed Solutions

### Option A: Remove Human Wallet Tools from Agent
- **Effort:** Small
- If the agent should never handle human wallet OTP, remove those tools entirely.

### Option B: Implement OTP Routing Through TUI
- **Effort:** Medium
- TUI intercepts wallet requests, prompts for OTP, injects it before forwarding.
- Fix `runStdioMode` to handle `approval_response` messages.

## Acceptance Criteria

- [x] Either human wallet tools removed from agent, or OTP flow works end-to-end
- [x] Stdio approval responses properly processed
- [x] No silently dropped messages

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by architecture and security review agents |
| 2026-02-16 | Fixed | Removed human wallet send_eth/send_usdc tools; added approval_response handling in stdio mode |
