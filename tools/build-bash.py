#!/usr/bin/env python3
"""Build GNU bash for x86_64 minix, from a pinned upstream commit.

bash is the port's largest C consumer and its broadest test of the C surface:
209 objects, its own configure, and everything from termios to globbing. It is
also the reason several of the C headers and libc entry points exist at all
(`C_BUILD.md` records which). This script is the build that keeps them honest:
it fetches bash at a pinned commit, builds minix-libc with the *same* stage1 the
link uses, configures bash against the port's headers, links it, and leaves the
result at `target/bash/bash`.

Everything it needs comes from `just fetch-stage1` (or `bootstrap`) plus clang,
gcc, make and git on the build host. The host has to be POSIX — the stage1, the
rlib and clang must all be the same host's — so on Windows the build re-enters
WSL. Override the distribution with `MINIX_WSL_DISTRO`; the default is the one
C_BUILD.md's write-up used.

    python tools/build-bash.py [--force] [--jobs N]

`--force` refetches the source and reconfigures (the default reuses both, and
still rebuilds the libc and relinks, so it is safe to re-run after a libc edit).

Deliberately not a `BOOT_BINS` entry: an image that always carries a 1.4 MB bash
would build only where bash had been built, and every other recipe would inherit
the dependency. `just test-bash` injects it through `MINIXFS_EXTRA` instead.
"""

from __future__ import annotations

import os
import pathlib
import shlex
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
TOOLS = ROOT / "tools"
sys.path.insert(0, str(TOOLS))

from lld import find_lld, host_triple  # noqa: E402

TARGET = "x86_64-pc-minix"

# Pinned: the commit the working build was validated against — "Bash-5.3 patch
# 15" (`git describe`: bash-5.3-16-gb4608166). bash's git repository ships its
# generated files (configure, y.tab.c, the builtins), so a checkout needs no
# autotools; what it does not ship is a stable URL, hence the commit rather than
# the tag.
BASH_GIT = "https://git.savannah.gnu.org/git/bash.git"
BASH_COMMIT = "b460816602167718f78a6233164e8875f49b75b2"
BASH_DESCRIBE = "Bash-5.3 patch 15"

SRC = ROOT / "target" / "bash-src"
BUILD = ROOT / "target" / "bash-build"
ARTIFACT = ROOT / "target" / "bash" / "bash"
CONFIGURE_LOG = ROOT / "target" / "bash-configure.log"
MAKE_LOG = ROOT / "target" / "bash-make.log"
# Written into the build directory: which commit it was configured for. A pin
# change means the tree on disk is not the tree the config describes.
BUILD_MARKER = BUILD / ".minixrs-commit"

# Two configure knobs a *static* libc makes necessary, both of which upstream
# never sees because a shared libc interposes instead (C_BUILD.md has the link
# errors each one fixes):
#   --without-bash-malloc  bash's lib/malloc and the libc's malloc are otherwise
#                          both in the link, and lld refuses the duplicate.
#   -DNEED_EXTERN_PC       readline defines PC/BC/UP for a curses-provided
#                          termcap, and bash's bundled lib/termcap defines the
#                          same three; the macro is how readline is told to
#                          declare them instead.
CONFIGURE_FLAGS = ("--host=x86_64-unknown-none", "--disable-nls", "--without-bash-malloc")
CFLAGS = "-DNEED_EXTERN_PC"

# The host tools (mkbuiltins, mksignames, ...) are built by CC_FOR_BUILD against
# buildconf.h, which has no HAVE_STDBOOL_H: bashansi.h then writes
# `typedef unsigned char bool`, which a C23 compiler rejects (gcc 16 defaults to
# it, clang to gnu17). Either compiler works; both need to be told C17.
CC_FOR_BUILD_COMPILER = ("gcc", "clang")
CC_FOR_BUILD_STD = "-std=gnu17"


def cc_for_build() -> str:
    for name in CC_FOR_BUILD_COMPILER:
        if shutil.which(name) is not None:
            return f"{name} {CC_FOR_BUILD_STD}"
    die(f"no {CC_FOR_BUILD_COMPILER[0]} or {CC_FOR_BUILD_COMPILER[1]} on PATH for "
        "bash's host tools (CC_FOR_BUILD)")


def note(msg: str) -> None:
    print(f"[bash] {msg}", flush=True)


def die(msg: str):
    sys.exit(f"error: {msg}")


def run(cmd: list[str], **kwargs) -> int:
    print("+", " ".join(str(c) for c in cmd), file=sys.stderr, flush=True)
    return subprocess.run([str(c) for c in cmd], **kwargs).returncode


def host_triple() -> str:
    try:
        out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True)
    except (OSError, subprocess.CalledProcessError) as e:
        die(f"cannot run `rustc -vV` to detect the host triple: {e}")
    for line in out.stdout.splitlines():
        if line.startswith("host: "):
            return line[6:].strip()
    die("`rustc -vV` did not report a host triple")


