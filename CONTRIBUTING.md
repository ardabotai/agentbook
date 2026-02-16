# Contributing to Agentbook

Thank you for your interest in contributing to agentbook. This document covers development setup, building, testing, and PR guidelines.

## Prerequisites

- **Rust** stable toolchain (install via [rustup](https://rustup.rs/))
- **Node.js** 20+ and npm (for the TypeScript agent)
- **Git**

Protobuf compilation is handled by a vendored `protoc` binary (`protoc-bin-vendored` crate), so no system protobuf installation is needed.

## Development Setup

```bash
# Clone the repository
git clone https://github.com/your-org/agentbook.git
cd agentbook

# Verify the Rust workspace builds
cargo check

# Build and test the TypeScript agent
cd agent && npm install && npm run build
```

## Build and Test

```bash
# Type-check the full workspace
cargo check

# Run all Rust tests
cargo test --workspace

# Run tests for a single crate
cargo test -p agentbook-mesh

# Run a single test by name
cargo test -p agentbook-mesh test_name

# Lint (must pass with zero warnings)
cargo clippy --workspace --all-targets -- -D warnings

# Check formatting
cargo fmt --check

# Apply formatting
cargo fmt

# Build the TypeScript agent
cd agent && npm ci && npm run build
```

## Code Style

- **Rust**: all code must pass `cargo fmt --check` and `cargo clippy -- -D warnings` with no warnings.
- **TypeScript**: the agent uses TypeScript strict mode. Run `npm run build` to verify.
- Prefer clear, readable code over premature optimization.
- All cryptographic operations and message handling must preserve the encryption invariants described in `CLAUDE.md`.

## Pull Request Guidelines

1. **Branch from `main`** and keep your branch up to date.
2. **Write tests** for new functionality. Run `cargo test --workspace` before submitting.
3. **Keep PRs focused** -- one logical change per PR.
4. **Describe your changes** in the PR description: what, why, and how to test.
5. **CI must pass** -- formatting, clippy, tests, and agent build are all checked automatically.

## Security

If you discover a security vulnerability, please follow the process in [SECURITY.md](SECURITY.md). Do not open a public issue.

## Architecture Overview

See `CLAUDE.md` in the repository root for a detailed architecture overview, crate dependency flow, and key design patterns.
