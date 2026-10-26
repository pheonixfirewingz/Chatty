#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
offline=(--offline)
case "${1:-}" in
    ""|--check) ;;
    --online) offline=() ;;
    *) printf 'Usage: %s [--check|--online]\n' "$0" >&2; exit 2 ;;
esac
# Install once before running: rustup target add wasm32-unknown-unknown
# Prebuilt modules are shipped so normal native builds need no WASM toolchain.
cargo build --release --locked "${offline[@]}" --target wasm32-unknown-unknown \
    -p chatty-adapter-openai-compatible -p chatty-adapter-ollama -p chatty-adapter-hugging-face -p chatty-adapter-freetoken
adapter_target="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release"
for adapter_name in openai-compatible ollama hugging-face freetoken; do
    artifact_name="${adapter_name//-/_}"
    if [[ "${1:-}" == --check ]]; then
        cmp "$adapter_target/chatty_adapter_${artifact_name}.wasm" "crates/chatty-adapters/$adapter_name/adapter.wasm"
    else
        cp "$adapter_target/chatty_adapter_${artifact_name}.wasm" "crates/chatty-adapters/$adapter_name/adapter.wasm"
        chmod 644 "crates/chatty-adapters/$adapter_name/adapter.wasm"
    fi
done
