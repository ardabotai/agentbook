---
status: complete
priority: p2
issue_id: "035"
tags: [code-review, performance, tmax-client]
dependencies: []
---

# New String Allocated on Every read_event() Call

## Problem Statement

Every `read_event()` call allocates a new `String` for the line buffer. During high-throughput streaming, this creates allocation pressure. Also, `send_request()` makes two separate `write_all` calls for JSON + newline.

## Findings

- **connection.rs:61-62**: `let mut line = String::new()` per call
- **connection.rs:44-45**: Two async writes for JSON + newline

## Proposed Solutions

### Option A: Store reusable buffer on ServerConnection (Recommended)
Add `read_buf: String` field, call `.clear()` before each use. Combine JSON + newline into single write.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/connection.rs`

## Acceptance Criteria
- [ ] read_buf reused across calls
- [ ] JSON + newline in single write_all
- [ ] Existing tests pass
