#!/usr/bin/env python3
"""The pinned Mesa + libdrm sources, and the release tag that identifies them.

Both the build (`tools/build-mesa.py`) and the fetch (`tools/fetch-mesa.py`) read
the pin from here, and `.github/workflows/mesa-release.yml` derives its release
tag from [`tag`] — the two commits plus the `rust` gitlink (`tools/pin.py`) — so an
artifact matches both the sources this file names and the toolchain the tree pins.
Bumping a version is editing the constants here: the next CI run sees no release
for the new tag, builds one, and every consumer fetches that instead.

Printed with no arguments (`python tools/mesa_pin.py`), which is how the
workflows resolve the tag without importing anything.
"""

from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from pin import rust_gitlink  # noqa: E402

MESA_GIT = "https://gitlab.freedesktop.org/mesa/mesa.git"
MESA_TAG = "mesa-25.3.6"
MESA_COMMIT = "06f9e28304d5d3f109c33535c1c25b9df5769af2"

DRM_GIT = "https://gitlab.freedesktop.org/mesa/drm.git"
DRM_TAG = "libdrm-2.4.129"
DRM_COMMIT = "a8e5e10a873f67f557dc70e5407af4553f35edd9"


def tag(rust_commit: str) -> str:
    """The release tag for these pins: both commits and the toolchain pin.

    The Mesa and libdrm commits name the sources; the toolchain pin is there because
    the DSOs are compiled by it and link the C++ runtime built from the fork's headers
    and the port's libc. A fork bump can change the target ABI, so a DSO built before
    it must not be handed out for the new one. The Mesa/libdrm halves are short (12 hex
    each) because the tag is what a human reads and types.
    """
    return f"mesa-{MESA_COMMIT[:12]}-{DRM_COMMIT[:12]}-r{rust_commit[:12]}"


if __name__ == "__main__":
    print(tag(rust_gitlink()))
