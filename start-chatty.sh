#!/usr/bin/env bash
set -Eeuo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
broker_pid=""

user_home=${HOME:-}
if [[ -n "${XDG_DATA_HOME:-}" && "$XDG_DATA_HOME" == /* ]]; then
    data_home=$XDG_DATA_HOME
elif [[ -n "$user_home" && "$user_home" == /* ]]; then
    data_home="$user_home/.local/share"
else
    printf '%s\n' "Cannot determine XDG data directory; set XDG_DATA_HOME or HOME." >&2
    exit 1
fi
if [[ -n "${XDG_STATE_HOME:-}" && "$XDG_STATE_HOME" == /* ]]; then
    state_home=$XDG_STATE_HOME
elif [[ -n "$user_home" && "$user_home" == /* ]]; then
    state_home="$user_home/.local/state"
else
    printf '%s\n' "Cannot determine XDG state directory; set XDG_STATE_HOME or HOME." >&2
    exit 1
fi

data_dir="$data_home/chatty"
state_dir="$state_home/chatty"
broker_log=${CHATTY_LOG_FILE:-$state_dir/broker.log}

listen_address=${CHATTY_LISTEN:-127.0.0.1:7443}
client_address=${CHATTY_BROKER:-$listen_address}
server_name=${CHATTY_SERVER_NAME:-localhost}
database=${CHATTY_DATABASE:-sqlite://$data_dir/chatty.db?mode=rwc}
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
            >>"$broker_log" 2>&1 &
        child_pid=$!
        wait "$child_pid" || true
        child_pid=""
        sleep 0.25
    done
}

mkdir -p "$data_dir" "$state_dir"

# Preserve installations that predate the XDG layout. Copy the complete
# SQLite file set once; the old project-local files remain as a backup.
legacy_database="$project_dir/.chatty/chatty.db"
xdg_database="$data_dir/chatty.db"
if [[ -z "${CHATTY_DATABASE+x}" && ! -e "$xdg_database" && -f "$legacy_database" ]]; then
    printf 'Migrating existing database to %s…\n' "$xdg_database"
    for suffix in "" "-wal" "-shm"; do
        legacy_file="$legacy_database$suffix"
        xdg_file="$xdg_database$suffix"
        if [[ -f "$legacy_file" ]]; then
            cp -p -- "$legacy_file" "$xdg_file"
        fi
    done
fi

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
        tail -n 30 "$broker_log" >&2 || true
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
        "$client_address" "$broker_log" >&2
    exit 1
fi

printf '%s\n' "Launching Chatty GUI…"
env \
    CHATTY_BROKER="$client_address" \
    CHATTY_SERVER_NAME="$server_name" \
    CHATTY_CA="$ca_certificate" \
    "$project_dir/target/release/chatty-gui"
