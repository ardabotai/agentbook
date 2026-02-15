---
status: complete
priority: p2
issue_id: "030"
tags: [code-review, security, input-validation, tmax-client]
dependencies: []
---

# Session ID Not Validated Before Protocol Use

## Problem Statement

The `session_id` CLI argument is passed directly into protocol requests without validation. A session ID containing newlines could break JSON-lines protocol framing. Control characters could cause log/terminal injection in error messages printed to stderr.

## Findings

- **main.rs:21**: `session_id: String` with no validation constraints
- Defense-in-depth requires client-side rejection of obviously malicious input
- Previous Phase 0 review emphasized input validation at system boundaries

## Proposed Solutions

### Option A: Add validation function (Recommended)
Validate after clap parsing: non-empty, max 256 chars, alphanumeric + hyphen + underscore only.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/main.rs`

## Acceptance Criteria
- [ ] Empty session IDs rejected with clear error
- [ ] Session IDs with control characters/newlines rejected
- [ ] Excessively long session IDs rejected
