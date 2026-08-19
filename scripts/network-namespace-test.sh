#!/bin/sh
set -eu

# End-to-end transport smoke test in a disposable namespace. This never shapes
# the host network interface and therefore cannot interrupt normal connectivity.
if [ "$(id -u)" -ne 0 ]; then
    printf '%s\n' "run as root: sudo $0" >&2
    exit 2
fi

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
namespace="chatty-nettest-$$"
database="/tmp/chatty-nettest-$$.db"
broker_log="/tmp/chatty-nettest-broker-$$.log"
client_log="/tmp/chatty-nettest-client-$$.log"
broker_pid=""

cleanup() {
    if [ -n "$broker_pid" ]; then
        kill "$broker_pid" 2>/dev/null || true
        wait "$broker_pid" 2>/dev/null || true
    fi
    ip netns del "$namespace" 2>/dev/null || true
    rm -f "$database" "$database-shm" "$database-wal"
}
trap cleanup EXIT INT TERM

ip netns add "$namespace"
ip -n "$namespace" link set lo up
ip netns exec "$namespace" tc qdisc add dev lo root netem \
    delay 250ms 75ms distribution normal \
    loss 3% 25% duplicate 0.2% reorder 1% 50% rate 256kbit

ip netns exec "$namespace" env \
    CHATTY_LISTEN=127.0.0.1:7443 \
    CHATTY_DATABASE="sqlite://$database?mode=rwc" \
    CHATTY_CERT="$project_dir/certs/server.pem" \
    CHATTY_KEY="$project_dir/certs/server.key" \
    CHATTY_LLAMA_URL=http://127.0.0.1:1/v1 \
    "$project_dir/target/release/chatty-broker" >"$broker_log" 2>&1 &
broker_pid=$!
sleep 1

printf 'register nettest-user long-network-test-password\npermissions\nquit\n' | \
    ip netns exec "$namespace" timeout 45 \
    "$project_dir/target/release/chatty-client" \
    --broker 127.0.0.1:7443 \
    --server-name localhost \
    --ca "$project_dir/certs/ca.pem" >"$client_log" 2>&1

grep -q 'Authenticated user' "$client_log"
grep -q 'Initial state synchronized' "$client_log"
grep -q 'ManageOwnRoleplay' "$client_log"
ip netns exec "$namespace" tc -s qdisc show dev lo
printf '%s\n' 'network namespace TLS/binary transport test passed'
