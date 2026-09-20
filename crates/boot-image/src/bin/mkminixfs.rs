//! Thin CLI wrapper for building the MinixFS root image.
//!
//! Usage: `cargo run -p boot-image --bin mkminixfs [x86_64|riscv64|aarch64|wasm32]`
//!
//! Reads the already-built userland + server binaries from the shared
//! `target/<triple>/release/` dir (built by `just build <target>`) and
//! writes `target/images/<triple>/minixfs.img`. The kernel build pipeline
//! assembles the same image directly via `crates/kernel/build.rs`; this CLI
//! exists for one-off inspection.
//!
//! `wasm32` is the one arch whose image is not built by `just build`: the files it wants are
//! *modules* (`manifest::WASM_MODULES`), the module build is a separate cargo workspace, and the
//! Asyncify pass between the two is `tools/wasm-servers/run.sh`'s — so the harness stages the
//! finished modules in `target/wasm32-minix/release/` and this CLI reads them from there, exactly
//! as it reads every other target's binaries.

use std::path::Path;
use std::process::ExitCode;

use boot_image::{manifest, minixfs, targets};

fn main() -> ExitCode {
    let arch = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "x86_64".to_string());
    let t = match targets::target_from_arch(&arch) {
        Some(t) => t,
        None => {
            eprintln!("mkminixfs: unknown arch '{arch}' (use x86_64, riscv64, aarch64, or wasm32)");
            return ExitCode::FAILURE;
        }
    };

    let workspace = Path::new(".");
    let release = targets::release_dir(&t, workspace);

    // Which files an image holds is the arch's business: three archs ship the same userland
    // executables under different triples, and wasm32 ships modules instead.
    let wanted: &[(&str, &str)] = if t.arch == "wasm32" {
        manifest::WASM_MODULES
    } else {
        manifest::BOOT_BINS
    };

    let mut files = Vec::new();
    for &(dest, bin_name) in wanted {
        let src = release.join(bin_name);
        if src.exists() {
            match std::fs::read(&src) {
                Ok(data) if !data.is_empty() => files.push((dest, data)),
                Ok(_) => eprintln!("mkminixfs: WARNING: {} is empty", src.display()),
                Err(e) => {
                    eprintln!("mkminixfs: failed to read {}: {e}", src.display());
                    return ExitCode::FAILURE;
                }
            }
        } else if t.arch == "wasm32" {
            // A missing *module* is not a warning: an image whose `/bin/sh` is absent would
            // boot and then fail every exec with a path error that points at the filesystem
            // rather than at the build that did not run.
            eprintln!(
                "mkminixfs: {} not found at {} (run `sh tools/wasm-servers/run.sh`)",
                bin_name,
                src.display()
            );
            return ExitCode::FAILURE;
        } else {
            eprintln!(
                "mkminixfs: WARNING: {bin_name} not found at {} (run `just build {arch}`)",
                src.display()
            );
        }
    }

    let image = minixfs::build_minixfs(&files);

    let img_path = workspace
        .join("target")
        .join("images")
        .join(t.out_dir)
        .join("minixfs.img");
    if let Some(parent) = img_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if write_if_changed(&img_path, &image).is_err() {
        eprintln!("mkminixfs: failed to write {}", img_path.display());
        return ExitCode::FAILURE;
    }
    println!("minixfs.img: {} bytes, {} files", image.len(), files.len());
    ExitCode::SUCCESS
}

fn write_if_changed(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Ok(existing) = std::fs::read(path)
        && existing == data
    {
        return Ok(());
    }
    std::fs::write(path, data)
}
