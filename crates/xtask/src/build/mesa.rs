//! The Mesa/GL clients: a dynamic image whose GL half is Mesa's shared objects.
//!
//! `gltriangle` and `gbmtest` are C-only clients, so they are
//! [`dynamic::link_program`] with a Mesa header path and extra `-l:` names.
//! `gui`, `wlgl`, `wlcompose` and `wldmabufgl` link Rust rlibs instead: their
//! entry is the crt0's renamed `_start`, their GL and Wayland halves are crates,
//! and an optional C render object provides the GL calls.

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

/// The source trees the GBM and DRM headers come from (`just build-mesa`).
fn sources(root: &Path) -> (PathBuf, PathBuf) {
    (
        root.join("target").join("mesa-src"),
        root.join("target").join("drm-src"),
    )
}

/// The C crt0's entry symbol, renamed at compile time. `minix-rt` defines `_start`
/// too (its own runtime's entry, unused here but pulled in with `minix-std`), and
/// two definitions of it do not link. Renaming this one and pointing the ELF entry
/// at it with `-e` leaves the crt0 as the entry: it is what sets up the thread
/// pointer and `environ` through `libc.so`, which a dynamic image needs.
const CRT0_ENTRY: &str = "minix_crt0_entry";

/// The crate rustc links the `userland` rlib and the optional C object into.
///
/// `call` is the `userland` function the crt0 calls. No `#[panic_handler]` here:
/// `minix-rt` brings one in through `minix-std`, and a second definition is a
/// duplicate lang item (E0152).
fn rust_stub(call: &str) -> String {
    format!(
        r#"#![no_std]
#![no_main]

// Written by `cargo xtask build`.

#[unsafe(no_mangle)]
pub extern "C" fn main(argc: i32, argv: *const *const u8, _envp: *const *const u8) -> i32 {{
    let mut buf = [""; 64];
    let args = unsafe {{ userland::parse_args(argc, argv, &mut buf) }};
    {call}(args)
}}
"#
    )
}

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

/// The GBM create/export probe: a C-only client that links GBM and libdrm.
pub fn gbmtest(root: &Path, target: Option<&str>) -> io::Result<bool> {
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
        &staging.lib.join("libgbm.so.1"),
        &format!("run `just build-mesa {} --build`", arch.name),
    )?;
    let (mesa_src, drm_src) = sources(root);
    for header in [
        mesa_src.join("src").join("gbm").join("main").join("gbm.h"),
        drm_src.join("xf86drm.h"),
    ] {
        require(
            &header,
            &format!(
                "run `just build-mesa {} --build` (the GBM probe compiles against the source checkouts)",
                arch.name
            ),
        )?;
    }

    let work = root.join("target").join("mesa").join(arch.name).join("bin");
    let out = work.join("gbmtest");
    let scratch = work.join("obj");
    let source = root.join("tools").join("gbm_test.c");

    let extra_cflags = vec![
        format!("-I{}", mesa_src.join("src").join("gbm").join("main").display()),
        format!("-I{}", drm_src.display()),
        format!("-I{}", drm_src.join("include").join("drm").display()),
    ];
    let extra_libs = vec!["-l:libgbm.so.1".to_string(), "-l:libdrm.so.2".to_string()];

    dynamic::link_program(&Program {
        root,
        arch,
        source: &source,
        out: &out,
        rustc: &rustc,
        lld: &lld,
        libc_dir: &libc_dir,
        scratch: &scratch,
        stub: Some("gbmtest"),
        extra_cflags: &extra_cflags,
        extra_link_dirs: std::slice::from_ref(&staging.lib),
        extra_libs: &extra_libs,
    })
}

pub fn gui(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let arch = resolve(target)?;
    let staging = staging(root, arch);
    let cflags = vec![
        format!("-I{}", staging.include.display()),
        "-DEGL_NO_PLATFORM_SPECIFIC_TYPES".to_string(),
    ];
    rust_client(
        root,
        arch,
        &RustClient {
            name: "gui",
            call: "userland::wlclient::wl_gui",
            render: None,
            cflags: &cflags,
            libs: &["libEGL.so.1", "libGLESv2.so.2"],
            checks: &egl_checks(arch, &staging),
        },
    )
}

/// `/bin/wlgl`, the Wayland GL present client (§6.10, 3d-a).
pub fn wlgl(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let arch = resolve(target)?;
    let staging = staging(root, arch);
    let cflags = vec![
        format!("-I{}", staging.include.display()),
        "-DEGL_NO_PLATFORM_SPECIFIC_TYPES".to_string(),
    ];
    rust_client(
        root,
        arch,
        &RustClient {
            name: "wlgl",
            call: "userland::wlclient::wl_gl",
            render: Some(&root.join("tools").join("wlgl_render.c")),
            cflags: &cflags,
            libs: &["libEGL.so.1", "libGLESv2.so.2"],
            checks: &egl_checks(arch, &staging),
        },
    )
}

/// `/bin/wlcompose`, the GPU-composition helper and its test peer (§6.10, 3d-b-5b).
pub fn wlcompose(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let arch = resolve(target)?;
    rust_client(
        root,
        arch,
        &RustClient {
            name: "wlcompose",
            call: "userland::wlcompose::wlcompose",
            render: Some(&root.join("tools").join("wlcompose_render.c")),
            cflags: &compose_cflags(root),
            libs: &[
                "libEGL.so.1",
                "libGLESv2.so.2",
                "libgbm.so.1",
                "libdrm.so.2",
            ],
            checks: &compose_checks(root, arch),
        },
    )
}

