//! Build script for kernel-boot.
//!
//! Assembles the ELF32 multiboot trampoline (trampoline.S → trampoline.elf)
//! which QEMU qboot loads to transition to 64-bit long mode before jumping
//! to the 64-bit kernel.
//!
//! If clang or rust-lld are not available (e.g., in rust-analyzer),
//! the build script silently skips rebuilding the trampoline.
//! A previously built trampoline.elf is used if present.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target");

    let trampoline_s = manifest_dir.join("src").join("trampoline.S");
    let trampoline_ld = manifest_dir.join("trampoline.ld");
    let trampoline_elf = target_dir.join("trampoline.elf");

    // Skip rebuild if tools aren't available (e.g. rust-analyzer).
    // The find_* functions return None gracefully.

    // Check tools are available before attempting to rebuild
    let clang = match find_clang() {
        Some(c) => c,
        None => {
            if trampoline_elf.exists() {
                return; // existing trampoline is fine
            }
            println!(
                "cargo::warning=kernel-boot build.rs: clang not found, skipping trampoline rebuild"
            );
            return;
        }
    };

    // Debug: print target_dir
    println!(
        "cargo::warning=kernel-boot build.rs: clang found, target_dir={}",
        target_dir.display()
    );

    let rust_lld = match find_rust_lld() {
        Some(l) => l,
        None => {
            if trampoline_elf.exists() {
                return;
            }
            println!(
                "cargo::warning=kernel-boot build.rs: rust-lld not found, skipping trampoline rebuild"
            );
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
            println!("cargo::warning=kernel-boot build.rs: clang assembly failed");
            return;
        }
    }

    // Link trampoline.o → trampoline.elf
    let status = Command::new(&rust_lld)
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
        Ok(s) if s.success() => {
            println!(
                "cargo::warning=Trampoline built: {}",
                trampoline_elf.display()
            );
            // Verify file exists
            if trampoline_elf.exists() {
                println!("cargo::warning=  FILE EXISTS: {}", trampoline_elf.display());
            } else {
                println!(
                    "cargo::warning=  FILE MISSING: {}",
                    trampoline_elf.display()
                );
            }
        }
        _ => {
            println!("cargo::warning=kernel-boot build.rs: lld linking failed");
        }
    }

    // Clean up object file
    std::fs::remove_file(&trampoline_obj).ok();
}

fn find_clang() -> Option<String> {
    for name in &["clang"] {
        let output = Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match output {
            Ok(s) if s.success() => return Some(name.to_string()),
            _ => continue,
        }
    }
    None
}

fn find_rust_lld() -> Option<String> {
    if Command::new("rust-lld")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        return Some("rust-lld".to_string());
    }

    if let Ok(home) = std::env::var("RUSTUP_HOME") {
        let candidates = [
            format!("{home}/toolchains/nightly-x86_64-pc-windows-msvc/bin/rust-lld.exe"),
            format!("{home}/toolchains/nightly-x86_64-pc-windows-msvc/bin/rust-lld"),
        ];
        for c in candidates {
            if std::path::Path::new(&c).exists() {
                return Some(c);
            }
        }
    }
    None
}
