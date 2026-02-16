---
title: "feat: tmax mesh - relay/NAT traversal + rendezvous hosts"
type: feat
status: active
date: 2026-02-15
---

# feat: tmax mesh - relay/NAT traversal + rendezvous hosts

## Goal

From the current state (local `tmax-node` gRPC daemon, session-scoped mailbox/tasks/workflows), reach the stage where two nodes on different machines can:

1. Connect without knowing each other's IP (invite link bootstrap).
2. Exchange authenticated, spam-resistant messages.
3. Work behind NAT using a relay host (TURN-like).
4. Optionally upgrade to direct peer-to-peer when possible (rendezvous-assisted), without breaking relay as fallback.

This plan is written to be implemented incrementally with end-to-end tests at each milestone.

## Current State (Starting Point)

- Local daemon exists: `tmax-node` (gRPC).
- Session lifecycle + PTY streaming + snapshots + markers are implemented.
- Inter-agent primitives exist (local only):
  - mailbox (`MessageSend/List/Ack/UnreadCount`)
  - workflows + membership enforcement
  - shared tasks scoped to workflow members
- Crypto baseline exists (local only):
  - per-session EVM-style wallet (secp256k1), used for message encryption/signing within a single node.
  - recovery key file exists for key-wrapping flows (filesystem key envelope), but there is no actual encrypted filesystem backend yet.

Gaps for mesh:

- No persistent node identity (needed for "friends" and routing).
- No transport for node-to-node delivery.
- No relay/rendezvous host.
- No invite token flow that bootstraps trust.

## Scope and Non-Goals

In scope for this stage:

- Node identity + friends + invite links.
- Node-to-node messaging over gRPC.
- Relay host enabling NAT traversal via outbound-only node connections.
- Rendezvous for direct dialing (best-effort optimization).
- Spam resistance via invite-only capabilities + rate limits.

Out of scope for this stage:

- General-purpose distributed task execution across nodes (remote session spawn).
- Full encrypted per-session filesystem mounts.
- Full ICE/STUN hole punching across arbitrary NATs. (We will add TURN-like relay first; direct is opportunistic.)
- Multi-hop onion routing.

## Architecture (Target)

### Roles

- `tmax-node` (existing): local PTY/session daemon + local mailbox/tasks/workflows API.
- `tmax-mesh` (new inside `tmax-node`): background component that manages:
  - persistent node identity
  - friends store
  - invite creation/acceptance
  - relay/rendezvous connectivity
  - inbound/outbound message routing
- `tmax-host` (new): public rendezvous + relay service. Nodes dial out to it; it relays frames between nodes. Think "TURN + rendezvous directory", not "central coordinator".

### Data Model

- Node identity: `node_id = 0x...` (EVM address derived from node secp256k1 public key).
- Friend: `(node_id, alias, added_at, blocked, endpoints, last_seen, trust_caps...)`.
- Capability invite token: signed by inviter node key, includes:
  - `inviter_node_id`
  - `relay_host` (or list)
  - `expiry`
  - `scopes` (message, optional future scopes)
  - `rate_limit` (token bucket params)
  - `token_id` (random nonce for replay protection / single-use)

### Message Routing

- Nodes exchange messages addressed to `friend_node_id` (not session ids).
- Each node can optionally "deliver" remote messages into a chosen local tmax session mailbox (root orchestrator session), but the mesh should also support a node-level inbox.

### Transport

- gRPC streaming (tonic) for:
  - node <-> host: bidirectional stream (`Connect`) kept open.
  - node <-> node (direct): best-effort, optional bidirectional stream.

### Security Model (First Version)

- Every message frame is signed by the sending node identity.
- First contact requires a valid invite/capability token. Unknown peers are rejected.
- Encryption:
  - v1: message-level encryption optional using ECDH between node keys (ChaCha20-Poly1305).
  - relay host never needs to decrypt payloads.
- Rate limiting:
  - v1: per-peer token bucket, applied at recipient node ingress; optionally also at host.

## Milestones

### M1: Persistent Node Identity + State Directory

Deliverable:

- `tmax-node` loads/creates a persistent node wallet at startup.
- State is stored under a configurable `--state-dir` (default to XDG_STATE_HOME or `~/.local/state/tmax` on Unix).

Implementation tasks:

- Add a `NodeIdentity` module to `libtmax` or a new crate `tmax-mesh-core`:
  - load or create secp256k1 secret key
  - derive EVM address
  - export public key bytes (SEC1) for verification
- Keystore file format:
  - v1: simple file with encrypted private key using the existing recovery key as KEK.
  - store `created_at_ms`, `address`, `public_key`.
- Add gRPC API to `tmax-node`:
  - `NodeInfo` (node_id, public_key, state_dir info).

Acceptance criteria:

- Node id is stable across restarts (same `node_id`).
- Keystore permissions are `0600` (or best effort on non-Unix).
- Unit test: "restart loads same node id" using a temp state dir.

### M2: Friends Store + Invite Link (No Networking Yet)

Deliverable:

- Create and accept invite tokens; maintain a friend list.

Implementation tasks:

- Add `friends.json` (or `friends.toml`) in `state_dir` with CRUD:
  - `friends add` (by invite accept)
  - `friends list`
  - `friends remove`
  - `friends block/unblock`
