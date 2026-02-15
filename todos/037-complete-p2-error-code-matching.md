---
status: complete
priority: p2
issue_id: "037"
tags: [code-review, architecture, tmax-client]
dependencies: []
---

# Error Fallback Uses String Matching Instead of Error Code

## Problem Statement

In `main.rs`, the attach error fallback matches `message.contains("edit")` to detect edit-denied errors. This is fragile — if the server changes the error message wording, the fallback silently breaks.

## Findings

- **main.rs:60**: `Response::Error { message, .. } if message.contains("edit")`
- The protocol has `ErrorCode::AttachmentDenied` which should be matched instead
- Previous Phase 0 review emphasized structured error handling

## Proposed Solutions

### Option A: Match on ErrorCode (Recommended)
Match `Response::Error { code: Some(ErrorCode::AttachmentDenied), .. }` instead of string matching.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/main.rs`

## Acceptance Criteria
- [ ] Error detection uses ErrorCode enum, not string matching
- [ ] Fallback to view mode still works correctly
