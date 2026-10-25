#!/usr/bin/env python3
"""Build libxkbcommon for the minix targets.

`libxkbcommon` is the C library smithay's `xkbcommon` binding links with, and rustc
emits a crate's native link flags whether or not its symbols are reachable — so any
binary in smithay's graph needs `-lxkbcommon`, and 4d cannot link a compositor
without it (`.notes/wayland.md` §6.13, 4d). It is also what a stock client parses a
`wl_keyboard.keymap` with, so it is on the path for input either way.

The build is meson, like Mesa's, pointed at `tools/cc-dso-minix.py` by a generated
cross file, and the build tree lives on WSL's *native* filesystem (meson refuses a
directory whose files land with drvfs mtimes). `--stage` copies the two names the
two halves of a link need into `target/xkbcommon/<arch>/lib`:

    libxkbcommon.so      what `-lxkbcommon` resolves at link time
    libxkbcommon.so.0    the `DT_NEEDED` the guest's loader resolves

Usage: python tools/build-xkbcommon.py [all|x86|riscv64|aarch64|<triple>]
                                      [--build] [--stage] [--force]
                                    (default: x86_64-pc-minix)

`--build` configures and compiles; without it the script only stages what an
earlier build produced. Prerequisites: `just dynlib-<arch>` (the `libc.so` the
shared object links against), meson + ninja + clang (`just build-mesa` needs the
same three), and on a Windows host a WSL distro, because the build re-enters it.

`just xkbcommon <arch> --build` is this script.
"""

from __future__ import annotations

import os
import pathlib
import shlex
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import ALL, Arch, resolve_argv  # noqa: E402

XKB_GIT = "https://github.com/xkbcommon/libxkbcommon"
XKB_TAG = "xkbcommon-1.9.2"
XKB_COMMIT = "dd642359f8d43c09968e34ca7f1eb1121b2dfd70"

SRC = ROOT / "target" / "xkbcommon-src"
TOOLS = ROOT / "tools"


def library_name() -> str:
    """The shared object's own file name, from the pinned version.

    xkbcommon keeps SONAME 0 while the ABI is stable, so version 1.9.2 builds
    `libxkbcommon.so.0.9.2` — the three components are not the project's three.
    """
    major, minor, patch = XKB_TAG.removeprefix("xkbcommon-").split(".")
    assert major == "1", f"{XKB_TAG}: the SONAME scheme assumes a 1.x library"
    return f"libxkbcommon.so.0.{minor}.{patch}"

# meson's `cpu_family`/`cpu` for each target. It is not the minix triple: meson only
# needs the machine class for its `host_machine` block.
MESON_CPU = {
    "x86": ("x86_64", "x86_64"),
    "riscv64": ("riscv64", "riscv64"),
    "aarch64": ("aarch64", "aarch64"),
}

# Only the core library. The tools need `xkeyboard-config`'s data and ICU, `-x11`
# needs libxcb, `-wayland` needs wayland-client + wayland-scanner, and `xkbregistry`
# needs libxml2 — none of which the compositor links.
#
# `-Dc_args=-fvisibility=default`: the library sets `gnu_symbol_visibility: 'hidden'`
# and exports through `xkbcommon.map`. GNU ld re-exports the listed symbols from a
# hidden default; lld does not, so the version script would localise *everything*
# and every consumer would link against an empty `.so` (the trap Mesa's DSOs hit).
# Our `c_args` land after meson's `-fvisibility=hidden`, so the later flag wins and
# the version script does the limiting instead.
#
# The project's `test/` and `bench/` programs are built by `ninja`'s default target
# and there is no option that turns them off, so this script builds the library
# target *by name* instead — they are not part of what an image carries, and their
# compile needs (`CLOCK_PROCESS_CPUTIME_ID`, `mkdtemp`, `sqrtl`) are not in the port.
MESON_OPTIONS = (
    "--buildtype=release",
    "-Ddefault_library=shared",
    "-Denable-tools=false",
    "-Denable-x11=false",
    "-Denable-wayland=false",
    "-Denable-xkbregistry=false",
    "-Denable-docs=false",
    "-Denable-bash-completion=false",
    "-Dc_args=-fvisibility=default",
)


def cache_root() -> pathlib.Path:
    """Where the meson build trees go: the native filesystem, never drvfs."""
    override = os.environ.get("MINIXRS_XKB_BUILD")
    if override:
        return pathlib.Path(override)
    return pathlib.Path.home() / ".cache" / "minixrs" / "xkbcommon"


def build_dir(arch: Arch) -> pathlib.Path:
    return cache_root() / arch.name / "build"


