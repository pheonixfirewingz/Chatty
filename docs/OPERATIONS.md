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
CHATTY_CERT=/etc/chatty/server.pem \
CHATTY_KEY=/etc/chatty/server.key \
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 \
/usr/local/bin/chatty-broker
```

The inference endpoint may be unavailable at startup; the broker remains available and retries generation paths later. `CHATTY_LLAMA_URL` initializes a new database but does not override persisted configuration in an existing database.

System and per-user service templates are in `packaging/`. Review their paths, user, network exposure, and inference URL before installation.

## Certificates

The broker accepts TLS 1.3 only. Clients trust the configured CA and verify the requested server name.

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

1. Back up SQLite.
2. Build and test broker and GUI from the same revision.
3. Stop the broker cleanly.
4. Replace the executable.
5. Start it and allow embedded migrations to run.
6. Verify logs, admin monitoring, login, conversation load, and a short generation.

There is no supported database downgrade path. Restore the pre-upgrade backup when rollback is required.
