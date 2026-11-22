//! Build the port's C library as `libc.a`, the archive a foreign linker driver
//! consumes.
//!
//! Every existing C program links through the fork's rustc, which resolves the
//! `minix-libc` rlib and its `core`/`compiler_builtins` dependencies. That is not
//! the shape a third-party toolchain's build has: LLVM's CMake links its own
//! executables, and an in-guest `clang`/`ld.lld` will too. Both need a plain
//! `libc.a` and a `crt0.o`, with no Rust in the link line.
//!
//! `--crate-type staticlib` is what produces that archive. It is a final
//! artifact, so it needs the `panic` lang item the `so` build also supplies;
//! the `staticlib` feature is that, and it leaves the static TLS path alone.
//!
//! The archive is copied to `libc.a` beside the crate's own `libminix_libc.a`
//! because the name is part of the interface a C toolchain expects.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::{self, Arch};
use crate::cc;
use crate::toolchain;

/// Cargo's name for the archive, before it is given the name a C link expects.
const CRATE_ARCHIVE: &str = "libminix_libc.a";
const ARCHIVE: &str = "libc.a";

pub fn run(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;

    let arches: Vec<&Arch> = match target {
        Some("all") => arch::ALL.to_vec(),
        Some(token) => vec![arch::resolve(token)?],
        None => vec![arch::DEFAULT],
    };

    for arch in arches {
        if !build(root, arch, &rustc)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn build(root: &Path, arch: &Arch, rustc: &Path) -> io::Result<bool> {
    let release = root.join("target").join(arch.triple).join("release");
    fs::create_dir_all(&release)?;

    let mut cargo = Command::new(toolchain::cargo());
    cargo
        .current_dir(root)
        .args([
            "rustc",
            "--release",
            "-p",
            "minix-libc",
            "--target",
            arch.triple,
            "--features",
            "staticlib",
            "--crate-type",
            "staticlib",
        ])
        .env("RUSTC", rustc);
    if !cc::run(&mut cargo)? {
        return Ok(false);
    }

    let built = release.join(CRATE_ARCHIVE);
    if !built.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} was not produced", built.display()),
        ));
    }
    let dest = release.join(ARCHIVE);
    fs::copy(&built, &dest)?;
    println!("wrote {}", dest.display());
    Ok(true)
}

/// Where [`run`] writes the archive for one arch.
pub(crate) fn archive_path(root: &Path, arch: &Arch) -> PathBuf {
    root.join("target")
        .join(arch.triple)
        .join("release")
        .join(ARCHIVE)
}

/// The archive a [`c_direct`](super::c_direct) link consumes, or the error naming
/// the command that produces it.
pub(crate) fn archive(root: &Path, arch: &Arch) -> io::Result<PathBuf> {
    let path = archive_path(root, arch);
    if path.is_file() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} is missing; run `cargo xtask build libc-static {}` first",
                path.display(),
                arch.name
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_is_named_the_way_a_c_link_expects() {
        assert_eq!(ARCHIVE, "libc.a");
        let path = archive_path(Path::new("repo"), &arch::RISCV64);
        assert_eq!(
            path.display().to_string().replace('\\', "/"),
            "repo/target/riscv64gc-unknown-minix/release/libc.a"
        );
    }
}
