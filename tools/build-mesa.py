#!/usr/bin/env python3
"""Fetch and build Mesa + libdrm for a minix target, and configure Mesa against
them.

This is the build plumbing of §6.10 stage 3c-0. It is the same shape as
`tools/build-bash.py` — fetch pinned upstream trees, drive their own build system
against the port's C toolchain, log everything — with three differences that come
from what Mesa is:

* the toolchain is `tools/cc-dso-minix.py`, the `cc` that produces shared
  objects, not `tools/cc-minix.py`, which links executable images against the
  static rlib;
* the build system is **meson**, so what is generated is a *cross file* naming
  that `cc` (and its `--cxx` mode), and meson itself is assumed to be on `PATH`
  (`MINIXRS_MESON` overrides; a venv install is enough);
* **libdrm is built first**, because Mesa's EGL cannot be built without the DRI
  path and DRI looks up libdrm — the build-time half of D6. libdrm installs into a
  prefix and Mesa reads it back through pkg-config.

The first milestone is the **softpipe** gallium driver, not llvmpipe: Mesa builds
llvmpipe only with LLVM (`-Dllvm=enabled`), and porting LLVM is a much larger
project than Mesa. softpipe is the same software family, with no JIT.

The meson build trees live on the *native* filesystem (under `~/.cache`), not
under `target/`: meson refuses a build directory whose files land with a future
mtime, which is what drvfs (`/mnt/c`) does against WSL's clock. `target/mesa/``<arch>/`
still holds the cross file and the setup logs, which are written directly and are
what the recipe reports.

Usage: python tools/build-mesa.py [x86|riscv64|aarch64] [--force] [--build]

Prerequisites: the fork's stage1 compiler and an LLD (`just bootstrap`), clang
and meson+ninja on `PATH`, and `just dynlib-<arch>` for the `libc.so` the DSOs
link against.
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

from ccarch import ALL, Arch, resolve_argv  # noqa: E402


# Mesa and libdrm are pinned in `tools/mesa_pin.py`, which the fetch
# (`tools/fetch-mesa.py`) and the release workflow read too: pinned by *tag* and
# asserted against the commit that tag resolved to when the pin was written. A
# shallow `--branch <tag>` clone is enough (unlike bash's commit pin, the tag is
# the fetchable name), and the assert is what turns a moved tag into a failure
# rather than a silently different build.
from mesa_pin import (  # noqa: E402
    DRM_COMMIT,
    DRM_GIT,
    DRM_TAG,
    MESA_COMMIT,
    MESA_GIT,
    MESA_TAG,
)

MESA_SRC = ROOT / "target" / "mesa-src"
DRM_SRC = ROOT / "target" / "drm-src"


def cache_root() -> pathlib.Path:
    """Where the meson build trees go: the native filesystem, never drvfs.

    `MINIXRS_MESA_BUILD` overrides. See the module docstring for why this is not
    `target/`."""
    override = os.environ.get("MINIXRS_MESA_BUILD")
    if override:
        return pathlib.Path(override)
    return pathlib.Path.home() / ".cache" / "minixrs" / "mesa"


def mesa_build_dir(arch: Arch) -> pathlib.Path:
    return cache_root() / "mesa" / arch.name


def drm_build_dir(arch: Arch) -> pathlib.Path:
    return cache_root() / "drm" / arch.name / "build"


def drm_prefix(arch: Arch) -> pathlib.Path:
    return cache_root() / "drm" / arch.name / "prefix"


def mesa_dir(arch: Arch) -> pathlib.Path:
    return ROOT / "target" / "mesa" / arch.name


def stage_dir(arch: Arch) -> pathlib.Path:
    """Where the guest-named DSOs are copied for the image to carry."""
    return mesa_dir(arch) / "lib"


def cross_file(arch: Arch) -> pathlib.Path:
    return mesa_dir(arch) / "cross.ini"


def meson_log(arch: Arch) -> pathlib.Path:
    return mesa_dir(arch) / "meson-setup.log"


def drm_log(arch: Arch) -> pathlib.Path:
    return mesa_dir(arch) / "drm-setup.log"


# meson's `cpu_family`/`cpu` for each target. It is not the minix triple: meson
# only needs the machine class for its `host_machine` block.
MESON_CPU = {
    "x86": ("x86_64", "x86_64"),
    "riscv64": ("riscv64", "riscv64"),
    "aarch64": ("aarch64", "aarch64"),
}

# libdrm: no test programs, no per-driver backends (the port has the render node
# from 3b; the backends are for real GPUs it is not driving). `tests` is a
# boolean; the drivers are features, which is why the spellings differ.
DRM_OPTIONS = (
    "--buildtype=release",
    "-Dtests=false",
    "-Dintel=disabled",
    "-Dradeon=disabled",
    "-Damdgpu=disabled",
    "-Dnouveau=disabled",
    "-Dvmwgfx=disabled",
    "-Dfreedreno=disabled",
    "-Dtegra=disabled",
    "-Dvc4=disabled",
    "-Detnaviv=disabled",
)

# A minimal, stable Mesa option set: the software rasteriser, EGL, GLES2, no
# window-system platforms (surfaceless), no LLVM, no GBM/glvnd. An unknown option
# fails meson setup by name, which is the point of 3c-0's gate.
#
# `-D__managarm__`: Mesa's `detect_os.h` has no minix case, and every OS branch it
# needs (`src/util/os_time.c`, `os_misc.c`, `u_thread.c`) has a managarm one — the
# closest POSIX-on-microkernel match. It selects POSIX/POSIX_LITE, which brings its
# own small API needs (`clock_nanosleep`, `sched_yield`, `pthread_setname_np`),
# all now in minix-libc. Scoped to Mesa only, so it cannot perturb libdrm.
#
# `-DEGL_NO_PLATFORM_SPECIFIC_TYPES`: `include/EGL/eglplatform.h` falls through to
# `#error "Platform not recognized"` for an OS it does not name, and the surfaceless
# platform needs no real native types, so the `void *` defaults are correct.
#
# `-fvisibility=default`: Mesa sets `gnu_symbol_visibility : 'hidden'` and relies
# on its version scripts/`--dynamic-list` to re-export the listed symbols. GNU ld
# does that for hidden symbols; lld (the only linker this port has) does not, so
# the version script would localise everything and the `libEGL`/`libGLESv2` links
# could not see `libgallium`'s `dri*`/`glapi` exports. Default visibility lets the
# version script do the limiting instead. Our `c_args` land after Mesa's
# `-fvisibility=hidden`, so the later flag wins.
#
# `-fno-exceptions`: the port has no C++ unwinder, so libc++abi is built without
# exception support; Mesa's C++ (the GLSL compiler) must not emit `__cxa_throw`
# and `_Unwind_Resume` references then.
#
# `-Dshader-cache=disabled`: the on-disk cache is the only user of `nftw` and of
# `flock`; a first GL triangle does not need it.
MESON_OPTIONS = (
    "--buildtype=release",
    "-Dgallium-drivers=softpipe",
    "-Dvulkan-drivers=",
    "-Dplatforms=",
    "-Dglx=disabled",
    "-Degl=enabled",
    "-Dgles1=disabled",
    "-Dgles2=enabled",
    "-Dllvm=disabled",
    "-Dgbm=disabled",
    "-Dglvnd=disabled",
    "-Dshader-cache=disabled",
    "-Dlmsensors=disabled",
    "-Dzlib:tests=disabled",
    "-Dc_args=-D__managarm__ -DEGL_NO_PLATFORM_SPECIFIC_TYPES -DNO_REGEX -fvisibility=default",
    "-Dcpp_args=-D__managarm__ -DEGL_NO_PLATFORM_SPECIFIC_TYPES -DNO_REGEX -fvisibility=default -fno-exceptions",
)


def note(msg: str) -> None:
    print(f"[mesa] {msg}", flush=True)


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
    """Re-enter this script in WSL: the build needs a POSIX host (see
    `tools/build-bash.py` and `C_BUILD.md`)."""
    wsl = shutil.which("wsl")
    if wsl is None:
        die("this host is Windows and has no `wsl` to build in; Mesa's build needs a "
            "POSIX host (see C_BUILD.md)")
    distro = os.environ.get("MINIX_WSL_DISTRO", "FedoraLinux-44")
    inner = (f"cd {shlex.quote(wsl_path(ROOT))} && "
             f"python3 tools/build-mesa.py {shlex.join(argv)}").strip()
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


# ------------------------------------------------------------------ source

def fetch(repo_url: str, tag: str, commit: str, dest: pathlib.Path, force: bool) -> None:
    git = ["git", "-c", "core.autocrlf=false", "-c", "core.eol=lf"]
    if force and dest.is_dir():
        shutil.rmtree(dest)
    if not (dest / ".git").is_dir():
        dest.parent.mkdir(parents=True, exist_ok=True)
        # `--depth 1 --branch <tag>`: the tag is fetchable (unlike a bare commit),
        # and one level of history is all a from-tag build reads.
        if run([*git, "clone", "--depth", "1", "--branch", tag, repo_url, str(dest)]) != 0:
            die(f"cloning {tag} from {repo_url} failed")
    if run([*git, "-C", str(dest), "checkout", "--force", tag]) != 0:
        die(f"checking out {tag} in {dest} failed")
    head = subprocess.run([*git, "-C", str(dest), "rev-parse", "HEAD"],
                          capture_output=True, text=True, check=True).stdout.strip()
    if head != commit:
        die(f"{dest} is at {head}, not the pinned {commit} ({tag} moved?)")
    note(f"source at {tag} ({commit[:12]}), {dest}")


# ------------------------------------------------------------------ cross file

def write_cross(arch: Arch) -> pathlib.Path:
    cpu_family, cpu = MESON_CPU[arch.name]
    py = sys.executable or "python3"
    cc_script = wsl_path(TOOLS / "cc-dso-minix.py")
    cc = f"['{py}', '{cc_script}', '{arch.name}']"
    cxx = f"['{py}', '{cc_script}', '{arch.name}', '--cxx']"
    # `system = 'linux'`: Mesa derives `system_has_kms_drm` from a fixed list of
    # systems and only that path builds EGL's DRI backend — which is what EGL
    # surfaceless needs. It also makes Mesa look up libdrm, which the prefix
    # below satisfies through pkg-config.
    text = f"""# Generated by tools/build-mesa.py — do not edit.
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


