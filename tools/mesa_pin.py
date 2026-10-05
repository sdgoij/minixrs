#!/usr/bin/env python3
"""The pinned Mesa + libdrm sources, and the release tag that identifies them.

Both the build (`tools/build-mesa.py`) and the fetch (`tools/fetch-mesa.py`) read
the pin from here, and `.github/workflows/mesa-release.yml` derives its release
tag from [`tag`], so an artifact can only ever match the sources this file names.
Bumping a version is editing the constants here: the next CI run sees no release
for the new tag, builds one, and every consumer fetches that instead.

Printed with no arguments (`python tools/mesa_pin.py`), which is how the
workflows resolve the tag without importing anything.
"""

MESA_GIT = "https://gitlab.freedesktop.org/mesa/mesa.git"
MESA_TAG = "mesa-25.3.6"
MESA_COMMIT = "06f9e28304d5d3f109c33535c1c25b9df5769af2"

DRM_GIT = "https://gitlab.freedesktop.org/mesa/drm.git"
DRM_TAG = "libdrm-2.4.129"
DRM_COMMIT = "a8e5e10a873f67f557dc70e5407af4553f35edd9"


def tag() -> str:
    """The release tag for these pins: both commits, so one tag names one pair.

    Short (12 hex each) because it is a release tag a human reads and types, and
    the full commits are what the build asserts its fetches against anyway.
    """
    return f"mesa-{MESA_COMMIT[:12]}-{DRM_COMMIT[:12]}"


if __name__ == "__main__":
    print(tag())
