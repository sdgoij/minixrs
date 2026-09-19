#!/bin/sh
# M2: build the wasm kernel and guest processes, apply Asyncify, run the loop.
#
#   sh tools/wasm-m2/run.sh
#
# Needs the wasm32-unknown-unknown target and Binaryen's wasm-opt (installed
# locally under tools/fork-spike/.tools by the fork spike).

set -e

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
wasm_opt="$root/tools/fork-spike/.tools/node_modules/binaryen/bin/wasm-opt"

echo "== building the wasm kernel instance =="
(cd "$root/crates/kernel-wasm" && cargo build --release --target wasm32-unknown-unknown)

echo "== building the guest processes =="
(cd "$root/crates/wasm-procs" && cargo build --release --target wasm32-unknown-unknown)

mkdir -p "$here/build"
cp "$root/crates/kernel-wasm/target/wasm32-unknown-unknown/release/kernel-wasm.wasm" \
   "$here/build/kernel.wasm"

if [ ! -f "$wasm_opt" ]; then
  echo "error: Binaryen not found at $wasm_opt" >&2
  echo "       install with: npm install binaryen --prefix tools/fork-spike/.tools --no-save" >&2
  exit 1
fi

echo "== applying Asyncify to the processes =="
node "$wasm_opt" --asyncify \
  "$root/crates/wasm-procs/target/wasm32-unknown-unknown/release/wasm_procs.wasm" \
  -o "$here/build/procs.async.wasm"

echo
node "$here/boot.js"
