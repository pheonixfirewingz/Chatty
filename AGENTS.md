# AGENTS.md

Read this before changing code. Read [Architecture](docs/architecture/ARCHITECTURE.md)
and the relevant [ADRs](docs/architecture/adr/) before changing subsystem contracts.
These instructions apply to agents and humans.

## Priorities

Resolve conflicts in this order:

1. **Correctness and compatibility:** preserve persisted data, stable IDs, wire
   compatibility, tenant isolation, and documented contracts.
2. **CPU and bandwidth:** minimize work per frame/request, packets, and idle activity.
3. **Memory:** bound allocations, queues, and retained state.
4. **Clarity and size:** prefer smaller, clear implementations without sacrificing
   correctness or resource use.

## Workspace and boundaries

- `chatty-gui`: native Rust `iced` desktop client; presentation, sessions, and network events.
- `chatty-broker`: authentication, authorization, SQLite, prompt assembly, inference,
  revisions, and synchronization. Keep it free of GUI dependencies.
- `chatty-protocol`: shared domain/wire types, framing, bincode 2, and bounded zstd.
- Keep model inference external through OpenAI-compatible HTTP or Ollama. Local
  GUI speech synthesis is separate from roleplay inference.
- Do not add a browser, Electron, or Node.js client runtime.
- Keep clients away from SQLite and inference endpoints. Enforce roles and ownership
  in the broker, including admin requests; never trust client claims.
- Use event-driven updates and bounded backpressure. Avoid idle polling, heartbeat
  traffic, full-state resends, and per-token packets. Discover model IDs dynamically.

## Persistence and protocol changes

- SQLite is authoritative. Add numbered migrations in `crates/chatty-broker/migrations`;
  never rewrite a migration used by a deployed database.
- Authenticate and check ownership/admin role before each mutation. Use a transaction
  for related writes, append the owner-scoped revision/delta, then publish after persistence.
- Increment the handshake protocol version for incompatible bincode layout changes;
  update broker and GUI together. See Architecture for framing and resource bounds.
- Preserve TLS 1.3, certificate verification, frame/decompression limits, cancellation
  ownership, and same-owner delta delivery. Never add a production TLS bypass.

## Build and verification

Use an edition-2024-compatible Rust toolchain and the locked dependencies. Native
Linux builds need Wayland, WGPU, SQLite, audio/speech dependencies, and the XDG file
portal; OpenSSL is needed for development certificates. Broker builds also require
the Rust `wasm32-unknown-unknown` target. Generated adapter modules belong only
in Cargo target directories. Use `--offline` once dependencies
are cached. An inference server is needed only for live generation tests.

```sh
cargo build --workspace --locked
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo build --release --workspace --locked --offline
```

Run checks appropriate to the change; complete all gates before release. Record actual
results rather than reusing historical test counts. See [Release](docs/RELEASE.md)
for functional and packaging checks.

- Run binaries with `cargo run -p chatty-broker -- --help` or
  `cargo run -p chatty-gui -- --help`.
- Protocol benchmark: `cargo run -p chatty-protocol --bin proto-bench`.
- Mock inference: `cargo run -p chatty-broker --bin chatty-mock-llama -- --help`.
- `cargo run -p chatty-broker --bin e2e-smoke` needs a separately started broker,
  certificates, and test variables. Read its source before use; it is a low-level harness.
- Protocol checks cover framing, fragmentation, compression, decompression bounds,
  and stress. Broker checks cover authorization, tenant isolation, cancellation,
  sessions, adapter parsing, and persistence. GUI checks cover sessions and app behavior.
- For visual inspection, use `./chatty-gui-inspect.sh start [width height]`, then its
  resize/screenshot commands. Save captures under `/tmp`; do not assume old egui
  accessibility or screenshot tests still apply after the move to `iced`.
- `scripts/multiclient-test.sh`, `scripts/network-namespace-test.sh`,
  `scripts/stream-soak-test.sh`, and `scripts/userspace-network-test.sh` invoke removed
  `chatty-client`/`chatty-net-proxy` binaries. Repair before using as verification evidence.
  `scripts/network-test.sh` is a privileged shaping wrapper; supply an existing command.

## Local setup and documentation

Use [README](README.md#quick-start) for startup and user workflows,
[Operations](docs/OPERATIONS.md) for environment variables, certificates, backups,
and deployment, [Release](docs/RELEASE.md) for packaging and publication, and
[GUI design rules](docs/UI.md) before building or changing GUI views.

GUI overrides: `CHATTY_BROKER` prefills the server, `CHATTY_CA` selects an explicit CA,
`CHATTY_SESSION_FILE` isolates session storage, and `CHATTY_INSPECT_CONTROL` selects
an inspection FIFO. Inspection also accepts `--inspect`, `--width`, and `--height`.

Keep each fact in its owning document. Keep engineering rules here, design contracts
and rationale in Architecture/ADRs, user instructions in README, deployment in
Operations, release gates in Release, and GUI design rules in UI. Git history preserves superseded status
reports and the original implementation audit; do not maintain duplicate handoff reports.

<!-- caveman-begin -->
Respond terse like smart caveman. All technical substance stay. Only fluff die.

Rules:
- Answer first: Answer, then reason, then next step.
- Kill ceremony: No greeting, hedging, pleasantries, recap, or closer.
- Short word: "fix" not "implement a solution for".
- Articles optional, meaning never: Drop a/an/the when the sentence still reads in one pass.
- One idea per sentence: ASD-STE100 is the floor: 20 words max, active voice, imperative for instructions, one term per thing, pronoun only with an obvious referent.
- Payload verbatim: Code blocks unchanged.
- Tool runs: bounded status: No text between routine calls.
- User's language: Compress the style, not the language.
- Never perform caveman: No "caveman mode on", no "me think", no "Caveman:" prefix, no normal answer plus caveman copy.

Switch: /caveman (default), /ultracave (fragments, each fact once), /megacave (Classical Chinese 文言文)
Stop: "stop caveman" or "normal mode"

Auto-Clarity: plain prose for security warnings, irreversible actions, step order a fragment could scramble, user confused, questions you ask the user. Resume after.

Boundaries: code, comments, commits, PRs, docs written normal; existing comments kept unless asked.
Floor: code, commands, paths, numbers and error strings verbatim; never drop not/never/no/only.
<!-- caveman-end -->
