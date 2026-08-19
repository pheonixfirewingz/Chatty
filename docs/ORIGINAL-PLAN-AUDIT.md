# Original plan and implementation audit

Last audited: 2026-08-19 against client/broker protocol version 8.

This document preserves the restored original project specification and maps it to the repository as it exists now. Status terms:

- **Met**: implemented in the current source and covered by an automated or documented live path.
- **Beyond**: the requirement is met and additional product behavior now exists.
- **Partial**: the core exists, but an exact detail remains incomplete or unverified.
- **Excluded**: deliberately outside the accepted scope for this development host.

## Original specification

> # Project: Lightweight Native Multi-Character Roleplay Platform
>
> You are Codex operating directly inside the development environment. Your task is to inspect the machine and repository, produce an implementation plan grounded in what is actually available, then build a complete minimum viable product.
>
> Do not treat this prompt as a request for an architectural essay. You are expected to inspect, plan, implement, compile, test, profile, fix, and leave behind a usable application.
>
> Do not ask for approval between normal engineering decisions. Investigate first and make sensible choices.
>
> The required result is a lightweight native roleplay-chat platform consisting of:
>
> 1. a very small Rust broker
> 2. a lightweight native client
> 3. an external llama.cpp / llama-server inference backend
>
> The AI runtime is never integrated into either client or broker.
>
> ## 1. SECURE + LOW-BANDWIDTH CLIENT ↔ BROKER TRANSPORT (HARD REQUIREMENT)
>
> ### 1.1 TLS 1.3 mandatory
>
> - No plaintext mode
> - No dev bypass in production
> - Enforced encryption always
> - Rust broker: `rustls`
> - Client: rustls or platform TLS
> - Self-signed cert allowed but MUST be pinned/verified
>
> ### 1.2 Binary protocol (NO JSON streaming)
>
> Allowed JSON usage:
>
> - initial handshake only
> - admin/debug tools only
>
> Streaming + runtime MUST be binary. Preferred encoding: `bincode` OR `postcard`.
>
> ### 1.3 Frame protocol (mandatory)
>
> ```text
> [length: u32/u64][compression_flag: u8][message_type: u8][request_id: u64][payload]
> ```
>
> ### 1.4 Compression (mandatory)
>
> - Use `zstd` (preferred) or `lz4`
> - Applied to chat streams, state deltas and large payloads
>
> ### 1.5 Streaming rules (critical)
>
> - NO per-token messages
> - Batch tokens (16–64 tokens OR 30–100ms window)
> - Compress batches before sending
> - Backpressure-aware streaming required
>
> ### 1.6 Connection model
>
> - Persistent TLS connection
> - No reconnect-per-request
> - No polling
> - No heartbeat spam (only >60s idle if needed)
>
> ### 1.7 Bandwidth philosophy
>
> Optimize for mobile hotspot usage, rural/low bandwidth, Raspberry Pi hosting and long RP sessions.
>
> ## 2. Core objective
>
> Build a native roleplay-chat system comparable to SillyTavern but fully native, low memory, multi-character group chat, persistent world state, strong RP orchestration, external inference only and race-to-idle design.
>
> NO Electron. NO browser runtime. NO Node.js client.
>
> ## 3. Existing inference server
>
> ```text
> http://192.168.0.97:11434/v1
> ```
>
> Broker MUST dynamically probe `/v1/models`, `/v1/chat/completions`, streaming support and actual model IDs. No hardcoding.
>
> ## 4. FULL SYSTEM ARCHITECTURE
>
> ```text
> Native Client
>    │ TLS + binary + compressed frames
>    ▼
> Rust Broker
>    ├── Auth / Accounts
>    ├── Permissions
>    ├── SQLite persistence
>    ├── RP engine
>    ├── Context compiler
>    ├── Group chat orchestration
>    ├── Streaming optimizer
>    ├── Delta state engine
>    ├── Backend adapter
>    ▼
> llama-server (OpenAI-compatible HTTP)
> ```
>
> ## 5. Broker responsibilities
>
> Core systems: authentication, user accounts, admin roles, permissions, SQLite persistence, character system, conversation system, message system, variants system, lore system, memory system, group chat system and RP context compiler.
>
> Transport systems: TLS termination, binary encoding/decoding, compression/decompression, delta computation, stream batching, bandwidth scheduling and frame validation.
>
> ## 6. Broker language
>
> Rust stable with `tokio`, `rustls`, `tokio-rustls`, `zstd`, `serde` internally, `bincode` or `postcard`, `sqlx` SQLite and `bytes`.
>
> ## 7. Streaming architecture
>
> ```text
> llama-server
>    ↓ HTTP stream
> Broker (batch + compress + optimize)
>    ↓ TLS binary frames
> Client
> ```
>
> Broker is NOT a proxy. It is a stream optimizer + RP orchestrator.
>
> ## 8. FULL RP SYSTEM
>
> ### 8.1 Characters
>
> `id`, `name`, `personality`, `scenario`, `system prompt`, `example dialogue`, `appearance`, `tags`, `avatar`.
>
> ### 8.2 Conversations
>
> `id`, type (`direct` / `group`), participants, title and state.
>
> ### 8.3 Messages
>
> `id`, author (`user`/`character`/`system`), content, `parent_id` for branching and timestamp.
>
> ### 8.4 Variants (swipes)
>
> Multiple generated responses per message, selectable variants and persistent history.
>
> ### 8.5 Lore system
>
> Keyword-triggered entries, always-on entries and priority-based injection.
>
> ### 8.6 Memory system
>
> Persistent facts, manual + optional AI extraction, scoped per character or conversation.
>
> ### 8.7 Context compiler
>
> Builds prompts from system rules, character card, group participants, lore, memory, recent messages and summary.
>
> ## 9. GROUP CHAT SYSTEM
>
> - Manual: user selects speaker.
> - Round robin: fixed order rotation.
> - Automatic: broker selects speaker via lightweight LLM call.
> - Automatic selection must validate output, deterministically fall back if invalid and never create infinite AI loops.
>
> ## 10. CLIENT REQUIREMENTS
>
> Maintain TLS connection, decode binary frames, decompress zstd, apply delta updates, render streaming chunks, never request full state repeatedly and support offline reconnection.
>
> ## 11. DELTA SYSTEM
>
> All updates are incremental:
>
> ```text
> { entity_id, operation, changed_fields }
> ```
>
> Operations: `ADD`, `UPDATE`, `DELETE`. No full resync unless explicitly requested.
>
> ## 12. MEMORY + BANDWIDTH RULES
>
> No polling, no full conversation resend, no repeated sync of static data, no heartbeat spam and no redundant updates.
>
> ## 13. STREAMING RULES (STRICT)
>
> Batch tokens, compress chunks, require backpressure, support cancellation and never send per-token packets.
>
> ## 14. ERROR HANDLING
>
> Handle TLS failure, frame corruption, compression mismatch, partial frame recovery, stream desynchronization, cancellation mid-stream, backend disconnect, missing model and llama-server HTTP failure.
>
> ## 15. SQLITE
>
> WAL allowed, migrations required, no full-table loads, indexed queries required and broker authoritative.
>
> ## 16. SECURITY MODEL
>
> TLS mandatory, session tokens required, no plaintext credentials after login, role enforcement server-side only and no client trust assumptions.
>
> ## 17. PERFORMANCE MODEL
>
> Priorities: bandwidth reduction, packet reduction, CPU efficiency, memory stability, simplicity.
>
> ## 18. RASPBERRY PI TARGET
>
> ARM64 Linux, systemd service, low idle CPU, low RAM usage and no GUI dependencies in broker.
>
> ## 19. TESTING
>
> Bandwidth simulation, packet-loss simulation, high-latency tests, compression-ratio tests, streaming stress tests and cancellation stress tests.
>
> ## 20. RACE-TO-IDLE
>
> Sleep when idle, avoid background loops, polling, unnecessary timers and idle network traffic.
>
> ## 21. DEFINITION OF DONE
>
> Transport: TLS enforced, binary protocol active, compression active, batching active, no JSON streaming, no per-token packets and no polling.
>
> RP: characters, conversations, group chat, variants, lore, memory and context compiler all work.
>
> Integration: real llama-server streaming, cancellation, reconnection and persistence work.
>
> ## 22. FINAL INSTRUCTION
>
> Implement binary protocol/frame format, TLS broker listener, framing/compression, batched streaming, then RP systems from characters through generation. Do not start with a JSON API, build a web architecture or treat the broker as a simple proxy.
>
> This is a secure, low-bandwidth, real-time RP orchestration system with AI streaming.

