# Chatty

Chatty is a Linux-focused native Rust application for persistent AI character roleplay. It consists of a TLS-only broker, an `egui` desktop client, a shared binary protocol, SQLite persistence, and an external inference service. The broker owns authentication, data, prompt assembly, streaming, and administration; model inference stays in Ollama or another OpenAI-compatible server.

## Current state

Chatty is a working pre-release application (`0.1.0`), not a packaged end-user release. The current workspace contains:

- `chatty-broker`: the multi-user TLS service and inference adapter
- `chatty-gui`: the responsive native desktop client
- `chatty-protocol`: shared framing, request/response, delta, and compression types
- `chatty-mock-llama`: a test-only inference backend

There is currently no terminal client in the workspace. Some older documents and network-test scripts still refer to the removed `chatty-client` binary and should be treated as historical until they are updated.

The desktop client currently supports:

- account registration, login, saved sessions, logout, admin/user roles, and tenant-isolated data
- character card creation/editing, SillyTavern JSON/PNG import, JSON export, tags, and public sharing
- private one-character conversations with automatic titles
- streamed and cancellable generation, response regeneration/deletion, and Markdown rendering
- automatic chat naming, reconnect/resume, and live state deltas across a user's connected clients
- a responsive desktop layout with compact navigation plus persistent dark/light and solid/glass appearance preferences
- per-account prompt/completion token totals; admins can also see totals for each user
- broker monitoring, persisted generation settings, registration/publishing policy, user management, sanitized metadata inspection, and Ollama model pull/load/unload/delete controls

The latest in-tree work refreshes the GUI, adds usage accounting across generation, speaker selection, memory extraction, and automatic naming, hardens sign-out/session restoration, and treats a concurrently deleted conversation as an expected empty result.

The broker and protocol also implement group modes, message variants/swipes, lore, scoped memories, world state, summaries, and system messages. The refreshed GUI does not currently expose those workflows and deliberately hides group conversations. They require a future GUI pass (or another protocol client) before they are usable from the shipped desktop interface.

## Quick start

Prerequisites:

- a current Rust toolchain
- OpenSSL, used by the development certificate script
- an Ollama server or another OpenAI-compatible chat-completions server
- Linux desktop libraries required by `eframe`/`egui`

To create development certificates, build release binaries, start the broker, and launch the GUI:

```sh
./start-chatty.sh
```

The launcher defaults to `http://192.168.0.97:11434/v1` for inference. Override that for a local Ollama installation, for example:

```sh
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 ./start-chatty.sh
```

Closing the GUI stops the broker started by the launcher. The first account registered against a new database becomes an administrator. Passwords must contain at least ten characters.

To run the components separately:

```sh
./scripts/create-dev-cert.sh
cargo build --release --workspace
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 cargo run --release -p chatty-broker
cargo run --release -p chatty-gui
```

The development certificate covers `localhost`, `rasp-server`, `127.0.0.1`, and the deployed device address `192.168.0.98`. The GUI defaults to `192.168.0.98:7443` with that IP as its TLS server name; `CHATTY_BROKER` and `CHATTY_SERVER_NAME` can override both. Distribute only `certs/ca.pem` to clients through a trusted channel, keep `ca.key` offline, and reissue the server certificate if the device address changes. Clients pin the configured CA and have no insecure TLS mode.

## Configuration and local data

Broker settings can be supplied as flags or environment variables:

| Variable | Purpose | Default |
|---|---|---|
| `CHATTY_LISTEN` | Broker listen address | launcher: `127.0.0.1:7443`; broker alone: `0.0.0.0:7443` |
| `CHATTY_DATABASE` | SQLite connection URL | XDG data directory |
| `CHATTY_CERT` | Server certificate | `certs/server.pem` |
| `CHATTY_KEY` | Server private key | `certs/server.key` |
| `CHATTY_LLAMA_URL` | Initial inference endpoint for a new database | `http://192.168.0.97:11434/v1` |

GUI variables are `CHATTY_BROKER`, `CHATTY_SERVER_NAME`, `CHATTY_CA`, and `CHATTY_SESSION_FILE`. The launcher additionally accepts `CHATTY_LOG_FILE`.

The inference URL only seeds a new database. After first launch, an administrator can change the persisted adapter URL, provider mode, model, generation defaults, and access policies from the Admin dialog.

Default per-user files follow the XDG Base Directory Specification:

- database: `$XDG_DATA_HOME/chatty/chatty.db` or `~/.local/share/chatty/chatty.db`
- saved session and appearance preferences: `$XDG_STATE_HOME/chatty/` or `~/.local/state/chatty/`
- launcher log: `$XDG_STATE_HOME/chatty/broker.log` or `~/.local/state/chatty/broker.log`

On first use, the launcher copies a legacy `.chatty/chatty.db` and its SQLite sidecar files into the XDG data directory while leaving the originals in place as a backup.

## Protocol and security

Chatty uses TLS 1.3 and a persistent connection. The initial handshake is JSON; runtime payloads use bincode 2. Stream and delta frames are always zstd-compressed, as are other payloads of at least 256 bytes. Frames are bounded to 8 MiB, writer queues are bounded for backpressure, and streamed model output is batched before transmission. The current wire contract is protocol version 9.

Passwords are hashed with Argon2. Authorization and ownership checks are enforced by the broker, and state deltas are scoped to other authenticated connections belonging to the same account. Admin metadata views intentionally exclude password hashes, session tokens, messages, and conversation content.

## Development

Run the current automated suite with:

```sh
cargo test --workspace
```

The suite covers protocol framing/compression, bounded decoding and streaming, broker authorization and tenant isolation, token accounting, reconnect/session behavior, delta application, and responsive GUI rendering. Visual GUI tests write inspection images under `/tmp`.

Useful design and operational background lives in [the architecture](docs/architecture/ARCHITECTURE.md), [operations notes](docs/OPERATIONS.md), [the implementation handoff](docs/HANDOFF.md), and [the original-plan audit](docs/ORIGINAL-PLAN-AUDIT.md). Those documents predate the removal of the terminal client in places; this README and the current source tree describe the runnable workspace.
