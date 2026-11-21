//! Fetch and build Mesa + libdrm for a minix target, and stage the DSOs.
//!
//! This is the build plumbing of the Wayland plan's §6.10 stage 3c-0. It is the
//! same shape as [`super::bash`] (fetch pinned upstream trees, drive their own
//! build system against the port's C toolchain, log everything) with three
//! differences that come from what Mesa is:
//!
//! * the toolchain is [`crate::cc::dso`], the `cc` that produces shared objects,
//!   not `cc minix`, which links executable images against the static rlib;
//! * the build system is **meson**, so what is generated is a *cross file* naming
//!   that `cc` (and its `--cxx` mode), and meson itself is assumed to be on
//!   `PATH` (`MINIXRS_MESON` overrides; a venv install is enough);
//! * **libdrm is built first**, because Mesa's EGL cannot be built without the DRI
//!   path and DRI looks up libdrm. libdrm installs into a prefix and Mesa reads it
//!   back through pkg-config.
//!
//! The first milestone is the **softpipe** gallium driver, not llvmpipe: Mesa
//! builds llvmpipe only with LLVM (`-Dllvm=enabled`), and porting LLVM is a much
//! larger project than Mesa. softpipe is the same software family, with no JIT.
//!
//! The meson build trees live on the *native* filesystem (under `~/.cache`), not
//! under `target/`: meson refuses a build directory whose files land with a future
//! mtime, which is what drvfs (`/mnt/c`) does against WSL's clock.
//! `target/mesa/<arch>/` still holds the cross file and the setup logs, which are
//! written directly and are what the recipe reports.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::arch::{self, Arch};
use crate::cc;
use crate::pin;
use crate::toolchain;

/// `cpu_family`/`cpu` for each target. It is not the minix triple: meson only
/// needs the machine class for its `host_machine` block.
fn meson_cpu(arch: &Arch) -> (&'static str, &'static str) {
    match arch.name {
        "x86" => ("x86_64", "x86_64"),
        "riscv64" => ("riscv64", "riscv64"),
        _ => ("aarch64", "aarch64"),
    }
}

/// libdrm: no test programs, no per-driver backends (the port has the render node
/// from 3b; the backends are for real GPUs it is not driving). `tests` is a
/// boolean; the drivers are features, which is why the spellings differ.
///
/// `-Dc_args=-D__DragonFly__`: libdrm has no `detect_os.h`-style OS hook (its
/// device discovery is chosen by a platform macro: `#ifdef __linux__` sysfs,
/// `#elif` the BSDs, `#else` "not implemented") and there is no minix branch. Its
/// OpenBSD/DragonFly branch is the one that reads the PCI identity from the
/// *driver* (`DRM_IOCTL_GET_PCIINFO`) instead of sysfs, so libdrm is built as that
/// branch and the render node answers the ioctl (`.notes/wayland.md` §5 row 24).
/// Mesa is not built this way: it has `DETECT_OS_MANAGARM` and stays
/// `-D__managarm__`. Both still take libdrm's BSD `_IOC` path, which keys on the
/// absence of `__linux__`, so headers stay consistent between them.
const DRM_OPTIONS: [&str; 12] = [
    "--buildtype=release",
    "-Dtests=false",
    "-Dc_args=-D__DragonFly__",
    "-Dintel=disabled",
    "-Dradeon=disabled",
    "-Damdgpu=disabled",
    "-Dnouveau=disabled",
    "-Dvmwgfx=disabled",
    "-Dfreedreno=disabled",
    "-Dtegra=disabled",
    "-Dvc4=disabled",
    "-Detnaviv=disabled",
];

