//! Cross-build the in-guest `rustc` and stage it into the toolchain root.
//!
//! The binary is `crates/rustc-minix`, its own workspace: `rustc_driver` is
//! declared `crate-type = ["dylib"]` and this target is a fully static ELF, so
//! the compiler depends on `rustc_driver_impl` directly and selects the LLVM
//! backend statically. That workspace's `[patch.crates-io]` supplies `libc`,
//! `rustix` and the stand-ins for `jobserver`, `libloading` and `ctrlc`, so
//! `rust/` itself is untouched.
//!
//! Output: `target/rustc-minix/<triple>/debug/rustc`, staged as `/usr/bin/rustc`
//! under `target/toolchain/<arch>/root`, with the target's sysroot at
//! `/usr/lib/rustlib/<triple>/lib` (where a binary in `/usr/bin` looks for it)
//! and `minix-libc`'s rlib beside it. `minix-libc` supplies the C ABI symbols
//! (`malloc`, `write`, `_Unwind_Backtrace`, the `__cxa_*` pair) that `std` and
//! the C++ runtime name but no Rust crate here imports.
//!
//! Three link facts are set here rather than in a wrapper, because they are part
//! of how this target links at all:
//!
//! * `LLVM_STATIC_STDCPP` names the port's C++ runtime, or `rustc_llvm` asks for
//!   `-lstdc++` and the link fails;
//! * `--no-dependent-libraries` is required because the C++ archives carry
//!   `.deplibs` records naming `pthread` and `rt`, whose symbols are Rust here;
//! * the sysroot and `minix-libc` are staged so a plain `/usr/bin/rustc` finds
//!   them without `--sysroot` or `--extern`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::{self, Arch};
use crate::build::{llvm_minix, std_hello};
use crate::cc;
use crate::toolchain;

/// The program a guest compiles and runs. Kept small on purpose: the first
/// question is whether the compiler starts and links at all.
const HELLO_RS: &str = r#"fn main() {
    println!("hello from Rust");
}
"#;

pub fn run(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let arches: Vec<&Arch> = match target {
        None => vec![arch::DEFAULT],
        Some("all") => arch::ALL.to_vec(),
        Some(token) => vec![arch::resolve(token)?],
    };
    for arch in arches {
        if !build(root, arch)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn build(root: &Path, arch: &Arch) -> io::Result<bool> {
    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;
    let release = release_of(&rustc)?;
    let out = root.join("target").join("rustc-minix");

    let stdcpp = root
        .join("target")
        .join("cxx")
        .join(format!("{}-toolchain", arch.name))
        .join("minix-runtime")
        .join("libstdc++.a");
    if !stdcpp.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} is missing; run `cargo xtask build libcxx {}`",
                stdcpp.display(),
                arch.name
            ),
        ));
    }
    let script = root.join("tools").join("minix-user.ld");
    let llvm_config = write_llvm_config_shim(root, arch)?;
    let rustflags = [
        format!("-C link-arg=-T{}", script.display()),
        "-C link-arg=--no-eh-frame-hdr".to_string(),
        "-C link-arg=--no-dependent-libraries".to_string(),
        "--cfg getrandom_backend=\"custom\"".to_string(),
    ]
    .join(" ");

    let mut cargo = Command::new(toolchain::cargo());
    cargo
        .current_dir(root.join("crates").join("rustc-minix"))
        .args(["build", "-p", "rustc-minix", "--features", "llvm", "--target", arch.triple])
        .env("RUSTC", &rustc)
        .env("RUSTC_BOOTSTRAP", "1")
        .env("CARGO_TARGET_DIR", &out)
        // The compiler is 300 MB before debug info and 57 GB with it; the
        // artifact the guest runs is the one without.
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("CARGO_INCREMENTAL", "0")
        // What x.py would have set. `CFG_RELEASE` reaches `rustc_span`, the
        // channel drives the unstable-feature gate, and the host triple is the
        // default target a bare `/usr/bin/rustc` compiles for.
        .env("CFG_RELEASE", &release)
        .env("CFG_RELEASE_CHANNEL", channel_of(&release))
        .env("CFG_VERSION", &release)
        .env("CFG_VER_HASH", "unknown")
        .env("CFG_VER_DATE", "unknown")
        .env("CFG_COMPILER_HOST_TRIPLE", arch.triple)
        // `rustc_interface` reads this to pick a codegen backend when neither
        // `-Zcodegen-backend` nor the target spec names one. Unset, it resolves to
        // `dummy`, and a real compile then fails at the link step with "crate type
        // ... not supported by the dummy codegen backend".
        .env("CFG_DEFAULT_CODEGEN_BACKEND", "llvm")
        .env("CFG_LIBDIR_RELATIVE", "lib")
        .env("RUSTC_INSTALL_BINDIR", "bin")
        .env("LLVM_CONFIG", &llvm_config)
        .env("LLVM_STATIC_STDCPP", &stdcpp)
        // `rustc_llvm`'s build script insists on a library-path variable; this
        // target's LLVM is a static archive set, so there is no path to give.
        .env("REAL_LIBRARY_PATH_VAR", "MINIX_DUMMY_PATH")
        .env("REAL_LIBRARY_PATH", "")
        .env("CC_x86_64-pc-minix", "clang")
        .env("CXX_x86_64-pc-minix", "clang++")
        .env("AR_x86_64-pc-minix", "llvm-ar")
        .env(
            format!("CARGO_TARGET_{}_LINKER", arch.triple.to_uppercase().replace('-', "_")),
            &lld,
        )
        .env("RUSTFLAGS", &rustflags);
    if !cc::run(&mut cargo)? {
        return Ok(false);
    }

    stage(root, arch, &rustc, &lld, &out)?;
    Ok(true)
}

