//! Link a C program with clang and LLD alone, against `libc.a`.
//!
//! This is the link line a toolchain that is not the fork's rustc has to get
//! right: `tools/hello.c` and the arch's `crt0`, an archive of the C library, and
//! LLD with the userland linker script. No Rust participates, so it is also the
//! shape an in-guest `clang`/`ld.lld` will use.
//!
//! The image base and the two `--defsym` values come from [`Arch`], which mirrors
//! the fork's `pre_link_args`: a link that bypasses the target spec has no other
//! source for them, and getting one wrong faults at `_start`.
//!
//! Output is `target/<triple>/release/cdirect`. The gate injects it as
//! `/bin/cdirect` and prints `hello from C`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::c_hello::linker_script;
use super::libc_static;
use crate::arch::{self, Arch};
use crate::cc;
use crate::toolchain;

/// Where the program lands, under the target's release directory.
const PROGRAM: &str = "cdirect";
const SOURCE: &str = "hello.c";

pub fn run(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;

    let arches: Vec<&Arch> = match target {
        Some("all") => arch::ALL.to_vec(),
        Some(token) => vec![arch::resolve(token)?],
        None => vec![arch::DEFAULT],
    };

    for arch in arches {
        if !build(root, arch, &lld)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn build(root: &Path, arch: &Arch, lld: &Path) -> io::Result<bool> {
    let libc = libc_static::archive(root, arch)?;
    let release = root.join("target").join(arch.triple).join("release");
    let work = root.join("target").join("c-direct").join(arch.name);
    fs::create_dir_all(&release)?;
    fs::create_dir_all(&work)?;

    let mut cflags = arch.base_cflags();
    cflags.extend(["-fno-builtin", "-O2", "-c"].map(String::from));
    cflags.extend(cc::compile_flags(root)?);

    let crt0 = work.join("crt0.o");
    let mut compile = Command::new("clang");
    compile
        .args(&cflags)
        .arg("-o")
        .arg(&crt0)
        .arg(arch.crt0(root));
    if !cc::run(&mut compile)? {
        return Ok(false);
    }

    let object = work.join("hello.o");
    let mut compile = Command::new("clang");
    compile
        .args(&cflags)
        .arg("-o")
        .arg(&object)
        .arg(root.join("tools").join(SOURCE));
    if !cc::run(&mut compile)? {
        return Ok(false);
    }

    let out = output(root, arch);
    let mut link = Command::new(lld);
    link.args(["-flavor", "gnu"]).args(link_args(arch, &crt0, &object, &libc, root, &out));
    if !cc::run(&mut link)? {
        return Ok(false);
    }
    println!("wrote {}", out.display());
    Ok(true)
}

/// The LLD line, as a vector so the per-arch values can be asserted.
fn link_args(
    arch: &Arch,
    crt0: &Path,
    object: &Path,
    libc: &Path,
    root: &Path,
    out: &Path,
) -> Vec<String> {
    vec![
        "-o".to_string(),
        out.display().to_string(),
        format!("--image-base={}", arch.image_base),
        format!("--defsym=BASE_ADDRESS={}", arch.image_base),
        format!("--defsym=LOADER_BASE={}", arch.loader_base),
        "-T".to_string(),
        linker_script(root).display().to_string(),
        "-z".to_string(),
        "norelro".to_string(),
        "--no-eh-frame-hdr".to_string(),
        crt0.display().to_string(),
        object.display().to_string(),
        libc.display().to_string(),
    ]
}

/// The archive [`build`] last wrote, for a caller that wants to inject it.
pub(crate) fn output(root: &Path, arch: &Arch) -> PathBuf {
    root.join("target").join(arch.triple).join("release").join(PROGRAM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(arch: &Arch) -> Vec<String> {
        link_args(
            arch,
            Path::new("crt0.o"),
            Path::new("hello.o"),
            Path::new("libc.a"),
            Path::new("repo"),
            Path::new("cdirect"),
        )
    }

    #[test]
    fn the_link_pins_the_archs_image_base_and_loader_base() {
        let x86 = args(&arch::X86_64);
        assert!(x86.contains(&"--image-base=0x4000000".to_string()));
        assert!(x86.contains(&"--defsym=BASE_ADDRESS=0x4000000".to_string()));
        assert!(x86.contains(&"--defsym=LOADER_BASE=0x8000000".to_string()));

        let riscv = args(&arch::RISCV64);
        assert!(riscv.contains(&"--image-base=0x1000000".to_string()));
        assert!(riscv.contains(&"--defsym=LOADER_BASE=0x4000000".to_string()));
    }

    #[test]
    fn the_link_names_the_script_the_archive_and_both_objects() {
        let args = args(&arch::AARCH64);
        assert!(args.iter().any(|arg| arg.ends_with("minix-user.ld")));
        assert!(args.contains(&"crt0.o".to_string()));
        assert!(args.contains(&"hello.o".to_string()));
        assert!(args.contains(&"libc.a".to_string()));
    }

    #[test]
    fn the_output_is_named_cdirect_under_the_target() {
        let out = output(Path::new("repo"), &arch::X86_64);
        assert!(
            out.display()
                .to_string()
                .replace('\\', "/")
                .ends_with("target/x86_64-pc-minix/release/cdirect")
        );
    }
}
