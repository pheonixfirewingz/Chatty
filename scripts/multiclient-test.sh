#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
database="/tmp/chatty-multiclient-$$.db"
broker_log="/tmp/chatty-multiclient-broker-$$.log"
observer_log="/tmp/chatty-multiclient-observer-$$.log"
other_log="/tmp/chatty-multiclient-other-$$.log"
broker_pid=""
observer_pid=""
other_pid=""

cleanup() {
    [ -z "$observer_pid" ] || kill "$observer_pid" 2>/dev/null || true
    [ -z "$other_pid" ] || kill "$other_pid" 2>/dev/null || true
    [ -z "$broker_pid" ] || kill "$broker_pid" 2>/dev/null || true
    [ -z "$observer_pid" ] || wait "$observer_pid" 2>/dev/null || true
    [ -z "$other_pid" ] || wait "$other_pid" 2>/dev/null || true
    [ -z "$broker_pid" ] || wait "$broker_pid" 2>/dev/null || true
    rm -f "$database" "$database-shm" "$database-wal"
}
trap cleanup EXIT INT TERM

env CHATTY_LISTEN=127.0.0.1:19443 \
    CHATTY_DATABASE="sqlite://$database?mode=rwc" \
    CHATTY_CERT="$project_dir/certs/server.pem" \
    CHATTY_KEY="$project_dir/certs/server.key" \
    CHATTY_LLAMA_URL=http://127.0.0.1:1/v1 \
    "$project_dir/target/release/chatty-broker" >"$broker_log" 2>&1 &
broker_pid=$!
sleep 1

printf 'register shared-user long-multiclient-password\nquit\n' | \
    "$project_dir/target/release/chatty-client" --broker 127.0.0.1:19443 \
    --server-name localhost --ca "$project_dir/certs/ca.pem" >/dev/null
printf 'register other-user long-other-tenant-password\nquit\n' | \
    "$project_dir/target/release/chatty-client" --broker 127.0.0.1:19443 \
    --server-name localhost --ca "$project_dir/certs/ca.pem" >/dev/null

{
    printf 'login shared-user long-multiclient-password\n'
    sleep 3
    printf 'permissions\nquit\n'
} | "$project_dir/target/release/chatty-client" --broker 127.0.0.1:19443 \
    --server-name localhost --ca "$project_dir/certs/ca.pem" >"$observer_log" 2>&1 &
observer_pid=$!
{
    printf 'login other-user long-other-tenant-password\n'
    sleep 3
    printf 'permissions\nquit\n'
} | "$project_dir/target/release/chatty-client" --broker 127.0.0.1:19443 \
    --server-name localhost --ca "$project_dir/certs/ca.pem" >"$other_log" 2>&1 &
other_pid=$!
sleep 1

printf 'login shared-user long-multiclient-password\ncharfull shared-character|Shared|Calm|Test|Stay in character.|Hello.|Plain|test|-\nquit\n' | \
    "$project_dir/target/release/chatty-client" --broker 127.0.0.1:19443 \
    --server-name localhost --ca "$project_dir/certs/ca.pem" >/dev/null
wait "$observer_pid"
observer_pid=""
wait "$other_pid"
other_pid=""

grep -q 'live delta character shared-character Add' "$observer_log"
if grep -q 'shared-character' "$other_log"; then
    printf '%s\n' 'cross-tenant live delta leak detected' >&2
    exit 1
fi
printf '%s\n' 'same-owner live multi-client delta test passed'