# ------------------------------------------------------------------ libdrm

def build_libdrm(arch: Arch, meson: str, ninja: str, force: bool) -> None:
    build = drm_build_dir(arch)
    prefix = drm_prefix(arch)
    if force or build.is_dir():
        shutil.rmtree(build, ignore_errors=True)
    # A fresh prefix, so a stale `.pc` from an earlier run cannot satisfy Mesa.
    shutil.rmtree(prefix, ignore_errors=True)
    build.parent.mkdir(parents=True, exist_ok=True)
    log_path = drm_log(arch)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    note("libdrm: meson setup")
    with open(log_path, "w", encoding="utf-8") as log:
        rc = run([meson, "setup", str(build), str(DRM_SRC),
                  f"--cross-file={cross_file(arch)}", f"--prefix={prefix}",
                  *DRM_OPTIONS], stdout=log, stderr=subprocess.STDOUT)
    if rc != 0:
        die(f"libdrm meson setup failed (rc={rc}); tail of {log_path}:\n"
            f"{log_tail(log_path)}")
    note("libdrm: ninja install")
    if run([ninja, "-C", str(build), "install"]) != 0:
        die(f"libdrm build/install failed; see {build}")
    note(f"libdrm installed to {prefix}")


# ------------------------------------------------------------------ Mesa

