#!/usr/bin/env bash
set -Eeuo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
runtime_dir="$project_dir/.chatty"
broker_pid=""

listen_address=${CHATTY_LISTEN:-127.0.0.1:7443}
client_address=${CHATTY_BROKER:-$listen_address}
server_name=${CHATTY_SERVER_NAME:-localhost}
database=${CHATTY_DATABASE:-sqlite://$runtime_dir/chatty.db?mode=rwc}
certificate=${CHATTY_CERT:-$project_dir/certs/server.pem}
private_key=${CHATTY_KEY:-$project_dir/certs/server.key}
ca_certificate=${CHATTY_CA:-$project_dir/certs/ca.pem}
llama_url=${CHATTY_LLAMA_URL:-http://192.168.0.97:11434/v1}

cleanup() {
    if [[ -n "$broker_pid" ]] && kill -0 "$broker_pid" 2>/dev/null; then
        kill "$broker_pid" 2>/dev/null || true
        wait "$broker_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

supervise_broker() {
    local child_pid=""
    trap 'if [[ -n "$child_pid" ]] && kill -0 "$child_pid" 2>/dev/null; then kill "$child_pid" 2>/dev/null || true; wait "$child_pid" 2>/dev/null || true; fi; exit 0' TERM INT
    while true; do
        env \
            CHATTY_LISTEN="$listen_address" \
            CHATTY_DATABASE="$database" \
            CHATTY_CERT="$certificate" \
            CHATTY_KEY="$private_key" \
            CHATTY_LLAMA_URL="$llama_url" \
            "$project_dir/target/release/chatty-broker" \
            >>"$runtime_dir/broker.log" 2>&1 &
        child_pid=$!
        wait "$child_pid" || true
        child_pid=""
        sleep 0.25
    done
}

mkdir -p "$runtime_dir"

if [[ ! -f "$certificate" || ! -f "$private_key" || ! -f "$ca_certificate" ]]; then
    printf '%s\n' "Creating pinned development certificates…"
    (cd "$project_dir" && ./scripts/create-dev-cert.sh)
fi

if [[ ! -x "$project_dir/target/release/chatty-broker" || ! -x "$project_dir/target/release/chatty-gui" ]]; then
    printf '%s\n' "Building Chatty release binaries…"
    cargo build --release --workspace --manifest-path "$project_dir/Cargo.toml"
fi

printf 'Starting broker on %s…\n' "$listen_address"
supervise_broker &
broker_pid=$!

host=${client_address%:*}
port=${client_address##*:}
ready=false
for _ in {1..50}; do
    if ! kill -0 "$broker_pid" 2>/dev/null; then
        printf '%s\n' "Broker failed to start. Last log lines:" >&2
        tail -n 30 "$runtime_dir/broker.log" >&2 || true
        exit 1
    fi
    if (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null; then
        ready=true
        break
    fi
    sleep 0.1
done

if [[ "$ready" != true ]]; then
    printf 'Broker did not become ready at %s. See %s\n' \
        "$client_address" "$runtime_dir/broker.log" >&2
    exit 1
fi

printf '%s\n' "Launching Chatty GUI…"
env \
    CHATTY_BROKER="$client_address" \
    CHATTY_SERVER_NAME="$server_name" \
    CHATTY_CA="$ca_certificate" \
    "$project_dir/target/release/chatty-gui"