/// `/bin/wldmabufgl`, the linux-dmabuf GL present client (§6.10, 3d-b-4d).
pub fn wldmabufgl(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let arch = resolve(target)?;
    rust_client(
        root,
        arch,
        &RustClient {
            name: "wldmabufgl",
            call: "userland::wlclient::wl_dmabuf_gl",
            render: Some(&root.join("tools").join("wldmabufgl_render.c")),
            cflags: &compose_cflags(root),
            libs: &[
                "libEGL.so.1",
                "libGLESv2.so.2",
                "libgbm.so.1",
                "libdrm.so.2",
            ],
            checks: &compose_checks(root, arch),
        },
    )
}

fn egl_checks(arch: &Arch, staging: &Staging) -> Vec<(PathBuf, String)> {
    vec![
        (
            staging.lib.join("libEGL.so.1"),
            format!(
                "run `just build-mesa {} --stage`, or `just fetch-mesa {}`",
                arch.name, arch.name
            ),
        ),
        (
            staging.include.join("EGL").join("egl.h"),
            format!(
                "run `just build-mesa {} --build`, or `just fetch-mesa {}`",
                arch.name, arch.name
            ),
        ),
    ]
}

fn compose_cflags(root: &Path) -> Vec<String> {
    let (mesa_src, drm_src) = sources(root);
    vec![
        format!("-I{}", mesa_src.join("include").display()),
        format!("-I{}", mesa_src.join("src").join("gbm").join("main").display()),
        format!("-I{}", drm_src.display()),
        format!("-I{}", drm_src.join("include").join("drm").display()),
        "-DEGL_NO_PLATFORM_SPECIFIC_TYPES".to_string(),
    ]
}

fn compose_checks(root: &Path, arch: &Arch) -> Vec<(PathBuf, String)> {
    let (mesa_src, _) = sources(root);
    let lib = root.join("target").join("mesa").join(arch.name).join("lib");
    let build = format!("run `just build-mesa {} --build`", arch.name);
    vec![
        (lib.join("libEGL.so.1"), build.clone()),
        (
            lib.join("libgbm.so.1"),
            format!("{build} (GBM is enabled in the build config)"),
        ),
        (
            mesa_src.join("include").join("EGL").join("eglext.h"),
            format!("{build} (the client compiles against the sources)"),
        ),
    ]
}

struct RustClient<'a> {
    name: &'a str,
    call: &'a str,
    render: Option<&'a Path>,
    cflags: &'a [String],
    libs: &'a [&'a str],
    checks: &'a [(PathBuf, String)],
}

/// Link a Rust-half client: the `userland` rlib, the crt0's renamed entry, and an
/// optional C render object, against `libs`.
fn rust_client(root: &Path, arch: &Arch, client: &RustClient<'_>) -> io::Result<bool> {
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
    for (path, why) in client.checks {
        require(path, why)?;
    }

    let work = root.join("target").join("mesa").join(arch.name).join("bin");
    let out = work.join(client.name);
    let scratch = work.join("obj");
    fs::create_dir_all(&scratch)?;

    let mut cflags = arch.base_cflags();
    cflags.extend(["-fno-builtin", "-O2", "-c"].map(String::from));
    cflags.extend(cc::compile_flags(root)?);
    cflags.extend(client.cflags.iter().cloned());

    let crt0 = scratch.join("crt0.o");
    let mut clang = Command::new("clang");
    clang
        .args(&cflags)
        .arg(format!("-D_start={CRT0_ENTRY}"))
        .arg("-o")
        .arg(&crt0)
        .arg(arch.crt0(root));
    step(&mut clang, "compiling the crt0")?;

    let mut objects = vec![crt0];
    if let Some(source) = client.render {
        let stem = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("render");
        let object = scratch.join(format!("{stem}.o"));
        let mut clang = Command::new("clang");
        clang.args(&cflags).arg("-o").arg(&object).arg(source);
        step(&mut clang, "compiling the C render object")?;
        objects.push(object);
    }

    let stub = scratch.join(format!("{}_stub.rs", client.name));
    fs::write(&stub, rust_stub(client.call))?;

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
    .arg(format!("link-arg={CRT0_ENTRY}"));
    for object in &objects {
        link.arg("-C").arg(format!("link-arg={}", object.display()));
    }
    link.arg("-C")
        .arg(format!("link-arg=-L{}", release.display()))
        .arg("-C")
        .arg(format!(
            "link-arg=-L{}",
            root.join("target").join("mesa").join(arch.name).join("lib").display()
        ))
        // rustc's own crate search, for `userland`'s rlib and its dependencies:
        // `-C link-arg=-L` reaches only the linker.
        .arg("-L")
        .arg(format!("dependency={}", arch.libc_deps(root).display()))
        .arg("-C")
        .arg("link-arg=-Bdynamic")
        .arg("-C")
        .arg("link-arg=--allow-shlib-undefined")
        .arg("-C")
        .arg(format!("link-arg=-l:{}", dynamic::LIBC));
    for library in client.libs {
        link.arg("-C").arg(format!("link-arg=-l:{library}"));
    }
    link.arg("-C")
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
    fn each_stub_calls_its_wayland_entry() {
        assert!(rust_stub("userland::wlclient::wl_gui").contains("wlclient::wl_gui"));
        assert!(rust_stub("userland::wlclient::wl_gl").contains("wlclient::wl_gl"));
        assert!(rust_stub("userland::wlcompose::wlcompose").contains("wlcompose::wlcompose"));
        assert!(rust_stub("userland::wlclient::wl_dmabuf_gl").contains("wl_dmabuf_gl"));
        assert!(!rust_stub("userland::wlclient::wl_gui").contains("panic_handler"));
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
