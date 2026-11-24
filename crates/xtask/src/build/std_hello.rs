//! Rebuild the std smoke-test binary (`/bin/hello`) with the fork's stage1
//! compiler.
//!
//! `tools/std-hello.rs` uses `std`, so the binary is built against the fork's
//! in-tree spec, which carries a built std for the target. The OS release output
//! directory is the triple itself for all three arches (see
//! `crates/boot-image/src/targets.rs`).
//!
//! Prerequisite: the fork's std for the chosen target, which `just bootstrap`
//! builds. After (re)building the binary the boot images have to be re-assembled
//! so `/bin/hello` is embedded.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::newest_libc_rlib;
use crate::arch::{self, Arch};
use crate::build::c_hello::linker_script;
use crate::cc;
use crate::toolchain;

pub fn run(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;

    let arches: Vec<&Arch> = match target {
        None => vec![arch::DEFAULT],
        Some("all") => arch::ALL.to_vec(),
        Some(token) => vec![arch::resolve(token)?],
    };

    for arch in arches {
        if !build(root, arch, &rustc, &lld)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn build(root: &Path, arch: &Arch, rustc: &Path, lld: &Path) -> io::Result<bool> {
    let out = root.join("target").join(arch.triple).join("release").join("hello");
    fs::create_dir_all(out.parent().expect("the output has a parent"))?;

    let rlib = libc_rlib(root, arch, rustc)?;

    compile(root, arch, rustc, lld, &rlib, &root.join("tools").join("std-hello.rs"), &out)?;

    println!("wrote {}", out.display());
    Ok(true)
}

/// `minix-libc`'s release archive for `arch`.
///
/// The archive has to come from the same `core` the program is compiled against:
/// the `minix-libc` a different build produced names generics instantiated from
/// `core`'s own codegen units, and mixing the two leaves them undefined.
pub fn libc_rlib(root: &Path, arch: &Arch, rustc: &Path) -> io::Result<PathBuf> {
    let mut cargo = Command::new(toolchain::cargo());
    cargo
        .current_dir(root)
        .args(["build", "-p", "minix-libc", "--target", arch.triple, "--release"])
        .env("RUSTC", rustc);
    if !cc::run(&mut cargo)? {
        return Err(io::Error::other("minix-libc did not build"));
    }

    newest_libc_rlib(&arch.libc_deps(root))
}

/// Compile one `std` program for `arch`, linking the port's C library beside it.
///
/// `rlib` is `minix-libc`'s archive for the target. std's backtrace machinery names
/// `_Unwind_Backtrace` and `_Unwind_GetIP` on the unix family, the port's stubs for
/// them are in `minix-libc` (`crates/minix-libc/src/c_unwind.rs`), and no Rust crate
/// imports them: the archive has to be named on this link or the two symbols stay
/// undefined.
pub fn compile(
    root: &Path,
    arch: &Arch,
    rustc: &Path,
    lld: &Path,
    rlib: &Path,
    source: &Path,
    out: &Path,
) -> io::Result<bool> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    let deps = arch.libc_deps(root);

    let mut link = Command::new(rustc);
    link.args(["--target", arch.triple, "--edition", "2024"])
        .arg("-C")
        .arg(format!("link-arg=-T{}", linker_script(root).display()))
        .arg("-C")
        .arg("link-arg=--no-eh-frame-hdr")
        .arg("-C")
        .arg(format!("linker={}", lld.display()))
        .arg("--extern")
        .arg(format!("minix_libc={}", rlib.display()))
        // `--extern` alone does not put the archive on the link line: rustc drops
        // an extern crate the program never names, and `_Unwind_Backtrace` is
        // referenced by `std`, not by the program. The raw argument does.
        .arg("-C")
        .arg(format!("link-arg={}", rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(out)
        .arg(source);
    cc::run(&mut link)
}