def host_stage1_rustc() -> pathlib.Path:
    """This host's stage1 rustc — the same one the rlib must be built with."""
    host = host_triple()
    exe = ROOT / "rust" / "build" / host / "stage1" / "bin" / "rustc"
    if not exe.is_file():
        die(f"no stage1 rustc at {exe} — run `just fetch-stage1` (or `just bootstrap`)")
    return exe


def log_tail(path: pathlib.Path, lines: int = 25) -> str:
    if not path.is_file():
        return f"({path} was not written)"
    text = path.read_text(encoding="utf-8", errors="replace").splitlines()
    return "\n".join(text[-lines:])


# ---------------------------------------------------------------- Windows host

def run_in_wsl(argv: list[str]) -> int:
    """Re-enter this script in WSL: the build needs a POSIX host.

    The source, the build tree and the artifact all live under `target/` in the
    repository, which WSL reaches as a mounted path, so the inner run works on
    the same files — and its `rust/build/<host>/stage1` is the Linux one.

    The inner shell is a *login* shell, which is what puts `~/.cargo/bin` (cargo,
    rustc) on PATH: a non-login `python3` straight from `wsl.exe` has neither.
    """
    wsl = shutil.which("wsl")
    if wsl is None:
        die("this host is Windows and has no `wsl` to build in; bash's build needs a "
            "POSIX host (see C_BUILD.md)")
    distro = os.environ.get("MINIX_WSL_DISTRO", "FedoraLinux-44")
    inner = (f"cd {shlex.quote(wsl_path(ROOT))} && "
             f"python3 tools/build-bash.py {shlex.join(argv)}").strip()
    note(f"Windows host: running the build in WSL ({distro}). "
         "Set MINIX_WSL_DISTRO to use another distribution.")
    rc = run([wsl, "-d", distro, "--", "bash", "-lc", inner])
    if rc != 0:
        note(f"the WSL build failed (rc={rc}). If the distribution is not installed, "
             f"set MINIX_WSL_DISTRO (tried {distro!r}).")
    return rc


def wsl_path(path: pathlib.Path) -> str:
    """`path` as WSL sees it: `C:\\a\\b` is `/mnt/c/a/b`."""
    text = str(path).replace("\\", "/")
    if len(text) > 1 and text[1] == ":":
        return f"/mnt/{text[0].lower()}{text[2:]}"
    return text


# ------------------------------------------------------------------ source

def fetch_source(force: bool) -> None:
    """Check bash out at the pinned commit, under `target/bash-src`.

    Cloned here rather than in the tree at the repository root so that a build
    is reproducible from a clean checkout, and checked out on a POSIX host so
    that the line endings are the ones git stores: a Windows checkout with
    `core.autocrlf=true` writes `configure` with CRLF, and `/bin/sh^M` is not a
    shell (C_BUILD.md, trap 1).
    """
    git = ["git", "-c", "core.autocrlf=false", "-c", "core.eol=lf"]
    commit = os.environ.get("MINIXRS_BASH_COMMIT", BASH_COMMIT)
    if force and SRC.is_dir():
        shutil.rmtree(SRC)
    if not (SRC / ".git").is_dir():
        SRC.parent.mkdir(parents=True, exist_ok=True)
        if run([*git, "clone", os.environ.get("MINIXRS_BASH_GIT", BASH_GIT), str(SRC)]) != 0:
            die(f"cloning {BASH_GIT} failed")
    elif not have_commit(git, commit):
        note("the pinned commit is not in the local clone; fetching")
        run([*git, "-C", str(SRC), "fetch", "origin"])

    if run([*git, "-C", str(SRC), "checkout", "--force", commit]) != 0:
        die(f"checking out {commit} in {SRC} failed (is the commit published?)")

    head = subprocess.run([*git, "-C", str(SRC), "rev-parse", "HEAD"],
                          capture_output=True, text=True, check=True).stdout.strip()
    if head != commit:
        die(f"{SRC} is at {head}, not the pinned {commit}")

    # make regenerates what is older than its input. Git writes a checkout in
    # index order, which happens to leave `configure` newer than configure.ac —
    # but that is luck, and it costs a build host `autoconf` (or `bison`) when it
    # goes the other way. Both pairs are generated files that ship in the tree,
    # so their mtimes are the only thing to fix.
    for generated in ("configure", "y.tab.c"):
        path = SRC / generated
        if path.is_file():
            os.utime(path, None)
    note(f"source at {BASH_DESCRIBE} ({commit[:12]}), {SRC}")


def have_commit(git: list[str], commit: str) -> bool:
    return subprocess.run([*git, "-C", str(SRC), "rev-parse", "--verify", "--quiet",
                           f"{commit}^{{commit}}"], capture_output=True).returncode == 0


# ------------------------------------------------------------------ libc

