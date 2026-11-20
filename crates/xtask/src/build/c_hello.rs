//! Build the C smoke-test binaries: `/bin/helloc`, `/bin/ctest` and `/bin/cabi`.
//!
//! Compiles the freestanding C sources (`tools/hello.c`, `tools/ctest.c`,
//! `tools/cabi.c`, `tools/crt0-<arch>.S`) with clang, builds the `minix-libc`
//! rlib (and its no_std `minix-std`/`net`/`minix-rt` deps) with the fork's
//! stage1 compiler for the target's minix triple, then links each program with
//! the fork rustc as the driver (it resolves the rlib metadata and the minix
//! sysroot's core/compiler_builtins).
//!
//! All of the C library surface (printf family, strtod, wcs*, ...) lives in
//! `minix-libc`; the C objects only supply the program code and `_start`.
//!
//! Outputs `target/<triple>/release/{helloc,ctest,cabi}`, the locations the
//! boot-image assembly reads from (`crates/boot-image/src/manifest.rs`). Those
//! entries are x86_64-only today, so on the other arches these binaries are
//! built for their own sake (a C surface check) and injected with
//! `MINIXFS_EXTRA` where a recipe wants to run one.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use super::newest_libc_rlib;
use crate::arch::{self, Arch};
use crate::cc;
use crate::toolchain;

/// (source, output name)
const PROGRAMS: [(&str, &str); 3] = [
    ("hello.c", "helloc"),
    ("ctest.c", "ctest"),
    ("cabi.c", "cabi"),
];

/// The crate that puts `minix-libc` in the link. The C objects call its
/// `extern "C"` symbols, but rustc only adds an rlib to the link when the local
/// crate references it.
pub(crate) const LINK_STUB: &str = "\
#![no_std]
#![no_main]

// Force `minix-libc` into the link: the C objects call its
// `extern \"C\"` symbols, but rustc only adds an rlib to the link
// when the local crate references it.
extern crate minix_libc;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
";

pub fn run(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;

    let arches: Vec<&Arch> = match target {
        Some("all") => arch::ALL.to_vec(),
        Some(token) => vec![arch::resolve(token)?],
        None => vec![arch::DEFAULT],
    };

    for arch in arches {
        if !build(root, arch, &rustc, &lld)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn build(root: &Path, arch: &Arch, rustc: &Path, lld: &Path) -> io::Result<bool> {
    let out_dir = root.join("target").join(arch.triple).join("release");
    let work = root.join("target").join("c-hello").join(arch.name);
    fs::create_dir_all(&out_dir)?;
    fs::create_dir_all(&work)?;

    let mut cflags = arch.base_cflags();
    cflags.extend(["-fno-builtin", "-O2", "-c"].map(String::from));
    cflags.extend(cc::compile_flags(root)?);

    let mut clang = Command::new("clang");
    clang
        .args(&cflags)
        .arg("-o")
        .arg(work.join("crt0.o"))
        .arg(arch.crt0(root));
    if !cc::run(&mut clang)? {
        return Ok(false);
    }

    for (source, _) in PROGRAMS {
        let stem = stem_of(source);
        let mut clang = Command::new("clang");
        clang
            .args(&cflags)
            .arg("-o")
            .arg(work.join(format!("{stem}.o")))
            .arg(root.join("tools").join(source));
        if !cc::run(&mut clang)? {
            return Ok(false);
        }
    }

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
    let stub = work.join("link_stub.rs");
    fs::write(&stub, LINK_STUB)?;

    for (source, name) in PROGRAMS {
        let stem = stem_of(source);
        let out = out_dir.join(name);
        let mut link = Command::new(rustc);
        link.args([
            "--crate-type",
            "bin",
            "--target",
            arch.triple,
            "--edition",
            "2024",
        ])
        .arg("-C")
        .arg(format!("link-arg=-T{}", linker_script(root).display()))
        .arg("-C")
        .arg(format!("linker={}", lld.display()))
        .arg("-C")
        .arg(format!("link-arg={}", work.join("crt0.o").display()))
        .arg("-C")
        .arg(format!("link-arg={}", work.join(format!("{stem}.o")).display()))
        .arg("--extern")
        .arg(format!("minix_libc={}", rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(&out)
        .arg(&stub);
        if !cc::run(&mut link)? {
            return Ok(false);
        }
        println!("wrote {}", out.display());
    }

    Ok(true)
}

/// The userland linker script: page-aligned sections for the kernel exec loader,
/// and the TLS block symbols the std runtime needs.
pub(crate) fn linker_script(root: &Path) -> std::path::PathBuf {
    root.join("tools").join("minix-user.ld")
}

fn stem_of(source: &str) -> &str {
    source.strip_suffix(".c").unwrap_or(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_names_its_object_by_its_stem() {
        assert_eq!(stem_of("hello.c"), "hello");
        assert_eq!(stem_of("cabi.c"), "cabi");
    }

    #[test]
    fn every_program_has_a_distinct_object_and_output() {
        let mut names: Vec<&str> = PROGRAMS.iter().map(|&(source, _)| source).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), PROGRAMS.len());
    }
}
