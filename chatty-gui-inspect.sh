#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
control_fifo="${CHATTY_INSPECT_CONTROL:-/tmp/chatty-gui-control}"

usage() {
    printf '%s\n' \
        "Usage:" \
        "  $0 start [width height]" \
        "  $0 resize <width> <height>" \
        "  $0 zoom <factor>" \
        "  $0 screenshot <png-path>" \
        "  $0 sidebar <open|closed>" \
        "  $0 tools <open|closed>" \
        "  $0 screen <login|main>" \
        "  $0 quit"
}

case "${1:-}" in
    start)
        width="${2:-1100}"
        height="${3:-720}"
        cd "$project_dir"
        exec cargo run --release -p chatty-gui -- \
            --inspect --inspect-control "$control_fifo" \
            --width "$width" --height "$height"
        ;;
    resize|zoom|screenshot|sidebar|tools|screen|quit)
        [[ -p "$control_fifo" ]] || {
            printf 'Inspector is not running: %s\n' "$control_fifo" >&2
            exit 1
        }
        printf '%s\n' "$*" > "$control_fifo"
        ;;
    *)
        usage
        exit 2
        ;;
esac
