# ADR 0003: WASM inference adapter plugins

Status: accepted; checked-in artifact policy superseded by [ADR 0004](0004-source-built-bundled-adapters.md)
Recorded: 2026-10-26

## Context

Provider-specific HTTP paths, generation options, model lifecycle calls, and streaming parsers were spread across broker systems. Adding another backend required changing broker code. Adapters should be independently installable translators behind one broker-facing interface, following the general pattern of [Zed's Rust/WASM extensions](https://zed.dev/docs/extensions/developing-extensions).

## Decision

Keep versioned neutral request/result types and the guest registration macro in `crates/chatty-adapters/shared/api.rs`. Keep shared OpenAI translation in `crates/chatty-adapters/shared/openai_compatible.rs`. These are source modules included by the broker/plugins, with no separate helper crates. Keep the WASM runtime and host HTTP transport inside `chatty-broker/src/adapters/runtime`. The `crates/chatty-adapters/` directory contains provider plugin crates, rather than a separate runtime crate. Build OpenAI-compatible, Ollama, and Hugging Face catalogue translators as import-free core WASM modules under `crates/chatty-adapters/`.

Use the small Wasmi interpreter rather than a WASI/component runtime. The v1 interface needs only JSON request/response translation and newline-delimited stream records. It needs no guest networking, files, processes, clocks, or host imports. This is Chatty's own ABI; Zed extensions are not binary compatible.

The broker submits typed messages, generation options, model operations, and catalogue searches. Plugins return HTTP plans, then normalize provider responses. The host checks the plan against the original operation, configured origin, allowed HTTP methods, time budget, and byte limits. The broker retains authentication, authorization, prompt construction, SQLite, revision publication, usage accounting, cancellation ownership, and client stream batching.

Bundle prebuilt WASM modules in the broker so ordinary builds and deployments need no WASM compiler or sidecar process. Discover operator-installed manifests at startup from `CHATTY_ADAPTER_DIR`, and load only the selected provider on first use. Cache one selected module; release it on selection changes or disabling inference. Load Hugging Face transiently for catalogue requests. Management must use the selected provider rather than loading another runtime. `CHATTY_ADAPTER_PLUGIN` selects a third-party inference/management adapter without rebuilding the broker. Existing persisted settings continue selecting the bundled OpenAI or Ollama plugin when no override is set.

## Consequences

- Provider API schemas and stream parsing leave the broker entirely.
- Plugins cannot access broker state, credentials, or arbitrary network origins through host imports.
- Model downloads still run on the configured inference server. The broker sends lifecycle commands only.
- Plugin traps, invalid memory access, excessive output, and exhausted fuel become request errors.
- All inference operations now use the selected adapter, including imports, memory extraction, titles, speaker selection, and portraits. Native Ollama mode no longer requires the OpenAI compatibility API for auxiliary calls.
- Plugin discovery happens at startup. Modules compile on demand, so invalid unused WASM does not block other providers. New plugin IDs require a restart; an already discovered file is reread after cache eviction. There is no marketplace or background reloader.
- The binary protocol remains version 15 and SQLite needs no migration. The independent guest ABI starts at version 1.
- JSON crossing a WASM memory boundary adds serialization cost. Calls occur per operation or received record batch, with no background polling or per-token guest calls.

See [adapter contract](../ADAPTERS.md), [Operations](../../OPERATIONS.md), and [Release](../../RELEASE.md) for API, installation, and artifact build instructions.
