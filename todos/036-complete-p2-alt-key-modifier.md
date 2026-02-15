---
status: complete
priority: p2
issue_id: "036"
tags: [code-review, architecture, tmax-client]
dependencies: []
---

# Alt Key Modifier Not Handled in key_to_bytes

## Problem Statement

`key_to_bytes` in `keybindings.rs` ignores the Alt modifier. Alt+b (word-back), Alt+f (word-forward), and other Alt combinations commonly used in bash/zsh will not work, making the terminal client less functional than expected.

## Findings

- **keybindings.rs**: `key_to_bytes` handles Char, Control, Enter, arrows, function keys, but not Alt modifier
- Alt sequences are typically ESC + char (e.g., Alt+b = `\x1b` + `b`)

## Proposed Solutions

### Option A: Prepend ESC byte for Alt modifier (Recommended)
Check for Alt in KeyModifiers, prepend `\x1b` to the character byte.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/keybindings.rs`

## Acceptance Criteria
- [ ] Alt+letter combinations produce ESC + letter
- [ ] Add test for Alt key handling