/// A minimal, stable Mesa option set: the software rasteriser, EGL, GLES2, GBM, no
/// window-system platforms (surfaceless), no LLVM, no glvnd. An unknown option
/// fails meson setup by name, which is the point of 3c-0's gate.
///
/// `-D__managarm__`: Mesa's `detect_os.h` has no minix case, and every OS branch it
/// needs (`src/util/os_time.c`, `os_misc.c`, `u_thread.c`) has a managarm one, the
/// closest POSIX-on-microkernel match. It selects POSIX/POSIX_LITE, which brings
/// its own small API needs (`clock_nanosleep`, `sched_yield`, `pthread_setname_np`),
/// all now in minix-libc. Scoped to Mesa only, so it cannot perturb libdrm.
///
/// `-DEGL_NO_PLATFORM_SPECIFIC_TYPES`: `include/EGL/eglplatform.h` falls through to
/// `#error "Platform not recognized"` for an OS it does not name, and the
/// surfaceless platform needs no real native types, so the `void *` defaults are
/// correct.
///
/// `-fvisibility=default`: Mesa sets `gnu_symbol_visibility : 'hidden'` and relies
/// on its version scripts/`--dynamic-list` to re-export the listed symbols. GNU ld
/// does that for hidden symbols; lld (the only linker this port has) does not, so
/// the version script would localise everything and the `libEGL`/`libGLESv2` links
/// could not see `libgallium`'s `dri*`/`glapi` exports. Default visibility lets
/// the version script do the limiting instead. Our `c_args` land after Mesa's
/// `-fvisibility=hidden`, so the later flag wins.
///
/// `-fno-exceptions`: the port has no C++ unwinder, so libc++abi is built without
/// exception support; Mesa's C++ (the GLSL compiler) must not emit `__cxa_throw`
/// and `_Unwind_Resume` references then.
///
/// `-Dshader-cache=disabled`: the on-disk cache is the only user of `nftw` and of
/// `flock`; a first GL triangle does not need it.
const MESON_OPTIONS: [&str; 16] = [
    "--buildtype=release",
    "-Dgallium-drivers=softpipe,virgl",
    "-Dvulkan-drivers=",
    "-Dplatforms=",
    "-Dglx=disabled",
    "-Degl=enabled",
    "-Dgles1=disabled",
    "-Dgles2=enabled",
    "-Dllvm=disabled",
    "-Dgbm=enabled",
    "-Dgbm-backends-path=/lib",
    "-Dglvnd=disabled",
    "-Dshader-cache=disabled",
    "-Dlmsensors=disabled",
    "-Dzlib:tests=disabled",
    "-Dc_args=-D__managarm__ -DEGL_NO_PLATFORM_SPECIFIC_TYPES -DNO_REGEX -fvisibility=default",
];
/// The C++ half, with the unwinder off.
const MESON_CPP_ARGS: &str =
    "-Dcpp_args=-D__managarm__ -DEGL_NO_PLATFORM_SPECIFIC_TYPES -DNO_REGEX -fvisibility=default -fno-exceptions";

const LOG_TAIL: usize = 30;

pub struct Options<'a> {
    pub target: Option<&'a str>,
    pub force: bool,
    pub build: bool,
    pub stage_only: bool,
}

pub fn run(root: &Path, options: &Options) -> io::Result<bool> {
    let arches: Vec<&Arch> = match options.target {
        None => vec![arch::DEFAULT],
        Some("all") => arch::ALL.to_vec(),
        Some(token) => vec![arch::resolve(token)?],
    };

    if cfg!(windows) {
        return reenter(root, options);
    }

    if options.stage_only {
        for arch in arches {
            stage(root, arch)?;
        }
        return Ok(true);
    }

    let meson = find_tool("MINIXRS_MESON", "meson")?;
    let ninja = find_tool("", "ninja")?;
    if meson.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no meson on PATH - install it (a venv is enough: `python3 -m venv ~/.local/mesonenv && ~/.local/mesonenv/bin/pip install meson ninja`), or set MINIXRS_MESON",
        ));
    }
    if ninja.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no ninja on PATH (install it beside meson, or set it up in the venv)",
        ));
    }
    let meson = meson.expect("checked above");
    let ninja = ninja.expect("checked above");
    for tool in ["clang", "git"] {
        if toolchain::find_on_path(tool).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("the build needs {tool} on PATH"),
            ));
        }
    }

    let pins = pin::load(root)?;
    let mesa = Pin {
        url: pins.get("mesa.git")?.to_string(),
        tag: pins.get("mesa.tag")?.to_string(),
        commit: pins.get("mesa.commit")?.to_string(),
    };
    let drm = Pin {
        url: pins.get("drm.git")?.to_string(),
        tag: pins.get("drm.tag")?.to_string(),
        commit: pins.get("drm.commit")?.to_string(),
    };

    for arch in arches {
        fetch(&mesa, &mesa_src(root), options.force)?;
        fetch(&drm, &drm_src(root), options.force)?;
        write_cross(root, arch)?;
        build_libdrm(root, arch, &meson, &ninja, options.force)?;
        configure_mesa(root, arch, &meson, options.force)?;
        if options.build {
            build_mesa(arch, &ninja)?;
        }
        stage(root, arch)?;
    }
    Ok(true)
}

