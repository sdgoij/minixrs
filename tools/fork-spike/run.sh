#!/bin/sh
# Fork spike driver: build the guest, then run both layers.
#
#   sh tools/fork-spike/run.sh
#
# Layer 1 (clone/resume mechanics) needs only node.
# Layer 2 (real Asyncify) additionally needs Binaryen:
#
#   npm install binaryen --prefix tools/fork-spike/.tools --no-save
#
# or point WASM_OPT at a native wasm-opt binary.

set -e

here=$(cd "$(dirname "$0")" && pwd)

echo "== building the guest (wasm32-unknown-unknown, stock toolchain) =="
(cd "$here/guest" && cargo build --release --target wasm32-unknown-unknown)

mkdir -p "$here/build"
cp "$here/guest/target/wasm32-unknown-unknown/release/fork_spike_guest.wasm" \
   "$here/build/guest.wasm"

status=0

echo
echo "== layer 1: host clone/resume mechanics =="
node "$here/host/clonefork.js" || status=1

echo
echo "== layer 2: fork under Binaryen Asyncify =="
node "$here/host/asyncify.js" || status=1

echo
if [ "$status" -eq 0 ]; then
  echo "both layers passed"
else
  echo "at least one layer failed"
fi
exit "$status"
