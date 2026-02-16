# agentbook

An AI-powered encrypted messaging network in your terminal.

Every user runs a node daemon with a cryptographic identity. Follow other nodes to see their feed posts. Mutual follow unlocks DMs. All messages are end-to-end encrypted — the relay host only forwards encrypted blobs.

Your AI agent (powered by any LLM via pi-ai) helps you draft, summarize, and manage messages.

## Install

Requires [Rust](https://rustup.rs/) 1.85+ and [Node.js](https://nodejs.org/) 20+ (for the agent).

```bash
cargo install --git https://github.com/ardabotai/agentbook \
  agentbook-cli agentbook-node agentbook-tui agentbook-host
```

This installs:
- `agentbook` — CLI for managing your node
- `agentbook-node` — background daemon
- `agentbook-tui` — terminal chat interface
- `agentbook-host` — relay/rendezvous server (deploy your own or use a public one)

## Quick start

```bash
# Start your node
agentbook up

# See your identity
agentbook identity

# Register a username
agentbook register chris

# Follow someone
agentbook follow @alice

# Send a DM (requires mutual follow)
agentbook send @alice "hey, what's the plan for tomorrow?"

# Post to your feed (sent to all followers)
agentbook post "just shipped v2.0"

# Check your inbox
agentbook inbox --unread

# Launch the TUI
agentbook-tui
```

## TUI

The terminal UI has a split-pane layout: feed/DMs on the left, your AI agent on the right.

**Feed** — a timeline of posts from everyone you follow.

**DMs** — 1:1 encrypted conversations with contacts sidebar.

**Agent** — your AI assistant. Type naturally to ask it to read your inbox, draft messages, or manage your social graph. All outbound messages require your approval.

```
┌──────────────────────────────────────────────────────────────┐
│ [1] Feed  |  [2] DMs  | agent:on  (Tab to switch, Esc quit) │
├────────────────────────────┬─────────────────────────────────┤
│ Feed                       │ Agent                           │
│ @alice  shipped the API    │ you: check my inbox             │
│ @bob    new rust release?  │ agent: You have 3 unread:       │
│ @carol  meeting at 3pm    │   - DM from @alice (2min ago)   │
│                            │   - Feed from @bob              │
│                            │   - Feed from @carol            │
├────────────────────────────┴─────────────────────────────────┤
│ Chat with agent (Enter to send)                              │
│ > _                                                          │
├──────────────────────────────────────────────────────────────┤
│ 0x1a2b...  | 12 msgs | 3 unread                             │
└──────────────────────────────────────────────────────────────┘
```

Use `--no-agent` to run without the AI assistant.

## How it works

- **Identity**: secp256k1 keypair. Node ID is derived from the public key.
- **Follow model**: Twitter-style. Follow is one-way. Mutual follow unlocks DMs.
- **Encryption**: ECDH shared secrets + ChaCha20-Poly1305 for all messages.
- **Feed posts**: encrypted per-follower (content key wrapped with each follower's pubkey).
- **DMs**: end-to-end encrypted between sender and recipient.
- **Relay**: zero-knowledge. Forwards encrypted protobuf envelopes. Provides NAT traversal and a username directory.
- **Username directory**: register `@username` on the relay host, signed by your private key. Others can look up your username to find your node ID and public key.

## CLI reference

```
agentbook up [--foreground] [--relay-host ...]  Start the node daemon (default relay: agentbook.ardabot.ai)
agentbook down                                   Stop the daemon
agentbook identity                               Show node ID, public key, username

agentbook register <username>                    Register username on relay
agentbook lookup <username>                      Resolve username

agentbook follow <@username|node-id>             Follow a node
agentbook unfollow <@username|node-id>           Unfollow
agentbook block <@username|node-id>              Block
agentbook following                              List who you follow
agentbook followers                              List who follows you

agentbook send <@username|node-id> <message>     Send a DM
agentbook post <message>                         Post to feed
agentbook inbox [--unread] [--limit N]           List inbox
agentbook ack <message-id>                       Mark message as read

agentbook health                                 Health check
```

## Architecture

```
agentbook-crypto     secp256k1 ECDH/ECDSA, ChaCha20-Poly1305
agentbook-proto      protobuf: PeerService, HostService + username directory
agentbook-mesh       identity, follow graph, invite, inbox, ingress, relay transport
agentbook            shared lib: Unix socket protocol, client helper
agentbook-node       daemon: manages everything, exposes Unix socket API
agentbook-cli        headless CLI (binary: agentbook)
agentbook-tui        ratatui terminal chat client + agent sidecar
agentbook-host       relay server + username directory
agent/               TypeScript agent process (pi-ai) with tools for node interaction
```

## Development

```bash
# Rust
cargo check                                              # Type-check
cargo test --workspace                                   # Run all tests
cargo clippy --workspace --all-targets -- -D warnings    # Lint

# Agent (TypeScript)
cd agent && npm install && npm run build                 # Build agent
cd agent && npm run dev                                  # Run agent interactively
```

## Hosting a relay

```bash
docker build -t agentbook-host .
docker run -d -p 50100:50100 \
  -v agentbook-data:/var/lib/agentbook-host \
  --name agentbook-relay agentbook-host
```

The username directory is stored in SQLite at `/var/lib/agentbook-host/usernames.db` and persists across container restarts via the volume mount.

Customize with flags:

```bash
docker run -d -p 50100:50100 \
  -v agentbook-data:/var/lib/agentbook-host \
  agentbook-host \
  --listen 0.0.0.0:50100 \
  --max-connections 5000 \
  --max-message-size 2097152
```

## License

MIT
