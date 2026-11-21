//! The Mesa/GL clients: a dynamic image whose GL half is Mesa's shared objects.
//!
//! Both links are "a normal dynamic minix executable" (non-PIC, `PT_INTERP` = the
//! loader, `DT_NEEDED` libs resolved at run time) plus the staged Mesa DSOs from
//! `just build-mesa <arch> --stage` or `just fetch-mesa <arch>`.
//!
//! `gltriangle` is the C-only client, so it is [`dynamic::link_program`] with a
//! Mesa header path and two extra `-l:` names. `gui` links Rust rlibs instead: its
//! entry is the crt0's renamed `_start`, and its GL and Wayland halves are crates.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::{self, Arch};
use crate::cc;
use crate::dynamic::{self, Program};
use crate::toolchain;

/// Where `just build-mesa --stage` (or `just fetch-mesa`) put the DSOs and headers.
struct Staging {
    lib: PathBuf,
    include: PathBuf,
}

fn staging(root: &Path, arch: &Arch) -> Staging {
    let mesa = root.join("target").join("mesa").join(arch.name);
    let include = if mesa.join("include").join("EGL").join("egl.h").is_file() {
        mesa.join("include")
    } else {
        root.join("target").join("mesa-src").join("include")
    };
    Staging {
        lib: mesa.join("lib"),
        include,
    }
}

/// The crate rustc links the `userland` rlib into. Its `main` is what the crt0
/// below calls; the crt0 ends the image on `exit`.
///
/// No `#[panic_handler]` here: `minix-rt` brings one in through `minix-std`, and a
/// second definition is a duplicate lang item (E0152).
const GUI_STUB: &str = "\
#![no_std]
#![no_main]

// Written by `cargo xtask build gui`.

#[unsafe(no_mangle)]
pub extern \"C\" fn main(argc: i32, argv: *const *const u8, _envp: *const *const u8) -> i32 {
    let mut buf = [\"\"; 64];
    let args = unsafe { userland::parse_args(argc, argv, &mut buf) };
    userland::wlclient::wl_gui(args)
}
";

/// The C crt0's entry symbol, renamed at compile time. `minix-rt` defines `_start`
/// too (its own runtime's entry, unused here but pulled in with `minix-std`), and
/// two definitions of it do not link. Renaming this one and pointing the ELF entry
/// at it with `-e` leaves the crt0 as the entry: it is what sets up the thread
/// pointer and `environ` through `libc.so`, which a dynamic image needs.
const CRT0_ENTRY: &str = "minix_crt0_entry";

pub fn gltriangle(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let arch = resolve(target)?;
    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;

    let libc_dir = root.join("target").join(arch.triple).join("release");
    require(
        &libc_dir.join(dynamic::LIBC),
        &format!("run `just dynlib-{}` first", arch.name),
    )?;
    let staging = staging(root, arch);
    require(
        &staging.lib.join("libEGL.so.1"),
        &format!(
            "run `just build-mesa {} --stage`, or `just fetch-mesa {}`",
            arch.name, arch.name
        ),
    )?;
    require(
        &staging.include.join("EGL").join("egl.h"),
        &format!(
            "run `just build-mesa {} --build`, or `just fetch-mesa {}`",
            arch.name, arch.name
        ),
    )?;

    let work = root.join("target").join("mesa").join(arch.name).join("bin");
    let out = work.join("gltriangle");
    let scratch = work.join("obj");
    let source = root.join("tools").join("gl_triangle.c");

    let extra_cflags = vec![
        format!("-I{}", staging.include.display()),
        "-DEGL_NO_PLATFORM_SPECIFIC_TYPES".to_string(),
    ];
    let extra_libs = vec![
        "-l:libEGL.so.1".to_string(),
        "-l:libGLESv2.so.2".to_string(),
    ];

    dynamic::link_program(&Program {
        root,
        arch,
        source: &source,
        out: &out,
        rustc: &rustc,
        lld: &lld,
        libc_dir: &libc_dir,
        scratch: &scratch,
        stub: Some("gltriangle"),
        extra_cflags: &extra_cflags,
        extra_link_dirs: std::slice::from_ref(&staging.lib),
        extra_libs: &extra_libs,
    })
}

