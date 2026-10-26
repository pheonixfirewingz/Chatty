# Operations

## Runtime topology

Operate the broker separately from the desktop clients and inference service:

```text
Chatty GUI -- TLS 1.3 / binary frames --> Chatty broker -- HTTP streaming --> inference service
                                               |
                                               +--> SQLite
```

The broker is the security and data boundary. Do not expose SQLite or inference management endpoints to clients as substitutes for broker requests.

## Broker startup

Required inputs are a writable SQLite location and a server certificate/private key pair. Example:

```sh
CHATTY_LISTEN=0.0.0.0:7443 \
CHATTY_DATABASE='sqlite:///var/lib/chatty/chatty.db?mode=rwc' \
CHATTY_LOG=/var/lib/chatty/chatty-boot.log \
CHATTY_CERT=/etc/chatty/server.pem \
CHATTY_KEY=/etc/chatty/server.key \
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 \
/usr/local/bin/chatty-broker
```

The inference endpoint may be unavailable at startup; the broker remains available and retries generation paths later. `CHATTY_LLAMA_URL` initializes a new database but does not override persisted configuration in an existing database.

The broker truncates its private current-boot log at every process start and records all broker trace, debug, info, warning, and error events. The default path is `~/.local/share/chatty/chatty-boot.log`; set `CHATTY_LOG` for system installations. The file is mode `0600` on Unix, rolls over at 16 MiB to bound disk use, and is readable from the admin portal's **Logs** tab. Every outbound AI prompt is recorded there, including system instructions, selected memories, and chat content, so treat the log as private user data. Only authenticated administrators can request it, and each response is limited to the newest 512 KiB.

System and per-user service templates are in `packaging/`. Review their paths, user, network exposure, and inference URL before installation.

## Broker environment

| Variable | Default | Purpose |
|---|---|---|
| `CHATTY_LISTEN` | `0.0.0.0:7443`; launcher uses `127.0.0.1:7443` | Listen address |
| `CHATTY_DATABASE` | XDG data directory | SQLite URL |
| `CHATTY_LOG` | XDG data directory | Current-boot broker log path |
| `CHATTY_CERT` | `certs/server.pem` | Server certificate chain |
| `CHATTY_KEY` | `certs/server.key` | Server private key |
| `CHATTY_LLAMA_URL` | `http://192.168.0.97:11434/v1` | Initial adapter URL for a new database |
| `CHATTY_ADAPTER_DIR` | Unset; bundled plugins only | Directory of operator-installed WASM adapter plugins |
| `CHATTY_ADAPTER_PLUGIN` | Existing native-Ollama setting selects `ollama` or `openai-compatible` | Explicit inference/management plugin ID; overrides that setting |
| `CHATTY_FREETOKEN_ORIGIN` | Unset | Explicit credential origin, e.g. `https://inference.example:1919`; required with the API key |
| `CHATTY_FREETOKEN_API_KEY` | Unset | FreeToken bearer key, supplied through the broker service’s private environment |
| `RUST_LOG` | Broker info plus library defaults | Tracing filter |

