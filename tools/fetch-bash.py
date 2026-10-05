#!/usr/bin/env python3
"""Install the prebuilt GNU bash for the pinned commit.

`just build-bash <arch>` clones bash (a full clone — the pin is a commit), rebuilds
`minix-libc` with this host's stage1, configures 209 objects and links them: minutes
per arch, and a 289 MiB clone the first time. This downloads the same binary from a
release published in this repository instead (see
`.github/workflows/bash-release.yml`), tagged by the commit `tools/bash_pin.py`
pins together with the `rust` gitlink, so the binary matches both the sources and
the toolchain it was built with.

`just build-bash` stays authoritative: it is what you run when the port's C headers
or libc change, or to bump the pin. This is the cache for everyone else — CI's
`bash` job fetches instead of building.

An existing bash is never replaced unless you ask for it with `--force`, and a
marker records which release it came from (a built one has no marker, so it is
reported as coming from nowhere).

Two environment overrides exist for testing and for consuming a release from
elsewhere:

  MINIXRS_BASH_BASE   base URL holding `<asset>` and `SHA256SUMS`
                      (default: this repository's release for the pinned tag)
  MINIXRS_BASH_DEST   install directory (default: `target/bash/<arch>`)

Usage: python tools/fetch-bash.py [x86|riscv64|aarch64] [--force]
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

import bash_pin  # noqa: E402
import pin  # noqa: E402
from ccarch import Arch, resolve_argv  # noqa: E402

RELEASE_REPO = "sdgoij/minixrs"
BINARY = "bash"
MARKER = ".minixrs-fetched"


def dest_dir(arch: Arch) -> pathlib.Path:
    return ROOT / "target" / "bash" / arch.name


def recorded_tag(dest: pathlib.Path) -> str:
    """The release a fetched bash came from, or "" if it did not come from here."""
    marker = dest / MARKER
    if not marker.is_file() or not marker.read_text(encoding="utf-8").strip():
        return ""
    return marker.read_text(encoding="utf-8").split()[0]


def download(url: str, dest: pathlib.Path) -> None:
    # GitHub answers release-asset requests without a User-Agent with 403.
    req = urllib.request.Request(url, headers={"User-Agent": "minixrs-fetch-bash"})
    try:
        with urllib.request.urlopen(req) as resp, open(dest, "wb") as f:
            shutil.copyfileobj(resp, f)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            sys.exit(
                f"error: no prebuilt bash at {url}\n"
                "       Nothing has been published for this commit yet.\n"
                "       Run `just build-bash <arch>` to build from source, or publish a\n"
                "       release with the `bash release` workflow."
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

    tag = bash_pin.tag(pin.rust_gitlink())
    dest = pathlib.Path(os.environ.get("MINIXRS_BASH_DEST") or dest_dir(arch))

    if (dest / BINARY).is_file() and not force:
        recorded = recorded_tag(dest)
        if recorded == tag:
            print(f"bash for {tag} is already installed at {dest / BINARY}")
        elif recorded:
            print(f"the bash at {dest / BINARY} was fetched for {recorded}, not {tag}")
            print("re-run with --force to replace it, or `just build-bash` to build from source")
        else:
            print(f"a bash already exists at {dest / BINARY} and did not come from here")
            print("(a source build leaves no marker, so it cannot be identified)")
            print("re-run with --force to replace it with the published one")
        return 0

    asset_name = f"{tag}-{arch.name}.tar.xz"
    base = os.environ.get("MINIXRS_BASH_BASE") or (
        f"https://github.com/{RELEASE_REPO}/releases/download/{tag}"
    )
    base = base.rstrip("/")

    tmp = ROOT / "target" / "bash-download"
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    asset = tmp / asset_name
    sums = tmp / "SHA256SUMS"

    print(f"fetching {asset_name}")
    download(f"{base}/{asset_name}", asset)
    download(f"{base}/SHA256SUMS", sums)
    verify(asset, sums)

    # Replace the binary, but leave the build tree: a source build's configure.log
    # and objects are not this script's to delete, and the artifact is one file.
    (dest / BINARY).unlink(missing_ok=True)
    print(f"extracting into {dest}")
    extract(asset, dest)
    shutil.rmtree(tmp, ignore_errors=True)

    if not (dest / BINARY).is_file():
        sys.exit(f"error: {asset_name} did not contain {BINARY}")

    (dest / MARKER).write_text(f"{tag}  {base}/{asset_name}\n", encoding="utf-8")
    size = (dest / BINARY).stat().st_size
    print(f"installed bash for {tag} ({arch.name}) at {dest / BINARY} ({size / 1e6:.1f} MB)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