// ------------------------------------------------------------------ Windows host

/// Re-enter WSL and run this command there, for the reason [`super::bash`] does.
fn reenter(root: &Path, options: &Options) -> io::Result<bool> {
    let Some(wsl) = toolchain::find_on_path("wsl") else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "this host is Windows and has no `wsl` to build in; Mesa's build needs a POSIX host (see .notes/c_build.md)",
        ));
    };
    let distro = std::env::var("MINIX_WSL_DISTRO").unwrap_or_else(|_| "FedoraLinux-44".to_string());
    let mut inner = format!(
        "cd {} && cargo run --quiet --package xtask --target-dir .xtask/linux -- build mesa",
        super::bash::quote(&super::bash::wsl_path(root))
    );
    if let Some(token) = options.target {
        inner.push(' ');
        inner.push_str(&super::bash::quote(token));
    }
    if options.force {
        inner.push_str(" --force");
    }
    if options.build {
        inner.push_str(" --build");
    }
    if options.stage_only {
        inner.push_str(" --stage");
    }

    println!("[mesa] Windows host: running the build in WSL ({distro}).");
    let status = Command::new(wsl)
        .args(["-d", &distro, "--", "bash", "-lc", &inner])
        .status()?;
    if !status.success() {
        println!("[mesa] the WSL build failed ({status}).");
    }
    Ok(status.success())
}

// ------------------------------------------------------------------ paths

fn mesa_src(root: &Path) -> PathBuf {
    root.join("target").join("mesa-src")
}

fn drm_src(root: &Path) -> PathBuf {
    root.join("target").join("drm-src")
}