## Compliance summary

| Original area | Status | Current evidence | Beyond the original plan |
|---|---|---|---|
| Three-runtime separation | **Met** | Separate broker, GUI/terminal clients and external OpenAI-compatible server; no inference crate/runtime is embedded | Two native clients exist: a product GUI and diagnostic CLI |
| TLS 1.3 and certificate pinning | **Met** | rustls TLS 1.3-only configs; explicit CA file; certificate creation and rejection paths | One launcher creates missing development certificates and wires the pinned CA automatically |
| Binary runtime protocol | **Met** | Protocol-v8 JSON handshake only; bincode 2 for requests, responses, deltas and streams | Handshake versioning deliberately rejects mismatched clients |
| Fixed frames | **Met** | 14-byte `u32 + u8 + u8 + u64` header in `chatty-protocol`; size/type/flag validation | 8 MiB pre-allocation limit and bounded zstd expansion defend memory |
| Compression | **Met** | zstd forced on streams/deltas and applied to other payloads from 256 bytes | Automated compression-ratio and decompression-limit tests |
| Batching/backpressure | **Met** | Flush at 32 whitespace units or 60 ms; bounded 32-frame per-connection writer queue | Slow consumers propagate backpressure rather than growing unbounded buffers |
| Persistent/no-idle-traffic connection | **Met** | One TLS connection, event-driven reads, reconnect/resume only after failure, no poller/heartbeat | Same-account live delta broadcast prevents manual refresh across devices |
| Native client/no web runtime | **Met** | Rust eframe/egui GUI and Rust CLI; no Electron/browser/Node client | Responsive compact/desktop UI and deterministic native screenshot harness |
| Dynamic inference adapter | **Beyond** | `/models` probe, actual returned model ID, SSE content-type validation and streamed `/chat/completions` | Admins can persistently enable/disable the adapter and edit its URL at runtime |
| Broker orchestration | **Met** | Broker compiles context, selects speakers, persists generations, batches streams and emits deltas | Automatic chat naming and optional AI memory extraction also use bounded model calls |
| Accounts/auth/permissions | **Beyond** | Argon2, opaque expiring sessions, roles, first-admin creation and broker-side authorization | Saved GUI sessions without saved passwords; dynamic registration policy; admin create/delete/promote/demote UI and CLI |
| Characters | **Beyond** | Every required field persists, including avatar bytes, tags, Appearance and Personality | SillyTavern V2 PNG/JSON import, JSON export, Chatty extension round-trip preservation and public sharing |
| Conversations/messages | **Met** | Direct/group conversations, ordered participants, state/summary, all author roles, parent links and timestamps | Default Assistant/chat creation, ChatGPT-like UI, Markdown, typing identity and message deletion/regeneration |
| Variants/swipes | **Met** | Normalized variants, selection validation/persistence and selected-context use | GUI regeneration avoids showing old responses as duplicate timeline messages |
| Lore | **Met** | Keyword matching, always-on behavior, priorities, scopes, persistence and context injection | Full CLI CRUD/debug surface |
| Memory | **Beyond** | Persistent conversation/character-scoped facts and context injection | Optional validated AI fact extraction through external inference |
| Context compiler | **Met** | Rules, full speaker card, bounded group cards, lore, memory, state, summary and selected recent history | Explicit system messages and selected variant history are preserved in model context |
| Manual/round-robin/automatic groups | **Met** | All three modes persist; explicit speaker validation; turn index; bounded automatic choice and deterministic fallback | Character dialog/new-chat flow preselects one or multiple characters |
| Client streaming/reconnection | **Met** | Binary/zstd decode, streamed rendering, cancellation, revision cursor, `Snapshot` then `Resume` | Saved-session restoration and expired-session recovery |
| Incremental deltas | **Partial** | Typed `ADD`/`UPDATE`/`DELETE`, revision log, live push and reconnect replay are implemented | Context and variant selection have narrow delta payloads; however, many ordinary entity updates still encode the full changed entity rather than a minimal per-field mask |
| No redundant state transfer | **Met** | Explicit initial Snapshot, incremental resume and conversation fetch only on selection; no polling | Live mutation broadcast to other connections for the same owner |
| Error handling | **Met** | TLS rejection, fragmented reads, corrupt/oversized frame rejection, zstd bounds, sequence-gap detection, cancellation and upstream HTTP/SSE errors | Lagged live-delta clients reconnect/resume instead of silently losing revisions |
| SQLite | **Beyond** | Five migrations, WAL, five-connection pool, indexed/bounded owner-scoped queries | Persisted broker policy, public character index and sanitized admin metadata view |
| Security model | **Beyond** | Mandatory TLS, Argon2, tokens, tenant predicates and server-only role checks | Registration/publishing policies are server-enforced; admin data excludes hashes, tokens and chat content; user deletion blocks self-delete and is transactional |
| Pi/race-to-idle | **Met** | GUI-free broker, event-driven idle behavior, systemd unit and previously measured low idle CPU/RSS | Hardened systemd settings and 256 MiB memory ceiling; ARM64 build is now performed natively on an aarch64 host |
| Network/stress testing | **Met** | Fragmentation, compression, bounded transport and cancellation tests plus user-space shaping, multiclient and stream-soak scripts | Guarded kernel namespace/interface netem scripts cover loss, delay, duplication and reordering when privileges exist |
| Usable application | **Beyond** | `start-chatty.sh` builds/starts broker+GUI and cleans up; persistence and real adapter paths exist | Native file chooser, frosted dark UI, responsive layouts, admin dialog and GUI automation/visual regression tooling |

