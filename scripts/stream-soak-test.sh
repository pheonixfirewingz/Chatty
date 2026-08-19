#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
database="/tmp/chatty-soak-$$.db"
broker_log="/tmp/chatty-soak-broker-$$.log"
mock_log="/tmp/chatty-soak-mock-$$.log"
setup_log="/tmp/chatty-soak-setup-$$.log"
stream_log="/tmp/chatty-soak-stream-$$.log"
cancel_log="/tmp/chatty-soak-cancel-$$.log"
broker_pid=""
mock_pid=""

cleanup() {
    [ -z "$broker_pid" ] || kill "$broker_pid" 2>/dev/null || true
    [ -z "$mock_pid" ] || kill "$mock_pid" 2>/dev/null || true
    [ -z "$broker_pid" ] || wait "$broker_pid" 2>/dev/null || true
    [ -z "$mock_pid" ] || wait "$mock_pid" 2>/dev/null || true
    rm -f "$database" "$database-shm" "$database-wal"
}
trap cleanup EXIT INT TERM

"$project_dir/target/release/chatty-mock-llama" \
    --listen 127.0.0.1:18114 --words 48 --chunk-delay-ms 2 >"$mock_log" 2>&1 &
mock_pid=$!
env CHATTY_LISTEN=127.0.0.1:18443 \
    CHATTY_DATABASE="sqlite://$database?mode=rwc" \
    CHATTY_CERT="$project_dir/certs/server.pem" \
    CHATTY_KEY="$project_dir/certs/server.key" \
    CHATTY_LLAMA_URL=http://127.0.0.1:18114/v1 \
    "$project_dir/target/release/chatty-broker" >"$broker_log" 2>&1 &
broker_pid=$!
sleep 1

printf 'register soak-user long-stream-soak-password\ncharfull speaker|Soaker|Patient and steady|A transport test|Stay in character.|Hello.|Calm|stress|-\nconversation Soak speaker\nquit\n' | \
    "$project_dir/target/release/chatty-client" --broker 127.0.0.1:18443 \
    --server-name localhost --ca "$project_dir/certs/ca.pem" >"$setup_log" 2>&1
conversation_id=$(sed -n 's/.*Accepted { entity_id: Some("\([^"]*\)"), revision: 2 }.*/\1/p' "$setup_log" | head -n 1)
if [ -z "$conversation_id" ]; then
    printf '%s\n' 'could not obtain soak conversation id' >&2
    exit 1
fi

{
    printf 'login soak-user long-stream-soak-password\n'
    index=0
    while [ "$index" -lt 100 ]; do
        printf 'generate %s speaker\n' "$conversation_id"
        sleep 0.4
        index=$((index + 1))
    done
    printf 'memory-extract %s speaker\nquit\n' "$conversation_id"
} | timeout 120 "$project_dir/target/release/chatty-client" \
    --broker 127.0.0.1:18443 --server-name localhost \
    --ca "$project_dir/certs/ca.pem" >"$stream_log" 2>&1

finished=$(grep -c 'GenerationFinished.*cancelled: false' "$stream_log")
[ "$finished" -eq 100 ]
grep -q 'delta memory' "$stream_log"

{
    printf 'login soak-user long-stream-soak-password\n'
    index=0
    while [ "$index" -lt 100 ]; do
        printf 'generate %s speaker\n' "$conversation_id"
        sleep 0.02
        printf 'cancel\n'
        sleep 0.3
        index=$((index + 1))
    done
    printf 'quit\n'
} | timeout 120 "$project_dir/target/release/chatty-client" \
    --broker 127.0.0.1:18443 --server-name localhost \
    --ca "$project_dir/certs/ca.pem" >"$cancel_log" 2>&1

cancelled=$(grep -c 'GenerationFinished.*cancelled: true' "$cancel_log")
[ "$cancelled" -eq 100 ]
printf 'stream soak passed: %s completed, %s cancelled\n' "$finished" "$cancelled"