/// Where the meson build trees go: the native filesystem, never drvfs.
/// `MINIXRS_MESA_BUILD` overrides; see the module doc for why this is not
/// `target/`.
fn cache_root() -> PathBuf {
    if let Some(override_path) = std::env::var_os("MINIXRS_MESA_BUILD") {
        return PathBuf::from(override_path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("minixrs").join("mesa")
}

fn mesa_build_dir(arch: &Arch) -> PathBuf {
    cache_root().join("mesa").join(arch.name)
}

fn drm_build_dir(arch: &Arch) -> PathBuf {
    cache_root().join("drm").join(arch.name).join("build")
}

fn drm_prefix(arch: &Arch) -> PathBuf {
    cache_root().join("drm").join(arch.name).join("prefix")
}

fn mesa_dir(root: &Path, arch: &Arch) -> PathBuf {
    root.join("target").join("mesa").join(arch.name)
}

/// Where the guest-named DSOs are copied for the image to carry.
fn stage_dir(root: &Path, arch: &Arch) -> PathBuf {
    mesa_dir(root, arch).join("lib")
}

fn cross_file(root: &Path, arch: &Arch) -> PathBuf {
    mesa_dir(root, arch).join("cross.ini")
}

fn meson_log(root: &Path, arch: &Arch) -> PathBuf {
    mesa_dir(root, arch).join("meson-setup.log")
}

fn drm_log(root: &Path, arch: &Arch) -> PathBuf {
    mesa_dir(root, arch).join("drm-setup.log")
}

// ------------------------------------------------------------------ tooling

/// `name` on PATH, or under `~/.local/bin` (where a venv install puts meson).
fn find_tool(override_key: &str, name: &str) -> io::Result<Option<String>> {
    if !override_key.is_empty()
        && let Some(override_path) = std::env::var_os(override_key)
        && !override_path.is_empty()
    {
        return Ok(Some(override_path.to_string_lossy().into_owned()));
    }
    if let Some(found) = toolchain::find_on_path(name) {
        return Ok(Some(found.to_string_lossy().into_owned()));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let local = home.join(".local").join("bin").join(name);
    Ok(local.is_file().then(|| local.to_string_lossy().into_owned()))
}

// ------------------------------------------------------------------ source

struct Pin {
    url: String,
    tag: String,
    commit: String,
}

/// Check one pinned tree out at its tag, and assert the commit the tag resolved to.
///
/// `--depth 1 --branch <tag>`: the tag is fetchable (unlike a bare commit), and one
/// level of history is all a from-tag build reads. The assert is what turns a moved
/// tag into a failure rather than a silently different build.
fn fetch(pin: &Pin, dest: &Path, force: bool) -> io::Result<()> {
    if force && dest.is_dir() {
        fs::remove_dir_all(dest)?;
    }
    if !dest.join(".git").is_dir() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut command = Command::new("git");
        command.args(["-c", "core.autocrlf=false", "-c", "core.eol=lf"]).args([
            "clone",
            "--depth",
            "1",
            "--branch",
            &pin.tag,
            &pin.url,
        ]);
        command.arg(dest);
        if !cc::run(&mut command)? {
            return Err(io::Error::other(format!(
                "cloning {} from {} failed",
                pin.tag, pin.url
            )));
        }
    }
    let mut checkout = Command::new("git");
    checkout.args([
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.eol=lf",
        "-C",
    ]);
    checkout.arg(dest);
    checkout.args(["checkout", "--force", &pin.tag]);
    if !cc::run(&mut checkout)? {
        return Err(io::Error::other(format!(
            "checking out {} in {} failed",
            pin.tag,
            dest.display()
        )));
    }
    let mut head = Command::new("git");
    head.args(["-C"]).arg(dest).args(["rev-parse", "HEAD"]);
    let output = head.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git rev-parse HEAD in {} failed",
            dest.display()
        )));
    }
    let got = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if got != pin.commit {
        return Err(io::Error::other(format!(
            "{} is at {got}, not the pinned {} ({} moved?)",
            dest.display(),
            pin.commit,
            pin.tag
        )));
    }
    println!(
        "[mesa] source at {} ({}), {}",
        pin.tag,
        &pin.commit[..12],
        dest.display()
    );
    Ok(())
}

// ------------------------------------------------------------------ cross file

/// The meson cross file, naming [`crate::cc::dso`] as the compiler.
///
/// `system = 'linux'`: Mesa derives `system_has_kms_drm` from a fixed list of
/// systems and only that path builds EGL's DRI backend, which is what EGL
/// surfaceless needs. It also makes Mesa look up libdrm, which the prefix
/// satisfies through pkg-config.
fn write_cross(root: &Path, arch: &Arch) -> io::Result<()> {
    let (cpu_family, cpu) = meson_cpu(arch);
    let exe = std::env::current_exe()?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    let cc = format!("['{}', 'cc', 'dso', '{}']", exe.display(), arch.name);
    let cxx = format!("['{}', 'cc', 'dso', '{}', '--cxx']", exe.display(), arch.name);
    let text = format!(
        "# Generated by `cargo xtask build mesa` - do not edit.\n\
         [binaries]\n\
         c = {cc}\n\
         cpp = {cxx}\n\
         ar = 'llvm-ar'\n\
         pkg-config = 'pkg-config'\n\
         \n\
         [host_machine]\n\
         system = 'linux'\n\
         cpu_family = '{cpu_family}'\n\
         cpu = '{cpu}'\n\
         endian = 'little'\n\
         \n\
         [properties]\n\
         needs_exe_wrapper = true\n"
    );
    let path = cross_file(root, arch);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, text)?;
    println!("[mesa] wrote {}", path.display());
    Ok(())
}

// ------------------------------------------------------------------ libdrm

