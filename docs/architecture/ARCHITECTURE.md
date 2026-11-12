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
- Direct- and group-conversation and character workflows
- Applying snapshots, deltas, and streamed chunks
- Admin controls for authorized accounts

The GUI does not read SQLite or contact the inference service. The `iced` client receives asynchronous network events through subscriptions instead of polling. Visual and interaction rules for its views are in [GUI design rules](../UI.md).

GUI source is one directory module per feature; each keeps state and messages in `mod.rs`, views in `view.rs`, and tests in `tests.rs`:

- `app/` owns application state, update dispatch, network event and delta handling, subscriptions, the inspection harness, and the root views.
- `network/` owns the broker connection task, TLS trust, broker addresses, and on-disk session and preferences.
- `characters/`, `conversation/`, `worlds/`, `memories/`, and `admin/` own their feature dialogs.
- `ui/` owns the theme, vector icons, and shared widgets; `tts/` owns local speech.

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

Broker source is split into domain directories, mirroring the GUI. Each domain keeps
its logic in `mod.rs`, its wire-request handlers in `handlers.rs`, and its unit tests in
`tests.rs`:

- `main.rs` owns arguments, startup, and the accept loop; `app.rs` owns shared state,
  concurrency gates, and live-delta publication.
- `dispatch/` maps each request to a domain handler and owns the per-request context
  (responses, delta delivery, busy and cancellation tracking) and error classification.
- `transport.rs` owns TLS connections, framing, idle close, and delta fan-out.
- `account/` owns registration, sign-in, sessions, authorization lookups, and the login throttle.
- `adapters/` selects WASM adapters, maps neutral results to broker types, and
  (`adapters/workload.rs`) serializes text and image models on a shared GPU host.
- `admin/` owns monitoring, broker configuration, user management, and authorized model administration.
- `characters/` owns character validation, storage, and encrypted portrait handling.
- `conversations/` owns conversation and message persistence, deletion, and automatic titles.
- `worlds/` owns world storage and character-to-world links.
- `sync/` owns the delta log, live-delta visibility, snapshots, and Sync/Resume replay.
- `emotion/` runs the in-process emotion classifier and applies reply portraits after persistence.
- `images/` owns image prompt validation, scene descriptions, PNG normalization, and image message persistence.
- `inference/` owns prompt assembly, typed inference requests, client stream batching, persistence of generated output, and speaker selection.
- `memory/` owns memory persistence and retrieval; `memory/extraction/` owns extraction
  scheduling, extraction, and condensation; `memory/eval.rs` replays fixtures.
- `crypto.rs` encrypts stored images; `logging.rs` owns the boot log and data paths;
  `utils.rs` holds bounded text helpers and usage accounting.
- `tests/` holds broker-level tests that drive `dispatch` end to end, split by domain.

### Broker WASM adapter runtime and shared guest API

The host loads provider translators as import-free WASM plugins, checks ABI version 1, and owns their HTTP transport. The broker calls one typed interface for discovery, completion, streaming, model lifecycle, and catalogue operations. Plugins translate API paths/options and normalize JSON/SSE/NDJSON into neutral results. No provider HTTP schema remains in broker systems.

OpenAI-compatible, FreeToken, Ollama, Hugging Face, and ComfyUI plugins live under `crates/chatty-adapters/` as Rust source and manifests. The broker build compiles them into its Cargo output directory and embeds those generated modules; no WASM binaries are stored in the source tree. Broker builds require the Rust WASM target, while deployed binaries need no compiler or additional module files. See [ADR 0004](adr/0004-source-built-bundled-adapters.md). Operator-installed manifests are discovered at startup without rebuilding the broker. Modules load on demand; only the selected inference provider is retained, while Hugging Face and image adapters exist only for active catalogue or image operations. Model management shares the selected provider. The admin portal selects installed inference plugins through broker-authorized configuration. An empty plugin selection preserves the legacy OpenAI/Ollama setting; migration 0013 adds the selection column without rewriting existing settings. `CHATTY_ADAPTER_PLUGIN` takes precedence over the persisted selection and is reported to administrators. Adapter lists come from startup manifests without loading unused modules; the catalogue-only Hugging Face adapter is excluded. Guest memory, fuel, messages, network origins, methods, timeouts, and response sizes are bounded. See [adapter contract](ADAPTERS.md) and [ADR 0003](adr/0003-wasm-inference-adapter-plugins.md).

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

