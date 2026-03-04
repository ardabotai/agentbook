---
status: pending
priority: p2
issue_id: "049"
tags: [code-review, security, robustness]
dependencies: []
---

# Add Destructive Action Deny-List to Auto-Accept

## Problem Statement

When `AGENTBOOK_AUTO_ASSUME_YES=1`, the Rules engine auto-sends `y\n` to any terminal prompt containing `(y/n)` with "continue"/"proceed"/"resume". This heuristic is too broad — a prompt like "Delete all files and continue? (y/n)" would be auto-accepted.

## Findings

- **File:** `automation.rs:316-337` — auto-accept sends y\n unconditionally
- **File:** `automation.rs:761-766` — has_yes_no_continue_prompt heuristic
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Add deny-list keywords
- Check for "delete", "remove", "format", "destroy", "drop", "overwrite", "force push", "reset --hard" before auto-accepting
- If destructive keyword found, skip auto-accept, show prompt in Sidekick chat for manual confirmation
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Prompts with destructive keywords are NOT auto-accepted
- [ ] User is notified when a prompt is skipped
- [ ] Safe prompts still auto-accepted

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI security review | Prevents accidental destructive operations |
