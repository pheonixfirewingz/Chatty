#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  printf '%s\n' "usage: sudo $0 INTERFACE 'TEST_COMMAND'" >&2
  exit 2
fi

test_interface=$1
test_command=$2

case "$test_interface" in
  ""|lo) printf '%s\n' 'refusing an empty or loopback interface' >&2; exit 2 ;;
esac

cleanup() {
  tc qdisc del dev "$test_interface" root 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Combined rural/mobile-hotspot profile: constrained bandwidth, high jitter,
# packet loss, duplication, and reordering. TCP supplies recovery while the
# framing tests verify arbitrary application-level fragmentation.
tc qdisc add dev "$test_interface" root netem \
  delay 250ms 75ms distribution normal \
  loss 3% 25% duplicate 0.2% reorder 1% 50% rate 256kbit

sh -c "$test_command"
