#!/bin/sh
# M1: build the wasm kernel instance and boot it under Node.
#
#   sh tools/wasm-boot/run.sh
#
# Needs only the stock toolchain with the wasm32-unknown-unknown target, which
# rustup installs by default here. No rust fork, no bootstrap, no QEMU.

set -e

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

echo "== building the wasm kernel instance =="
(cd "$root/crates/kernel-wasm" && cargo build --release --target wasm32-unknown-unknown)

mkdir -p "$here/build"
cp "$root/crates/kernel-wasm/target/wasm32-unknown-unknown/release/kernel-wasm.wasm" \
   "$here/build/kernel.wasm"

echo
node "$here/boot.js" "$here/build/kernel.wasm"
