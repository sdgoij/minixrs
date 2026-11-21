//! Build the shipped dynamic artifacts: `/lib/libc.so` and `/bin/dynclib`.
//!
//! Both land in the target's release directory (`target/<triple>/release/`, where
//! `BOOT_BINS` reads them, the way `cargo xtask build c-hello` puts `helloc`/`ctest`
//! there):
//!
//! * `libc.so`, the C library the port ships statically today, built
//!   position-independent and linked as a `cdylib`, with `libc.so` as its soname;
//! * `dynclib`, a C program (`tools/dynclib.c`) linked *against* that object rather
//!   than the `minix-libc` rlib, so every libc symbol it calls is resolved by the
//!   loader at run time. The link itself is [`crate::dynamic::link_program`], which is
//!   also what `cargo xtask build cdyn` uses for a program of your own, so the flags
//!   a user gets are the ones this is exercised for.
//!
//! `/libexec/ld.so` is the third image entry, and it comes from `cargo build -p ldso`
//! (`just dynlib-<arch>` runs that too).
//!
//! Two things about the library are not obvious:
//!
//! * The shared object is built for the arch's `-elf` triple, a target of the fork's
//!   own (`compiler/rustc_target/src/spec/targets/*_minix_elf.rs`), which differs from
//!   the executable one in being `pic` and allowing dylibs. Neither is a flag that
//!   could be passed instead: what a `cdylib` must not contain is the absolute
//!   relocations of the precompiled `core`/`alloc`, and those come from the target's
//!   sysroot, so the target is what has to be PIC. `cargo xtask rust-config` lists these
//!   triples with `no-std = true`, so their sysroots hold `core` and `alloc` and
//!   nothing else.
//! * `--features so` gives the object the `panic` lang item a final artifact needs,
//!   and `--soname=libc.so` is what makes a program's `DT_NEEDED` the name the loader
//!   searches for: `/lib/libc.so`.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use crate::arch::{self, Arch};
use crate::cc;
use crate::dynamic::{self, Program};
use crate::toolchain;

/// What cargo names the object, before it is given the name its soname declares.
const CRATE_SO: &str = "libminix_libc.so";
const DSO: &str = "libc.so";
/// The program that links against it, as the image spells it.
const PROGRAM: &str = "dynclib";

/// The PIC triple for `arch`, whose sysroot holds `core` and `alloc`.
///
/// The `-elf` suffix is what the fork's target list and `cargo xtask rust-config` agree
/// on, and cargo's build directory for it follows from the name.
fn dyn_triple(arch: &Arch) -> String {
    format!("{}-elf", arch.triple)
}

pub fn run(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;
    let arch = match target {
        Some(token) => arch::resolve(token)?,
        None => arch::DEFAULT,
    };
    build(root, arch, &rustc, &lld)
}

fn build(root: &Path, arch: &Arch, rustc: &Path, lld: &Path) -> io::Result<bool> {
    let work = root.join("target").join("dynlink").join(arch.name);
    let release = root.join("target").join(arch.triple).join("release");
    fs::create_dir_all(&work)?;
    fs::create_dir_all(&release)?;
    let triple = dyn_triple(arch);

    // The stage1 compiler is what has this target built in and a sysroot for it, so it
    // is the RUSTC here as it is in every other recipe. Cargo drives the object's link
    // so the linker can be passed explicitly: the target spec names `lld` on PATH,
    // which is not the LLD the rest of the build uses.
    let mut cargo = Command::new(toolchain::cargo());
    cargo
        .current_dir(root)
        .args([
            "rustc",
            "--release",
            "-p",
            "minix-libc",
            "--features",
            "so",
            "--target",
        ])
        .arg(&triple)
        .args(["--crate-type", "cdylib", "--"])
        .arg("-C")
        .arg(format!("linker={}", lld.display()))
        .arg("-C")
        .arg("link-arg=--soname=libc.so")
        .args(dynamic::no_relro_as_rustc_link_args())
        .env("RUSTC", rustc);
    if !cc::run(&mut cargo)? {
        return Ok(false);
    }

    let built = root.join("target").join(&triple).join("release").join(CRATE_SO);
    if !built.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} was not produced", built.display()),
        ));
    }
    let dest = release.join(DSO);
    fs::copy(&built, &dest)?;
    println!("wrote {}", dest.display());

    // And the program that links against it. The link is the shared one, so the
    // shipped `/bin/dynclib` and a program built by `cargo xtask build cdyn` are built
    // by the same flags, including the ones that are easy to leave out (`-Bdynamic`
    // against the target's static default, and `--allow-shlib-undefined` for the four
    // symbols the loader answers for an object).
    let source = root.join("tools").join("dynclib.c");
    let out = release.join(PROGRAM);
    dynamic::link_program(&Program {
        root,
        arch,
        source: &source,
        out: &out,
        rustc,
        lld,
        libc_dir: &release,
        scratch: &work,
        stub: None,
        extra_cflags: &[],
        extra_link_dirs: &[],
        extra_libs: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_built_for_the_pic_triple() {
        assert_eq!(dyn_triple(&arch::X86_64), "x86_64-pc-minix-elf");
        assert_eq!(dyn_triple(&arch::RISCV64), "riscv64gc-unknown-minix-elf");
    }
}
