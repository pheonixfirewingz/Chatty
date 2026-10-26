# ADR 0004: Build bundled adapters from source

Status: accepted  
Recorded: 2026-10-26

## Context

ADR 0003 bundled checked-in WASM modules so native builds needed no WASM target.
Rebuilding adapters copied generated binaries into source directories, creating
binary Git changes and a risk that committed modules did not match their source.

## Decision

Keep only adapter Rust source and manifests in the repository. The broker's
`build.rs` compiles the four bundled adapters for `wasm32-unknown-unknown` and
embeds the resulting modules from its Cargo `OUT_DIR`. Guest compilation uses
locked, cached dependencies, one worker, and a separate target directory beneath
`OUT_DIR` to avoid the parent Cargo invocation's artifact lock. Native compiler
flags and workspace lint wrappers are not forwarded to the guest build.

The standalone adapter script builds or checks guests in Cargo's target directory;
it never copies outputs into the source tree. Deployment builds the broker once,
letting its build script create the embedded modules in the temporary build tree.

## Consequences

- Broker builds require the matching Rust `wasm32-unknown-unknown` target. GUI-only
  builds do not require it.
- Source changes invalidate the broker's generated adapter output automatically.
- Generated WASM stays in ignored or explicitly selected Cargo target directories.
- Deployed binaries still embed their bundled modules. No compiler, sidecar, or
  additional runtime file is required on the deployment host.
- ABI 1, plugin discovery, interpreter bounds, lazy module loading, and adapter
  selection are unchanged. No client protocol or database migration is needed.
- This supersedes only ADR 0003's checked-in artifact/build prerequisite policy.
