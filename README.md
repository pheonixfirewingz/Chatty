# Chatty

Chatty is a small native Rust roleplay platform: a TLS-only broker, native GUI and terminal clients, and an external OpenAI-compatible llama.cpp server. The broker owns persistence and RP orchestration; it never embeds inference.

For a complete cold-start description of the implementation, UI, admin controls, verification state, constraints and next work, read [the handoff](docs/HANDOFF.md). The preserved project brief and requirement-by-requirement comparison are in [the original-plan audit](docs/ORIGINAL-PLAN-AUDIT.md).

## Quick start

Prerequisites: Rust, OpenSSL (certificate creation), SQLite, and a running `llama-server`.

To build anything missing and launch the broker plus GUI together:

```sh
./start-chatty.sh
```

Closing the GUI also stops the broker. Per-user files follow the Linux XDG Base
Directory Specification in debug and release builds:

- database: `$XDG_DATA_HOME/chatty/chatty.db` or `~/.local/share/chatty/chatty.db`
- session: `$XDG_STATE_HOME/chatty/session` or `~/.local/state/chatty/session`
- launcher log: `$XDG_STATE_HOME/chatty/broker.log` or `~/.local/state/chatty/broker.log`

The launcher copies a legacy `.chatty/chatty.db` into the XDG data directory on
first use and leaves the original as a backup. Existing `CHATTY_*` environment
variables override the defaults.

```sh
./scripts/create-dev-cert.sh
cargo build --release
CHATTY_LLAMA_URL=http://192.168.0.97:11434/v1 cargo run --release -p chatty-broker
cargo run --release -p chatty-client
cargo run --release -p chatty-gui
```

The certificate script pins `localhost` and `127.0.0.1`. Copy only `certs/ca.pem` to each client through a trusted channel and pass it with `--ca`; clients trust only that local CA and do not accept insecure TLS. Keep `ca.key` offline in real deployments and set `--server-name` to a SAN in the deployed certificate.

Both native clients maintain one connection and automatically reconnect/resume after an outage. The GUI covers accounts and admin roles, complete character cards, SillyTavern import/JSON export, public sharing, direct/group conversations, messages, Markdown, lore, memory, swipes, streamed generation, cancellation and CRUD actions. Admins can persist adapter/policy configuration, manage users, override character visibility and inspect sanitized non-chat metadata. The first registered account becomes admin. Passwords must be at least ten characters. Runtime messages use bincode; stream/delta frames are always zstd-compressed, as are other payloads of at least 256 bytes. The current wire contract is protocol version 8.

Selected commands:

```text
help
register USER PASSWORD
login USER PASSWORD
users
role USER_ID admin|user
useradd USER PASSWORD user|admin
userdel USER_ID
char NAME
charfull ID|-|NAME|PERSONALITY|SCENARIO|SYSTEM|EXAMPLE|APPEARANCE|TAG,TAG|AVATAR_PATH|-
conversation TITLE CHARACTER_ID
conversation-update CONVERSATION_ID TITLE|CHARACTER_ID,CHARACTER_ID
group manual|round|auto TITLE|CHARACTER_ID,CHARACTER_ID
state CONVERSATION_ID WORLD_STATE|SUMMARY
send CONVERSATION_ID TEXT
speak CONVERSATION_ID CHARACTER_ID TEXT
system CONVERSATION_ID TEXT
generate CONVERSATION_ID [CHARACTER_ID]
swipe CONVERSATION_ID PARENT_MESSAGE_ID
select MESSAGE_ID VARIANT_ID
lore CONVERSATION_ID|- KEY,KEY|CONTENT
lorefull ID|-|CONVERSATION|-|ALWAYS_TRUE_OR_FALSE|PRIORITY|KEY,KEY|CONTENT
memory CONVERSATION_ID|- CONTENT
memoryfull ID|-|CONVERSATION|-|CHARACTER|-|CONTENT
memory-extract CONVERSATION_ID CHARACTER_ID|-
delete character|conversation|message|lore|memory ENTITY_ID
```

Authentication performs one explicit typed snapshot. After that, reconnects request only deltas newer than the client's revision cursor.
Mutations are also pushed immediately to every other authenticated connection owned by the same account. Other accounts never receive those deltas.

## Configuration

Broker flags have matching environment variables: `CHATTY_LISTEN`, `CHATTY_DATABASE`, `CHATTY_CERT`, `CHATTY_KEY`, and `CHATTY_LLAMA_URL`. The launcher also accepts `CHATTY_LOG_FILE`. Client variables are `CHATTY_BROKER`, `CHATTY_SERVER_NAME`, `CHATTY_CA`, and optional `CHATTY_SESSION_FILE`.

`CHATTY_LLAMA_URL` seeds persistent broker settings on a new database. Admins can later change the adapter URL/enabled state, self-registration policy and non-admin publishing policy in the GUI. Disabling registration removes the GUI Register action and is independently enforced by the broker; admin-created accounts remain available.

Run `cargo test --workspace` for framing, compression ratio, fragmentation, decompression limits, SSE integrity, GUI delta application, tenant isolation, and bounded streaming stress. The executable verification harnesses are `scripts/userspace-network-test.sh`, `scripts/multiclient-test.sh`, and `scripts/stream-soak-test.sh`; the privileged kernel-netem variants are documented in [operations](docs/OPERATIONS.md). See [architecture](docs/architecture/ARCHITECTURE.md) for system boundaries.
