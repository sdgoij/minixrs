#!/bin/sh
# Real servers as wasm processes: DS, RS and PM spawned by the kernel and driven
# to their main loops.
#
#   sh tools/wasm-servers/run.sh
#
# The artifacts it runs are built by `build.sh` next to it, and `tools/wasm-browser/` runs the
# same ones — see that script for what is built and why it is shared.

set -e

here=$(cd "$(dirname "$0")" && pwd)

sh "$here/build.sh"

echo
# Bounded, because a boot that deadlocks deadlocks *silently*: an instance waiting on
# an answer that never comes leaves the host looping in the dispatch step, with no
# output to say so. `timeout` was already the house answer for the QEMU recipes; this is
# the same failure mode and it should not be able to cost a quarter of an hour.
/usr/bin/timeout -s 9 120 node "$here/boot.cjs"