pub fn gui(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let arch = resolve(target)?;
    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;

    let release = root.join("target").join(arch.triple).join("release");
    let userland = release.join("libuserland.rlib");

    // The rlib pulls `render` in with it.
    let mut cargo = Command::new(toolchain::cargo());
    cargo
        .current_dir(root)
        .env("RUSTC", &rustc)
        .args([
            "build",
            "-p",
            "userland",
            "--lib",
            "--release",
            "--target",
            arch.triple,
        ]);
    step(&mut cargo, "building the userland rlib")?;

    require(&userland, "run `just build-x86` first, which builds it")?;
    require(
        &release.join(dynamic::LIBC),
        &format!("run `just dynlib-{}` first", arch.name),
    )?;
    let staging = staging(root, arch);
    require(
        &staging.lib.join("libEGL.so.1"),
        &format!(
            "run `just build-mesa {} --stage`, or `just fetch-mesa {}`",
            arch.name, arch.name
        ),
    )?;
    require(
        &staging.include.join("EGL").join("egl.h"),
        &format!(
            "run `just build-mesa {} --build`, or `just fetch-mesa {}`",
            arch.name, arch.name
        ),
    )?;

    let work = root.join("target").join("mesa").join(arch.name).join("bin");
    let out = work.join("gui");
    let scratch = work.join("obj");
    fs::create_dir_all(&scratch)?;

    let mut cflags = arch.base_cflags();
    cflags.extend(["-fno-builtin", "-O2", "-c"].map(String::from));
    cflags.extend(cc::compile_flags(root)?);
    cflags.push(format!("-I{}", staging.include.display()));
    cflags.push("-DEGL_NO_PLATFORM_SPECIFIC_TYPES".to_string());

    let crt0 = scratch.join("crt0.o");
    let mut clang = Command::new("clang");
    clang
        .args(&cflags)
        .arg(format!("-D_start={CRT0_ENTRY}"))
        .arg("-o")
        .arg(&crt0)
        .arg(arch.crt0(root));
    step(&mut clang, "compiling the crt0")?;

    let stub = scratch.join("gui_stub.rs");
    fs::write(&stub, GUI_STUB)?;

    let mut link = Command::new(&rustc);
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
    // The entry is the crt0's renamed `_start`, not `minix-rt`'s: `minix-user.ld`
    // names `_start`, and `-e` overrides it.
    .arg("-C")
    .arg("link-arg=-e")
    .arg("-C")
    .arg(format!("link-arg={CRT0_ENTRY}"))
    .arg("-C")
    .arg(format!("link-arg={}", crt0.display()))
    .arg("-C")
    .arg(format!("link-arg=-L{}", release.display()))
    .arg("-C")
    .arg(format!("link-arg=-L{}", staging.lib.display()))
    // rustc's own crate search, for `userland`'s rlib and its dependencies:
    // `-C link-arg=-L` reaches only the linker.
    .arg("-L")
    .arg(format!("dependency={}", arch.libc_deps(root).display()))
    .arg("-C")
    .arg("link-arg=-Bdynamic")
    .arg("-C")
    .arg("link-arg=--allow-shlib-undefined")
    .arg("-C")
    .arg(format!("link-arg=-l:{}", dynamic::LIBC))
    .arg("-C")
    .arg("link-arg=-l:libEGL.so.1")
    .arg("-C")
    .arg("link-arg=-l:libGLESv2.so.2")
    .arg("-C")
    .arg(format!("link-arg=--dynamic-linker={}", dynamic::INTERP))
    .arg("--extern")
    .arg(format!("userland={}", userland.display()))
    .arg("-o")
    .arg(&out)
    .arg(&stub);
    if !cc::run(&mut link)? {
        return Ok(false);
    }
    println!("wrote {}", out.display());
    Ok(true)
}

fn resolve(target: Option<&str>) -> io::Result<&'static Arch> {
    match target {
        Some(token) => arch::resolve(token),
        None => Ok(arch::DEFAULT),
    }
}

fn require(path: &Path, why: &str) -> io::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no {}: {why}", path.display()),
        ))
    }
}

fn step(command: &mut Command, what: &str) -> io::Result<()> {
    if cc::run(command)? {
        Ok(())
    } else {
        Err(io::Error::other(format!("{what} failed")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_calls_into_the_in_house_renderer() {
        assert!(GUI_STUB.contains("wlclient::wl_gui"));
        assert!(!GUI_STUB.contains("panic_handler"));
    }

    #[test]
    fn the_staging_prefers_the_published_headers_then_the_checkout() {
        let root = std::env::temp_dir().join("minixrs-xtask-staging");
        let mesa = root.join("target").join("mesa").join("x86");
        std::fs::create_dir_all(mesa.join("include").join("EGL")).unwrap();
        std::fs::write(mesa.join("include").join("EGL").join("egl.h"), b"").unwrap();
        assert_eq!(staging(&root, &arch::X86_64).include, mesa.join("include"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