def arch_dir(arch: Arch) -> pathlib.Path:
    """The per-arch artifacts the Windows host can read (cross file, log, staged DSOs)."""
    return ROOT / "target" / "xkbcommon" / arch.name


def cross_file(arch: Arch) -> pathlib.Path:
    return arch_dir(arch) / "cross.ini"


def setup_log(arch: Arch) -> pathlib.Path:
    return arch_dir(arch) / "meson-setup.log"


def stage_dir(arch: Arch) -> pathlib.Path:
    """Where the guest-named DSOs are copied for an image to carry."""
    return arch_dir(arch) / "lib"


def note(msg: str) -> None:
    print(f"[xkbcommon] {msg}", flush=True)


def die(msg: str):
    sys.exit(f"error: {msg}")


def run(cmd: list[str], **kwargs) -> int:
    print("+", " ".join(str(c) for c in cmd), file=sys.stderr, flush=True)
    return subprocess.run([str(c) for c in cmd], **kwargs).returncode


def log_tail(path: pathlib.Path, lines: int = 30) -> str:
    if not path.is_file():
        return f"({path} was not written)"
    text = path.read_text(encoding="utf-8", errors="replace").splitlines()
    return "\n".join(text[-lines:])


# ---------------------------------------------------------------- Windows host

def wsl_path(path: pathlib.Path) -> str:
    text = str(path).replace("\\", "/")
    if len(text) > 1 and text[1] == ":":
        return f"/mnt/{text[0].lower()}{text[2:]}"
    return text


def run_in_wsl(argv: list[str]) -> int:
    """Re-enter this script in WSL: meson wants a POSIX host on a native filesystem."""
    wsl = shutil.which("wsl")
    if wsl is None:
        die("this host is Windows and has no `wsl` to build in; the meson build needs a "
            "POSIX host (see .notes/c_build.md)")
    distro = os.environ.get("MINIX_WSL_DISTRO", "FedoraLinux-44")
    inner = (f"cd {shlex.quote(wsl_path(ROOT))} && "
             f"python3 tools/build-xkbcommon.py {shlex.join(argv)}").strip()
    note(f"Windows host: running the build in WSL ({distro}).")
    rc = run([wsl, "-d", distro, "--", "bash", "-lc", inner])
    if rc != 0:
        note(f"the WSL build failed (rc={rc}).")
    return rc


# -------------------------------------------------------------------- tooling

def find_meson() -> str | None:
    override = os.environ.get("MINIXRS_MESON")
    if override:
        return override
    found = shutil.which("meson")
    if found:
        return found
    home = pathlib.Path.home() / ".local" / "bin" / "meson"
    return str(home) if home.is_file() else None


def find_ninja() -> str | None:
    found = shutil.which("ninja")
    if found:
        return found
    home = pathlib.Path.home() / ".local" / "bin" / "ninja"
    return str(home) if home.is_file() else None


# --------------------------------------------------------------------- source

def fetch(force: bool) -> None:
    git = ["git", "-c", "core.autocrlf=false", "-c", "core.eol=lf"]
    if force and SRC.is_dir():
        shutil.rmtree(SRC)
    if not (SRC / ".git").is_dir():
        SRC.parent.mkdir(parents=True, exist_ok=True)
        if run([*git, "clone", "--depth", "1", "--branch", XKB_TAG, XKB_GIT, str(SRC)]) != 0:
            die(f"cloning {XKB_TAG} from {XKB_GIT} failed")
    if run([*git, "-C", str(SRC), "checkout", "--force", XKB_TAG]) != 0:
        die(f"checking out {XKB_TAG} in {SRC} failed")
    head = subprocess.run([*git, "-C", str(SRC), "rev-parse", "HEAD"],
                          capture_output=True, text=True, check=True).stdout.strip()
    if head != XKB_COMMIT:
        die(f"{SRC} is at {head}, not the pinned {XKB_COMMIT} ({XKB_TAG} moved?)")
    note(f"source at {XKB_TAG} ({XKB_COMMIT[:12]}), {SRC}")


# ------------------------------------------------------------------ cross file