## Exact requirement audit

### 1. Transport

| Requirement | Result | Where |
|---|---|---|
| TLS 1.3 only | **Met** | `crates/chatty-broker/src/main.rs`, `crates/chatty-gui/src/main.rs`, `crates/chatty-client/src/main.rs` |
| Pinned/verified certificate | **Met** | Client RootCertStore loads only configured CA; `scripts/create-dev-cert.sh` |
| JSON handshake only | **Met** | Broker `serve` handshake; all runtime types in `chatty-protocol` use bincode |
| Required frame fields | **Met** | `crates/chatty-protocol/src/lib.rs` |
| zstd streams/deltas/large payloads | **Met** | Shared frame writer/compression policy and tests |
| 16–64 units or 30–100 ms batch | **Met** | 32 whitespace-unit or 60 ms broker flush policy |
| Backpressure | **Met** | Bounded Tokio channel per connection |
| Persistent/no polling/no heartbeat | **Met** | GUI/CLI network loops and broker connection task |

### 2. RP and persistence

| Requirement | Result | Where |
|---|---|---|
| Required character fields | **Met** | Protocol `CharacterInput`/`Character`, characters migration/table and GUI editor |
| Conversation types/participants/state | **Met** | Protocol kinds, broker CRUD, participants/context migrations |
| Messages/parent/timestamp/authors | **Met** | Initial schema, broker validation/context queries |
| Variants/swipes | **Met** | `0003_normalize_variants.sql`, broker selection/generation, GUI swipe state |
| Lore | **Met** | Broker lore handlers/context query and GUI/CLI fields |
| Memory/manual + AI | **Beyond** | Broker memory handlers and `ExtractMemory` adapter call |
| Context compiler | **Met** | Broker context-building/generation functions |
| Three group modes | **Met** | `ConversationKind`, `choose_speaker`, persistent turn index |
| Migrations/WAL/indexes | **Met** | Five SQL migrations and broker startup |