See [README](../README.md#data-locations) for XDG paths and
[AGENTS.md](../AGENTS.md#local-setup-and-documentation) for GUI development overrides.

## WASM adapter plugins

The broker bundles `openai-compatible`, `ollama`, `freetoken`, and `hugging-face` WASM modules. Normal deployment needs no WASM compiler or plugin directory. Model files and inference remain on the configured remote server; plugins only translate requests and responses inside the broker.

To install a third-party adapter, place its manifest and module in a broker-readable directory:

```text
/etc/chatty/adapters/my-provider/adapter.json
/etc/chatty/adapters/my-provider/adapter.wasm
```

The manifest must identify ABI version 1 and a module filename within the same directory:

```json
{"id":"my-provider","api_version":1,"module":"adapter.wasm"}
```

Set `CHATTY_ADAPTER_DIR=/etc/chatty/adapters` and `CHATTY_ADAPTER_PLUGIN=my-provider` in the broker's environment, then restart the broker. Keep the existing inference URL and generation settings in the admin portal. The explicit plugin ID overrides the native-Ollama toggle for inference and model management. Catalogue searches continue using the bundled Hugging Face plugin. Custom IDs cannot shadow bundled IDs. The former `openai` selector remains an alias for `openai-compatible`, so existing service configuration keeps working. Missing IDs, incompatible manifest versions, or invalid manifests fail startup explicitly. WASM bytes are read and validated only when an adapter is used; invalid modules fail that operation explicitly.

Startup discovers plugin manifests without loading every WASM module. The broker compiles the selected inference adapter on first use and retains only that adapter. Model management uses the same selection; it does not load Ollama as a second provider. Select native Ollama mode to use Ollama lifecycle operations. Changing the selection or disabling inference releases its old cache entry immediately; active requests finish with their own references. Hugging Face loads only for catalogue requests and is released when those requests finish, including errors. FreeToken credential validation does not load a guest.

Installing new plugin IDs requires a broker restart. Changes to an already discovered module file take effect after its cached module is evicted and next used. To roll back an operator override, remove it and restart; persisted settings again select bundled OpenAI-compatible or Ollama behavior. The database and client wire protocol do not change.

Plugin development and limits are documented in [Adapter contract](architecture/ADAPTERS.md). Rebuilding bundled modules is documented in [Release](RELEASE.md#adapter-artifacts).

### FreeToken on a remote machine

FreeToken is bundled: set `CHATTY_ADAPTER_PLUGIN=freetoken` in the broker service environment and restart the broker. No additional plugin directory is needed. The source, ABI-1 manifest, and import-free `wasm32-unknown-unknown` artifact live in `crates/chatty-adapters/freetoken/`. Do not copy a bundled ID into `CHATTY_ADAPTER_DIR`; duplicate IDs are rejected.

In the admin portal, enable inference and set the existing server URL to the remote **serve** endpoint, for example `https://inference.example:1919/v1`. Both the server root and a trailing `/v1` are accepted, including an optional reverse-proxy prefix. Select a dynamically discovered model ID or leave the existing default selection behavior. Do not point this URL at FreeToken's daemon (normally port 1900). FreeToken's serve port defaults to 1919 and its bind address defaults to loopback; configure access on the inference machine or through a reverse proxy. Model installation, weight storage, and inference execution remain on that machine.

FreeToken authentication is optional. A server started with `--api-key` or `FREETOKEN_API_KEY` requires a bearer key on every route except `/health`. For such a server, inject `CHATTY_FREETOKEN_API_KEY` through the broker service's private environment and set `CHATTY_FREETOKEN_ORIGIN` to its exact scheme, host, and port (no `/v1`, path, query, or URL credentials). Configure these together, then restart the broker. Use HTTPS for credentials over untrusted networks. Never put keys in the server URL, manifest, guest source, or command-line arguments. Chatty attaches the sensitive authorization header only after origin checks, only to the selected FreeToken adapter. Changing the configured server to another origin fails until the operator changes the credential origin. Catalogue requests and other adapters never receive this key; redirects are rejected.

Supported operations:

- Discovery: `GET /v1/models`; IDs come from the server, including its `--served-model-name` override.
- Completion and streaming: `POST /v1/chat/completions`; SSE requests include usage. Only assistant `content` enters final output. Separate `reasoning_content`, `reasoning`, and `thinking` fields are discarded, never promoted to final content. Reasoning-only responses therefore have no final text.
- Options: model, messages, temperature, top-p, top-k, nonnegative max tokens, and `reasoning_effort=none` when reasoning is disabled. Negative max-token sentinels omit the limit and use the server default. Thinking control remains model/template dependent.
- State: `GET /health` reports the server version and the serving model when status is `ok` and maintenance is `serving`. Loading reports no running model; fatal health errors remain errors. Installed-model inventory is unavailable. Size, VRAM, and expiry metadata are unavailable and use zero/empty neutral defaults.

Unsupported operations return explicit errors before HTTP: download, delete, load, unload, and catalogue search. FreeToken's separate daemon documents engine start/stop/switch and checkpoint conversion, but those use another control endpoint and authentication scheme; this adapter does not manage that daemon. Neither the serve API nor documented daemon API provides an installed-weight download/delete contract matching Chatty's operations. Manage models on the inference machine. Seed, repetition penalty, context size, and keep-alive are not consumed by FreeToken's chat sampler and are omitted. Tool calls, multimodal inputs, cached-token detail, and reasoning display are outside Chatty's current neutral chat contract.

Compatibility was checked against FreeToken commit `ba31db2ea708b61d7313bccb3ee4adf9d11ecb90` on 2026-10-26: [official CLI/auth documentation](https://github.com/FlashML-org/FreeToken/blob/ba31db2ea708b61d7313bccb3ee4adf9d11ecb90/docs/cli.md), [request schema](https://github.com/FlashML-org/FreeToken/blob/ba31db2ea708b61d7313bccb3ee4adf9d11ecb90/python/freetoken/server/api_models.py), [chat/SSE implementation](https://github.com/FlashML-org/FreeToken/blob/ba31db2ea708b61d7313bccb3ee4adf9d11ecb90/python/freetoken/server/openai_api.py), [health implementation](https://github.com/FlashML-org/FreeToken/blob/ba31db2ea708b61d7313bccb3ee4adf9d11ecb90/python/freetoken/server/control_api.py), and [daemon API documentation](https://github.com/FlashML-org/FreeToken/blob/ba31db2ea708b61d7313bccb3ee4adf9d11ecb90/python/freetoken/daemon/README.md).

## Certificates

The broker accepts TLS 1.3 only. CA-based and public-PKI connections verify the requested server
name. First-use private certificate pins bind the exact confirmed certificate to the entered server.

The GUI uses this trust order:

1. An explicit `CHATTY_CA`/`--ca` file.
2. A per-server `<host>.ca.pem` file.
3. A previously confirmed `<host>.ca.der` trust anchor or legacy `<host>.cert.der` leaf pin.
4. Standard public web PKI roots.
5. For an unknown private certificate, a one-time fingerprint confirmation before the exact
   certificate is saved and a verified connection is retried.

The discovery handshake never sends account credentials. Do not tell users to accept an
unconfirmed fingerprint; compare it with `openssl x509 -in ca.pem -noout -fingerprint -sha256`
on the broker or through another trusted channel.

For private deployments, configure `server.pem` as a chain containing the server leaf followed by
the public private-CA certificate. This lets clients enroll the CA and permits future leaf renewal
without re-enrollment. A leaf-only chain remains supported but is pinned exactly.

- Create production certificates outside the repository.
- Include every broker IP address or DNS name used by clients in the certificate SANs.
- Copy only the public CA certificate to clients.
- Never distribute the CA private key or broker private key.
- Rotate certificates before expiry and test a client with the replacement CA/certificate pair.

The repository certificate script is intended for development. Its generated server certificate is valid for the names and addresses in `scripts/dev-cert.ext` and will not automatically follow deployment address changes.

## Persistence and backup

The database uses SQLite migrations and WAL mode. Back up a consistent database snapshot; do not copy only the main `.db` file while the broker is actively writing and ignore its `-wal` file.

Preferred procedure:

1. Stop the broker or use SQLite's online backup mechanism.
2. Copy the database and verify the backup can be opened.
3. Retain the executable version that created the backup.
4. Restart the broker and confirm migration/startup logs are clean.

Restore into a staging path first, start the same or newer broker against it, and test login plus conversation loading before replacing production data.

## Sessions and accounts

Sessions expire 30 days after creation. The first registered user in an empty database becomes an administrator. After bootstrap:

- Confirm the administrator can open the admin portal.
- Disable self-registration if the service is private.
- Create managed users from the admin portal.
- Maintain more than one administrator; the active admin cannot demote or delete itself.

GUI session files contain an opaque bearer token and are created with user-only permissions on Unix. Treat them as credentials.

## Monitoring

The admin portal reports:

- Process uptime and approximate CPU use
- Resident memory and detected cgroup memory limit
- Active connections
- Inference-adapter status, model count, and latency
- A bounded list of recent broker errors

The provided systemd units set `MemoryMax=256M`. Investigate sustained pressure before changing the limit. Argon2 work is capped at ten concurrent computations, writer queues are bounded to 32 frames per connection, and idle connections close after 120 seconds unless traffic or generation work keeps them active.

## Failure handling

| Symptom | Check |
|---|---|
| Client cannot connect | Broker listener, firewall, certificate SAN, client CA path, port `7443` |
| Login repeatedly fails | System clock, session expiry, username, registration policy, recent broker errors |
| Generation fails | Adapter enabled state, persisted URL/model, `/v1/models`, model availability, adapter logs |
| Ollama controls fail | Enable native Ollama mode and verify the configured URL belongs to an Ollama server |
| Clients reconnect repeatedly | Broker logs for frame/protocol mismatch, delta lag, TLS errors, or idle-close behavior |
| Data missing for one user | Confirm account identity and ownership; tenant isolation is intentional |

Protocol mismatch is fatal by design. Deploy broker and GUI builds from the same source revision.

## Upgrade

### Deploy the broker service

Run this on the **broker host**, from a clean Chatty Git checkout with an upstream
branch. Use the checkout owner's account with Rust and rustup installed (and sudo access for
a system service):

```sh
./scripts/deploy-broker.sh
```

The script detects the system service first, then the current account's per-user
service. Use `--user` or `--system` to select explicitly. For the per-user template
`packaging/chatty-broker-user.service`, the default binary is
`~/.local/libexec/chatty/chatty-broker`; deployment uses `systemctl --user` without
sudo. Run it as the account that owns that service. If neither service is installed,
the script stops before pulling or building; it does not create a service.

The system defaults match `packaging/chatty-broker.service`: system service
`chatty-broker.service` and executable `/usr/local/bin/chatty-broker`. For a custom
installation:

```sh
./scripts/deploy-broker.sh --repo /path/to/Chatty \
  --service chatty-broker.service --binary /usr/local/bin/chatty-broker
```

Back up SQLite first. The script fast-forwards the checkout with `git pull
--ff-only`, ensures the `wasm32-unknown-unknown` Rust target is installed, and rebuilds
all bundled adapters before building the broker that embeds them. Both builds use
a fresh temporary target directory and `Cargo.lock`; the adapter build requires
cached Cargo dependencies because `scripts/build-adapters.sh` runs offline.
The script then stops the service and atomically replaces the binary. The
old executable is retained alongside it as `chatty-broker.backup.<timestamp>.<pid>`.
Temporary build files are removed on exit. The service configuration, certificates,
and database paths are not replaced.

After starting the broker, the script checks that the service and its main process
remain running for ten seconds. This is a process check; also verify a client can
connect and generate a reply. If startup fails, the service is stopped and the
backup binary is retained for recovery. There is no automatic binary rollback
after startup because embedded migrations may already have changed SQLite.
Deploy a GUI built from the same Git revision when the protocol changes.

### Manual upgrade

1. Back up SQLite.
2. Build and test broker and GUI from the same revision.
3. Stop the broker cleanly.
4. Replace the executable.
5. Start it and allow embedded migrations to run.
6. Verify logs, admin monitoring, login, conversation load, and a short generation.

There is no supported database downgrade path. Restore the pre-upgrade backup when rollback is required.
