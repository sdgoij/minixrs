//! Link a C program of your own against the port's shared C library.
//!
//! Writes `target/dync/<arch>/<name>`, `name` defaulting to the source's stem, and
//! prints the command that boots it. The link is [`crate::dynamic::link_program`]'s, which
//! is the same one the shipped `/bin/dynclib` gets, so a program built here is built
//! with the flags that program is tested with (`just test-cdyn-x86`).

use std::io;
use std::path::Path;

use crate::arch;
use crate::dynamic::{self, Program};
use crate::paths::relative_posix;
use crate::toolchain;

pub fn run(
    root: &Path,
    target: Option<&str>,
    source: &Path,
    name: Option<&str>,
) -> io::Result<bool> {
    let arch = match target {
        Some(token) => arch::resolve(token)?,
        None => arch::DEFAULT,
    };
    if !source.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no such source: {}", source.display()),
        ));
    }
    let name = name.map_or_else(
        || {
            source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("program")
                .to_string()
        },
        str::to_string,
    );

    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;

    let work = root.join("target").join("dync").join(arch.name);
    let out = work.join(&name);
    // The library is the one the image ships: `cargo xtask build dynlibc` puts it in the
    // release directory, which is where `BOOT_BINS` reads it from.
    let libc_dir = root.join("target").join(arch.triple).join("release");
    let libc = libc_dir.join(dynamic::LIBC);
    if !libc.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no {}; run `just dynlib-{}` first", libc.display(), arch.name),
        ));
    }

    if !dynamic::link_program(&Program {
        root,
        arch,
        source,
        out: &out,
        rustc: &rustc,
        lld: &lld,
        libc_dir: &libc_dir,
        scratch: &work,
    })? {
        return Ok(false);
    }

    // Relative to the workspace, which is how the recipes name things and how
    // `MINIXFS_EXTRA` resolves a value (`crates/kernel/build.rs`). On Windows the value
    // must also be excluded from MSYS's path conversion, or `/bin/<name>` arrives as the
    // MSYS `/bin`; the Justfile's own recipes avoid that by setting the variable inside a
    // recipe.
    let exclusion = if cfg!(windows) {
        "MSYS2_ENV_CONV_EXCL=MINIXFS_EXTRA "
    } else {
        ""
    };
    println!(
        "run it with: {exclusion}MINIXFS_EXTRA='/bin/{name}={}' just run {}",
        relative_posix(root, &out),
        arch.name
    );
    Ok(true)
}
