---
status: pending
priority: p2
issue_id: "048"
tags: [code-review, refactor, dry]
dependencies: []
---

# Consolidate Duplicated Code Across TUI Modules

## Problem Statement

Several functions and patterns are duplicated across TUI modules, creating maintenance burden and inconsistency (e.g., the ui.rs truncate uses byte-based slicing that can panic on multi-byte UTF-8, while automation.rs uses char-safe version).

## Findings

- **truncate** — 3 variants: `ui.rs:953` (byte-based, buggy), `automation.rs:802` (char-safe), `input.rs:976` (char-safe copy)
- **terminal_content_area** — identical in `input.rs:1247` and `main.rs:452`
- **sidekick reset** — 8-field reset block copied 3x in `input.rs:425-433, 496-502, 839-848`
- **Decision/SidekickChatCompletion** — identical structs with trivial converter functions at `automation.rs:632-656`
- **PI command resolution** — same 12-line block at `automation.rs:397-409` and `automation.rs:434-446`
- **save_anthropic_api_key** — duplicated unix/non-unix blocks at `automation.rs:836-867`
- **Source:** architecture-strategist, code-simplicity-reviewer

## Proposed Solutions

### Solution A: Consolidate all duplicates
1. Single `truncate()` in shared util (char-safe version), fixes the UTF-8 panic bug
2. Move `terminal_content_area` to `ui.rs` as pub function
3. Add `AutoAgentState::reset()` method
4. Merge Decision into SidekickChatCompletion, derive Default, delete converter functions
5. Extract `fn pi_command() -> Result<String>` shared helper
6. Simplify save_anthropic_api_key with conditional cfg on mode only
- **Effort:** Small per item | **Risk:** Low

## Acceptance Criteria

- [ ] No duplicated truncate functions — single char-safe version
- [ ] terminal_content_area defined once
- [ ] Sidekick reset logic in one method
- [ ] Single struct for decisions/completions
- [ ] PI command resolution in one function
- [ ] ~150 LOC reduction total

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI review | Fixes latent UTF-8 panic in truncate |