Messages are ordered by `seq`, a per-conversation sequence assigned at insert by a trigger (migration 0019 backfilled existing rows in `created_at`, then insertion, order; migration 0020 makes it unique per conversation). Every timeline query uses it: the conversation view (the newest 2000 root messages, oldest first), prompt history, speaker choice, scene descriptions, and memory extraction. Edits raise a message's revision but never move it, and the order survives `VACUUM`. The wire format does not carry `seq`; clients keep the order they receive.

### Inference service

The broker reaches inference services through its selected WASM adapter. Bundled plugins support an OpenAI-compatible model surface and native Ollama mode. It discovers models, selects a configured or first available model, streams generated content, and records reported token usage. Ollama mode additionally supports runtime settings and model pull/load/unload/delete operations.

## Interfaces

### Client to broker

Transport is TLS 1.3 over one TCP connection at port `7443` by default. Both ends accept only the X25519MLKEM768 hybrid post-quantum key exchange, so recorded traffic stays private unless both X25519 and ML-KEM-768 are broken; peers without it fail the handshake rather than downgrade. Certificate signatures remain classical.

The broker first sends a JSON handshake:

```json
{"protocol":28,"encoding":"bincode2","compression":"zstd","tls":"1.3"}
```

All later messages use a 14-byte header:

```text
payload_length:u32-be | flags:u8 | message_type:u8 | request_id:u64-be
```

Payloads are bincode 2. Stream chunks and deltas are always zstd-compressed; other payloads are compressed at 256 bytes. Encoded and decoded payloads are bounded to 8 MiB.

Client message types are requests and cancellation. Broker message types include responses, errors, deltas, stream chunks, and stream completion. Request IDs correlate direct responses and generation cancellation.

Admin model search is broker-only and rechecks the administrator role. It queries the public Hugging Face GGUF catalogue on explicit submission, with a 20-second timeout, a 2 MiB response limit, at most 20 repositories, and at most 128 single-file GGUF choices per repository. Downloads use Ollama's `hf.co/owner/repository:filename.gguf` pull support. No inference runtime or model files are added to the GUI. Protocol 19 includes persisted runtime adapter selection, installed inference plugin IDs and operator overrides, request-scoped download progress and terminal cancellation/completion responses, scoped memory condensation, and conversation-scoped generation context usage in start/end responses. Broker and GUI must upgrade together. Protocol 20 adds image service settings, the image capability flag, unsaved portrait generation, scene image messages, and on-demand message image fetches. Protocol 21 adds an optional seed to `GenerateImage` for consistent emotion sets. Protocol 22 adds the administrator's image workflow template to the image settings. Protocol 23 adds the emotion classifier path to broker settings. Protocol 26 adds the optional memory model to broker settings. Protocol 27 lets `UpdateConversation` change the group speaker mode. Protocol 28 adds memory status and successor to `MemoryEntry`, optional history to `ListMemories`, and the `MemoriesExtracted` response to `ExtractMemory`.

### Image generation

Image generation uses a separately configured image adapter; see [ADR 0005](adr/0005-image-generation-adapters.md). `GenerateImage` returns an unsaved PNG for review, used for character portrait drafts. An optional seed keeps a series consistent: the editor's emotion set requests 12 labelled expressions with one seed, one at a time, saves them at 384 px so a full set fits the 32-image/5 MiB character limit, and skips labels the character already has. `GenerateSceneImage` persists a message with `author_type` `image` and the prompt as content. An empty prompt asks the text model to describe the latest six non-image messages and participant appearance. Image bytes are encrypted in `message_images` and fetched with `GetMessageImage` after an owner check; snapshots and deltas carry only the message. Text prompts exclude image messages entirely, so captions spend no model context: reply history, automatic speaker choice, scene descriptions, and memory extraction all skip them. They are also never sent as mid-history system messages, which many chat templates reject. Image messages cannot be regenerated as variants. The GUI keeps bytes for at most the 12 newest image messages in the open chat.

When the inference and image URLs share a host, `adapters/workload.rs` admits text and image work one kind at a time. Each model-backed call holds a guard for its lifetime, including a reply stream. The first holder of a new kind unloads the other kind: running Ollama models for images, or the image adapter's `ImageUnload` for text. Image work starts only after the runtime confirms no chat model is running; text work frees image memory on a best-effort basis. Waiting work of the other kind blocks new arrivals of the loaded kind. Guards are dropped before follow-up calls, such as memory extraction after a reply, so no task waits on itself.

