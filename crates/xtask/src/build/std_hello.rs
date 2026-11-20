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
use std::path::Path;
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

    let mut cargo = Command::new(toolchain::cargo());
    cargo
        .current_dir(root)
        .args(["build", "-p", "minix-libc", "--target", arch.triple, "--release"])
        .env("RUSTC", rustc);
    if !cc::run(&mut cargo)? {
        return Ok(false);
    }

    let deps = arch.libc_deps(root);
    let rlib = newest_libc_rlib(&deps)?;

    // std's backtrace machinery names `_Unwind_Backtrace` and `_Unwind_GetIP` on
    // the unix family, and the port's stubs for them are in `minix-libc`
    // (`crates/minix-libc/src/c_unwind.rs`). The rlib has to be named on this
    // link, or the two symbols stay undefined.
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
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(&out)
        .arg(root.join("tools").join("std-hello.rs"));
    if !cc::run(&mut link)? {
        return Ok(false);
    }

    println!("wrote {}", out.display());
    Ok(true)
}
