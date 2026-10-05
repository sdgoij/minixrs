#!/usr/bin/env python3
"""Install the prebuilt Mesa + libdrm shared objects for the pinned version.

`just build-mesa <arch> --build` compiles Mesa, libdrm and the C++ runtime from
source — tens of minutes and a meson host per arch. This downloads the same seven
DSOs, and the handful of headers the surfaceless client compiles against, from a
release published in this repository instead (see
`.github/workflows/mesa-release.yml`), tagged by the Mesa and libdrm commits
`tools/mesa_pin.py` pins, so they always match the sources in the tree.

`just build-mesa` stays authoritative: it is what you run when the port's C
toolchain or `tools/cc-dso-minix.py` changes, or to bump the pin. This is the
cache for everyone else — CI's `gltriangle` job fetches instead of building.

An existing set of DSOs is never replaced unless you ask for it with `--force`,
and a marker records which release it came from (a built set has no marker, so it
is reported as coming from nowhere).

Two environment overrides exist for testing and for consuming a release from
elsewhere:

  MINIXRS_MESA_BASE   base URL holding `<asset>` and `SHA256SUMS`
                      (default: this repository's release for the pinned tag)
  MINIXRS_MESA_DEST   install directory (default: `target/mesa/<arch>`)

Usage: python tools/fetch-mesa.py [x86|riscv64|aarch64] [--force]
"""

from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import sys
import tarfile
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

import mesa_pin  # noqa: E402
from ccarch import Arch, resolve_argv  # noqa: E402

RELEASE_REPO = "sdgoij/minixrs"
# The header whose presence means "a usable set is installed", alongside the DSO
# the loader looks up first (`gltriangle` links it and includes this).
SONAME = "libEGL.so.1"
HEADER = "include/EGL/egl.h"
MARKER = ".minixrs-fetched"


def dest_dir(arch: Arch) -> pathlib.Path:
    return ROOT / "target" / "mesa" / arch.name


def recorded_tag(dest: pathlib.Path) -> str:
    """The release a fetched set came from, or "" if it did not come from here."""
    marker = dest / MARKER
    if not marker.is_file() or not marker.read_text(encoding="utf-8").strip():
        return ""
    return marker.read_text(encoding="utf-8").split()[0]


def download(url: str, dest: pathlib.Path) -> None:
    # GitHub answers release-asset requests without a User-Agent with 403.
    req = urllib.request.Request(url, headers={"User-Agent": "minixrs-fetch-mesa"})
    try:
        with urllib.request.urlopen(req) as resp, open(dest, "wb") as f:
            shutil.copyfileobj(resp, f)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            sys.exit(
                f"error: no prebuilt Mesa DSOs at {url}\n"
                "       Nothing has been published for this pin yet.\n"
                "       Run `just build-mesa <arch> --build` to build from source, or publish a\n"
                "       release with the `mesa release` workflow."
            )
        sys.exit(f"error: downloading {url} failed: {e}")
    except urllib.error.URLError as e:
        sys.exit(f"error: downloading {url} failed: {e}")


def verify(asset: pathlib.Path, sums: pathlib.Path) -> None:
    """Check `asset` against its line in `sums` (a `sha256sum` file)."""
    wanted = None
    for line in sums.read_text(encoding="utf-8").splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == asset.name:
            wanted = parts[0].lower()
            break
    if wanted is None:
        sys.exit(f"error: {sums.name} has no entry for {asset.name}")

    digest = hashlib.sha256()
    with open(asset, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            digest.update(chunk)
    actual = digest.hexdigest()
    if actual != wanted:
        sys.exit(
            f"error: checksum mismatch for {asset.name}\n"
            f"       expected {wanted}\n"
            f"       got      {actual}"
        )
    print(f"checksum ok: {asset.name}")


def extract(asset: pathlib.Path, dest: pathlib.Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    with tarfile.open(asset, "r:xz") as tf:
        # The archive is checksum-verified and built by our own workflow, so the
        # `tar` filter (the 3.12 default) is enough; it still refuses paths that
        # would escape `dest`.
        if hasattr(tarfile, "tar_filter"):
            tf.extractall(path=dest, filter="tar")
        else:
            tf.extractall(path=dest)


def main(argv: list[str]) -> int:
    force = "--force" in argv
    rest = [a for a in argv[1:] if a != "--force"]
    arch, rest = resolve_argv(rest)
    if rest:
        sys.exit(f"error: unknown argument {rest[0]!r}\n"
                 f"       usage: {argv[0]} [x86|riscv64|aarch64] [--force]")

    tag = mesa_pin.tag()
    dest = pathlib.Path(os.environ.get("MINIXRS_MESA_DEST") or dest_dir(arch))
    lib = dest / "lib"

    if (lib / SONAME).is_file() and not force:
        recorded = recorded_tag(dest)
        if recorded == tag:
            print(f"Mesa DSOs for {tag} are already installed at {lib}")
        elif recorded:
            print(f"the DSOs at {lib} were fetched for {recorded}, not {tag}")
            print("re-run with --force to replace them, or `just build-mesa` to build from source")
        else:
            print(f"DSOs already exist at {lib} and did not come from here")
            print("(a source build leaves no marker, so it cannot be identified)")
            print("re-run with --force to replace them with the published set")
        return 0

    asset_name = f"{tag}-{arch.name}.tar.xz"
    base = os.environ.get("MINIXRS_MESA_BASE") or (
        f"https://github.com/{RELEASE_REPO}/releases/download/{tag}"
    )
    base = base.rstrip("/")

    tmp = ROOT / "target" / "mesa-download"
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    asset = tmp / asset_name
    sums = tmp / "SHA256SUMS"

    print(f"fetching {asset_name}")
    download(f"{base}/{asset_name}", asset)
    download(f"{base}/SHA256SUMS", sums)
    verify(asset, sums)

    # Replace rather than merge: a set built for another pin may not contain
    # everything this one does, and leftovers would be staged into the image.
    shutil.rmtree(lib, ignore_errors=True)
    print(f"extracting into {dest}")
    extract(asset, dest)
    shutil.rmtree(tmp, ignore_errors=True)

    if not (lib / SONAME).is_file():
        sys.exit(f"error: {asset_name} did not contain lib/{SONAME}")
    if not (dest / HEADER).is_file():
        sys.exit(f"error: {asset_name} did not contain {HEADER}")

    dest.mkdir(parents=True, exist_ok=True)
    (dest / MARKER).write_text(f"{tag}  {base}/{asset_name}\n", encoding="utf-8")
    count = len(list(lib.glob("*.so*")))
    print(f"installed Mesa DSOs for {tag} ({arch.name}) at {lib} ({count} objects)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
