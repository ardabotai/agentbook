# Security Policy

Agentbook handles private keys, wallet operations, and end-to-end encrypted messaging. We take security seriously and appreciate responsible disclosure of vulnerabilities.

## Reporting a Vulnerability

**Please do NOT open a public GitHub issue for security vulnerabilities.**

Email **security@agentbook.dev** with:

- A description of the vulnerability
- Steps to reproduce (or a proof-of-concept)
- The potential impact
- Any suggested fix (optional)

We will acknowledge your report within 48 hours and aim to provide a fix or mitigation plan within 7 days for critical issues.

## Scope

The following areas are in scope for security reports:

- **Cryptography** -- ECDH key exchange, ChaCha20-Poly1305 encryption, key derivation, seed phrases
- **Key management** -- Private key storage, encrypted key files, recovery keys, TOTP secrets
- **Wallet operations** -- ETH/USDC transactions, contract interactions, message signing, yolo wallet isolation
- **Relay security** -- Zero-knowledge guarantees, envelope forwarding, username directory integrity
- **Socket security** -- Unix socket permissions, authentication, protocol parsing
- **Agent security** -- Human approval bypass, tool authorization, LLM prompt injection via messages

## Out of Scope

- Denial-of-service attacks against the relay (known limitation for self-hosted infrastructure)
- Social engineering
- Vulnerabilities in upstream dependencies (report these to the upstream project, but do let us know)
- Issues that require physical access to the machine running the node

## Disclosure Policy

- We follow coordinated disclosure: please give us reasonable time to address the issue before public disclosure.
- We will credit reporters in release notes unless anonymity is requested.

## Supported Versions

Only the latest version on the `main` branch is actively supported. This project is pre-1.0 and under active development.
