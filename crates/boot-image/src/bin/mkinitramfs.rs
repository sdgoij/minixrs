//! Thin CLI wrapper for building the initramfs CPIO archive.
//!
//! Usage: `cargo run -p boot-image --bin mkinitramfs [x86_64|riscv64|aarch64]`
//!
//! Reads the already-built userland + server binaries from the shared
//! `target/<triple>/release/` dir (built by `just build <target>`) and
//! writes `target/images/<triple>/initramfs.cpio`. The kernel build
//! pipeline assembles the same archive directly via
//! `crates/kernel/build.rs`; this CLI exists for one-off inspection.

use std::path::Path;
use std::process::ExitCode;

use boot_image::{cpio, manifest, targets};

fn main() -> ExitCode {
    let arch = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "x86_64".to_string());
    let t = match targets::target_from_arch(&arch) {
        Some(t) => t,
        None => {
            eprintln!("mkinitramfs: unknown arch '{arch}' (use x86_64, riscv64, or aarch64)");
            return ExitCode::FAILURE;
        }
    };

    let workspace = Path::new(".");
    let release = targets::release_dir(&t, workspace);

    let mut bins = Vec::new();
    for &(dest, bin_name) in manifest::BOOT_BINS {
        let src = release.join(bin_name);
        if src.exists() {
            let data = match std::fs::read(&src) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("mkinitramfs: failed to read {}: {e}", src.display());
                    return ExitCode::FAILURE;
                }
            };
            bins.push((dest, data));
        } else {
            eprintln!(
                "mkinitramfs: WARNING: {bin_name} not found at {} (run `just build {arch}`)",
                src.display()
            );
        }
    }

    let cpio_bytes = cpio::standard_initramfs(&bins);

    let cpio_path = workspace
        .join("target")
        .join("images")
        .join(t.out_dir)
        .join("initramfs.cpio");
    if let Some(parent) = cpio_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if write_if_changed(&cpio_path, &cpio_bytes).is_err() {
        eprintln!("mkinitramfs: failed to write {}", cpio_path.display());
        return ExitCode::FAILURE;
    }
    println!(
        "initramfs.cpio: {} bytes, {} files",
        cpio_bytes.len(),
        bins.len()
    );
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
