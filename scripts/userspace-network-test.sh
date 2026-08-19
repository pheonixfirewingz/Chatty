#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
database="/tmp/chatty-userspace-nettest-$$.db"
broker_log="/tmp/chatty-userspace-broker-$$.log"
proxy_log="/tmp/chatty-userspace-proxy-$$.log"
client_log="/tmp/chatty-userspace-client-$$.log"
broker_pid=""
proxy_pid=""

cleanup() {
    [ -z "$proxy_pid" ] || kill "$proxy_pid" 2>/dev/null || true
    [ -z "$broker_pid" ] || kill "$broker_pid" 2>/dev/null || true
    [ -z "$proxy_pid" ] || wait "$proxy_pid" 2>/dev/null || true
    [ -z "$broker_pid" ] || wait "$broker_pid" 2>/dev/null || true
    rm -f "$database" "$database-shm" "$database-wal"
}
trap cleanup EXIT INT TERM

env CHATTY_LISTEN=127.0.0.1:17443 \
    CHATTY_DATABASE="sqlite://$database?mode=rwc" \
    CHATTY_CERT="$project_dir/certs/server.pem" \
    CHATTY_KEY="$project_dir/certs/server.key" \
    CHATTY_LLAMA_URL=http://127.0.0.1:1/v1 \
    "$project_dir/target/release/chatty-broker" >"$broker_log" 2>&1 &
broker_pid=$!

"$project_dir/target/release/chatty-net-proxy" \
    --listen 127.0.0.1:17444 --upstream 127.0.0.1:17443 \
    --latency-ms 75 --jitter-ms 25 --loss-percent 3 \
    --rate-kbit 256 --fragment-bytes 113 \
    --disconnect-after-bytes 1800 >"$proxy_log" 2>&1 &
proxy_pid=$!
sleep 1

printf 'register userspace-net long-userspace-network-password\npermissions\nquit\n' | \
    timeout 45 "$project_dir/target/release/chatty-client" \
    --broker 127.0.0.1:17444 --server-name localhost \
    --ca "$project_dir/certs/ca.pem" >"$client_log" 2>&1

grep -q 'Authenticated user' "$client_log"
grep -q 'Initial state synchronized' "$client_log"
grep -q 'ManageOwnRoleplay' "$client_log"
grep -q 'broker offline\|connection lost' "$client_log"
printf '%s\n' 'user-space impaired TLS/binary reconnect test passed'
