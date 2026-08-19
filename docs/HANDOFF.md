# Chatty cold-start handoff

Last updated: 2026-08-19. Read this file first when resuming without chat history.

## 2026-08-19 recovery

The GUI was recovered after an incomplete AI refactor replaced the established client with a small, disconnected prototype. The recovery deliberately keeps the useful additions from that refactor: broker resource monitoring and a split GUI source layout.

The active GUI modules are now:

- `main.rs`: application state, event handling, responsive shell, login and deterministic visual fixtures.
- `network.rs`: TLS 1.3 connection, binary protocol, reconnection, protected saved-session handling and ordered send-then-generate commands.
- `conversation.rs`: chat rendering, fixed-bottom composer, compact navigation, create/group/send/continue/cancel/regenerate/delete flows.
- `characters.rs`: responsive character editor, ownership/public controls, SillyTavern JSON/PNG import and JSON export.
- `admin_monitor.rs`: Broker/Users/Data administration and live broker monitoring.
- `ui.rs`: Markdown rendering and shared native widgets.

The invalid generated `import_restore_chatty.rs` fragment was removed. The invalid integration suite that imported a nonexistent `chatty_gui` library was replaced with a real binary-protocol monitoring test. Deterministic visual coverage now renders desktop chat at 1440x900, compact chat at 430x760 and the admin monitor at 1100x760.

The restored original specification, exact compliance matrix, work beyond scope and remaining gaps are preserved separately in `docs/ORIGINAL-PLAN-AUDIT.md`.

## Product state

Chatty is a usable native Rust roleplay-chat MVP with three separate runtimes:

- `chatty-broker`: TLS-only authoritative server, SQLite persistence, RP context compilation, group orchestration, inference adaptation, admin policy and stream optimization.
- `chatty-gui`: eframe/egui desktop client. It is the primary product surface and has no browser, Electron or Node runtime.
- `chatty-client`: terminal diagnostic/automation client.
- External inference remains an OpenAI-compatible llama.cpp/llama-server. It is never linked into the broker or clients.

The current binary wire contract is protocol **version 8**. Broker and clients must be rebuilt together after protocol enum changes.

## Start here

From the repository root:

```sh
./start-chatty.sh
```

This creates development certificates if missing, builds release binaries if missing, starts the broker, waits for its TCP listener, starts the GUI, and stops the broker when the GUI exits. Runtime state is under `.chatty/`:

- `.chatty/chatty.db`: SQLite database
- `.chatty/broker.log`: broker log

The GUI session defaults to `$XDG_STATE_HOME/chatty/session` or `~/.local/state/chatty/session`. Override it with `CHATTY_SESSION_FILE`; inspection mode does not load a real session.

The default inference URL is `http://192.168.0.97:11434/v1`. Environment overrides are listed in `README.md` and `docs/OPERATIONS.md`.

## Current GUI behavior

### Authentication

- Responsive native login UI, dynamically scaled by viewport/platform DPI.
- TLS connection feedback and validation feedback are visible.
- Session token is saved with mode `0600`; passwords are never saved.
- The broker publishes a safe `registration_enabled` capability before login.
- When self-registration is disabled, the Register UI is absent and registration is still rejected server-side.
- The first account on an empty broker becomes admin.

### Core chat

- ChatGPT-like main layout: conversation selector at left, messages and composer at right.
- Empty composer action is `Continue`; non-empty action is `Send`.
- Sending creates a user message and generation request; generation streams from the real adapter.
- Empty database automatically creates a default chat/Assistant character path.
- User bubbles fit short text rather than occupying an arbitrary wide block.
- Character messages render Markdown and visually distinguish the character name.
- AI typing feedback sits above the composer and identifies the active character.
- User messages can be deleted. Character responses can be regenerated; old responses are not retained as visible duplicate messages, while server-side variant selection remains supported.
- Chat deletion uses a borderless hover `×` on the conversation tile.
- No redundant chat title is rendered above the chat; the selector already supplies it.

### Characters and groups

- Central responsive Characters dialog; compact layout stacks list/editor and desktop uses split panes.
- Complete card fields: name, personality, appearance, scenario, system prompt, example dialogue, tags and avatar bytes/path.
- Personality and Appearance are intentionally retained as dedicated Chatty fields even when newer SillyTavern cards omit legacy properties.
- SillyTavern V2 JSON and PNG card import uses the desktop file chooser.
- JSON export preserves Personality and Appearance in legacy-compatible properties and `extensions.chatty` to prevent round-trip loss.
- Create, select, edit, save, export and delete are functional.
- Character action row is one line: Chat, Use, Public toggle, Save, Export, Delete.
- Owned characters may be private/public, subject to broker policy. Public characters are usable by other accounts but not editable by them.
- Creating a chat from a character preselects it. Multiple selected characters create a group chat.
- Manual, round-robin and automatic group modes exist in broker/protocol; automatic selection validates model output and deterministically falls back.

