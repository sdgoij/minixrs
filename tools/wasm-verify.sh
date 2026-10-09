#!/bin/sh
# Run CI's `wasm-tests` job (`.github/workflows/ci.yml`) locally: build and
# stage the three wasm workspaces, then run the three harnesses over them —
# `boot.cjs` (the boot chain and every device, at quiescence), `run.js` (the
# same artifacts with a shell at a live prompt) and `page.test.js` (the page's
# own code under a stub DOM).
#
#   sh tools/wasm-verify.sh            # all three
#   sh tools/wasm-verify.sh boot       # one of: boot | run | page
#   WASM_TRACE_IPC=1 sh tools/wasm-verify.sh boot
#
# Run it from a Linux host — CI, or a Linux guest such as WSL. A Windows host
# cannot link the modules: the checkout's stage1 is a Windows `rustc`, and its
# `lld` does not do `-flavor wasm`. When the checkout's own stage1 is not the
# linking host's, point the build at one that is — e.g. a stage1 fetched with
# `MINIXRS_STAGE1_DEST=/tmp/stage1 python tools/fetch-stage1.py`:
#
#   MINIXRS_RUSTC=/tmp/stage1/bin/rustc \
#   MINIXRS_WASM_LINKER=/tmp/stage1/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld \
#     sh tools/wasm-verify.sh
#
# Env: `MINIXRS_RUSTC` and `MINIXRS_WASM_LINKER` select the toolchain (see
# above); `WASM_TIMEOUT` is the per-harness wall-clock bound in seconds
# (default 600). `WASM_TRACE`/`WASM_TRACE_IPC` are read by `boot.cjs` and pass
# through untouched.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

# `tools/lld.py`, the build scripts and `just` all call `python`, but some
# hosts ship only `python3` (CI works around it with a `sudo ln -sf`). Shim it
# in a temp dir so the tool runs on a stock Fedora/WSL without sudo.
shim=$(mktemp -d)
trap 'rm -rf "$shim"' 0
if ! command -v python >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  ln -s "$(command -v python3)" "$shim/python"
fi
if [ -d "$HOME/.cargo/bin" ]; then
  PATH="$HOME/.cargo/bin:$PATH"
fi
PATH="$shim:$PATH"
export PATH

only=${1:-all}
budget=${WASM_TIMEOUT:-600}

echo "== build and stage the wasm artifacts =="
sh tools/wasm-browser/build.sh

run() {
  echo
  echo "== $1: node $2 =="
  timeout "$budget" node "$2"
}

case "$only" in
  all)
    run boot tools/wasm-servers/boot.cjs
    run run  tools/wasm-browser/run.js
    run page tools/wasm-browser/page.test.js
    ;;
  boot) run boot tools/wasm-servers/boot.cjs ;;
  run)  run run  tools/wasm-browser/run.js ;;
  page) run page tools/wasm-browser/page.test.js ;;
  *)
    echo "usage: sh tools/wasm-verify.sh [all|boot|run|page]" >&2
    exit 2
    ;;
esac

echo
echo "wasm-verify: ok"