fn build_libdrm(
    root: &Path,
    arch: &Arch,
    meson: &str,
    ninja: &str,
    force: bool,
) -> io::Result<()> {
    let build = drm_build_dir(arch);
    let prefix = drm_prefix(arch);
    if force || build.is_dir() {
        remove_dir(&build)?;
    }
    // A fresh prefix, so a stale `.pc` from an earlier run cannot satisfy Mesa.
    remove_dir(&prefix)?;
    if let Some(parent) = build.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = drm_log(root, arch);
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    println!("[mesa] libdrm: meson setup");
    let mut command = Command::new(meson);
    command
        .args(["setup"])
        .arg(&build)
        .arg(drm_src(root))
        .arg(format!("--cross-file={}", cross_file(root, arch).display()))
        .arg(format!("--prefix={}", prefix.display()))
        .args(DRM_OPTIONS);
    if !run_logged(&mut command, &log)? {
        return Err(io::Error::other(format!(
            "libdrm meson setup failed; tail of {}:\n{}",
            log.display(),
            log_tail(&log)
        )));
    }
    println!("[mesa] libdrm: ninja install");
    let mut command = Command::new(ninja);
    command.arg("-C").arg(&build).arg("install");
    if !cc::run(&mut command)? {
        return Err(io::Error::other(format!(
            "libdrm build/install failed; see {}",
            build.display()
        )));
    }
    println!("[mesa] libdrm installed to {}", prefix.display());
    Ok(())
}

// ------------------------------------------------------------------ Mesa

fn configure_mesa(root: &Path, arch: &Arch, meson: &str, force: bool) -> io::Result<()> {
    let build = mesa_build_dir(arch);
    if force || build.is_dir() {
        remove_dir(&build)?;
    }
    if let Some(parent) = build.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = meson_log(root, arch);
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    println!("[mesa] mesa: meson setup");
    let mut command = Command::new(meson);
    command
        .args(["setup"])
        .arg(&build)
        .arg(mesa_src(root))
        .arg(format!("--cross-file={}", cross_file(root, arch).display()))
        .args(MESON_OPTIONS)
        .arg(MESON_CPP_ARGS)
        // Restrict pkg-config to libdrm's prefix, so a host libdrm cannot be found
        // by accident and the cross build stays hermetic.
        .env(
            "PKG_CONFIG_LIBDIR",
            drm_prefix(arch).join("lib").join("pkgconfig"),
        )
        .env("PKG_CONFIG_PATH", "");
    if !run_logged(&mut command, &log)? {
        return Err(io::Error::other(format!(
            "mesa meson setup failed; tail of {}:\n{}",
            log.display(),
            log_tail(&log)
        )));
    }
    println!("[mesa] mesa: meson setup ok (log: {})", log.display());
    Ok(())
}

fn build_mesa(arch: &Arch, ninja: &str) -> io::Result<()> {
    let build = mesa_build_dir(arch);
    println!("[mesa] mesa: ninja");
    let mut command = Command::new(ninja);
    command.arg("-C").arg(&build);
    if !cc::run(&mut command)? {
        return Err(io::Error::other(format!(
            "mesa build failed; the log is {}",
            build.join("meson-logs").join("meson-log.txt").display()
        )));
    }
    println!("[mesa] mesa: build ok");
    Ok(())
}

/// The sonames the client and the driver ask for.
///
/// `libEGL`/`libGLESv2` name `libgallium-25.3.6.so` in their DT_NEEDED, and the EGL
/// DRI path dlopens the software driver by its `_dri.so` name, so that one merged
/// file is staged under its own name and as `swrast_dri.so`. A hardware driver
/// needs no third name: the `virtio_gpu` descriptor comes out of the same
/// `libgallium`.
fn stage_sources(arch: &Arch) -> Vec<(&'static str, PathBuf)> {
    let build = mesa_build_dir(arch);
    let gallium = build
        .join("src")
        .join("gallium")
        .join("targets")
        .join("dri")
        .join("libgallium-25.3.6.so");
    vec![
        ("libEGL.so.1", build.join("src").join("egl").join("libEGL.so.1.0.0")),
        (
            "libGLESv2.so.2",
            build
                .join("src")
                .join("mesa")
                .join("glapi")
                .join("es2api")
                .join("libGLESv2.so.2.0.0"),
        ),
        ("libgbm.so.1", build.join("src").join("gbm").join("libgbm.so.1.0.0")),
        // The GBM DRI backend, which `libgbm` dlopens by name from
        // `gbm-backends-path` (`/lib`): the one file an EGL-only build does not
        // have. It is a backend module, not a library a client links.
        (
            "dri_gbm.so",
            build.join("src").join("gbm").join("backends").join("dri").join("dri_gbm.so"),
        ),
        ("libgallium-25.3.6.so", gallium.clone()),
        ("swrast_dri.so", gallium),
        (
            "libdrm.so.2",
            drm_prefix(arch).join("lib").join("libdrm.so.2.129.0"),
        ),
        (
            "libz.so.1",
            build
                .join("subprojects")
                .join("zlib-1.3.1")
                .join("libz.so.1.3.1"),
        ),
        (
            "libexpat.so.1",
            build
                .join("subprojects")
                .join("expat-2.5.0")
                .join("libexpat.so.1.8.10"),
        ),
    ]
}

