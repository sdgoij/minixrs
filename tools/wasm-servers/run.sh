#!/bin/sh
# Real servers as wasm processes: DS, RS and PM spawned by the kernel and driven
# to their main loops.
#
#   sh tools/wasm-servers/run.sh
#
# Unlike the M1/M2 harnesses this builds for `wasm32-minix`, so the servers
# compile with the real `target_os = "minix"` bodies rather than the host stubs.
# That target is a JSON spec built with `-Z build-std`, so this needs a nightly
# toolchain and Binaryen's wasm-opt (installed under tools/fork-spike/.tools).

set -e

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
target="$root/tools/wasm-target/wasm32-minix.json"
wasm_opt="$root/tools/fork-spike/.tools/node_modules/binaryen/bin/wasm-opt"

echo "== building the wasm kernel instance =="
(cd "$root/crates/kernel-wasm" && \
  cargo +nightly build -Z json-target-spec -Z build-std=core,alloc \
    --release --target "$target")

echo "== building the servers as a wasm module =="
(cd "$root/crates/wasm-servers" && \
  cargo +nightly build -Z json-target-spec -Z build-std=core,alloc \
    --release --target "$target")

if [ ! -f "$wasm_opt" ]; then
  echo "error: Binaryen not found at $wasm_opt" >&2
  echo "       install with: npm install binaryen --prefix tools/fork-spike/.tools --no-save" >&2
  exit 1
fi

mkdir -p "$here/build"
cp "$root/crates/kernel-wasm/target/wasm32-minix/release/kernel-wasm.wasm" \
   "$here/build/kernel.wasm"

echo "== applying Asyncify to the servers =="
node "$wasm_opt" --asyncify \
  "$root/crates/wasm-servers/target/wasm32-minix/release/wasm_servers.wasm" \
  -o "$here/build/servers.async.wasm"

echo
node "$here/boot.cjs"