Reply portraits come from an in-process llama.cpp emotion classifier, not the inference service. Replies persist with the default portrait; a bounded background task later publishes a message update. See [ADR 0006](adr/0006-in-process-emotion-classifier.md).

### Synchronization

Every durable owner-scoped mutation receives a monotonically increasing revision and a delta-log row. Initial login uses a bounded snapshot. Resume requests replay deltas newer than the client's revision; a resume at revision 0 starts at the head because the client reloads its lists after authenticating.

Live deltas are published only to other authenticated connections for the same owner. A lagged subscriber is disconnected so it can resume from the durable log instead of silently skipping revisions.

Some deltas describe a narrow change, such as context or selected variant; others contain the full updated entity. Field-minimal updates are therefore an optimization opportunity, not a current invariant.

### Generation

```text
user message -> persist + delta -> compile context -> select speaker
             -> stream model response -> batch -> compressed chunks
             -> persist message/variant + token usage -> final delta
```

Group chats store ordered participants and one of three speaker modes. An explicit speaker always wins; manual mode requires one. Round-robin follows participant order. Automatic mode answers the participant named earliest in the latest message, as a whole word; otherwise it asks the model, and falls back to round-robin. The latest message's author never answers itself, and single-character chats skip selection work. In a group prompt, only the speaker's own lines are assistant turns; other characters' lines are user turns prefixed with their name, and the system prompt lists the other characters and forbids writing for them.

The compiler combines system rules, the active character, bounded cards for the other group characters, world lore, scoped memories, conversation state, summary, and selected recent message history. Owner-scoped worlds contain character links and facts. Common knowledge is selected first; other enabled facts match keywords in the latest six messages. Character memories are described below.

#### Character memory

See [ADR 0007](adr/0007-structured-character-memory.md) for the rationale.

- **Holders and perspective.** Each memory belongs to one character (`character_id`) and is written in the third person: "the user", and characters by name. Learned memories always name a holder; `character_id` NULL means every character of the owner and is only set manually.
- **Retrieval.** Each reply selects at most ten active memories of the speaking character within a 6144-character budget. Candidates are the top 24 by pinned state and importance plus the top 32 FTS5 matches. The query uses the latest six messages, newest first: at most 24 distinct non-stop words of three or more letters, lightly stemmed and matched as prefixes; bm25 weights rarer terms. Pinned memories come first; the rest rank by `0.5·relevance + 0.3·importance + 0.2·recency` plus a use bonus of at most 0.15. Relevance falls linearly over the 32 FTS ranks, and recency halves every 30 days since last use or update. The prompt introduces the list as memories held by the speaker.
- **Use counts.** Retrieval writes nothing. Selected memories are counted in a bounded in-memory map (4096 memories); the memory job writes the counts in one transaction after each run. Counts pending at a crash are lost; they only affect ranking and are not on the wire.
- **When extraction runs.** Automatic extraction runs after each completed, non-cancelled root reply without delaying the streamed reply. One background job per owner/conversation coalesces overlapping replies into one follow-up run. Explicit `ExtractMemory` uses the same pipeline over the newest 40 rows, never moves the checkpoint, optionally keeps only one holder's operations, and is refused while an automatic job runs for that chat.
- **Memory model.** An optional administrator-selected memory model on the inference server serves extraction and condensation with a context of at most 16384 tokens. Loading it evicts the chat model, so automatic extraction then waits for 60 quiet seconds, restarted by each reply up to six times.
- **One call per batch.** One extraction call covers every participant of a group. Its 40-row input takes rows changed since the checkpoint first, oldest change first, so an edited old message is seen, then the newest unchanged rows as context, shown in timeline order and numbered for evidence. With no changed rows it skips the model call. The prompt lists participants, compact cards, active world lore, and up to 32 saved memories (at least four per holder), most relevant to the new rows first. Unlocked memories get reference numbers; pinned and manual memories are marked locked. Input is budgeted against the model context.
- **Operations.** The model returns at most eight JSON operations: `add` (holder, kind, scope, importance 1–5, content, evidence), `update` (reference, new content), or `retire` (reference). The broker validates every field, maps reference and evidence numbers to the IDs it showed, and drops operations that are invalid, in the wrong perspective, or aimed at locked, foreign, or since-edited rows. An unknown or `all` holder saves one row per participant, so memories never reach characters outside the chat. `add` skips exact text already known from cards or lore, exact holder memories, and near-duplicates (same numbers and negation, at least 85% shared content words and 60% shared ordered word pairs). Missing scope makes events chat-scoped and other kinds cross-chat.
- **History.** `update` writes a new row and marks the old one superseded, pointing at its successor; `retire` marks a row retired. Rows are never deleted by extraction, except that each (owner, holder) keeps at most 200 superseded or retired rows. Retrieval, extraction, and condensation read only active rows; snapshots carry only active rows; `ListMemories` returns history on request.
- **Checkpoint.** Operations, history pruning, and the checkpoint commit in one transaction, and deltas publish after commit. The checkpoint advances only past changed rows that fit the row limit and token budget (including after a valid empty result); the rest stay pending. When no row fits the budget, the run fails and keeps the checkpoint. Failed work remains pending for the next reply; after three consecutive invalid model outputs at the same checkpoint, the checkpoint skips the rows that run would read. Transport errors never skip rows.
- **Condensation.** Explicit condensation groups unpinned active memories by conversation, character, and kind within the model context budget, validates model output, and transactionally supersedes the sources of shortened groups with their replacements while emitting owner-scoped deltas.
- **Deletion.** Deleting a conversation promotes all of its memories, history included, to cross-chat character scope instead of deleting them.

