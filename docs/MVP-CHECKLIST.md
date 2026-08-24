# Restored-plan feature audit

This is the acceptance matrix for the restored MVP specification. “Verified” means the current source and the named test or live path both support the claim.

| Plan requirement | Status | Evidence |
|---|---|---|
| TLS 1.3 mandatory; no plaintext listener | Verified | Both rustls configs allow only TLS 1.3; pinned local-CA handshake passed and an invalid certificate was rejected |
| Binary runtime protocol; JSON handshake only | Verified | Protocol-v9 handshake is JSON; every request, response, delta, and stream payload uses bincode |
| Fixed length/flag/type/request-id frame | Verified | Shared 14-byte header implementation and fragmented round-trip tests |
| zstd for streams, deltas, and large payloads | Verified | Stream/delta frames force zstd; compression activation and ratio tests pass |
| 16–64 unit or 30–100 ms batching | Verified | Broker flushes at 32 whitespace units or 60 ms; no llama token event is sent directly |
| Backpressure and bounded memory | Verified | Per-client queue has 32 frames; 1,000-frame tiny-buffer stress and decompression-limit tests pass |
| Persistent connection, no polling or heartbeat spam | Verified | Event-driven connection loop has no connected-state timers; retries occur only while disconnected |
| Dynamic llama models and streaming support | Verified | `/models` IDs are discovered at runtime; `/chat/completions` must return `text/event-stream`; live SSE generation passed |
| Accounts, sessions, admin roles, permissions | Verified | Argon2 passwords, expiring/saved tokens, atomic first admin, registration policy, admin account create/delete, role mutation, permission query, logout, and server-side checks |
| Broker administration | Verified | Persisted adapter enabled/URL settings, self-registration/publication policy, sanitized database metadata and character visibility overrides are role-gated and broker-enforced |
| Tenant isolation | Verified | Owner predicates cover RP reads/mutations; automated cross-tenant character mutation is rejected |
| Characters: all card fields, tags, avatar | Verified | Protocol/database contain every field; `charfull` supports create/update and local avatar-file upload |
| Conversations: direct/group, participants, title, state | Verified | Persistent ordered participants, modes, world state, and summary; live restart/readback passed |
| Messages: user, character, system, parent, timestamp | Verified | All author types persist; character authors must be participants; system role is preserved in model context |
| Variants/swipes and selection | Verified | Branch messages and variants persist; selection is validated, delta-applied, and used instead of unselected branches in context; live swipe/readback passed |
| Lore: keyword, always-on, priority injection | Verified | Scoped CRUD/read client paths and priority query; selected-context keyword matching; live prompt path passed |
| Memory: character/conversation scope | Verified | Scoped CRUD/read client paths and context injection; validated optional AI extraction through the external model; live persistence passed |
| Context compiler | Verified | Includes rules, complete speaker card, bounded group cards, selected history, lore, memory, world state, and summary |
| Manual group mode | Verified | Requires an explicit participant speaker; automated test covers rejection without one |
| Round-robin group mode | Verified | Persistent turn index and deterministic selection; automated test passes |
| Automatic group mode | Verified | One bounded lightweight model call receives recent RP, validates a participant name, and deterministically falls back; automated fallback test passes |
| No infinite AI loops | Verified | Each generation performs at most one speaker-selection call and one response call; another turn requires a new client request |
| Typed ADD/UPDATE/DELETE deltas | Verified | Full typed payloads, correct upsert operations, transactional revision log, immediate mutation delivery, typed client application tests |
| Explicit initial state and incremental reconnect | Verified | One race-free typed `Snapshot` after authentication; only `Resume` deltas thereafter; restart/resume path passed |
| Simultaneous-client live updates | Verified | Committed deltas are pushed through a bounded broker channel only to other connections authenticated as the same owner; origin and tenant isolation tests pass |
| Cascade deletion consistency | Verified | Conversation deletes emit child message/lore/memory deltas transactionally; recursive message branches and character references are handled; automated test passes |
| Cancelable streams | Verified | Per-connection/request watch registry interrupts HTTP response wait or body stream; live cancellation and 1,000-request stress passed |
| Stream desynchronization detection | Verified | Client validates message ID and monotonically contiguous sequence; gap/wrong-stream tests pass |
| TLS/frame/compression/backend errors | Verified | TLS rejection, unknown flags/types, size bounds, bounded zstd expansion, malformed SSE, HTTP/model errors, and disconnect propagation |
| SQLite migrations, WAL, indexes, bounded queries | Verified | Five migrations, five-connection pool, WAL, context/sync/public indexes, persisted broker settings, streamed snapshots/deltas, bounded context/list queries |
| Race-to-idle | Verified | No background pollers, heartbeat, or maintenance loops; release broker measured 9.7 MiB RSS and 0.0% settled idle CPU on this host |
| Mobile/rural network testing | Verified; kernel netem profile included | User-space real-TLS test passed with forced reconnect, 113-byte fragmentation, 75±25 ms latency, deterministic 3% TCP retransmission-delay loss, and 256 kbit/s; guarded namespace/dedicated-interface netem profiles additionally cover kernel duplication/reordering when administrator credentials are available |
| Streaming/cancellation soak | Verified | Deterministic external OpenAI-compatible test server completed 100 SSE generations, AI memory extraction, and 100 mid-stream cancellations without frame desynchronization |
| Raspberry Pi deployment | Implemented | GUI-free Rust broker, no embedded inference, hardened systemd unit, bounded resources; native ARM64 release built and verified on an aarch64 host |

## Verification commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

Live protocol verification used a fresh database under `/tmp`, the pinned TLS client, and the configured llama-server. It covered registration, permissions, a complete character card, state/summary, system/user messages, lore, scoped memory, generation, a new-process typed snapshot, broker restart, and incremental resume. Current local broker/GUI/client builds use protocol version 9.
