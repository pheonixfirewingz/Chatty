# Operations and verification

The broker is event-driven: no poller, heartbeat, maintenance loop, or idle timer runs. SQLite uses a five-connection pool and WAL. `packaging/chatty-broker.service` provides a hardened ARM64-compatible systemd unit.

## Broker resource rules

The service is capped at 256 MiB by systemd. Monitoring reports RSS (`VmRSS`) and
the detected cgroup limit; it does not allocate or retain conversation content.
CPU is sampled as a low-cost process average. The broker must remain event-driven:
admin refresh is explicit, and the GUI must not create a connected-state polling
loop. Streaming remains bounded by the 32-frame writer queue and batched-token
rules below. Treat sustained memory near the service cap or sustained high CPU as
an operational incident, not as a reason to raise the cap automatically.

## Local startup and state

Use `./start-chatty.sh` to create missing development certificates, build missing release binaries, launch the broker and GUI, and stop the broker when the GUI exits. The per-user database is `$XDG_DATA_HOME/chatty/chatty.db` (fallback `~/.local/share/chatty/chatty.db`), while the launcher log and GUI session are under `$XDG_STATE_HOME/chatty/` (fallback `~/.local/state/chatty/`). Debug and release builds use the same locations. On first use, the launcher copies an existing `.chatty/chatty.db` and its SQLite sidecars to the XDG data directory, retaining the originals as a backup. The script respects existing `CHATTY_LISTEN`, `CHATTY_BROKER`, `CHATTY_SERVER_NAME`, `CHATTY_DATABASE`, `CHATTY_LOG_FILE`, `CHATTY_CERT`, `CHATTY_KEY`, `CHATTY_CA` and `CHATTY_LLAMA_URL` values.

The current client/broker handshake protocol is version 9. A version mismatch is intentionally fatal; rebuild all Rust components together. On a fresh database, `CHATTY_LLAMA_URL` seeds the singleton broker settings row. Later URL/enabled, provider, model, generation-default and policy changes are persistent and should be made from Admin > Broker. Admin > Ollama controls models through the configured server's native API.

The GUI saves an opaque session token, never a password, at `$XDG_STATE_HOME/chatty/session` (falling back to `~/.local/state/chatty/session`). Debug and release builds use the same location and never save relative to the build/current directory. Logout removes it. For isolated GUI automation/tests, set `CHATTY_SESSION_FILE` to a temporary path or use inspection mode so a real login is neither loaded nor overwritten.

Admin > Data is deliberately not a raw database console. Password hashes, session tokens, messages and conversation bodies must remain unavailable. Account deletion is destructive and transactional; the active admin cannot delete itself.

On the development x86-64 host, the stripped release broker measured 9.7 MiB RSS and 0.0% settled idle CPU after 30 seconds with no clients. Release artifact sizes were approximately 11 MiB for the broker and 5.3 MiB for the client.

The ARM64 release is built natively on an `aarch64-unknown-linux-gnu` host (this project's current build host is aarch64). `cargo build --release --workspace` produces ARM64 `chatty-broker` (8.5 MiB stripped) and `chatty-gui` (13 MiB stripped) ELF executables targeting `aarch64`. No cross toolchain is required when building on arm64; deploy the binaries with `packaging/chatty-broker.service` and `packaging/chatty.desktop` as on x86-64.

## Network simulation

The privilege-free harness has been executed successfully on this host. It
forces one reconnect and relays real TLS traffic in 113-byte fragments with a
75±25 ms delay, deterministic 3% retransmission-delay loss simulation, and a
256 kbit/s ceiling:

```sh
./scripts/userspace-network-test.sh
```

For kernel-level loss, duplication, jitter, and reordering, use either the
disposable namespace harness (safest) or the dedicated-interface helper. The
namespace command requires administrator privileges:

```sh
sudo ./scripts/network-namespace-test.sh
```

Run the broker and client in separate network namespaces or apply these settings to a dedicated test interface (never a production interface):

Use the guarded helper on a dedicated test interface. It removes the qdisc on exit:

```sh
sudo ./scripts/network-test.sh TEST_IF 'cargo test --workspace'
```

The deterministic external-backend soak completes 100 streams, performs AI
memory extraction, then cancels another 100 streams mid-flight:

```sh
./scripts/stream-soak-test.sh
```

The protocol tests intentionally fragment frames into two-byte writes with delay, reject oversized/corrupt headers before allocation, and verify zstd activation. For streaming stress, run concurrent clients through the shaped interface and watch resident memory and traffic:

```sh
/usr/bin/time -v target/release/chatty-broker
sar -n DEV 1
```

Cancellation is a `Cancel` frame whose request ID is the generation request ID. It is scoped by connection to prevent cross-client collisions. Dropping a client cancels all generation tasks owned by that connection. Test backend disconnects by stopping llama-server mid-generation and verify that the listener continues accepting clients. Client reconnect retries occur only while disconnected; there is no connected-state polling or heartbeat.

## Release verification

The handoff baseline uses offline dependency resolution:

```sh
cargo fmt --all -- --check
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo build --release -p chatty-broker -p chatty-gui -p chatty-client --offline
```

See `docs/HANDOFF.md` for the current test count, UI inspection commands, known constraints and next-work list.
