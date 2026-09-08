# Chatty

Chatty is a Linux-native client and broker for persistent AI character roleplay. The desktop client connects to a separately operated broker over TLS 1.3; the broker owns accounts, conversations, prompt assembly, persistence, synchronization, and model access. Inference stays in Ollama or another OpenAI-compatible service.

Chatty is currently a working `0.1.0` pre-release. It is suitable for development and controlled deployments, but the release checks and Flatpak submission are not finished. See [Project status](docs/PROJECT-STATUS.md) for the current baseline.

## Workspace

| Package | Purpose |
|---|---|
| `chatty-gui` | Native `egui`/`eframe` desktop client |
| `chatty-broker` | Multi-user TLS broker, SQLite owner, and inference adapter |
| `chatty-protocol` | Shared bincode messages, framing, compression, and error types |

The repository does not contain a terminal client. Scripts that invoke `chatty-client` or `chatty-net-proxy` are legacy and are not part of the supported verification path.

## What works

- Broker selection, connection testing, registration, login, saved sessions, and logout
- User/admin roles with broker-side authorization and tenant isolation
- Character creation and editing, public sharing, and SillyTavern JSON/PNG import
- Private direct conversations, streamed generation, cancellation, regeneration, deletion, and automatic titles
- Reconnect/resume and live deltas across a user's connected clients
- Dark/light and solid/glass appearance settings
- Per-user prompt and completion token accounting
- Admin monitoring, user management, broker policy, inference settings, and Ollama model controls
- Broker/protocol support for group modes, variants, lore, memories, conversation state, and summaries

The current GUI intentionally exposes direct conversations only. Group workflows, lore, memory, world state, summaries, and variant selection still need a GUI pass.

## Quick start

Requirements:

- A current Rust toolchain
- OpenSSL for development certificates
- Linux desktop libraries required by `eframe`/Wayland
- Ollama or another OpenAI-compatible chat-completions server for generation

Run the broker and GUI together:

```sh
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 ./start-chatty.sh
```

The launcher creates development certificates when absent, builds release binaries when needed, starts the broker on `127.0.0.1:7443`, and opens the GUI. Closing the GUI stops the broker started by the launcher.

The first account created in a new database becomes an administrator. Passwords must be 10–1024 bytes; usernames must be 3–64 bytes.

To run each component yourself:

```sh
./scripts/create-dev-cert.sh
cargo build --release --workspace
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 cargo run --release -p chatty-broker
CHATTY_BROKER=127.0.0.1 CHATTY_CA=certs/ca.pem cargo run --release -p chatty-gui
```

`CHATTY_LLAMA_URL` seeds only a new database. After initialization, change the persisted adapter configuration from the GUI's admin portal.

## Data locations

Defaults follow the XDG Base Directory Specification:

| Data | Default location |
|---|---|
| Broker database | `$XDG_DATA_HOME/chatty/chatty.db` or `~/.local/share/chatty/chatty.db` |
| GUI session and preferences | `$XDG_STATE_HOME/chatty/` or `~/.local/state/chatty/` |
| Launcher broker log | `$XDG_STATE_HOME/chatty/broker.log` or `~/.local/state/chatty/broker.log` |
| Per-server GUI CA | `$XDG_CONFIG_HOME/chatty/server-cas/<host>.ca.pem` or `~/.config/chatty/server-cas/<host>.ca.pem` |

Only distribute `ca.pem` to clients. Keep `ca.key` private and offline from client systems.

## Verify

```sh
cargo fmt --all -- --check
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo build --release --workspace --offline
```

At the 2026-09-08 baseline, all 88 tests pass; formatting and strict Clippy still fail. The exact issues are recorded in [Project status](docs/PROJECT-STATUS.md).

## Documentation

- [Project status](docs/PROJECT-STATUS.md)
- [Development](docs/DEVELOPMENT.md)
- [Operations](docs/OPERATIONS.md)
- [Release](docs/RELEASE.md)
- [Architecture](docs/architecture/ARCHITECTURE.md)
- [Original plan and historical audit](docs/ORIGINAL-PLAN-AUDIT.md)

## License

MIT. See [LICENSE](LICENSE).
