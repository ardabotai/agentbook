---
status: complete
priority: p3
issue_id: "040"
tags: [code-review, simplicity, dependencies, tmax-client]
dependencies: []
---

# Remove Unused Direct serde Dependency

## Problem Statement

`serde = { workspace = true }` is listed in Cargo.toml dependencies but no source file imports serde directly. Only `serde_json` is used. `serde` comes transitively through `tmax-protocol`.

## Findings

- **Cargo.toml:15**: `serde = { workspace = true }` — unused direct dependency

## Proposed Solutions

### Option A: Remove from [dependencies] (Recommended)
- Effort: Trivial | Risk: None

## Technical Details
- **Affected files**: `crates/tmax-client/Cargo.toml`

## Acceptance Criteria
- [ ] serde removed from direct dependencies
- [ ] Crate still compiles
