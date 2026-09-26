#!/bin/sh
# Build the three wasm artifacts and the boot filesystem image.
#
#   sh tools/wasm-servers/build.sh
#
# These are the inputs every wasm front end runs: `tools/wasm-servers/boot.cjs` (the check
# harness) and `tools/wasm-browser/` (the page). They are built here rather than in each front
# end because they are one system with two front ends, and a page running artifacts built by a
# different pipeline from the ones the checks ran would be a page nobody had tested.
#
# Unlike the M1/M2 harnesses this builds for `wasm32-minix`, so the servers compile with the real
# `target_os = "minix"` bodies rather than the host stubs. That target is one of the fork's own
# (`compiler/rustc_target/src/spec/targets/wasm32_minix.rs`), so this needs no nightly and no
# `-Z build-std`: the stage1 compiler everything else uses has the target built in, and
# `tools/rust-config.py` lists it with `no-std = true`, so its sysroot holds the `core` and
# `alloc` a module links. Binaryen's wasm-opt is the one thing still fetched separately
# (tools/fork-spike/.tools). Artifacts land in `$here/build/`.

set -e

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
wasm_opt="$root/tools/fork-spike/.tools/node_modules/binaryen/bin/wasm-opt"

rustc=$(ls "$root"/rust/build/*/stage1/bin/rustc.exe "$root"/rust/build/*/stage1/bin/rustc 2>/dev/null | head -1)
if [ -z "$rustc" ]; then
  echo "error: the fork's stage1 compiler was not found - run \`just bootstrap\`" >&2
  exit 1
fi

# The target asks for `rust-lld`, which a Windows stage1 does not ship, so the linker is passed
# explicitly. `tools/lld.py` is the same resolver the Justfile uses for the minix triples, and
# what it finds handles `-flavor wasm`. It goes in through `CARGO_TARGET_WASM32_MINIX_LINKER`
# rather than `RUSTFLAGS`: rustflags would displace the `--export`/`--import-memory` flags each
# crate sets in its own `.cargo/config.toml`, and the env key is what cargo reads for exactly
# this setting.
lld=$(python "$root/tools/lld.py")
if [ -z "$lld" ]; then
  echo "error: no lld to link the wasm modules with - run \`just bootstrap\`" >&2
  exit 1
fi

build() {
  echo "== building $1 =="
  (cd "$root/crates/$1" && \
    RUSTC="$rustc" CARGO_TARGET_WASM32_MINIX_LINKER="$lld" \
      cargo build --release --target wasm32-minix)
}

build kernel-wasm
build wasm-servers
build wasm-program

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

# A program module needs the same treatment, for the same reason and more: the dispatch loop
# reads `asyncify_get_state` after every entry to tell a blocked process from a finished one,
# so a module that was not instrumented cannot be driven at all — and a program that blocks
# (step 2 of the M7a plan) has to unwind through Asyncify for the host to get control back.
echo "== applying Asyncify to the program =="
node "$wasm_opt" --asyncify \
  "$root/crates/wasm-program/target/wasm32-minix/release/wasm_program.wasm" \
  -o "$here/build/program.async.wasm"

# The RAM disk instance is given the boot filesystem image. On the other arches that image holds
# ELF executables built by `just build`; here it holds the *program module* (§7.2's step 4), so
# the pipeline is: build the module, Asyncify it, stage it where the image builder looks, and
# build the image from it. An image whose module is stale is worse than no image, because the
# failure it produces is an exec after a successful boot — which is also why the staging has to
# come *after* the Asyncify pass: staging what the previous run left in `build/` puts the
# previous run's module in the image, and the only symptom is an exec whose `argv[0]` the module
# in the image has never heard of.
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
