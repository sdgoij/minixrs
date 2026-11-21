#!/bin/sh
# Print one pinned third-party value, for the CI jobs that only decide whether a
# release has to be built.
#
# Those jobs check out the repository with `submodules: false`, and `cargo` cannot
# resolve this workspace without the `rust` submodule: `minix-rt` depends on
# `compiler_builtins` by path inside it. So `cargo xtask pin` cannot run there, and
# this reads the two things the tag is made of directly: `tools/pins.ini` and the
# `rust` gitlink, which `git rev-parse HEAD:rust` answers without a checkout.
#
# The fields are the ones `cargo xtask pin` prints, and the tag format is the one
# `crates/xtask/src/pin.rs` builds. Change both together; a host test in
# `crates/xtask` cannot compare them, because `sh` is not on a Windows runner's
# PATH.
#
#   tools/pin.sh bash [tag|describe|commit]
#   tools/pin.sh mesa [tag|mesa-tag|mesa-commit|drm-tag|drm-commit|build-config]

set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
pins="$root/tools/pins.ini"

pin() {
    value=$(sed -n "s/^$1=//p" "$pins")
    if [ -z "$value" ]; then
        echo "tools/pin.sh: $pins has no $1" >&2
        exit 1
    fi
    printf '%s\n' "$value"
}

short() {
    cut -c1-12
}

# The commit the `rust` submodule pins. Read from the tree, not from the submodule,
# so it works in a checkout that never fetched it.
rust=$(git -C "$root" rev-parse HEAD:rust | short)

kind=${1:-}
field=${2:-tag}

case "$kind:$field" in
    bash:tag) printf 'bash-%s-r%s\n' "$(pin bash.commit | short)" "$rust" ;;
    bash:describe) pin bash.describe ;;
    bash:commit) pin bash.commit ;;
    mesa:tag)
        printf 'mesa-%s-%s-r%s-c%s\n' \
            "$(pin mesa.commit | short)" \
            "$(pin drm.commit | short)" \
            "$rust" \
            "$(pin mesa.build_config)"
        ;;
    mesa:mesa-tag) pin mesa.tag ;;
    mesa:mesa-commit) pin mesa.commit ;;
    mesa:drm-tag) pin drm.tag ;;
    mesa:drm-commit) pin drm.commit ;;
    mesa:build-config) pin mesa.build_config ;;
    *)
        echo "usage: tools/pin.sh bash|mesa [tag|describe|commit|mesa-tag|mesa-commit|drm-tag|drm-commit|build-config]" >&2
        exit 1
        ;;
esac
