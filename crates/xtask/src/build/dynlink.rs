//! Build the dynamic-linking gate's artifacts, shipped by no image.
//!
//! Produces, under `target/dynlink/<arch>/`:
//!
//! * `libdyn.so`, `libdyn2.so`, `libdyn3.so`, the shared objects (`tools/libdyn.c`,
//!   `tools/libdyn2.c`, `tools/libdyn3.c`), built with LLD directly;
//! * `dynhello`, a classic-dynamic `ET_EXEC` (`tools/dynhello.c`) with `PT_INTERP`
//!   (`/libexec/ld.so`) and `DT_NEEDED` for all three objects;
//! * `libdlopen.so`, `libtls1.so` and `dynopen`, the `dlopen` pair and its consumer.
//!
//! The executable is linked with the fork's stage1 rustc as the driver (the same way
//! `cargo xtask build c-hello` does it), because its `write`/`exit` come from the
//! `minix-libc` rlib; the C objects and the shared objects are clang + LLD's work.
//!
//! `libdyn2` is the file `dynhello` and `libdyn` both name, and the two aliases are the
//! same object linked again under a path soname: they are what let `libdyn` name one
//! file by a path rather than a name, which the loader's "already loaded" check has to
//! settle by the file (`st_dev`/`st_ino`) and not by the string. Neither alias is
//! installed, so all three names are one file with one inode.
//!
//! `libdyn3` is named by the executable alone and depends on nothing, so what the three
//! objects exhaust is the *address space* rather than the symbol tables.
//!
//! `libdlopen.so` and `libtls1.so` are named by nothing at link time: `dynopen` asks the
//! loader for them at run time, one to call through the handle and one to be refused for
//! its thread-local.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use super::newest_libc_rlib;
use crate::arch::{self, Arch};
use crate::cc;
use crate::dynamic::{self, Program};
use crate::toolchain;

/// Linked in an order that puts `libdyn2` first: `libdyn` names it as its own
/// `DT_NEEDED`, which is what makes the loader follow a dependency of a dependency.
const DSOS: [&str; 3] = ["libdyn2", "libdyn", "libdyn3"];

/// The objects nothing links against: `dynopen` asks the loader for these by path at
/// run time, so they are in no `DT_NEEDED`.
const DYNOPEN_DSOS: [&str; 2] = ["libdlopen", "libtls1"];

