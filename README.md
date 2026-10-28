# Chatty

Chatty is a Linux-native client and broker for persistent AI character roleplay. The desktop client connects to a separately operated broker over TLS 1.3; the broker owns accounts, conversations, prompt assembly, persistence, synchronization, and model access. Inference stays in Ollama or another OpenAI-compatible service.

Chatty is currently a working `0.1.0` pre-release. It is suitable for development and controlled deployments, but the release checks and Flatpak submission are not finished. See [Release](docs/RELEASE.md) for the remaining gates.

## Workspace

| Package | Purpose |
|---|---|
| `chatty-gui` | Native `iced` desktop client |
| `chatty-broker` | Multi-user TLS broker, SQLite owner, and inference adapter |
| `chatty-protocol` | Shared bincode messages, framing, compression, and error types |

## What works

- Broker selection, connection testing, registration, login, saved sessions, and logout
- User/admin roles with broker-side authorization and tenant isolation
- Character creation and editing, public sharing, and SillyTavern JSON/PNG import
- Private direct conversations, streamed generation, cancellation, regeneration, deletion, and automatic titles
- Reconnect/resume and live deltas across a user's connected clients
- Dark glass appearance with a visible desktop and readable content plates
- Layout follows compositor-assigned space: compact navigation, stacked character fields, and scrollable dialogs
- Local neural text-to-speech for character replies, with optional automatic playback
- OS-level screen-capture exclusion in the official client where the windowing platform supports it
- Per-user prompt and completion token accounting
- Admin monitoring, user management, broker policy, inference settings, and Ollama model controls
- Sandboxed WASM adapter plugins for OpenAI-compatible servers, remote [FreeToken](docs/OPERATIONS.md#freetoken-on-a-remote-machine), Ollama, and Hugging Face model discovery
- Optional [ComfyUI image generation](docs/OPERATIONS.md#image-generation-with-comfyui): **Generate portrait** and **Generate emotions** (12 labelled expressions with one shared seed, used to pick the reply portrait) in the character editor, and **Image** in the chat composer to illustrate the scene. Text in the composer becomes the image prompt; an empty composer asks the text model to describe the scene
- Broker/protocol support for group modes, variants, lore, memories, conversation state, and summaries

The GUI focuses on direct conversations. World lore has a native editor with character links, common knowledge, and scene-triggered facts. Broker/protocol support does not imply a complete GUI workflow for every feature.

## Quick start

Requirements:

- A current Rust toolchain
- OpenSSL for development certificates
- Linux desktop, audio, and speech libraries required by `iced`/Wayland and local speech
- Ollama or another OpenAI-compatible chat-completions server for generation

Run the broker and GUI together:

```sh
rustup target add wasm32-unknown-unknown
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 ./start-chatty.sh
```

The launcher creates development certificates when absent, builds release binaries when needed, starts the broker on `127.0.0.1:7443`, and opens the GUI. Closing the GUI stops the broker started by the launcher.

The first account created in a new database becomes an administrator. Passwords must be 10–1024 bytes; usernames must be 3–64 bytes.

To run each component yourself:

```sh
./scripts/create-dev-cert.sh
cargo build --release --workspace
CHATTY_LLAMA_URL=http://127.0.0.1:11434/v1 cargo run --release -p chatty-broker
CHATTY_BROKER=127.0.0.1 CHATTY_CA=certs/ca.pem cargo run --release -p chatty-gui
```

`CHATTY_LLAMA_URL` seeds only a new database. After initialization, change the persisted adapter configuration from the GUI's admin portal.

Admins can open **Admin Portal → Models** to manage the inference server:

- **Installed:** filter by model name or family, inspect disk/VRAM use, load or unload models, and confirm deletion.
- **Find models:** search public Hugging Face GGUF repositories, select a file/quantization, and download it through Ollama. You can also enter an Ollama model tag or `hf.co/owner/repository:filename.gguf` directly. Search excludes gated repositories and split GGUF files.
- **Downloads:** track and cancel parallel downloads.
- **Settings:** choose the runtime adapter, chat model, server URL, and generation defaults, then save. The runtime list comes from installed broker adapters, including FreeToken, Ollama, OpenAI-compatible, and custom plugins. Advanced sampling controls are collapsed by default; hover field labels for help. These defaults apply across the broker. Unload a model and save a different chat model before deleting it.

Downloads are stored on the inference server, not the desktop client or broker. When inference runs on another machine, set the adapter URL to that machine's reachable Ollama address (for example, `http://192.168.1.50:11434`), then save before downloading. `127.0.0.1` refers to the broker's machine. Hugging Face search requires broker internet access; downloads require an Ollama server with Hugging Face support. Search runs only when requested. Up to four downloads run in parallel; the Downloads tab shows per-layer progress and cancellation controls. Clear finished removes completed entries without touching active downloads. Other models can be deleted while downloads run. Downloads stop when their client disconnects; retrying lets Ollama reuse available partial data.


## Local speech

Open **Settings → Speech → Test voice** to check speech and your audio output.
Use the speaker action on a character reply to read it aloud, or enable
**Automatically read new replies**. **Stop speaking** cancels playback and pending
speech. Model preparation, synthesis, and audio errors are shown in the app.

Speech uses the small KittenTTS Nano model on your CPU. The model and voices download
on first use (about 28 MB combined), then stay cached for offline use. No reply text
is sent to a speech service. The Flatpak includes ONNX Runtime and requests audio
server access; no host Python installation or speech packages are required.

To test the complete speech path from a terminal:

```sh
flatpak run io.github.pheonixfirewingz.Chatty//25.08 --test-speech
```

The test plays a short phrase and exits with an error if model loading, synthesis,
or audio output fails.

## Data locations

Defaults follow the XDG Base Directory Specification:

| Data | Default location |
|---|---|
| Broker database | `$XDG_DATA_HOME/chatty/chatty.db` or `~/.local/share/chatty/chatty.db` |
| Broker current-boot log | `$XDG_DATA_HOME/chatty/chatty-boot.log` or `~/.local/share/chatty/chatty-boot.log` |
| GUI session and preferences | `$XDG_STATE_HOME/chatty/` or `~/.local/state/chatty/` |
| Launcher broker log | `$XDG_STATE_HOME/chatty/broker.log` or `~/.local/state/chatty/broker.log` |
| Per-server GUI CA | `$XDG_CONFIG_HOME/chatty/server-cas/<host>.ca.pem` or `~/.config/chatty/server-cas/<host>.ca.pem` |
| First-use trust material | `$XDG_CONFIG_HOME/chatty/server-cas/<host>.ca.der` (or `.cert.der` for a leaf-only legacy chain) |

Only distribute `ca.pem` to clients. Keep `ca.key` private and offline from client systems.

The official client requests native screen-capture exclusion for its window. This is
effective on supported Windows versions and best-effort on macOS. Current Linux X11
and Wayland windowing APIs used by Chatty do not provide a portable equivalent, so
the setting cannot prevent screenshots there. It also cannot prevent photography,
modified clients, or capture outside the operating system's managed APIs.

Public brokers whose certificates chain to a standard public CA connect without a Chatty-specific
certificate file. For private or LAN brokers, the GUI retrieves the public private-CA certificate
from the TLS chain, shows its SHA-256 fingerprint for confirmation, and stores it automatically.
Confirm the fingerprint through a separate trusted channel before selecting **Trust and connect**:

```sh
openssl x509 -in certs/ca.pem -noout -fingerprint -sha256
```

Private broker certificate files should contain the leaf certificate followed by the private CA
certificate; the development certificate script produces this chain. Older leaf-only brokers fall
back to an exact `<host>.cert.der` pin. If a private CA intentionally changes, remove the saved
`<host>.ca.der` (or legacy `.cert.der`) and confirm the replacement fingerprint on next connection.

## Development and release

See [AGENTS.md](AGENTS.md#build-and-verification) for build commands, checks, and
contribution rules. Release readiness requires fresh check results and the
[functional and packaging gates](docs/RELEASE.md); historical test counts are not
release evidence.

## Documentation

- [Engineering guide](AGENTS.md)
- [Operations](docs/OPERATIONS.md): configuration, trust, backups, and upgrades
- [Release](docs/RELEASE.md): acceptance checks and Flatpak packaging
- [Architecture](docs/architecture/ARCHITECTURE.md): boundaries, wire contract, and decisions

## Context usage

The pie inside the message input shows context used by the latest request in that
conversation. Hover over it for tokens used, the context-window size, and percentage
used. A `~` marks an estimate; provider counts replace estimates when available after
the reply. Unsent text is not included. A dash means
no request has been measured in this app session yet.

## World lore

Open **Worlds** in the sidebar to create and edit world lore. When creating or editing
a character, use the **World lore** checkboxes in the character form to link or unlink
that character. Character details and link changes are saved together. Worlds are
private to your account; other users do not inherit your lore.

- **Common knowledge** is eligible on every reply by a linked character.
- Other enabled facts activate through case-insensitive keyword/phrase matching in
  the latest six messages, using selected variants. They leave context when those
  triggers leave the window; lore is not written into chat history or memory.
- Common knowledge takes precedence, then descending priority. Whole entries fit
  within an 8 KiB per-reply lore budget; entries that do not fit are skipped.
- A character can use multiple worlds. Unlinked worlds and disabled facts are excluded.
- Save applies edits; removing an entry or deleting a world removes it from future replies.

Upgrade the GUI and broker together. Migration `0010_remove_legacy_lore.sql` removes
old global/conversation lore and its sync deltas; that data is not converted to worlds.
Existing characters, conversations, and memories remain. Historical migrations are
retained so existing databases can upgrade normally.

## License

MIT. See [LICENSE](LICENSE).

Provider integrations run as WASM plugins behind a shared broker interface. See [Operations](docs/OPERATIONS.md#wasm-adapter-plugins) to install/select a plugin and [Adapter contract](docs/architecture/ADAPTERS.md) to build one.
