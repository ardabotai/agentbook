---
status: complete
priority: p3
issue_id: "042"
tags: [code-review, architecture, rendering, tmax-client]
dependencies: []
---

# Bold and Dim Both Emit NormalIntensity to Toggle Off

## Problem Statement

In `renderer.rs`, both bold-off and dim-off emit `NormalIntensity` (SGR 22). This means clearing bold also clears dim and vice versa. If a cell is both bold and dim (unusual but valid), toggling one off incorrectly clears the other.

## Findings

- **renderer.rs:56-66**: Bold off → `NormalIntensity`
- **renderer.rs:67-77**: Dim off → `NormalIntensity`
- Known SGR limitation — NormalIntensity resets both bold and dim per ECMA-48

## Proposed Solutions

### Option A: Re-emit the surviving attribute after NormalIntensity
After emitting NormalIntensity, check if the other attribute should still be set and re-emit it.
- Effort: Small | Risk: Low

### Option B: Accept the limitation
Document it as a known issue. Bold+dim cells are rare.
- Effort: None | Risk: None

## Technical Details
- **Affected files**: `crates/tmax-client/src/renderer.rs`

## Acceptance Criteria
- [ ] Bold+dim cells render correctly, or limitation is documented