/// Two link-only copies of `libdyn2.o` beside the installed object, whose *sonames* are
/// paths to it. lld records a shared library's soname as the `DT_NEEDED` of whatever
/// links it, so these are how `libdyn` names one file by a path rather than a name.
const ALIASES: [(&str, &str); 2] = [
    ("libdynpath.so", "/lib/libdyn2.so"),
    ("libdyndot.so", "/lib/./libdyn2.so"),
];

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
    fs::create_dir_all(&work)?;

    let mut cflags = arch.base_cflags();
    cflags.extend(["-fno-builtin", "-O2", "-c"].map(String::from));
    cflags.extend(cc::compile_flags(root)?);

    // crt0 and the program are non-PIC (a non-PIE executable, per the OS's linker
    // script).
    for (source, object) in [
        (arch.crt0(root), OsString::from("crt0.o")),
        (root.join("tools").join("dynhello.c"), OsString::from("dynhello.o")),
    ] {
        let mut clang = Command::new("clang");
        clang
            .args(&cflags)
            .arg("-o")
            .arg(work.join(&object))
            .arg(&source);
        if !cc::run(&mut clang)? {
            return Ok(false);
        }
    }

    // The shared objects must be position-independent.
    let mut dso_flags = cflags.clone();
    dso_flags.push("-fPIC".to_string());
    for name in DSOS {
        if !compile_dso_source(root, &work, name, &dso_flags)? {
            return Ok(false);
        }
    }

    // Link each .so with LLD directly (the minix target specs ask for `lld` on PATH; we
    // have the toolchain's own). `libdyn2.so` first: it is the file `dynhello` and
    // `libdyn` both name, and the two aliases are the same object linked again under a
    // path soname.
    let dso2 = work.join("libdyn2.so");
    if !link_shared(lld, "libdyn2.so", &dso2, &work.join("libdyn2.o"), &[])? {
        return Ok(false);
    }
    println!("wrote {}", dso2.display());

    let mut alias_args: Vec<OsString> = Vec::new();
    for (alias_name, soname) in ALIASES {
        let alias = work.join(alias_name);
        if !link_shared(lld, soname, &alias, &work.join("libdyn2.o"), &[])? {
            return Ok(false);
        }
        println!("wrote {} (soname {soname})", alias.display());
        alias_args.push(OsString::from(format!("-L{}", work.display())));
        alias_args.push(OsString::from(format!("-l:{alias_name}")));
    }

    // `libdyn.so` names `libdyn2` three ways: by soname, which the loader has to find on
    // its search path, and by the two paths above, which it must recognise as the file it
    // already mapped rather than mapping a second copy of. `--no-as-needed` keeps the
    // aliases in `DT_NEEDED`: they export exactly the same symbols as `libdyn2.so`, so
    // only the first of them would otherwise be recorded.
    let dso1 = work.join("libdyn.so");
    let mut extra: Vec<OsString> = vec![OsString::from("--no-as-needed")];
    extra.push(OsString::from(format!("-L{}", work.display())));
    extra.push(OsString::from("-l:libdyn2.so"));
    extra.extend(alias_args);
    if !link_shared(lld, "libdyn.so", &dso1, &work.join("libdyn.o"), &extra)? {
        return Ok(false);
    }
    println!("wrote {}", dso1.display());

    // The third object: named by the executable alone, with nothing depending on it, so
    // what it exercises is the address space rather than resolution.
    let dso3 = work.join("libdyn3.so");
    if !link_shared(lld, "libdyn3.so", &dso3, &work.join("libdyn3.o"), &[])? {
        return Ok(false);
    }
    println!("wrote {}", dso3.display());

    // The `dlopen` objects, built like the others: to the loader an object asked for at
    // run time is no different from one that arrived through `DT_NEEDED`.
    for name in DYNOPEN_DSOS {
        if !compile_dso_source(root, &work, name, &dso_flags)? {
            return Ok(false);
        }
        let so = work.join(format!("{name}.so"));
        if !link_shared(
            lld,
            &format!("{name}.so"),
            &so,
            &work.join(format!("{name}.o")),
            &[],
        )? {
            return Ok(false);
        }
        println!("wrote {}", so.display());
    }

    // minix-libc for the target: the executable's write/exit come from here.
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
    fs::write(&stub, crate::build::c_hello::LINK_STUB)?;

    let out = work.join("dynhello");
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
    .arg(format!(
        "link-arg=-T{}",
        crate::build::c_hello::linker_script(root).display()
    ))
    .arg("-C")
    .arg(format!("linker={}", lld.display()))
    .arg("-C")
    .arg(format!("link-arg={}", work.join("crt0.o").display()))
    .arg("-C")
    .arg(format!("link-arg={}", work.join("dynhello.o").display()))
    // Resolve the calls and the data symbol against the shared objects, and record the
    // interpreter that must run before `main`. `-Bdynamic` is needed because the minix
    // target's `crt_static_default` makes rustc pass `-static`, under which lld refuses
    // to link a `.so` at all.
    .arg("-C")
    .arg(format!("link-arg=-L{}", work.display()))
    .arg("-C")
    .arg("link-arg=-Bdynamic");
    for name in DSOS {
        link.arg("-C").arg(format!("link-arg=-l:{name}.so"));
    }
    link.arg("-C")
        .arg(format!("link-arg=--dynamic-linker={}", dynamic::INTERP))
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

    // `dynopen`: the `dlopen` consumer, and the second program that names nothing beyond
    // libc. Its link is [`dynamic::link_program`]'s rather than the one above, and that is
    // not tidiness: naming `minix_libc` (which `dynhello` does, for `write`/`exit`) puts
    // the static library in the program, and its `dlopen` would then satisfy the
    // reference instead of the `libc.so` that reaches the loader.
    let source = root.join("tools").join("dynopen.c");
    let out = work.join("dynopen");
    let release = root.join("target").join(arch.triple).join("release");
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

fn compile_dso_source(
    root: &Path,
    work: &Path,
    name: &str,
    dso_flags: &[String],
) -> io::Result<bool> {
    let mut clang = Command::new("clang");
    clang
        .args(dso_flags)
        .arg("-o")
        .arg(work.join(format!("{name}.o")))
        .arg(root.join("tools").join(format!("{name}.c")));
    cc::run(&mut clang)
}

/// `lld -flavor gnu -shared -z norelro -soname <soname> -o <out> <object> <extra…>`
fn link_shared(
    lld: &Path,
    soname: &str,
    out: &Path,
    object: &Path,
    extra: &[OsString],
) -> io::Result<bool> {
    let mut link = Command::new(lld);
    link.args(["-flavor", "gnu", "-shared"])
        .args(dynamic::NO_RELRO)
        .arg("-soname")
        .arg(soname)
        .arg("-o")
        .arg(out)
        .arg(object)
        .args(extra);
    cc::run(&mut link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libdyn2_is_linked_before_the_object_that_names_it() {
        assert_eq!(DSOS[0], "libdyn2");
        assert!(DSOS.contains(&"libdyn"));
    }

    #[test]
    fn the_aliases_are_paths_to_the_same_file() {
        for (_, soname) in ALIASES {
            assert!(soname.starts_with("/lib/"));
            assert!(soname.ends_with("libdyn2.so"));
        }
    }
}
