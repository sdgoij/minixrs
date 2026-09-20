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

echo "== building a program as a wasm module =="
(cd "$root/crates/wasm-program" && \
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

# The RAM disk instance is given the boot filesystem image. On the other arches that image holds
# ELF executables built by `just build`; here it holds the *program module* (§7.2's step 4), so
# the pipeline is: build the module, Asyncify it, stage it where the image builder looks, and
# build the image from it. An image whose module is stale is worse than no image, because the
# failure it produces is an exec after a successful boot.
wasm_release="$root/target/wasm32-minix/release"
mkdir -p "$wasm_release"
cp "$here/build/program.async.wasm" "$wasm_release/program.async.wasm"

image="$root/target/images/wasm32-minix/minixfs.img"
echo "== building the wasm boot filesystem image =="
(cd "$root" && cargo run -q -p boot-image --bin mkminixfs wasm32)

if [ ! -f "$image" ]; then
  echo "error: no image at $image" >&2
  exit 1
fi

echo "== applying Asyncify to the servers =="
node "$wasm_opt" --asyncify \
  "$root/crates/wasm-servers/target/wasm32-minix/release/wasm_servers.wasm" \
  -o "$here/build/servers.async.wasm"

# A program module needs the same treatment, for the same reason and more: the dispatch loop
# reads `asyncify_get_state` after every entry to tell a blocked process from a finished one,
# so a module that was not instrumented cannot be driven at all — and a program that blocks
# (step 2 of the M7a plan) has to unwind through Asyncify for the host to get control back.
echo "== applying Asyncify to the program =="
node "$wasm_opt" --asyncify \
  "$root/crates/wasm-program/target/wasm32-minix/release/wasm_program.wasm" \
  -o "$here/build/program.async.wasm"

echo
# Bounded, because a boot that deadlocks deadlocks *silently*: an instance waiting on
# an answer that never comes leaves the host looping in the dispatch step, with no
# output to say so. `timeout` was already the house answer for the QEMU recipes; this is
# the same failure mode and it should not be able to cost a quarter of an hour.
/usr/bin/timeout -s 9 120 node "$here/boot.cjs"
