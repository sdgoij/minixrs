#!/bin/sh
# Build the wasm system and stage the demo where GitHub Pages can serve it.
#
#   sh tools/wasm-browser/publish.sh      (or: just publish-wasm)
#
# Why `docs/`: it is the one directory a repository can point Pages at without a build step on
# anyone else's machine, so the demo needs no CI, no server and no account. What that costs is
# that the assets have to be *in* the repository: `.gitignore` keeps `*.wasm` out everywhere
# else and makes an exception for `docs/build/`, and the boot image is 16 MiB — so a published
# demo is about 19 MiB of history per build. Rebuild it when the demo should change, not on
# every commit.
#
# The site is the browser-side files plus the three artifacts `tools/wasm-servers/build.sh`
# produces. Nothing else goes in: the harnesses, the tests, `serve.js` and the build scripts are
# development tools, and none of them can run in a browser.

set -e

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
site="$root/docs"

# The build, the same one the page and the check harness run — including the staging into
# `tools/wasm-browser/build/`, which is where this script copies the artifacts from.
sh "$here/build.sh" >/dev/null

mkdir -p "$site/build"

for file in index.html page.js host.js display.js store.js terminal.js net.js package.json; do
  cp "$here/$file" "$site/$file"
done

for artifact in kernel.wasm servers.async.wasm minixfs.img; do
  cp "$here/build/$artifact" "$site/build/$artifact"
done

# Pages runs everything through Jekyll unless it is told not to. Nothing here is a template, and
# a site with no build step should say so rather than be processed by one.
: > "$site/.nojekyll"

echo "staged in $site:"
ls -l "$site" "$site/build" | sed 's/^/  /'
echo
echo "  total: $(du -sh "$site" 2>/dev/null | cut -f1)"

echo
echo "== checking the staged site =="
node "$here/publish-check.mjs"

cat <<'END'

to publish it (once):
  git add docs && git commit -m "docs: publish the wasm demo"
  git push
  Settings → Pages → Source: "Deploy from a branch", branch <main>, folder "/docs"

the demo is then at https://<user>.github.io/<repo>/
END
