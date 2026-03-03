---
status: pending
priority: p3
issue_id: "030"
tags: [code-review, agent-native]
dependencies: ["021"]
---

# Auto-Poll for Arda Login in TUI (Remove Empty-Enter Requirement)

## Problem Statement

When the TUI is waiting for an API key, an agent must press Enter with empty input to trigger a re-scan for the Arda key file. This implicit interaction pattern is not agent-friendly. The TUI should detect the key file automatically.

## Findings

- **File:** `crates/agentbook-tui/src/input.rs:1184-1201` — empty-Enter detection
- **Source:** agent-native-reviewer agent

## Proposed Solutions

### Solution A: Timer-based polling in tick() (Recommended)
- When `awaiting_api_key` is true, check `has_arda_login()` every ~5 seconds in the tick handler
- Auto-resume Sidekick if key file appears
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] TUI auto-detects Arda login without requiring manual Enter
- [ ] Polling interval is reasonable (5-10 seconds)
- [ ] Empty-Enter still works as fallback

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |
