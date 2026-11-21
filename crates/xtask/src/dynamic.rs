//! Link a C program against the port's shared C library.
//!
//! An image carries `/lib/libc.so` and the loader (`crates/boot-image/src/manifest.rs`),
//! so a program linked this way runs on a booted system with nothing else added to the
//! image. The program is a normal, non-PIC minix executable (VFS execs it at its link
//! addresses like any other), and what makes it dynamic is three things in the link:
//!
//! * `-Bdynamic`, because the minix target's `crt_static_default` makes rustc pass
//!   `-static`, under which lld refuses a shared object at all;
//! * `-l:libc.so` against the directory holding it, which gives the program a
//!   `DT_NEEDED` the loader can find (`/lib/libc.so` on the search path);
//! * `--dynamic-linker=/libexec/ld.so`, which is the `PT_INTERP` the kernel's exec
//!   looks for.
//!
//! `--allow-shlib-undefined` comes with the second: `libc.so` leaves four symbols
//! undefined for the loader (`__tls_get_addr` and the three TLS bounds) and lld checks a
//! linked shared object's undefined symbols by default.
//!
//! This is shared: the shipped `/bin/dynclib` and `/bin/dynopen` are built with
//! [`link_program`], and `cargo xtask build cdyn` is it for a program of your own.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::Arch;
use crate::cc;

pub const INTERP: &str = "/libexec/ld.so";
pub const LIBC: &str = "libc.so";

/// The link flags every shared object takes.
///
/// `GNU_RELRO` is a `PT_LOAD` of its own in a shared object, and the loader spends one VM
/// region per `PT_LOAD` (`crates/servers/src/vm/region.rs::MAX_REGIONS`). Nothing in
/// `crates/ldso` reads the header, so the segment costs a region per object and buys
/// nothing. `-z norelro` merges `.data.rel.ro` and `.got` into the writable `PT_LOAD`
/// they are already writable in, and drops it.
///
/// Not `--no-rosegment`, which would merge the object's read-only data into the
/// executable segment for one more region: that maps `.rodata` and `.dynstr` executable,
/// which the segment above does not.
pub const NO_RELRO: [&str; 2] = ["-z", "norelro"];

/// NO_RELRO split into the `-C link-arg=` pairs a rustc-driven link takes.
pub fn no_relro_as_rustc_link_args() -> Vec<String> {
    NO_RELRO
        .iter()
        .flat_map(|arg| ["-C".to_string(), format!("link-arg={arg}")])
        .collect()
}

/// `rustc` drives the link (the C objects alone are not a crate), and the crate it is
/// given exists only for that: it must not name `minix_libc`, or the link would put a
/// second, static copy of the C library in the program.
const STUB: &str = "\
#![no_std]
#![no_main]

// Written by `cargo xtask build cdyn`: rustc needs a crate to link the C objects
// into. It deliberately does not name `minix_libc`: the C library is meant to
// arrive from `libc.so` at run time, and an rlib here would be a static copy.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
";

/// One program to build: `source` compiled and linked against the `libc.so` in
/// `libc_dir`, written to `out`, with `scratch` holding the objects in between.
pub struct Program<'a> {
    pub root: &'a Path,
    pub arch: &'a Arch,
    pub source: &'a Path,
    pub out: &'a Path,
    pub rustc: &'a Path,
    pub lld: &'a Path,
    pub libc_dir: &'a Path,
    pub scratch: &'a Path,
    /// The stub's base name (default: the source's stem), which is also the crate
    /// name rustc derives for it.
    pub stub: Option<&'a str>,
    /// Extra compile flags: a header tree, a feature define.
    pub extra_cflags: &'a [String],
    /// Extra `-L` directories for the link.
    pub extra_link_dirs: &'a [PathBuf],
    /// Extra `-l:` names for the link, in order.
    pub extra_libs: &'a [String],
}

pub fn link_program(program: &Program<'_>) -> io::Result<bool> {
    fs::create_dir_all(program.scratch)?;
    if let Some(parent) = program.out.parent() {
        fs::create_dir_all(parent)?;
    }

    let stem = program
        .source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("program");
    let stub_stem = program.stub.unwrap_or(stem);

    let mut cflags = program.arch.base_cflags();
    cflags.extend(["-fno-builtin", "-O2", "-c"].map(String::from));
    cflags.extend(cc::compile_flags(program.root)?);
    cflags.extend(program.extra_cflags.iter().cloned());

    let crt0 = program.scratch.join("crt0.o");
    let object = program.scratch.join(format!("{stem}.o"));
    let crt0_source = program.arch.crt0(program.root);
    for (source, destination) in [
        (crt0_source.as_path(), crt0.as_path()),
        (program.source, object.as_path()),
    ] {
        let mut clang = Command::new("clang");
        clang
            .args(&cflags)
            .arg("-o")
            .arg(destination)
            .arg(source);
        if !cc::run(&mut clang)? {
            return Ok(false);
        }
    }

    let stub = program.scratch.join(format!("{stub_stem}_stub.rs"));
    fs::write(&stub, STUB)?;

    let mut link = Command::new(program.rustc);
    link.args([
        "--crate-type",
        "bin",
        "--target",
        program.arch.triple,
        "--edition",
        "2024",
    ])
    .arg("-C")
    .arg(format!(
        "link-arg=-T{}",
        crate::build::c_hello::linker_script(program.root).display()
    ))
    .arg("-C")
    .arg(format!("linker={}", program.lld.display()))
    .arg("-C")
    .arg(format!("link-arg={}", crt0.display()))
    .arg("-C")
    .arg(format!("link-arg={}", object.display()))
    .arg("-C")
    .arg(format!("link-arg=-L{}", program.libc_dir.display()));
    for directory in program.extra_link_dirs {
        link.arg("-C")
            .arg(format!("link-arg=-L{}", directory.display()));
    }
    link.arg("-C")
        .arg("link-arg=-Bdynamic")
        .arg("-C")
        .arg("link-arg=--allow-shlib-undefined")
        .arg("-C")
        .arg(format!("link-arg=-l:{LIBC}"));
    for library in program.extra_libs {
        link.arg("-C").arg(format!("link-arg={library}"));
    }
    link.arg("-C")
        .arg(format!("link-arg=--dynamic-linker={INTERP}"))
        .arg("-o")
        .arg(program.out)
        .arg(&stub);
    if !cc::run(&mut link)? {
        return Ok(false);
    }

    println!("wrote {}", program.out.display());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_flags_reach_rustc_as_link_args() {
        assert_eq!(
            no_relro_as_rustc_link_args(),
            vec!["-C", "link-arg=-z", "-C", "link-arg=norelro"]
        );
    }

    #[test]
    fn the_stub_does_not_name_the_static_library() {
        assert!(!STUB.contains("extern crate minix_libc"));
        assert!(STUB.contains("#[panic_handler]"));
    }
}
