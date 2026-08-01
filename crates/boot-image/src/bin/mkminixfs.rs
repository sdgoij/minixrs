//! Thin CLI wrapper for building the MinixFS root image.
//!
//! Usage: `cargo run -p boot-image --bin mkminixfs [x86_64|riscv64|aarch64]`
//!
//! Reads the already-built userland + server binaries from the shared
//! `target/<triple>/release/` dir (built by `just build <target>`) and
//! writes `target/images/<triple>/minixfs.img`. The kernel build pipeline
//! assembles the same image directly via `crates/kernel/build.rs`; this CLI
//! exists for one-off inspection.

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
            eprintln!("mkminixfs: unknown arch '{arch}' (use x86_64, riscv64, or aarch64)");
            return ExitCode::FAILURE;
        }
    };

    let workspace = Path::new(".");
    let release = targets::release_dir(&t, workspace);

    let mut files = Vec::new();
    for &(dest, bin_name) in manifest::BOOT_BINS {
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