def write_cross(arch: Arch) -> pathlib.Path:
    cpu_family, cpu = MESON_CPU[arch.name]
    py = sys.executable or "python3"
    cc_script = wsl_path(TOOLS / "cc-dso-minix.py")
    cc = f"['{py}', '{cc_script}', '{arch.name}']"
    cxx = f"['{py}', '{cc_script}', '{arch.name}', '--cxx']"
    # `system = 'linux'`: the same host-machine choice Mesa's cross file makes, so
    # the two libraries' platform `#ifdef`s agree. `pkg-config` is named because the
    # build looks `xkeyboard-config` up with it (optionally: nothing asks for the
    # data until a keymap is loaded from names).
    text = f"""# Generated by tools/build-xkbcommon.py — do not edit.
[binaries]
c = {cc}
cpp = {cxx}
ar = 'llvm-ar'
pkg-config = 'pkg-config'

[host_machine]
system = 'linux'
cpu_family = '{cpu_family}'
cpu = '{cpu}'
endian = 'little'

[properties]
needs_exe_wrapper = true
"""
    path = cross_file(arch)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    note(f"wrote {path}")
    return path


# ---------------------------------------------------------------------- build

def configure(arch: Arch, meson: str, force: bool) -> None:
    build = build_dir(arch)
    if force or build.is_dir():
        shutil.rmtree(build, ignore_errors=True)
    build.parent.mkdir(parents=True, exist_ok=True)
    log_path = setup_log(arch)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    note("meson setup")
    with open(log_path, "w", encoding="utf-8") as log:
        rc = run([meson, "setup", str(build), str(SRC),
                  f"--cross-file={cross_file(arch)}",
                  *MESON_OPTIONS], stdout=log, stderr=subprocess.STDOUT)
    if rc != 0:
        die(f"meson setup failed (rc={rc}); tail of {log_path}:\n"
            f"{log_tail(log_path)}")
    note(f"meson setup ok (log: {log_path.name})")


def compile_library(arch: Arch, ninja: str) -> pathlib.Path:
    """Build the shared object alone, so the test and bench programs stay unbuilt."""
    build = build_dir(arch)
    lib = build / library_name()
    note(f"ninja {lib.name}")
    if run([ninja, "-C", str(build), lib.name]) != 0:
        die(f"build failed; the log is {build / 'meson-logs' / 'meson-log.txt'}")
    if not lib.is_file():
        die(f"{lib} was not produced")
    note("build ok")
    return lib


# --------------------------------------------------------------------- staging

def stage(arch: Arch) -> None:
    """Copy the built library into `target/xkbcommon/<arch>/lib` under its two names.

    The build tree is on WSL's native filesystem, which the Windows `build-x86`
    cannot read, so the file has to come back under `target/` for the image to
    carry it. Both names are copies, not symlinks: one is what `-lxkbcommon`
    resolves at link time and the other is the SONAME the guest's loader asks for.
    """
    build = build_dir(arch)
    versioned = build / library_name()
    if not versioned.is_file():
        note(f"xkbcommon: nothing built to stage (run `just xkbcommon {arch.name} --build`)")
        return
    dest = stage_dir(arch)
    dest.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(versioned, dest / "libxkbcommon.so.0")
    # The unversioned name is what the linker searches for; meson makes it a
    # symlink into the versioned file in the build dir, so copy through it.
    dev = build / "libxkbcommon.so"
    shutil.copyfile(dev if dev.exists() else versioned, dest / "libxkbcommon.so")
    note(f"staged libxkbcommon.so and libxkbcommon.so.0 to {dest}")


# ------------------------------------------------------------------------ main

def main(argv: list[str]) -> int:
    if "--help" in argv or "-h" in argv:
        print(__doc__)
        return 0
    force = "--force" in argv
    do_build = "--build" in argv
    stage_only = "--stage" in argv
    rest = [a for a in argv if a not in ("--force", "--build", "--stage")]
    if rest and rest[0] == "all":
        arches = list(ALL)
        rest = rest[1:]
    else:
        arch, rest = resolve_argv(rest)
        arches = [arch]
    if rest:
        die(f"unknown argument {rest[0]!r} (see --help)")

    if os.name == "nt":
        return run_in_wsl(argv)

    if not stage_only and not do_build:
        # Staging is the usual no-flag call: it is what a build that already
        # happened needs, and it is cheap.
        stage_only = True

    if stage_only:
        for arch in arches:
            stage(arch)
        return 0

    meson = find_meson()
    if meson is None:
        die("no meson on PATH — install it (a venv is enough: "
            "`python3 -m venv ~/.local/mesonenv && ~/.local/mesonenv/bin/pip "
            "install meson ninja`), or set MINIXRS_MESON")
    ninja = find_ninja()
    if ninja is None:
        die("no ninja on PATH (install it beside meson, or set it up in the venv)")
    for tool in ("clang", "git"):
        if shutil.which(tool) is None:
            die(f"the build needs {tool} on PATH")

    for arch in arches:
        fetch(force)
        write_cross(arch)
        configure(arch, meson, force)
        compile_library(arch, ninja)
        stage(arch)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