def configure_mesa(arch: Arch, meson: str, force: bool) -> None:
    build = mesa_build_dir(arch)
    if force or build.is_dir():
        shutil.rmtree(build, ignore_errors=True)
    build.parent.mkdir(parents=True, exist_ok=True)
    # Restrict pkg-config to libdrm's prefix, so a host libdrm cannot be found by
    # accident and the cross build stays hermetic.
    env = {**os.environ,
           "PKG_CONFIG_LIBDIR": str(drm_prefix(arch) / "lib" / "pkgconfig"),
           "PKG_CONFIG_PATH": ""}
    log_path = meson_log(arch)
    note("mesa: meson setup")
    with open(log_path, "w", encoding="utf-8") as log:
        rc = run([meson, "setup", str(build), str(MESA_SRC),
                  f"--cross-file={cross_file(arch)}", *MESON_OPTIONS],
                 env=env, stdout=log, stderr=subprocess.STDOUT)
    if rc != 0:
        die(f"mesa meson setup failed (rc={rc}); tail of {log_path}:\n"
            f"{log_tail(log_path)}")
    note(f"mesa: meson setup ok (log: {log_path.name})")


def build_mesa(arch: Arch, ninja: str) -> None:
    build = mesa_build_dir(arch)
    note("mesa: ninja")
    if run([ninja, "-C", str(build)]) != 0:
        die(f"mesa build failed; the log is {build / 'meson-logs' / 'meson-log.txt'}")
    note("mesa: build ok")