### Sidebar/footer

- No hamburger or application-name chrome.
- Sidebar background fills exactly to the chat split with no width-dependent gap.
- Floating footer contains Online/Offline, role-gated Admin, Sync and Out.
- Compact view swaps between sidebar/chat without polling.

### Administration

The role-gated central Admin dialog has three tabs:

- **Broker**: enable/disable inference adapter, edit adapter base URL, allow/disallow non-admin public character publishing, allow/disallow public self-registration. Settings persist in SQLite and are enforced by the broker.
- **Users**: refresh users, create User/Admin accounts even when public registration is disabled, promote/demote, and reveal a borderless `×` on row hover to delete an account. The active admin cannot demote or delete itself.
- **Data**: sanitized read-only view of user metadata, character metadata and adapter status. It deliberately excludes password hashes, session tokens, credentials, messages and conversation content. Character public state can be toggled here by an admin.

While Admin > Broker is visible, the client requests a compact monitor snapshot every two seconds and sleeps again when the dialog/tab closes. Snapshots contain sampled process CPU/RAM, connection count, errors, and broker-probed adapter health (disabled/online/offline, model count, and latency). The adapter enable switch and URL edit the broker's persisted authoritative configuration; ordinary chat users never probe or receive adapter details.

`start-chatty.sh` supervises the broker process. Admin soft reboot deliberately disconnects clients, the launcher restarts the broker, and clients reconnect and resume their saved sessions until it is available. The systemd unit independently uses `Restart=always` for installed deployments.

User deletion is transactional. It removes sessions, owned lore/memory/conversations/characters/deltas and participant references before deleting the user. This avoids half-deleted accounts and foreign-key failures.

## Broker and protocol facts

- TLS 1.3 only using rustls; clients pin the configured CA. There is no plaintext mode.
- One JSON handshake only: `{protocol:8, encoding:bincode2, compression:zstd, tls:1.3}`.
- Runtime frames: `[length:u32 BE][flags:u8][message_type:u8][request_id:u64 BE][payload]` (14-byte header).
- Runtime serialization: bincode 2. zstd is forced for stream/delta frames and used for payloads at least 256 bytes.
- Payload limit is 8 MiB; decompression is bounded.
- Persistent connection, reconnect/resume, no polling and no heartbeat traffic.
- Writer queue is bounded at 32 frames for backpressure.
- llama SSE is batched at 32 whitespace units or 60 ms; never one client packet per token.
- Adapter URL/enabled state is read from persisted broker settings. Disabled adapter returns a request error without stopping the listener.
- `/models` is probed dynamically and generation uses an actual returned model ID.
- Auth uses Argon2 password hashes and expiring opaque session tokens.
- Broker owns all authorization. GUI visibility is never the security boundary.

Protocol admin requests currently include listing/creating/deleting users, setting roles, reading/updating broker config, sanitized database inspection and public-character overrides. Public pre-auth capability exposure is limited to the registration-enabled boolean.

## SQLite

Migrations live in `crates/chatty-broker/migrations/`:

1. `0001_initial.sql`: users, sessions, RP entities, deltas and indexes.
2. `0002_conversation_context.sql`: persistent conversation summary/context.
3. `0003_normalize_variants.sql`: normalized swipe/variant persistence.
4. `0004_public_characters.sql`: public character flag/index.
5. `0005_broker_settings.sql`: singleton adapter and policy settings.

The broker opens a five-connection pool, runs migrations, enables WAL and is authoritative. Queries are owner-scoped and bounded. Do not expose raw tables through admin UI: sessions and password hashes are sensitive, and chat content was explicitly excluded from the admin database view.

## Terminal client

Run `target/release/chatty-client`, then type `help`. Important account commands:

```text
register USER PASSWORD        # only when broker self-registration is enabled
login USER PASSWORD
users                         # admin
role USER_ID admin|user       # admin
useradd USER PASSWORD ROLE    # admin; ROLE is user or admin
userdel USER_ID               # admin
```

The terminal client is diagnostic, not the primary UX. Keep it synchronized with protocol changes.

## GUI inspection and visual regression