/// Copy the built DSOs into `target/mesa/<arch>/lib` under their guest names.
///
/// The Mesa build tree lives on WSL's native filesystem (see the module doc),
/// which the Windows `just build-x86` cannot read, so the files have to be staged
/// back under `target/` for the image to carry them.
fn stage(root: &Path, arch: &Arch) -> io::Result<()> {
    let sources = stage_sources(arch);
    if !sources[0].1.is_file() {
        println!("[mesa] no built DSOs to stage (run `just build-mesa x86 --build`)");
        return Ok(());
    }
    let dest = stage_dir(root, arch);
    fs::create_dir_all(&dest)?;
    for (name, src) in &sources {
        if !src.is_file() {
            return Err(io::Error::other(format!(
                "staged DSO missing: {}",
                src.display()
            )));
        }
        fs::copy(src, dest.join(name))?;
    }
    println!("[mesa] staged {} DSOs to {}", sources.len(), dest.display());
    Ok(())
}

// ------------------------------------------------------------------ helpers

fn remove_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Run `command` with its output in `log`, echoing the command line first.
fn run_logged(command: &mut Command, log: &Path) -> io::Result<bool> {
    println!("[mesa] {}", cc::command_line(command));
    let file = fs::File::create(log)?;
    let copy = file.try_clone()?;
    let status = command
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(copy))
        .status()?;
    Ok(status.success())
}

fn log_tail(log: &Path) -> String {
    let text = match fs::read_to_string(log) {
        Ok(text) => text,
        Err(_) => return format!("({} was not written)", log.display()),
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(LOG_TAIL);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meson_machine_is_not_the_triple() {
        assert_eq!(meson_cpu(&arch::X86_64), ("x86_64", "x86_64"));
        assert_eq!(meson_cpu(&arch::RISCV64), ("riscv64", "riscv64"));
        assert_eq!(meson_cpu(&arch::AARCH64), ("aarch64", "aarch64"));
    }

    #[test]
    fn the_build_trees_are_under_the_cache_and_the_artifacts_under_target() {
        let root = Path::new("/repo");
        assert!(mesa_dir(root, arch::DEFAULT).ends_with("target/mesa/x86"));
        assert!(stage_dir(root, arch::DEFAULT).ends_with("target/mesa/x86/lib"));
        assert!(cross_file(root, arch::DEFAULT).ends_with("cross.ini"));
        assert!(mesa_build_dir(arch::DEFAULT).ends_with("mesa/x86"));
        assert!(drm_prefix(arch::DEFAULT).ends_with("drm/x86/prefix"));
    }

    #[test]
    fn the_dso_names_are_the_ones_the_loader_asks_for() {
        let names: Vec<&str> = stage_sources(arch::DEFAULT)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for wanted in [
            "libEGL.so.1",
            "libGLESv2.so.2",
            "libgbm.so.1",
            "dri_gbm.so",
            "libgallium-25.3.6.so",
            "swrast_dri.so",
            "libdrm.so.2",
            "libz.so.1",
            "libexpat.so.1",
        ] {
            assert!(names.contains(&wanted), "missing {wanted}");
        }
    }

    #[test]
    fn a_missing_setup_log_says_so() {
        let missing = std::env::temp_dir().join("minixrs-xtask-no-such-meson.log");
        let _ = fs::remove_file(&missing);
        assert!(log_tail(&missing).contains("was not written"));
    }
}
