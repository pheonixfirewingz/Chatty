# Architecture

## Context

Chatty is a client/server desktop application for persistent AI character roleplay. Its boundary contains the native GUI, broker, shared wire contract, and SQLite database. Model execution is deliberately external.

The primary journey is: connect securely, authenticate, choose a character/conversation, send a message, and receive a streamed response that is persisted and synchronized to the user's other clients.

The design prioritizes low idle activity, bounded memory, low packet counts, tenant isolation, and operation on small Linux servers.

See [diagram.mmd](diagram.mmd) for the container and trust-boundary view.

## Containers

### Native GUI (`chatty-gui`)

Responsibilities:

- Server selection, public-PKI TLS validation, and confirmed private-CA enrollment
- Registration, login, bearer-session storage, and resume
- Local presentation state and appearance preferences
- Direct-conversation and character workflows
- Applying snapshots, deltas, and streamed chunks
- Admin controls for authorized accounts

The GUI does not read SQLite or contact the inference service. The `iced` client receives asynchronous network events through subscriptions instead of polling.

### Broker (`chatty-broker`)

Responsibilities:

- TLS termination and protocol validation
- Authentication, authorization, and tenant boundaries
- SQLite migrations and all durable state mutations
- Prompt/context assembly and speaker orchestration
- Typed adapter operations, model selection, and usage accounting
- Stream batching, cancellation, delta persistence, and live fan-out
- Admin monitoring, policy, user management, Hugging Face GGUF search, and Ollama lifecycle calls

The broker is stateless only with respect to process memory: SQLite remains authoritative across restart. In-memory connection, cancellation, health, and broadcast state is reconstructible.

Broker source is split along runtime boundaries:

- `main.rs` owns startup and request dispatch.
- `transport.rs` owns TLS connections, framing, idle close, and delta fan-out.
- `auth.rs` owns sessions and authorization lookups.
- `adapters.rs` selects WASM adapters and maps neutral results to existing broker types.
- `systems/admin.rs` owns monitoring, broker configuration, and authorized model administration.
- `systems/conversations.rs` owns conversation persistence, snapshots, world loading, and automatic titles.
- `systems/inference.rs` owns prompt assembly, typed inference requests, client stream batching, persistence of generated output, and speaker selection.
- `utils.rs` contains validation, error classification, bounded text helpers, and usage accounting.
- `tests.rs` contains broker unit and integration-style tests.

### Broker WASM adapter runtime and shared guest API

The host loads provider translators as import-free WASM plugins, checks ABI version 1, and owns their HTTP transport. The broker calls one typed interface for discovery, completion, streaming, model lifecycle, and catalogue operations. Plugins translate API paths/options and normalize JSON/SSE/NDJSON into neutral results. No provider HTTP schema remains in broker systems.

OpenAI-compatible, FreeToken, Ollama, and Hugging Face plugins ship under `crates/chatty-adapters/` as Rust source and prebuilt WASM. Operator-installed manifests are discovered at startup without rebuilding the broker. Modules load on demand; only the selected inference provider is retained, while Hugging Face exists only for active catalogue operations. Model management shares the selected provider. Existing database settings select the bundled providers unless an operator selects a custom plugin. Guest memory, fuel, messages, network origins, methods, timeouts, and response sizes are bounded. See [adapter contract](ADAPTERS.md) and [ADR 0003](adr/0003-wasm-inference-adapter-plugins.md).

### Shared protocol (`chatty-protocol`)

Responsibilities:

- Request, response, error, delta, stream, and domain transfer types
- The fixed frame header and message-type registry
- Bincode serialization
- Bounded zstd compression/decompression
- Small utilities shared by broker and GUI

Because bincode encodes Rust data layouts, broker and client must use compatible protocol definitions. The handshake version is the compatibility gate.

### SQLite

SQLite owns users, sessions, characters, conversations, participants, messages, variants, worlds, memories, broker settings, token totals, and the revisioned delta log. Foreign keys encode lifecycle relationships; owner predicates provide the main tenant boundary.

### Inference service

The broker reaches inference services through its selected WASM adapter. Bundled plugins support an OpenAI-compatible model surface and native Ollama mode. It discovers models, selects a configured or first available model, streams generated content, and records reported token usage. Ollama mode additionally supports runtime settings and model pull/load/unload/delete operations.

## Interfaces

### Client to broker

Transport is TLS 1.3 over one TCP connection at port `7443` by default.

The broker first sends a JSON handshake:

```json
{"protocol":15,"encoding":"bincode2","compression":"zstd","tls":"1.3"}
```

All later messages use a 14-byte header:

```text
payload_length:u32-be | flags:u8 | message_type:u8 | request_id:u64-be
```

