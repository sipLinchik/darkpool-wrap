#!/usr/bin/env bash
# Starts a throwaway solana-test-validator with the freshly built program and runs the suite.
# Usage: (cd .. && cargo build-sbf) && npm ci && npm test
set -euo pipefail
cd "$(dirname "$0")"
SO=${SO:-../target/deploy/darkpool_wrap.so}
PROGRAM_ID=${PROGRAM_ID:-4WMGnz33sRKHynF2an3kWmfZtd8eudTVCExtSmMWuaCc}
[ -f "$SO" ] || { echo "missing $SO: run 'cargo build-sbf' in the repo root first" >&2; exit 1; }
LEDGER=$(mktemp -d)
solana-test-validator --reset --quiet --ledger "$LEDGER" --bpf-program "$PROGRAM_ID" "$SO" >/dev/null 2>&1 &
VALIDATOR=$!
cleanup() { kill "$VALIDATOR" 2>/dev/null || true; wait "$VALIDATOR" 2>/dev/null || true; rm -rf "$LEDGER"; }
trap cleanup EXIT
for _ in $(seq 60); do
  curl -sf -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' http://127.0.0.1:8899 | grep -q ok && break
  sleep 1
done
rc=0
PROGRAM_ID=$PROGRAM_ID node wrap.test.mjs || rc=$?
cleanup
trap - EXIT
exit $rc
