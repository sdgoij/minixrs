//! Build script for kernel-boot.
//!
//! Assembles the ELF32 multiboot trampoline (trampoline.S → trampoline.elf)
//! which QEMU qboot loads to transition to 64-bit long mode before jumping
//! to the 64-bit kernel.
//!
//! This one has no kernel address compiled in and no kernel image in it: it
//! only keeps a plain `cargo build` of the x86 target bootable with a
//! separately loaded kernel.bin. `tools/mkboot.rs` rebuilds the same
//! trampoline with kmain and the kernel embedded, and that is what the
//! recipes boot.
//!
//! If clang or lld are not available (e.g., in rust-analyzer), the build
//! script silently skips rebuilding the trampoline. A previously built
//! trampoline.elf is used if present.

use std::path::PathBuf;
use std::process::Command;

mod toolchain_tools;

use toolchain_tools::{find_tool, host_triple, tool_dirs};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest_dir.parent().unwrap().parent().unwrap();
    let target_dir = workspace.join("target");

    let trampoline_s = manifest_dir.join("src").join("trampoline.S");
    let trampoline_ld = manifest_dir.join("trampoline.ld");
    let trampoline_elf = target_dir.join("trampoline.elf");

    // Cargo hands build scripts the host triple, which is also what x.py names
    // its build directory after.
    let host = std::env::var("HOST").unwrap_or_else(|_| host_triple());
    let dirs = tool_dirs(workspace, &host);

    // Skip rebuild if tools aren't available (e.g. rust-analyzer).
    let clang = match find_tool(&dirs, &["clang"]) {
        Some(clang) => clang,
        None => {
            if trampoline_elf.exists() {
                return;
            }
            eprintln!("kernel-boot: clang not found, cannot build trampoline");
            return;
        }
    };

    let lld = match find_tool(&dirs, &["rust-lld", "lld"]) {
        Some(lld) => lld,
        None => {
            if trampoline_elf.exists() {
                return;
            }
            eprintln!("kernel-boot: lld not found, cannot link trampoline");
            return;
        }
    };

    println!("cargo::rerun-if-changed={}", trampoline_s.display());
    println!("cargo::rerun-if-changed={}", trampoline_ld.display());

    std::fs::create_dir_all(&target_dir).ok();

    // Assemble trampoline.S → trampoline.o
    let trampoline_obj = target_dir.join("trampoline.o");
    let status = Command::new(&clang)
        .args([
            "-c",
            "-target",
            "i386-pc-none-elf",
            "-m32",
            "-o",
            &trampoline_obj.to_string_lossy(),
            &trampoline_s.to_string_lossy(),
        ])
        .status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("kernel-boot: clang assembly failed");
            return;
        }
    }

    // Link trampoline.o → trampoline.elf
    let status = Command::new(&lld)
        .args([
            "-flavor",
            "gnu",
            "-m",
            "elf_i386",
            "-T",
            &trampoline_ld.to_string_lossy(),
            "-o",
            &trampoline_elf.to_string_lossy(),
            &trampoline_obj.to_string_lossy(),
        ])
        .status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("kernel-boot: lld linking failed");
        }
    }

    // Clean up object file
    std::fs::remove_file(&trampoline_obj).ok();
}