Payloads are bincode 2. Stream chunks and deltas are always zstd-compressed; other payloads are compressed at 256 bytes. Encoded and decoded payloads are bounded to 8 MiB.

Client message types are requests and cancellation. Broker message types include responses, errors, deltas, stream chunks, and stream completion. Request IDs correlate direct responses and generation cancellation.

Admin model search is broker-only and rechecks the administrator role. It queries the public Hugging Face GGUF catalogue on explicit submission, with a 20-second timeout, a 2 MiB response limit, at most 20 repositories, and at most 128 single-file GGUF choices per repository. Downloads use Ollama's `hf.co/owner/repository:filename.gguf` pull support. No inference runtime or model files are added to the GUI. Protocol 15 adds the search request/response; broker and GUI must upgrade together.

### Synchronization

Every durable owner-scoped mutation receives a monotonically increasing revision and a delta-log row. Initial login uses a bounded snapshot. Resume requests replay deltas newer than the client's revision.

Live deltas are published only to other authenticated connections for the same owner. A lagged subscriber is disconnected so it can resume from the durable log instead of silently skipping revisions.

Some deltas describe a narrow change, such as context or selected variant; others contain the full updated entity. Field-minimal updates are therefore an optimization opportunity, not a current invariant.

### Generation

```text
user message -> persist + delta -> compile context -> select speaker
             -> stream model response -> batch -> compressed chunks
             -> persist message/variant + token usage -> final delta
```

The compiler combines system rules, the active character, bounded group participant cards, world lore, scoped memories, conversation state, summary, and selected recent message history. Owner-scoped worlds contain character links and facts. Common knowledge is selected first; other enabled facts match keywords in the latest six messages. Character memories use SQLite FTS5 plus pinned state, importance, and recency to select at most ten relevant records within a fixed character budget. Automatic extraction runs after each completed, non-cancelled root reply without delaying the streamed reply. It extracts up to eight new durable facts into owner-scoped, cross-chat character memory, budgets its input against the configured model context, and advances its message checkpoint only after successful persistence (including a valid empty result). One background job per owner/conversation coalesces overlapping replies; failed work remains pending for the next reply. Deleting a conversation promotes its memories to cross-chat character scope instead of deleting them.

Stream output flushes after 32 whitespace-delimited units, 60 ms, or completion. Each connection has a 32-frame writer queue, which propagates backpressure. Cancellation keys include both connection ID and request ID to prevent cross-client cancellation.

## Data ownership and authorization

- The broker trusts no client-supplied role or ownership claim.
- Session tokens identify users and expire after 30 days.
- Characters, conversations, worlds, memories, deltas, and usage are owner-scoped.
- Public characters are readable across accounts only when broker policy permits; only their owner can edit them.
- Admin-only requests re-read role state from the database.
- Admin data inspection excludes password hashes, tokens, and conversation bodies.
- The first account in an empty database becomes admin; subsequent accounts become users.

Passwords are hashed with Argon2. Concurrent Argon2 operations are capped at ten to bound memory during authentication bursts.

## Reliability and resource bounds

- SQLite uses WAL and a pool capped at five connections.
- Connection writer queues are bounded.
- Protocol allocation and decompression are bounded.
- Active generation prevents idle close while the upstream model is silent.
- Active Ollama lifecycle requests also prevent idle close; pulls have a six-hour upstream timeout.
- Otherwise a connection quiet in both directions closes after 120 seconds; the GUI reconnects on demand and resumes from its revision.
- Disconnect cancels generation work owned by that connection.
- Startup inference probing is best-effort and does not prevent the broker from accepting clients.
- Systemd templates cap broker memory at 256 MiB.

## Scaling assumptions

The current design targets a single broker process and one SQLite database on a small Linux host. It is appropriate for a modest number of users and bounded concurrent generations, where inference is the dominant workload.

Horizontal broker scaling is not supported: live broadcasts, cancellation ownership, revision coordination, and SQLite writes are process-local. Supporting multiple broker replicas would require a shared transactional database plus distributed event/cancellation coordination.

## Known architectural gaps

- The GUI exposes only a subset of the broker's RP domain.
- Bincode request layouts couple client and broker releases tightly.
- Some update deltas send more data than a minimal changed-field representation.
- The main broker dispatch and GUI application files are large and carry many responsibilities.
- Monitoring is process-local and visible only through the admin request path; there is no metrics export.
- The delta log has no documented retention/compaction policy.

## Decisions

- [ADR 0001: Broker-owned state over a binary TLS protocol](adr/0001-broker-owned-state-and-binary-tls.md)
- [ADR 0002: Hybrid public PKI and private certificate enrollment](adr/0002-hybrid-public-pki-and-private-certificate-enrollment.md)

- [ADR 0003: WASM inference adapter plugins](adr/0003-wasm-inference-adapter-plugins.md)