/// Copy the binary and its sysroot into the toolchain root the image is built
/// from.
fn stage(root: &Path, arch: &Arch, rustc: &Path, lld: &Path, out: &Path) -> io::Result<()> {
    let binary = out.join(arch.triple).join("debug").join("rustc");
    if !binary.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} was not produced by the cargo build", binary.display()),
        ));
    }

    let stage = llvm_minix::stage_root(root, arch);
    let usr_bin = stage.join("usr/bin");
    let usr_lib = stage.join("usr/lib");
    let rustlib = usr_lib.join("rustlib").join(arch.triple).join("lib");
    let usr_src = stage.join("usr/src");
    for dir in [&usr_bin, &rustlib, &usr_src] {
        fs::create_dir_all(dir)?;
    }

    fs::copy(&binary, usr_bin.join("rustc"))?;

    // The target's std, core and alloc. `.rmeta` is not optional: the sysroot's
    // rlibs are metadata stubs and rustc wants the full metadata from the file
    // beside them (`FullMetadataNotFound` names exactly this).
    let sysroot = sysroot_libdir(rustc, arch)?;
    let mut copied = 0;
    for entry in fs::read_dir(&sysroot)? {
        let path = entry?.path();
        if !matches!(path.extension().and_then(|ext| ext.to_str()), Some("rlib" | "rmeta")) {
            continue;
        }
        fs::copy(&path, rustlib.join(path.file_name().expect("an rlib has a name")))?;
        copied += 1;
    }
    if copied == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no rlibs in {}; did `just bootstrap` build this target?",
                sysroot.display()
            ),
        ));
    }

    // One stable name, so the scenario's link line can name it: the rlib hash
    // belongs to the compiler build, not to the guest. The archive is the
    // *release* build of `minix-libc`, the same one `threadprobe` links: a
    // debug-profile archive names generics instantiated from `core`'s own codegen
    // units, and a link against the release-profile sysroot leaves them undefined
    // (`std_hello::libc_rlib` documents the mismatch).
    let libc = std_hello::libc_rlib(root, arch, rustc)?;
    fs::copy(&libc, usr_lib.join("libminix-libc.rlib"))?;

    fs::write(usr_src.join("hello.rs"), HELLO_RS)?;

    // A `std` program that starts a thread, run by the scenario before the
    // compiler. A session is built on a spawned thread, and the in-guest `rustc`
    // cannot build this probe until that works, so it is built here with the
    // stage1 compiler.
    let written = std_hello::compile(
        root,
        arch,
        rustc,
        lld,
        &libc,
        &root.join("tools").join("std-thread-probe.rs"),
        &usr_bin.join("threadprobe"),
    )?;
    if !written {
        return Err(io::Error::other("the std thread probe did not build"));
    }
    Ok(())
}

/// `<sysroot>/lib/rustlib/<triple>/lib`, from the stage1 `rustc`'s path.
fn sysroot_libdir(rustc: &Path, arch: &Arch) -> io::Result<PathBuf> {
    let stage1 = rustc.parent().and_then(Path::parent).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} has no stage1 root", rustc.display()),
        )
    })?;
    let dir = stage1.join("lib").join("rustlib").join(arch.triple).join("lib");
    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} is missing; run `just bootstrap` so the fork's std for {} exists",
                dir.display(),
                arch.triple
            ),
        ));
    }
    Ok(dir)
}

/// The version the in-guest compiler reports, taken from the stage1 compiler so
/// the two agree about the sysroot they share.
fn release_of(rustc: &Path) -> io::Result<String> {
    let output = Command::new(rustc).arg("--version").output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().nth(1).map(str::to_string).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("`{} --version` did not print a version", rustc.display()),
        )
    })
}

/// `1.100.0-dev` -> `dev`, `1.100.0` -> `stable`.
fn channel_of(release: &str) -> &str {
    release.split_once('-').map_or("stable", |(_, channel)| channel)
}

/// The `llvm-config` a `rustc_llvm` build script can run. On Windows it has to be
/// a `.cmd`, because the build script's `Command` goes through `cmd.exe`; on
/// every other host the shell script beside it is enough.
fn write_llvm_config_shim(root: &Path, arch: &Arch) -> io::Result<PathBuf> {
    let dir = root.join("target").join(format!("llvm-{}", arch.name));
    fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    let exe = exe.display().to_string();
    let path = dir.join(if cfg!(windows) { "llvm-config.cmd" } else { "llvm-config" });
    fs::write(&path, format!("@echo off\n\"{exe}\" llvm-config --arch {} %*\n", arch.name))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channel_is_the_suffix() {
        assert_eq!(channel_of("1.100.0-dev"), "dev");
        assert_eq!(channel_of("1.100.0-nightly"), "nightly");
        assert_eq!(channel_of("1.100.0"), "stable");
    }
}
