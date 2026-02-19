# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is agentbook?

An AI-powered encrypted messaging network. Each user runs a node daemon with a secp256k1 identity. Users follow each other (Twitter-style) and communicate through encrypted DMs (mutual follow required) and encrypted feed posts. Every message is end-to-end encrypted via ECDH + ChaCha20-Poly1305. A relay host provides NAT traversal and a username directory — it only forwards encrypted blobs (zero-knowledge).

The TUI and CLI connect to the node daemon via a Unix socket JSON-lines protocol. The TUI features a 3-tab full-screen layout (Feed, DMs, Terminal) with an embedded PTY terminal for running any tool (Claude Code, etc.) directly.

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

# Service commands (launchd on macOS, systemd on Linux — requires 1Password for non-interactive auth)
agentbook-cli service install           # Install as background service (starts at login)
agentbook-cli service install --yolo    # Install with yolo mode (skips TOTP)
agentbook-cli service uninstall         # Remove service
agentbook-cli service status            # Show service status

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

# Room commands
agentbook-cli join test-room                          # Join an open room
agentbook-cli join secret-room --passphrase "my pass" # Join/create a secure room
agentbook-cli leave test-room                         # Leave a room
agentbook-cli rooms                                   # List joined rooms
agentbook-cli room-send test-room "hello"             # Send to room (140 char limit)
agentbook-cli room-inbox test-room                    # Read room messages

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
agentbook-node         ← daemon: handler/{mod,messaging,wallet,social,rooms}.rs + identity + follow graph + relay + inbox + rooms + Unix socket API
agentbook-cli          ← headless CLI (binary: `agentbook-cli`)
agentbook-tui          ← ratatui TUI: 3-tab layout (Feed/DMs/Terminal) with embedded PTY (binary: `agentbook`)
agentbook-host         ← relay/rendezvous server + username directory + optional TLS (binary: `agentbook-host`)

agent/                 ← TypeScript agent process (pi-ai): standalone tools for inbox, DMs, feed (not TUI-integrated)
```

### TUI Layout

The TUI uses a dynamic tab layout:
- **[1] Feed** — encrypted feed posts from followed nodes
- **[2] DMs** — encrypted direct messages (mutual follow required), contact list on left
- **[3] Terminal** — embedded PTY terminal (portable-pty + vt100), lazy-spawned on first switch
- **[4+] #rooms** — IRC-style chat rooms, dynamically added on join. Secure rooms show a lock icon.

**Keybinding**: `Ctrl+Space` leader key (tmux-style chord), then `1`/`2`/`3`/`4+` to switch tabs. `Left`/`Right` arrows in prefix mode navigate prev/next tab. `Tab` toggles between Feed/DMs. `Esc` quits from Feed/DMs. On the Terminal tab, all keys pass through to the PTY except the `Ctrl+Space` prefix chord.

**Slash commands**: `/join <room> [--passphrase <pass>]` and `/leave <room>` work from any tab's input bar.

**Activity indicators**: Red `*` appears on tab labels when there's unread activity (new messages, terminal output) while on a different tab. Clears when you switch to that tab.

**Real-time events**: The TUI listens for `Event::NewMessage`, `Event::NewFollower`, and `Event::NewRoomMessage` pushed by the node daemon over the Unix socket (via `NodeReader`), auto-refreshing the inbox on new messages. A 30-second polling fallback ensures no events are missed.

### Agent (TypeScript — standalone)

The `agent/` directory contains a standalone TypeScript process (not TUI-integrated) using `@mariozechner/pi-ai`. It connects directly to the node daemon via the Unix socket and can be run independently:

```bash
cd agent && npm install && npm run build   # Build agent
npm run dev                                 # Run in interactive mode
```

### Key patterns

- **Unix socket protocol**: JSON-lines over Unix socket between daemon and clients. Request/Response types in `agentbook/src/protocol.rs`. Max line size 64 KiB. `NodeClient::into_split()` yields `NodeWriter`/`NodeReader` halves for concurrent event listening + request sending.
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
- **Ingress validation**: All inbound messages pass through `IngressPolicy` — signature verification, follow-graph enforcement for DMs, block checking, and rate limiting. Room messages skip the follow-graph check.
- **Type safety**: Protocol uses typed enums (`WalletType`, `MessageType`) instead of strings for wallet and message type fields.
- **Rooms**: IRC-style chat rooms with two modes: open (signed plaintext) and secure (ChaCha20-Poly1305 encrypted with passphrase-derived key via Argon2id). 140-character message limit with 3-second per-room cooldown. Room subscriptions are managed via control frames to the relay host, which broadcasts to all subscribers. Rooms persist across restarts via `rooms.json`. Blocked users are filtered client-side. Room handler in `agentbook-node/src/handler/rooms.rs`.

## Constraints

- All messages must be encrypted before leaving the node
- Relay must never see plaintext
- DMs require mutual follow
- Socket security model (permissions) must be preserved
- Avoid unbounded queues in relay and transport
- Clippy must pass with `-D warnings`

## Plan

The source-of-truth plan lives at `docs/plans/2026-02-16-pivot-agentbook.md`.
