#!/usr/bin/env python3
"""The pinned GNU bash source, and the release tag that identifies it.

The pin is the commit the working build was validated against — "Bash-5.3 patch
15" (`git describe`: `bash-5.3-16-gb4608166`). bash's git repository ships its
generated files (configure, y.tab.c, the builtins), so a checkout needs no
autotools, and the pin is the commit rather than the tag because upstream tags mark
releases, not each patch. The clone comes from [`BASH_GIT`] — a mirror in this
project's own GitHub org, refreshed by `just mirror-bash` when the pin moves —
because savannah is slow and times out.

Both the build (`tools/build-bash.py`) and the fetch (`tools/fetch-bash.py`) read
the pin from here, and `.github/workflows/bash-release.yml` derives its release tag
from [`tag`] — the commit plus the `rust` gitlink (`tools/pin.py`) — so an artifact
matches both the sources this file names and the toolchain the tree pins. Bumping
bash is editing the constants here: the next CI run sees no release for the new
tag, builds one, and every consumer fetches that instead.

Printed with no arguments (`python tools/bash_pin.py`), which is how the
workflows resolve the tag without importing anything.
"""

from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from pin import rust_gitlink  # noqa: E402

# The build clones the mirror, not upstream: savannah is slow and times out, and no
# CI run should depend on it. `BASH_GIT` is the credential-free read URL, `BASH_PUSH`
# the write one, and `just mirror-bash` refreshes the mirror from [`BASH_UPSTREAM`].
BASH_UPSTREAM = "https://git.savannah.gnu.org/git/bash.git"
BASH_GIT = "https://github.com/sdgoij/bash.git"
BASH_PUSH = "git@github.com:sdgoij/bash.git"
BASH_COMMIT = "b460816602167718f78a6233164e8875f49b75b2"
BASH_DESCRIBE = "Bash-5.3 patch 15"


def tag(rust_commit: str) -> str:
    """The release tag for this pin: the bash commit and the toolchain pin.

    The toolchain belongs in the identity because bash is compiled and linked with
    it (and against the port's libc it builds): a fork bump can change the target ABI
    or the C headers, and a shell built before it must not be handed out for the new
    one. Reconciling the two is what makes a fork bump rebuild rather than ship a
    stale binary (`rust-pin` is where the fork pin is watched, in `ci.yml`).
    """
    return f"bash-{BASH_COMMIT[:12]}-r{rust_commit[:12]}"


if __name__ == "__main__":
    print(tag(rust_gitlink()))
