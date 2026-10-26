# Release

Chatty has not yet completed its first packaged release. Treat the following as the release gate for `v0.1.0`.

## Code gate

From a clean checkout, run every command in
[Build and verification](../AGENTS.md#build-and-verification) with the locked
dependency set available. All commands must pass, including strict Clippy. Record
results, the source revision, Rust version, and target architecture used for artifacts.

## Adapter artifacts

The broker embeds the checked-in WASM modules from `crates/chatty-adapters/`; native builds do not need a WASM Rust target. When changing guest code or the shared adapter API module, rebuild and include the resulting modules:

```sh
rustup target add wasm32-unknown-unknown
./scripts/build-adapters.sh
./scripts/build-adapters.sh --check
```

Record the Rust toolchain used to build the modules. Run workspace tests and strict Clippy after rebuilding: host tests exercise the actual embedded WASM, including dynamic loading, request translation, fragmented streaming, remote downloads, fuel/memory limits, and cancellation. Check that an operator-installed plugin can be selected without rebuilding the broker. Never publish guest source changes with stale embedded modules. The script builds and checks OpenAI, Ollama, Hugging Face, and FreeToken artifacts. FreeToken host tests cover its remote serving API, fragmented UTF-8 SSE, reasoning exclusion, finish/usage metadata, unsupported lifecycle operations, cancellation, and credential origin isolation.

## Functional gate

- Create a fresh database and verify the first account is admin.
- Create/import a character and complete a streamed response.
- Cancel a response, regenerate it, and delete a message.
- Restart broker and GUI; verify session resume and persisted conversation state.
- Connect a second GUI as the same account and verify live deltas.
- Verify a second account cannot access the first account's private data.
- Exercise admin user, policy, monitor, adapter, and Ollama controls.
- Test an unavailable inference server without losing broker availability.

## Flatpak bundle

The supported local bundle builder vendors locked dependencies and builds only the GUI inside the Freedesktop SDK:

```sh
flatpak install flathub \
  org.freedesktop.Platform//25.08 \
  org.freedesktop.Sdk//25.08 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08
./scripts/build-flatpak.sh
```

The result is written to `dist/chatty-<version>-<arch>.flatpak`. Install and test it with:

```sh
flatpak install --user ./dist/chatty-*.flatpak
flatpak run io.github.pheonixfirewingz.Chatty//25.08
flatpak run io.github.pheonixfirewingz.Chatty//25.08 --test-speech
```

The sandbox has network, Wayland, audio server, and DRI permissions. ONNX Runtime is
bundled; speech model files download once into the app-private cache at
`~/.var/app/io.github.pheonixfirewingz.Chatty/cache/huggingface/`. Test both first-use
download and subsequent cached playback with `--test-speech`. It does not package or
start the broker. Install a trusted broker CA in the app-private configuration directory:

```sh
mkdir -p ~/.var/app/io.github.pheonixfirewingz.Chatty/config/chatty/server-cas
cp /trusted/path/ca.pem \
  ~/.var/app/io.github.pheonixfirewingz.Chatty/config/chatty/server-cas/broker.example.test.ca.pem
```

Use the exact host entered in the GUI as the CA filename stem.

## Flathub manifest status

`packaging/flatpak/io.github.pheonixfirewingz.Chatty.yml` is not submission-ready. Before use:

- Generate and add `chatty-cargo-sources.json`.
- Replace the placeholder Anitya project ID or use an appropriate checker.
- Host at least one real PNG screenshot and enable it in AppStream metadata.
- Ensure tag `v0.1.0` exists and resolves to the reviewed release commit.
- Run `flatpak-builder-lint` against the manifest and repository.

## Release artifacts

Publish together:

- Tagged source archive
- Checksummed broker binary for each supported server architecture
- Checksummed GUI binary or Flatpak bundle
- License and release notes
- Protocol version and minimum compatible broker/GUI version

Do not publish development CA keys, server private keys, databases, logs, or GUI session files.

## Deployment

Follow [Operations: Upgrade](OPERATIONS.md#upgrade) for service deployment, database
backups, migration checks, and rollback limits. Deploy compatible broker and GUI builds.
