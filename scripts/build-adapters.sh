#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
offline=(--offline)
operation=build
case "${1:-}" in
    "") ;;
    --check) operation=check ;;
    --online) offline=() ;;
    *) printf 'Usage: %s [--check|--online]\n' "$0" >&2; exit 2 ;;
esac
# Optional standalone guest build. Cargo writes only into its target directory.
# The broker's build.rs compiles its own embedded guests automatically.
cargo "$operation" --release --locked "${offline[@]}" --target wasm32-unknown-unknown \
    -p chatty-adapter-openai-compatible -p chatty-adapter-ollama -p chatty-adapter-hugging-face -p chatty-adapter-freetoken
