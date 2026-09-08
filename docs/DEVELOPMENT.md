# Development

## Requirements

- Rust with Cargo and the repository's edition-2024-compatible toolchain
- Native Linux build dependencies for Wayland, WGPU, SQLite, and the XDG file portal
- OpenSSL when generating development certificates
- An inference server only for live generation tests

Dependencies are locked in `Cargo.lock`. Prefer `--locked` for builds and `--offline` after dependencies are available locally.

## Workspace commands

```sh
cargo build --workspace --locked
cargo test --workspace --offline
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
```

Run one package:

```sh
cargo run -p chatty-broker -- --help
cargo run -p chatty-gui -- --help
cargo run -p chatty-protocol --bin proto-bench
```

The broker also provides test-support binaries:

```sh
cargo run -p chatty-broker --bin chatty-mock-llama -- --help
cargo run -p chatty-broker --bin e2e-smoke
```

`e2e-smoke` expects a separately started broker, certificates, and its test environment variables; inspect its source before use because it is a low-level harness rather than a stable public command.

## Local environment

Generate a development CA and server certificate:

```sh
./scripts/create-dev-cert.sh
```

Start the complete local stack:

```sh
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 ./start-chatty.sh
```

Useful broker variables:

| Variable | Default | Purpose |
|---|---|---|
| `CHATTY_LISTEN` | `0.0.0.0:7443` in the broker; `127.0.0.1:7443` in the launcher | Listen address |
| `CHATTY_DATABASE` | XDG data directory | SQLite URL |
| `CHATTY_CERT` | `certs/server.pem` | Server certificate chain |
| `CHATTY_KEY` | `certs/server.key` | Server private key |
| `CHATTY_LLAMA_URL` | `http://192.168.0.97:11434/v1` | Initial adapter URL for a new database |
| `RUST_LOG` | broker info plus library defaults | Tracing filter |

Useful GUI variables:

| Variable | Purpose |
|---|---|
| `CHATTY_BROKER` | Prefill the server host/address |
| `CHATTY_CA` | Select an explicit pinned CA file |
| `CHATTY_SESSION_FILE` | Override session storage for isolated testing |

The GUI also accepts `--inspect`, `--width`, and `--height` for deterministic interface inspection. Visual tests write images under `/tmp`.

## Database changes

Migrations live in `crates/chatty-broker/migrations` and are compiled into the broker with `sqlx::migrate!()`. Add a new numbered migration; do not rewrite a migration already used by a deployed database.

SQLite is authoritative. Mutations must:

1. Authenticate the session and check ownership or admin role.
2. Apply related writes transactionally where consistency spans tables.
3. Append the owner-scoped delta with the resulting revision.
4. Publish the encoded delta after persistence succeeds.

## Protocol changes

The broker sends a JSON handshake declaring protocol version `9`; all runtime requests, responses, errors, deltas, and stream chunks use bincode 2.

Any incompatible request/response/type-layout change must increment the handshake version and update broker and GUI together. Keep frames bounded to 8 MiB and preserve decompression bounds.

## Test structure

- Protocol tests cover framing, fragmentation, compression, decompression bounds, and stress.
- Broker tests cover authorization, tenant isolation, cancellation, adapter parsing, sessions, title/memory validation, and state changes.
- GUI tests cover network/session helpers, application behavior, accessibility queries, and responsive visual layouts.
- `crates/chatty-gui/tests/integration_tests.rs` checks a protocol round trip.

The four shell scripts listed as legacy in [Project status](PROJECT-STATUS.md) depend on removed command-line tools. Repair them before including them in CI or release evidence.
