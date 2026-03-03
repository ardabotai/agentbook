---
status: pending
priority: p3
issue_id: "029"
tags: [code-review, agent-native]
dependencies: []
---

# Port Arda Gateway Support to Standalone Agent (agent/src/index.ts)

## Problem Statement

The standalone TypeScript agent (`agent/src/index.ts`) only resolves `AGENTBOOK_OAUTH_CREDENTIALS` and direct API keys. It does not check `AGENTBOOK_GATEWAY_API_KEY` or support baseURL routing, so it cannot use Arda Gateway.

## Findings

- **File:** `agent/src/index.ts:60-85` — no Arda Gateway env var check
- **Source:** agent-native-reviewer agent

## Proposed Solutions

### Solution A: Add resolveInferenceConfig() pattern from pi-terminal-agent.mjs
- Check `AGENTBOOK_GATEWAY_API_KEY` and pass `baseURL` to the stream() call
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Standalone agent checks `AGENTBOOK_GATEWAY_API_KEY` env var
- [ ] Routes through gateway when key is present

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |
