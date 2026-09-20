#!/bin/sh
# Stage the artifacts the page fetches.
#
#   sh tools/wasm-browser/build.sh
#
# The build itself is `tools/wasm-servers/build.sh`, which is the same script the check harness
# runs — deliberately, because the page and the harness run the same system and a page built by a
# different pipeline from the ones the checks ran would be a page nobody had tested. This script
# only copies the results to where the page fetches them from (`build/`, relative to `index.html`),
# so `fetch('build/kernel.wasm')` resolves.

set -e

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
servers="$root/tools/wasm-servers/build"
image="$root/target/images/wasm32-minix/minixfs.img"

sh "$root/tools/wasm-servers/build.sh"

mkdir -p "$here/build"
cp "$servers/kernel.wasm" "$here/build/kernel.wasm"
cp "$servers/servers.async.wasm" "$here/build/servers.async.wasm"
cp "$image" "$here/build/minixfs.img"

echo
echo "staged in $here/build:"
ls -l "$here/build"
echo
echo "now: node tools/wasm-browser/serve.js    (then open http://127.0.0.1:8080/)"
