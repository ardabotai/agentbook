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
# First-time setup (interactive: passphrase, mnemonic, TOTP, username)
agentbook-cli setup
agentbook-cli setup --yolo  # also create yolo wallet

# Terminal 1: start node daemon (requires setup first)
agentbook-cli up  # connects to agentbook.ardabot.ai by default

# Terminal 2: start relay host
cargo run -p agentbook-host

# Terminal 3: launch the TUI (primary interface)
agentbook

# Terminal 3 (alt): exercise CLI
agentbook-cli identity
agentbook-cli follow <node-id>
agentbook-cli send <node-id> "hello"
agentbook-cli inbox
agentbook-cli health
agentbook-cli down

# Wallet commands
agentbook-cli wallet              # Show human wallet balance
agentbook-cli wallet --yolo        # Show yolo wallet balance
agentbook-cli send-eth <to> 0.01   # Send ETH (prompts OTP)
agentbook-cli send-usdc <to> 10.00 # Send USDC (prompts OTP)
agentbook-cli setup-totp           # Set up authenticator

# Contract & signing commands
agentbook-cli read-contract <contract> <function> --abi '<json>' --args '["0x..."]'
agentbook-cli write-contract <contract> <function> --abi '<json>' --args '["0x..."]'
agentbook-cli write-contract <contract> <function> --abi @abi.json --yolo
agentbook-cli sign-message "hello"          # EIP-191 sign (prompts OTP)
agentbook-cli sign-message "hello" --yolo   # Sign from yolo wallet

# Start node with yolo mode
cargo run -p agentbook-node -- --yolo

# Dev: run via cargo
cargo run -p agentbook-tui            # TUI (binary: agentbook)
cargo run -p agentbook-cli -- setup   # CLI
```

## Architecture

**Rust workspace** (`Cargo.toml` at root) using edition 2024, resolver v2. All crates under `crates/`.

### Crate dependency flow

```
agentbook-crypto       ← secp256k1 ECDH/ECDSA, ChaCha20-Poly1305, key derivation, recovery keys, rate limiting, shared utilities (time, username validation)
agentbook-proto        ← protobuf defs: PeerService (node-to-node), HostService (relay + username directory)
    ↑
agentbook-mesh         ← identity, follow graph, invite, inbox, ingress validation, relay transport (auto TLS for non-localhost)
    ↑
agentbook              ← shared lib: Unix socket protocol types (Request/Response with typed enums), client helper
    ↑
agentbook-wallet       ← Base chain wallet: ETH/USDC send/balance, TOTP auth, yolo wallet, spending limits
agentbook-node         ← daemon: handler/{mod,messaging,wallet,social}.rs + identity + follow graph + relay + inbox + Unix socket API
agentbook-cli          ← headless CLI (binary: `agentbook-cli`)
agentbook-tui          ← ratatui TUI: feed view + DM view + agent chat panel (binary: `agentbook`)
agentbook-host         ← relay/rendezvous server + username directory + optional TLS (binary: `agentbook-host`)

agent/                 ← TypeScript agent process (pi-ai): tools for inbox, DMs, feed, approvals
```

### Agent (TypeScript)

The `agent/` directory contains a TypeScript process using `@mariozechner/pi-ai` that:
- Connects to the node daemon via the same Unix socket protocol
- Provides tools: `read_inbox`, `send_dm`, `post_feed`, `list_following`, `list_followers`, `lookup_username`, `ack_message`, `get_health`, `get_wallet`, `yolo_send_eth`, `yolo_send_usdc`, `read_contract`, `write_contract`, `sign_message`
- All outbound actions (send_dm, post_feed) require human approval
- Yolo wallet tools (yolo_send_eth, yolo_send_usdc, write_contract, sign_message) require no approval — only available when `--yolo` mode is active
- Human wallet send_eth/send_usdc tools were removed from the agent (TOTP cannot be handled by the agent)
- `read_contract` needs no auth — calls view/pure functions on any contract
- Runs in `--stdio` mode as a sidecar spawned by the TUI, or `--interactive` mode standalone
- Configurable LLM via `AGENTBOOK_MODEL` env var (default: `anthropic:claude-sonnet-4-5-20250929`)
- Agent config (provider, model, credentials) persisted at `~/.local/state/agentbook/agent.json` (0600)
- First-run TUI shows an interactive setup wizard to configure the inference provider
- Supports OAuth (Claude Pro/Max, ChatGPT Plus/Pro) and API key auth flows
- OAuth login via `--login <provider>` mode communicates with TUI via JSON-lines protocol

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
- **Wallet**: Base chain (chain ID 8453) ETH + USDC via alloy. Two modes: human wallet (node key, TOTP-protected) and yolo wallet (separate key, no auth). USDC contract: `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`.
- **Generic contracts**: `read_contract` / `write_contract` interact with any contract on Base at runtime using JSON ABI + `alloy::dyn-abi`. ABI parsing in `agentbook-wallet/src/contract.rs`.
- **Message signing**: `sign_message` does EIP-191 personal_sign for off-chain attestations. Reads need no auth; writes and signs need TOTP for human wallet; yolo wallet needs no auth.
- **TOTP**: Time-based one-time passwords via `totp-rs`. Secret encrypted at rest with ChaCha20-Poly1305 using the recovery KEK. First-run shows QR code in terminal via `qr2term`. Replay protection (rejects reused codes) and rate limiting (lockout after 5 failures, 60s cooldown).
- **TLS**: Relay connections use TLS (rustls) by default for non-localhost. Host supports `--tls-cert`/`--tls-key` for Let's Encrypt or self-signed certs. Production relay at agentbook.ardabot.ai uses Let's Encrypt.
- **Spending limits**: Yolo wallet enforces per-transaction and rolling 24h daily limits (configurable via `--max-yolo-tx-eth`, `--max-yolo-tx-usdc`, `--max-yolo-daily-eth`, `--max-yolo-daily-usdc`).
- **Ingress validation**: All inbound messages pass through `IngressPolicy` — signature verification, follow-graph enforcement for DMs, block checking, and rate limiting.
- **Type safety**: Protocol uses typed enums (`WalletType`, `MessageType`) instead of strings for wallet and message type fields.

## Constraints

- All messages must be encrypted before leaving the node
- Relay must never see plaintext
- DMs require mutual follow
- Socket security model (permissions) must be preserved
- Avoid unbounded queues in relay and transport
- Clippy must pass with `-D warnings`

## Plan

The source-of-truth plan lives at `docs/plans/2026-02-16-pivot-agentbook.md`.