def build_libc() -> None:
    """Build minix-libc with this host's stage1.

    The rlib records the `core` it was built against, so a host that links with
    a different stage1 gets E0460 ("found possibly newer version of crate
    core") — and cargo cannot tell: the Linux build's fingerprint says it is
    current while the Windows build, seeing the output change, recompiles. So
    delete the rlib first and rebuild, in this order, with no build from another
    host in between.
    """
    rlibs = list((ROOT / "target" / TARGET / "release" / "deps").glob("libminix_libc-*.rlib"))
    for rlib in rlibs:
        rlib.unlink()
    rustc = host_stage1_rustc()
    note(f"minix-libc with {rustc}")
    env = {**os.environ, "RUSTC": str(rustc)}
    if run(["cargo", "build", "-p", "minix-libc", "--target", TARGET, "--release"],
           cwd=ROOT, env=env) != 0:
        die("building minix-libc failed")
    note("minix-libc rebuilt with this host's stage1")


# ------------------------------------------------------------------ configure + make

def configured_for() -> str:
    return BUILD_MARKER.read_text(encoding="utf-8").strip() if BUILD_MARKER.is_file() else ""


def configure(force: bool) -> None:
    """Run bash's configure out-of-tree, if the tree is not configured for the pin."""
    want = os.environ.get("MINIXRS_BASH_COMMIT", BASH_COMMIT)
    if not force and (BUILD / "config.status").is_file() and configured_for() == want:
        note("configure: reusing the existing config.status")
        return
    if force and BUILD.is_dir():
        shutil.rmtree(BUILD)
    BUILD.mkdir(parents=True, exist_ok=True)

    env = {
        **os.environ,
        "CC": f"python3 {TOOLS / 'cc-minix.py'}",
        "CC_FOR_BUILD": cc_for_build(),
        "CFLAGS": CFLAGS,
    }
    lld = find_lld()
    if lld is not None:
        env["LD"] = str(lld)
    note(f"CC={env['CC']}  CC_FOR_BUILD={env['CC_FOR_BUILD']}  LD={env.get('LD', '-')}")

    note(f"configure {SRC}/configure {' '.join(CONFIGURE_FLAGS)}")
    with open(CONFIGURE_LOG, "w", encoding="utf-8") as log:
        rc = run([str(SRC / "configure"), *CONFIGURE_FLAGS], cwd=BUILD, env=env,
                 stdout=log, stderr=subprocess.STDOUT)
    if rc != 0:
        die(f"configure failed (rc={rc}); tail of {CONFIGURE_LOG}:\n"
            f"{log_tail(CONFIGURE_LOG)}")
    BUILD_MARKER.write_text(want, encoding="utf-8")
    note(f"configure ok (log: {CONFIGURE_LOG.name})")


def make(jobs: int) -> None:
    """Link bash. The relink is forced: make compares the shell against its
    objects, and the rlib is a prerequisite of none of them, so a libc change
    alone leaves every object newer and make reports success without relinking
    (C_BUILD.md, trap 2)."""
    previous = BUILD / "bash"
    if previous.exists():
        previous.unlink()
    env = {**os.environ, "CC": f"python3 {TOOLS / 'cc-minix.py'}"}
    note(f"make -j{jobs}")
    with open(MAKE_LOG, "w", encoding="utf-8") as log:
        rc = run(["make", f"-j{jobs}", f"CC_FOR_BUILD={cc_for_build()}"], cwd=BUILD,
                 env=env, stdout=log, stderr=subprocess.STDOUT)
    if rc != 0:
        die(f"make failed (rc={rc}); tail of {MAKE_LOG}:\n{log_tail(MAKE_LOG)}")


# ------------------------------------------------------------------ publish

def publish() -> None:
    built = BUILD / "bash"
    if not built.is_file():
        die(f"{built} was not produced — make ran but linked nothing")
    data = built.read_bytes()[:4]
    if data != b"\x7fELF":
        die(f"{built} is not an ELF ({data!r})")
    size = built.stat().st_size
    if size < 200_000:
        die(f"{built} is {size} bytes, which is too small to be the shell")
    ARTIFACT.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, ARTIFACT)
    note(f"wrote {ARTIFACT} ({size} bytes)")
    note("boot it: just test-bash  (or inject it with "
         f"MINIXFS_EXTRA=/bin/bash={ARTIFACT.relative_to(ROOT)} and boot an image)")


def main(argv: list[str]) -> int:
    if "--help" in argv or "-h" in argv:
        print(__doc__)
        return 0
    force = "--force" in argv
    jobs = os.cpu_count() or 4
    rest = []
    it = iter(argv)
    for arg in it:
        if arg == "--force":
            continue
        if arg == "--jobs":
            value = next(it, None)
            if value is None:
                die("--jobs needs a number")
            jobs = int(value)
            continue
        rest.append(arg)
    if rest:
        die(f"unknown argument {rest[0]!r} (see --help)")

    if os.name == "nt":
        return run_in_wsl(argv)

    missing = [tool for tool in ("git", "make", "clang") if shutil.which(tool) is None]
    if missing:
        die(f"the build needs {', '.join(missing)} on PATH")
    cc_for_build()  # fail here rather than inside configure

    fetch_source(force)
    build_libc()
    configure(force)
    make(jobs)
    publish()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
