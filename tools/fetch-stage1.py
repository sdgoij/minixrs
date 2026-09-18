#!/usr/bin/env python3
"""Install the prebuilt stage1 toolchain for the pinned rust commit.

`just bootstrap` builds the fork's stage1 compiler from source, LLVM included:
tens of minutes and tens of GB. This downloads the same toolchain from a release
published in this repository instead (see
`.github/workflows/toolchain-release.yml`), tagged by the commit the `rust`
submodule pins, so the toolchain always matches the sources in the tree.

`just bootstrap` stays authoritative: it is what you run when the fork itself
changes (std edits, a new arch, a rebase that moves the LLVM pin). This is the
cache for everyone else.

An existing toolchain is never replaced unless you ask for it with `--force`.
The toolchain cannot identify its own commit - a bootstrap-built rustc reports
`commit-hash: unknown` - so a source build is indistinguishable from a fetched
one; the only record is the marker this script writes next to what it installs.

This repository is public, so no credentials are involved - unlike publishing a
release into the fork, which would need a cross-repo token.

`tools/verify-stage1.py` consumes what this installs, on a Linux host: it runs
this script and then links the smoke binaries with the result, which is how a
release gets checked before an arch job depends on it.

Two environment overrides exist for testing this script and for consuming a
release from elsewhere:

  MINIXRS_STAGE1_BASE   base URL holding `<asset>` and `SHA256SUMS`
                        (default: the fork's release for the pinned commit)
  MINIXRS_STAGE1_DEST   install directory (default: `rust/build/<host>`)

Usage: python tools/fetch-stage1.py [--force]
"""

from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
RELEASE_REPO = "sdgoij/minixrs"
MINIX_TARGETS = ("x86_64-pc-minix", "riscv64gc-unknown-minix", "aarch64-unknown-minix")
# Written inside the installed toolchain, which is the only way to know which
# commit a fetched one came from (see the module docstring).
MARKER = ".minixrs-fetched"


def host_triple() -> str:
    """The build host triple, which is also what x.py names the build dir after."""
    try:
        out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        sys.exit(f"error: cannot run `rustc -vV` to detect the host triple: {e}")
    for line in out.splitlines():
        if line.startswith("host: "):
            return line[6:].strip()
    sys.exit("error: `rustc -vV` did not report a host triple")


def pinned_sha() -> str:
    """The commit the rust submodule pins - the identity of the toolchain."""
    try:
        return subprocess.run(
            ["git", "-C", str(ROOT / "rust"), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        sys.exit(
            "error: cannot read the rust submodule commit "
            f"(`git -C rust rev-parse HEAD`): {e}\n"
            "       Run `git submodule update --init rust` first."
        )


def rustc_in(stage1: pathlib.Path) -> pathlib.Path | None:
    for name in ("rustc.exe", "rustc"):
        exe = stage1 / "bin" / name
        if exe.is_file():
            return exe
    return None


def recorded_sha(stage1: pathlib.Path) -> str:
    """The commit a fetched toolchain was installed for, or "" if unknown."""
    marker = stage1 / MARKER
    if not marker.is_file():
        return ""
    return marker.read_text(encoding="utf-8").split()[0] if marker.read_text().strip() else ""


def download(url: str, dest: pathlib.Path) -> None:
    # GitHub answers release-asset requests without a User-Agent with 403.
    req = urllib.request.Request(url, headers={"User-Agent": "minixrs-fetch-stage1"})
    try:
        with urllib.request.urlopen(req) as resp, open(dest, "wb") as f:
            shutil.copyfileobj(resp, f)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            sys.exit(
                f"error: no prebuilt stage1 at {url}\n"
                "       Nothing has been published for this rust commit and host yet.\n"
                "       Run `just bootstrap` to build the toolchain from source, or publish a\n"
                "       release with the `toolchain release` workflow."
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
    force = "--force" in argv[1:]
    for arg in argv[1:]:
        if arg != "--force":
            sys.exit(f"error: unknown argument {arg!r}\n       usage: {argv[0]} [--force]")

    sha = pinned_sha()
    host = host_triple()
    dest = pathlib.Path(os.environ.get("MINIXRS_STAGE1_DEST") or (ROOT / "rust" / "build" / host))
    stage1 = dest / "stage1"

    if rustc_in(stage1) is not None and not force:
        recorded = recorded_sha(stage1)
        if recorded == sha:
            print(f"stage1 for {sha[:12]} is already installed at {stage1}")
        elif recorded:
            print(f"the stage1 at {stage1} was fetched for {recorded[:12]}, not {sha[:12]}")
            print("re-run with --force to replace it, or `just bootstrap` to build from source")
        else:
            print(f"a stage1 already exists at {stage1} and did not come from here")
            print("(a source build reports no commit, so it cannot be identified)")
            print("re-run with --force to replace it with the published toolchain")
        return 0

    asset_name = f"stage1-{sha}-{host}.tar.xz"
    base = os.environ.get("MINIXRS_STAGE1_BASE") or (
        f"https://github.com/{RELEASE_REPO}/releases/download/stage1-{sha}"
    )
    base = base.rstrip("/")

    tmp = ROOT / "target" / "stage1-download"
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    asset = tmp / asset_name
    sums = tmp / "SHA256SUMS"

    print(f"fetching {asset_name}")
    download(f"{base}/{asset_name}", asset)
    download(f"{base}/SHA256SUMS", sums)
    verify(asset, sums)

    # Replace rather than merge: a toolchain built from another commit may not
    # contain everything this one does, and leftovers from it would be used.
    shutil.rmtree(stage1, ignore_errors=True)
    print(f"extracting into {dest}")
    extract(asset, dest)
    shutil.rmtree(tmp, ignore_errors=True)

    if rustc_in(stage1) is None:
        sys.exit(f"error: {asset.name} did not contain stage1/bin/rustc")
    missing = [t for t in MINIX_TARGETS if not (stage1 / "lib" / "rustlib" / t / "lib").is_dir()]
    if missing:
        sys.exit(f"error: the toolchain has no std for: {', '.join(missing)}")

    (stage1 / MARKER).write_text(f"{sha}  {base}/{asset_name}\n", encoding="utf-8")
    size = sum(f.stat().st_size for f in stage1.rglob("*") if f.is_file())
    print(f"installed stage1 {sha[:12]} ({host}) at {stage1} ({size / 1e9:.2f} GB)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
