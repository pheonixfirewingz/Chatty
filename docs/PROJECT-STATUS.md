# Project status

Status date: 2026-09-08  
Version: `0.1.0`  
Wire protocol: `9`  
Release state: pre-release

This status is derived from the current workspace, not from historical handoff claims.

## Summary

The core application works: the workspace builds far enough to execute the complete automated suite, and all 88 tests pass. The broker, native GUI, protocol, persistence, authentication, inference streaming, synchronization, and admin paths are present.

The repository is not release-ready. Formatting and strict Clippy gates fail, some deeper broker capabilities are not exposed in the GUI, several test scripts target binaries removed from the workspace, and Flatpak submission assets are incomplete.

## Verification baseline

Run on 2026-09-08:

| Check | Result |
|---|---|
| `cargo test --workspace --offline` | Pass: 88 tests, 0 failures |
| `cargo fmt --all -- --check` | Fail: formatting drift in broker, GUI, and protocol sources |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | Fail |

The strict Clippy run currently reports:

- Unused `SystemTime` import in protocol tests
- `Request` has a large enum variant
- Manual multiple-of test in the local base64 utility
- Collapsible nested condition in the ID utility
- Needless reference in the PEM parser

Additional compiler warnings exist in the e2e smoke binary and the GUI inspection argument path.

## Capability status

| Area | Status | Notes |
|---|---|---|
| TLS broker/client transport | Working | TLS 1.3-only configuration with a pinned client CA |
| Binary protocol | Working | JSON handshake, then bincode 2 frames; protocol version 9 |
| Compression/backpressure | Working | zstd for streams/deltas and payloads at least 256 bytes; bounded writer queue |
| Accounts and authorization | Working | Argon2 passwords, 30-day sessions, first-user admin, tenant-scoped data |
| Direct character chat | Working in GUI | Streaming, cancellation, regenerate/delete, Markdown, automatic naming |
| Character management | Working in GUI | Identity/card fields, avatar, tags, public sharing, JSON/PNG import |
| Admin controls | Working in GUI | Users, policy, adapter configuration, monitoring, Ollama model actions |
| Reconnect and state sync | Working | Snapshot plus revision-based resume and same-owner live deltas |
| Token accounting | Working | Prompt/completion totals include generation and auxiliary model calls |
| Group conversations | Broker/protocol only | GUI hides group conversations |
| Lore, memories, state, summaries | Broker/protocol only | No complete GUI workflow |
| Variants/swipes | Broker/protocol only | GUI offers regeneration but not a full variant selector |
| Field-minimal deltas | Partial | Some update payloads still carry a complete entity |
| Flatpak bundle | In progress | Local bundle script exists; Flathub manifest is incomplete |

## Repository state

The current branch is `master` at `21f40ac`, synchronized with `origin/master` when checked on 2026-09-08. Packaging/license edits are present in the working tree and are not yet committed.

The current Flatpak work still needs:

- `chatty-cargo-sources.json` for the declarative Flathub manifest
- A real hosted PNG screenshot in AppStream metadata
- A valid Anitya project ID or removal/replacement of that checker configuration
- A complete bundle/install/run test

## Legacy scripts

These scripts currently invoke removed binaries and should not be treated as passing verification:

- `scripts/multiclient-test.sh`
- `scripts/network-namespace-test.sh`
- `scripts/stream-soak-test.sh`
- `scripts/userspace-network-test.sh`

`scripts/network-test.sh` remains a generic privileged network-shaping wrapper, but its supplied test command must reference binaries that actually exist.

## Recommended next sequence

1. Apply `cargo fmt` and fix strict Clippy findings.
2. Replace or remove legacy terminal-client test scripts.
3. Add GUI workflows for broker-supported RP features, beginning with group conversations and variants.
4. Complete the Flatpak metadata and validate a clean bundle installation.
5. Tag `v0.1.0` only after the release checklist passes from a clean checkout.
