//! Write the MFS root filesystem image to a raw disk file for QEMU.
//!
//! The kernel build mirrors the embedded MFS blob (the root filesystem)
//! to `target/images/<target>/minixfs.img`. This tool copies it to the
//! per-arch `target/images/<target>/disk.img`, which QEMU attaches as a
//! virtio-blk drive so MFS mounts its root from a real block device. The
//! disk image is kept per-target (like the kernel and userland binaries) so
//! building one arch never clobbers another arch's disk. The kernel and
//! server binaries still come from the embedded initramfs.
//!
//! Usage: rustc tools/mkfs.rs --edition 2021 -o target/mkfs
//!        && target/mkfs [x86_64|riscv64|aarch64]
//!
//! The source image must exist: build the kernel first (`just build-x86`
//! or equivalent), which runs `crates/kernel/build.rs`.

use std::fs;
use std::path::Path;

fn main() {
    let arch = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "x86_64".to_string());

    let out_dir = match arch.as_str() {
        "x86_64" => "x86_64-pc-minix",
        "riscv64" => "riscv64gc-unknown-minix",
        "aarch64" => "aarch64-unknown-minix",
        other => {
            eprintln!("mkfs: unknown arch {other:?} (expected x86_64|riscv64|aarch64)");
            std::process::exit(1);
        }
    };

    let src = Path::new("target")
        .join("images")
        .join(out_dir)
        .join("minixfs.img");
    let bytes = match fs::read(&src) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "mkfs: reading {} failed: {e} — build the kernel first (`just build {}`)",
                src.display(),
                arch
            );
            std::process::exit(1);
        }
    };

    let dst = Path::new("target")
        .join("images")
        .join(out_dir)
        .join("disk.img");
    if let Err(e) = fs::write(&dst, &bytes) {
        eprintln!("mkfs: writing {} failed: {e}", dst.display());
        std::process::exit(1);
    }
    println!("mkfs: wrote {} ({} bytes)", dst.display(), bytes.len());
}