### 3. Integration and operations

| Requirement | Result | Where |
|---|---|---|
| Dynamic model discovery | **Met** | Broker `probe_backend` |
| Real SSE streaming | **Met** | Broker generation adapter and documented live/soak path |
| Cancellation | **Met** | Connection-scoped cancellation registry and Cancel frames |
| Reconnection | **Met** | GUI/CLI reconnect plus Snapshot/Resume logic |
| Race-to-idle | **Met** | No connected-state loop/timer/poller; event-driven tasks |
| systemd | **Met** | `packaging/chatty-broker.service` |
| ARM64 build | **Met** | Native ARM64 release built on an `aarch64-unknown-linux-gnu` host via `cargo build --release --workspace`; broker has no GUI dependencies |
| Bandwidth/loss/latency/stress tests | **Met** | `scripts/userspace-network-test.sh`, `multiclient-test.sh`, `stream-soak-test.sh`, netem scripts and Rust tests |

## Work added after the original plan

These features were not required by the original specification but now define the product:

1. A polished responsive eframe/egui GUI with desktop/compact layouts, dark translucent/frosted styling and ChatGPT-like chat composition.
2. Deterministic inspection mode, FIFO app control, screenshots and egui accessibility-driven UI tests.
3. Saved login sessions with protected file permissions and automatic expired-session fallback.
4. Dynamic pre-login registration capability so disabled registration is absent from the UI as well as rejected by the broker.
5. Persistent admin-configurable inference URL/enabled state.
6. Persistent server policies for self-registration and non-admin public-character publishing.
7. Admin-created accounts, role management and transactional account deletion through GUI and CLI.
8. Sanitized read-only admin database metadata rather than an unsafe raw database browser.
9. Cross-user public character sharing with broker-enforced ownership and admin visibility overrides.
10. SillyTavern V2 JSON/PNG import and JSON export with Chatty Personality/Appearance extension preservation.
11. Markdown rendering, responsive user bubbles, character typing identity and streamlined message/chat actions.
12. Automatic default chat/Assistant creation and model-generated conversation naming.
13. A single launcher for certificates, release build, broker readiness, GUI startup and cleanup.

## Remaining gaps relative to the strictest reading

1. Entity deltas are incremental and revisioned, but not every update is a minimal changed-field patch; many carry the complete changed entity.
2. ARM64 compilation is now executed natively on an aarch64 host; deployment/runtime verification on a real Pi device has not been executed.
3. Upstream broker-to-llama HTTP is assumed to stay on a trusted LAN/private interface; the original mandatory TLS boundary was client-to-broker.
4. Character PNG export is not implemented. PNG and JSON import plus JSON export are implemented; PNG export was beyond, not part of the original definition of done.
5. There is no durable delta-retention/compaction policy yet, which matters for very long-lived installations.

## Audit evidence

The handoff baseline immediately before this audit passed:

```sh
cargo fmt --all -- --check
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo build --release -p chatty-broker -p chatty-gui -p chatty-client --offline
```

That baseline contains 45 tests: broker 6, terminal client 2, GUI 30 and protocol 7. See `docs/HANDOFF.md` for operational details and `docs/MVP-CHECKLIST.md` for the concise acceptance matrix.