- Invite token design:
  - Base64url-encoded JSON or protobuf message.
  - Include `token_id` (random 16-32 bytes), `expiry`, `relay_hosts`, and scopes.
  - Signed by inviter node key over a canonical byte payload.
  - Include inviter public key (optional) so accept can verify without network.
- Add gRPC API to `tmax-node`:
  - `InviteCreate` -> returns string token
  - `InviteAccept` -> returns Friend record
  - `FriendsList`
  - `FriendsBlock/Unblock/Remove`

Acceptance criteria:

- Invite token validates signature and expiry.
- Accept adds friend entry deterministically (id = inviter node id).
- Tests cover malformed tokens, expired tokens, wrong signature.

### M3: Direct Node-to-Node Messaging (Port Forward / Public IP OK)

Deliverable:

- A `tmax-peer` gRPC service that can receive signed/encrypted messages from known friends.
- A `tmax-node` client that can dial a friend's endpoint and deliver a message.

Implementation tasks:

- Add a new proto (suggested crate: `tmax-mesh-proto`):
  - `PeerService.SendMessage(Envelope) -> Ack`
  - optional `PeerService.Stream()` for push notifications / keepalive
  - `Envelope` includes: from_node_id, to_node_id, timestamp, nonce, ciphertext, signature, optional invite token (only for first contact)
- Ingress policy:
  - allow if sender is a friend OR invite token is valid and targets recipient
  - rate limit before doing expensive crypto work
- Delivery integration:
  - node-level inbox (persisted) and/or deliver into a configured local session mailbox.
- Add minimal CLI wrappers (optional) but keep gRPC as primary:
  - `tmax-nodectl friends list`
  - `tmax-nodectl msg send --to <friend> --body ...`

Acceptance criteria:

- E2E test with two `tmax-node` processes on localhost, distinct state dirs:
  1. node A creates invite
  2. node B accepts invite
  3. node B sends message to A
  4. A receives message via subscribe/inbox API
- Rejection tests:
  - unknown sender without invite rejected
  - invalid signature rejected
  - rate limit enforced

### M4: Relay Host (TURN-like) + Rendezvous Directory

Deliverable:

- `tmax-host` binary:
  - listens on a public port
  - accepts outbound node connections
  - relays envelopes between connected nodes
  - provides rendezvous lookup (node_id -> connection)

Design choice:

- v1 NAT traversal is achieved by relay only. Nodes behind NAT just keep an outbound stream to host.

Implementation tasks:

- Define host protocol in `tmax-mesh-proto`:
  - `HostService.Connect(stream NodeFrame) returns (stream HostFrame)`
  - frames include:
    - `Register { node_id, public_key, signature }`
    - `RelaySend { to_node_id, envelope }`
    - `Ping/Pong`
  - host validates registration signature (proof of key ownership).
- Host routing:
  - maintain in-memory map `node_id -> sender stream`.
  - relay frames to recipient stream if online, else return error (v1).
  - optional: store-and-forward queue (bounded) per node (v1.1).
- Node integration:
  - `tmax-node` connects to host and registers.
  - When sending to friend, prefer relay path if friend has no direct endpoint or direct fails.
  - When receiving via relay, apply the same ingress checks (friend/invite/rate limit).

Acceptance criteria:

- E2E test with:
  - one `tmax-host`
  - two nodes that do NOT expose peer listeners (simulate NAT-only)
  - message succeeds via relay
- Host abuse tests:
  - unauthenticated register rejected
  - relay send to unknown node returns not found
  - per-connection message size limits enforced

### M5: Rendezvous-Assisted Direct Upgrade (Optional Optimization)

Deliverable:

- When possible, nodes attempt direct peer connection for lower latency, but always fall back to relay.

Implementation tasks:

- Host records observed remote address for each node connection.
- Add a `HostService.Lookup(node_id) -> observed_endpoints` method (authenticated).
- Nodes attempt direct dial to the observed endpoint.
- Keep direct connections cached with health checking; fallback on failure.

Acceptance criteria:

- E2E test where both nodes expose peer listeners:
  - direct dial is attempted and succeeds
  - messages bypass relay after direct established
- Failure mode test:
  - direct dial fails, relay path still works

## Testing Strategy

Add integration tests under a new crate (suggested) `crates/tmax-mesh-tests/` or inside `tmax-host` and `tmax-node`:

- Spawn child processes with temp state dirs.
- Use fixed ports or OS-assigned ports discovered at runtime.
- Tests:
  - `invite_round_trip`
  - `direct_peer_message_round_trip`
  - `relay_message_round_trip`
  - `reject_unknown_sender`
  - `rate_limit_enforced`
  - `restart_persists_node_identity`

## Operational Notes (v1)

- `tmax-host` should be deployable as a single binary behind systemd with a static port.
- Logging:
  - structured logs with node_id and connection_id fields.
- Resource bounds:
  - bound per-node inbound queue
  - bound per-message size
  - avoid unbounded maps (use LRU for token replay protection)

## Open Questions (Decide Before M3)

1. Where do remote messages land by default: node-level inbox, or forwarded to a configured local session mailbox?
2. Do we require TLS for host transport in v1, or rely on message-level encryption/signatures only?
3. Invite token replay policy: single-use strictly, or time-bound reusable?

