# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is agentbook?

An AI-powered encrypted messaging network. Each user runs a node daemon with a secp256k1 identity. Users follow each other (Twitter-style) and communicate through encrypted DMs (mutual follow required) and encrypted feed posts. Every message is end-to-end encrypted via ECDH + ChaCha20-Poly1305. A relay host provides NAT traversal and a username directory — it only forwards encrypted blobs (zero-knowledge).

The TUI and CLI connect to the node daemon via a Unix socket JSON-lines protocol. A TypeScript agent process (using pi-ai for LLM access) runs as a sidecar, providing AI assistance for drafting, summarizing, and managing messages.

## Build & Test Commands

```bash
cargo check                          # Type-check the full workspace
cargo test --workspace               # Run all tests
cargo test -p agentbook-mesh         # Test a single crate
cargo test -p agentbook-mesh test_name  # Run a single test
cargo fmt --check                    # Check formatting
cargo clippy --workspace --all-targets -- -D warnings  # Lint
```

## Smoke Testing

```bash
# Terminal 1: start node daemon
cargo run -p agentbook-node  # connects to agentbook.ardabot.ai by default

# Terminal 2: start relay host
cargo run -p agentbook-host

# Terminal 3: exercise CLI
cargo run -p agentbook-cli -- identity
cargo run -p agentbook-cli -- follow <node-id>
cargo run -p agentbook-cli -- send <node-id> "hello"
cargo run -p agentbook-cli -- inbox
cargo run -p agentbook-cli -- health
cargo run -p agentbook-cli -- down

# Or launch the TUI
cargo run -p agentbook-tui
```

## Architecture

**Rust workspace** (`Cargo.toml` at root) using edition 2024, resolver v2. All crates under `crates/`.

### Crate dependency flow

```
agentbook-crypto       ← secp256k1 ECDH/ECDSA, ChaCha20-Poly1305, key derivation, recovery keys
agentbook-proto        ← protobuf defs: PeerService (node-to-node), HostService (relay + username directory)
    ↑
agentbook-mesh         ← identity, follow graph, invite, inbox, ingress validation, relay transport
    ↑
agentbook              ← shared lib: Unix socket protocol types (Request/Response), client helper
    ↑
agentbook-node         ← daemon: identity + follow graph + relay + inbox + Unix socket API
agentbook-cli          ← headless CLI (binary: `agentbook`)
agentbook-tui          ← ratatui TUI: feed view + DM view + agent chat panel
agentbook-host         ← relay/rendezvous server + username directory (binary: `agentbook-host`)
agentbook-tests        ← E2E test helpers

agent/                 ← TypeScript agent process (pi-ai): tools for inbox, DMs, feed, approvals
```

### Agent (TypeScript)

The `agent/` directory contains a TypeScript process using `@mariozechner/pi-ai` that:
- Connects to the node daemon via the same Unix socket protocol
- Provides tools: `read_inbox`, `send_dm`, `post_feed`, `list_following`, `list_followers`, `lookup_username`, `ack_message`, `get_health`
- All outbound actions (send_dm, post_feed) require human approval
- Runs in `--stdio` mode as a sidecar spawned by the TUI, or `--interactive` mode standalone
- Configurable LLM via `AGENTBOOK_MODEL` env var (default: `anthropic:claude-sonnet-4-20250514`)

```bash
cd agent && npm install && npm run build   # Build agent
npm run dev                                 # Run in interactive mode
npm run dev -- --stdio                      # Run as TUI sidecar
```

### Key patterns

- **Unix socket protocol**: JSON-lines over Unix socket between daemon and clients. Request/Response types in `agentbook/src/protocol.rs`. Max line size 64 KiB.
- **Follow model**: one-way follow for feed posts, mutual follow for DMs, block cuts everything.
- **Encryption**: ECDH shared secrets + ChaCha20-Poly1305. Feed posts encrypted per-follower (content key wrapped per-recipient). DMs encrypted directly.
- **Socket security**: runtime dir `0700`, socket `0600`. Preserve this.
- **Relay is zero-knowledge**: only forwards encrypted protobuf Envelopes. Cannot read message content.
- **Username directory**: nodes register `@username` on relay host (signed by private key). Lookup resolves username → node_id + public key.

## Constraints

- All messages must be encrypted before leaving the node
- Relay must never see plaintext
- DMs require mutual follow
- Socket security model (permissions) must be preserved
- Avoid unbounded queues in relay and transport
- Clippy must pass with `-D warnings`

## Plan

The source-of-truth plan lives at `docs/plans/2026-02-16-pivot-agentbook.md`.