# The sonames the client and the driver ask for. libEGL/libGLESv2 name
# `libgallium-25.3.6.so` in their DT_NEEDED, and the EGL DRI path dlopens the
# driver by its `_dri.so` name, so that one file is staged under both names.
def stage_sources(arch: Arch) -> list[tuple[str, pathlib.Path]]:
    build = mesa_build_dir(arch)
    gallium = build / "src" / "gallium" / "targets" / "dri" / "libgallium-25.3.6.so"
    return [
        ("libEGL.so.1", build / "src" / "egl" / "libEGL.so.1.0.0"),
        ("libGLESv2.so.2", build / "src" / "mesa" / "glapi" / "es2api" / "libGLESv2.so.2.0.0"),
        ("libgallium-25.3.6.so", gallium),
        ("swrast_dri.so", gallium),
        ("libdrm.so.2", drm_prefix(arch) / "lib" / "libdrm.so.2.129.0"),
        ("libz.so.1", build / "subprojects" / "zlib-1.3.1" / "libz.so.1.3.1"),
        ("libexpat.so.1", build / "subprojects" / "expat-2.5.0" / "libexpat.so.1.8.10"),
    ]


def stage(arch: Arch) -> None:
    """Copy the built DSOs into `target/mesa/<arch>/lib` under their guest names.

    The Mesa build tree lives on WSL's native filesystem (see the module
    docstring), which the Windows `just build-x86` cannot read, so the files have
    to be staged back under `target/` for the image to carry them."""
    sources = stage_sources(arch)
    if not sources[0][1].is_file():
        note("mesa: no built DSOs to stage (run `just build-mesa x86 --build`)")
        return
    dest = stage_dir(arch)
    dest.mkdir(parents=True, exist_ok=True)
    for name, src in sources:
        if not src.is_file():
            die(f"mesa: staged DSO missing: {src}")
        shutil.copyfile(src, dest / name)
    note(f"mesa: staged {len(sources)} DSOs to {dest}")


# ------------------------------------------------------------------ main

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
    else:
        arch, rest = resolve_argv(rest)
        arches = [arch]
    if rest:
        die(f"unknown argument {rest[0]!r} (see --help)")

    if os.name == "nt":
        return run_in_wsl(argv)

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
        fetch(MESA_GIT, MESA_TAG, MESA_COMMIT, MESA_SRC, force)
        fetch(DRM_GIT, DRM_TAG, DRM_COMMIT, DRM_SRC, force)
        write_cross(arch)
        build_libdrm(arch, meson, ninja, force)
        configure_mesa(arch, meson, force)
        if do_build:
            build_mesa(arch, ninja)
        stage(arch)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
