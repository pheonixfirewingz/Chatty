# Architecture

## Context

Chatty is a client/server desktop application for persistent AI character roleplay. Its boundary contains the native GUI, broker, shared wire contract, and SQLite database. Model execution is deliberately external.

The primary journey is: connect securely, authenticate, choose a character/conversation, send a message, and receive a streamed response that is persisted and synchronized to the user's other clients.

The design prioritizes low idle activity, bounded memory, low packet counts, tenant isolation, and operation on small Linux servers.

See [diagram.mmd](diagram.mmd) for the container and trust-boundary view.

## Containers

### Native GUI (`chatty-gui`)

Responsibilities:

- Server selection and pinned-CA TLS connection
- Registration, login, bearer-session storage, and resume
- Local presentation state and appearance preferences
- Direct-conversation and character workflows
- Applying snapshots, deltas, and streamed chunks
- Admin controls for authorized accounts

The GUI does not read SQLite or contact the inference service. A current-thread Tokio runtime runs on a dedicated network thread; network events request an egui repaint instead of polling.

### Broker (`chatty-broker`)

Responsibilities:

- TLS termination and protocol validation
- Authentication, authorization, and tenant boundaries
- SQLite migrations and all durable state mutations
- Prompt/context assembly and speaker orchestration
- External model discovery, requests, streaming, and usage accounting
- Stream batching, cancellation, delta persistence, and live fan-out
- Admin monitoring, policy, user management, and Ollama lifecycle calls

The broker is stateless only with respect to process memory: SQLite remains authoritative across restart. In-memory connection, cancellation, health, and broadcast state is reconstructible.

### Shared protocol (`chatty-protocol`)

Responsibilities:

- Request, response, error, delta, stream, and domain transfer types
- The fixed frame header and message-type registry
- Bincode serialization
- Bounded zstd compression/decompression
- Small utilities shared by broker and GUI

Because bincode encodes Rust data layouts, broker and client must use compatible protocol definitions. The handshake version is the compatibility gate.

### SQLite

SQLite owns users, sessions, characters, conversations, participants, messages, variants, lore, memories, broker settings, token totals, and the revisioned delta log. Foreign keys encode lifecycle relationships; owner predicates provide the main tenant boundary.

### Inference service

The broker supports an OpenAI-compatible model surface and an optional native Ollama mode. It discovers models, selects a configured or first available model, streams generated content, and records reported token usage. Ollama mode additionally supports runtime settings and model pull/load/unload/delete operations.

## Interfaces

### Client to broker

Transport is TLS 1.3 over one TCP connection at port `7443` by default.

The broker first sends a JSON handshake:

```json
{"protocol":9,"encoding":"bincode2","compression":"zstd","tls":"1.3"}
```

All later messages use a 14-byte header:

```text
payload_length:u32-be | flags:u8 | message_type:u8 | request_id:u64-be
```

Payloads are bincode 2. Stream chunks and deltas are always zstd-compressed; other payloads are compressed at 256 bytes. Encoded and decoded payloads are bounded to 8 MiB.

Client message types are requests and cancellation. Broker message types include responses, errors, deltas, stream chunks, and stream completion. Request IDs correlate direct responses and generation cancellation.

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

The compiler combines system rules, the active character, bounded group participant cards, lore, scoped memories, conversation state, summary, and selected recent message history.

Stream output flushes after 32 whitespace-delimited units, 60 ms, or completion. Each connection has a 32-frame writer queue, which propagates backpressure. Cancellation keys include both connection ID and request ID to prevent cross-client cancellation.

## Data ownership and authorization

- The broker trusts no client-supplied role or ownership claim.
- Session tokens identify users and expire after 30 days.
- Characters, conversations, lore, memories, deltas, and usage are owner-scoped.
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