Generation start/end responses carry conversation-scoped prompt/reply token counts and the configured context window. Start counts are estimated from the assembled prompt; final counts use provider usage when available and otherwise remain marked as estimates. The GUI retains one reading per loaded conversation for its composer indicator; these readings are transient, cleared on sign-out, and do not require polling or database writes.

Stream output flushes after 32 whitespace-delimited units, 60 ms, or completion. Each connection has a 32-frame writer queue, which propagates backpressure. Cancellation keys include both connection ID and request ID to prevent cross-client cancellation.

## Data ownership and authorization

- The broker trusts no client-supplied role or ownership claim.
- Session tokens identify users and expire after 30 days. SQLite stores only their SHA-256 hashes; startup rehashes plaintext tokens left by older brokers.
- Login allows ten failed attempts per client IPv4 address or IPv6 /64 in 15 minutes, counting unknown usernames, and verifies a dummy Argon2 hash for them. The in-memory throttle tracks at most 4096 clients and resets on restart. A TCP proxy in front of the broker makes all clients share its address.
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
- Active Ollama lifecycle requests also prevent idle close; pulls have a six-hour upstream timeout. Up to four model downloads run concurrently across the broker. Progress is direct to the requesting connection, reports the current layer, and is coalesced to at most four updates per second plus the terminal update. Downloads never hold the SQLite snapshot gate. Same-model lifecycle operations are rejected while an operation is active; unrelated deletions remain available. Cancellation is scoped by connection and request ID, including before upstream connection establishment. Disconnect or transport failure cancels outstanding pulls. Partial blobs remain managed by the inference server.
- Otherwise a connection quiet in both directions closes after 180 seconds; the GUI reconnects on the next keyboard or mouse input and resumes from its revision.
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
- The main broker dispatch file is large and carries many responsibilities.
- Monitoring is process-local and visible only through the admin request path; there is no metrics export.
- The delta log has no documented retention/compaction policy.

## Decisions

- [ADR 0001: Broker-owned state over a binary TLS protocol](adr/0001-broker-owned-state-and-binary-tls.md)
- [ADR 0002: Hybrid public PKI and private certificate enrollment](adr/0002-hybrid-public-pki-and-private-certificate-enrollment.md)

- [ADR 0003: WASM inference adapter plugins](adr/0003-wasm-inference-adapter-plugins.md)

- [ADR 0004: Build bundled adapters from source](adr/0004-source-built-bundled-adapters.md)

- [ADR 0005: Image generation adapters](adr/0005-image-generation-adapters.md)
- [ADR 0006: In-process emotion classifier](adr/0006-in-process-emotion-classifier.md)
- [ADR 0007: Structured character memory](adr/0007-structured-character-memory.md)