The repository has deterministic egui inspection support. It never logs in or contacts production:

```sh
./chatty-gui-inspect.sh
```

Default FIFO: `/tmp/chatty-gui-control`. Commands include:

```text
resize WIDTH HEIGHT
zoom FACTOR
screenshot /tmp/output.png
sidebar open|closed
tools open|closed
screen main|login
quit
```

Run all visual tests plus GUI lint/tests/release checks:

```sh
/home/digitech/.codex/skills/debug-native-egui-ui/scripts/run-visual-checks.sh chatty-gui visual_
```

Visual baselines are rendered at desktop `1440x900` and compact `430x760`. Relevant current screenshots are written under `/tmp/chatty-*.png`, including core chat, Markdown, message actions, character dialogs, admin Broker/Users/Data tabs, registration-enabled/disabled login, typing feedback and new-chat UI.

## Verification baseline

At handoff, all commands below passed on this x86-64 host:

```sh
cargo fmt --all -- --check
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo build --release -p chatty-broker -p chatty-gui -p chatty-client --offline
```

Current post-recovery automated count is 26 tests: broker 7, terminal client 2, GUI 10 (eight rendered visual cases, one live-monitor behavior test, and one monitoring protocol integration case), and protocol 7. The older 45-test GUI suite described by the pre-damage handoff was not present in recoverable source and must not be claimed as restored.

Network/stream harnesses:

```sh
./scripts/userspace-network-test.sh
./scripts/multiclient-test.sh
./scripts/stream-soak-test.sh
sudo ./scripts/network-namespace-test.sh
sudo ./scripts/network-test.sh TEST_IF 'cargo test --workspace'
```

ARM64 compilation has now been performed: `cargo build --release --workspace` on an `aarch64-unknown-linux-gnu` host produced native ARM64 `chatty-broker` and `chatty-gui` binaries. The broker has no GUI dependencies and a systemd unit exists at `packaging/chatty-broker.service`.

## Important implementation locations

- Shared wire types/framing: `crates/chatty-protocol/src/lib.rs`
- Broker dispatch, persistence, RP compiler and adapter: `crates/chatty-broker/src/main.rs`
- GUI state/shell and visual tests: `crates/chatty-gui/src/main.rs`
- GUI networking: `crates/chatty-gui/src/network.rs`
- Chat UI/actions: `crates/chatty-gui/src/conversation.rs`
- Character UI/import/export: `crates/chatty-gui/src/characters.rs`
- Admin monitoring and controls: `crates/chatty-gui/src/admin_monitor.rs`
- CLI commands and local delta state: `crates/chatty-client/src/main.rs`
- Startup: `start-chatty.sh`
- Certificates: `scripts/create-dev-cert.sh`
- Original plan audit: `docs/ORIGINAL-PLAN-AUDIT.md`
- Architecture/operations: `docs/architecture/ARCHITECTURE.md`, `docs/OPERATIONS.md`

The broker remains concentrated in `main.rs`; the GUI is intentionally split along the boundaries above. Protocol enum ordering remains bincode-sensitive.

## Known constraints and next work

- ARM64 native build is verified (`cargo build --release --workspace` on aarch64); real Pi deployment/runtime verification on device remains unverified.
- The broker-to-llama HTTP hop assumes a trusted LAN/private interface; TLS is mandatory only on client-to-broker today.
- Admin adapter changes are persisted but there is no explicit “Test adapter” button/status probe in the dialog yet.
- Admin user deletion is immediate on hover-`×`, matching chat behavior; there is no confirmation dialog.
- Public-character policy is always enforced server-side. A non-admin editor may temporarily show a Public control until a failed save if its session has not received policy beyond the public registration capability; improve capability reporting if desired.
- Admin sanitized Data view is intentionally curated rather than a general SQL browser.
- Session storage is a protected file, not an OS keyring.
- Delta retention/compaction policy is not implemented.
- Character PNG export is not implemented; import supports PNG and JSON, export is JSON.
- The workspace has no discoverable Git repository at this path (`git status` reports not a repository). Establish/version the repository before relying on diffs or commits.

## Non-negotiable product direction

- Keep broker/client transport TLS 1.3, binary, compressed, persistent, batched and backpressure-aware.
- Do not introduce Electron, a browser runtime, Node client, JSON runtime streaming, polling, per-token frames or embedded inference.
- Keep the broker as authoritative RP orchestrator, not a transparent inference proxy.
- Preserve low idle CPU, low memory, narrow-screen responsiveness and the dark frosted-glass native visual direction.
