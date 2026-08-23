// x86 post-link build helper: compiles the kernel, extracts the kmain
// address, rebuilds the trampoline with it, and produces target/kernel.bin.
//
// The initramfs and MinixFS images are assembled by crates/kernel/build.rs;
// this tool only does the steps that must run AFTER the kernel ELF links
// (a build script cannot do post-link work).
//
// Usage: rustc tools/mkboot.rs --edition 2024 -o target/mkboot
//        target/mkboot [features] [stem]
//          features: comma-joined feature list (default: embed_initramfs,embed_minixfs)
//          stem:     output name — <stem>.bin + <stem>-trampoline.elf (default: kernel,
//                    which keeps the historical names kernel.bin + trampoline.elf)

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate the rust fork's stage1 rustc (`rust/build/<host-triple>/stage1/`),
/// built by `just bootstrap`, via the default toolchain's host triple.
fn find_stage1_rustc(workspace: &Path) -> PathBuf {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("rustc -vV failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let host = stdout
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .expect("host triple not found in rustc -vV")
        .to_string();
    let exe = if cfg!(windows) { "rustc.exe" } else { "rustc" };
    workspace
        .join("rust")
        .join("build")
        .join(host)
        .join("stage1")
        .join("bin")
        .join(exe)
}

fn main() {
    let workspace = Path::new(".");

    // Parse optional --features argument (e.g. "embed_initramfs,integration-tests")
    // and an optional output stem (the last arg when two or more are given).
    let mut extra_args: Vec<String> = std::env::args().skip(1).collect();
    let stem = if extra_args.len() >= 2 {
        extra_args.pop().expect("stem arg")
    } else {
        "kernel".to_string()
    };
    let features = if extra_args.is_empty() {
        "embed_initramfs,embed_minixfs,x86".to_string()
    } else {
        let mut all = extra_args.join(",");
        if !all.contains("embed_initramfs") {
            all = format!("embed_initramfs,{}", all);
        }
        if !all.contains("embed_minixfs") {
            all = format!("{},embed_minixfs", all);
        }
        // The kernel-boot bin is feature-gated like the riscv64/aarch64
        // ones, so plain workspace builds skip the no_main kernel image.
        if !all.split(',').any(|f| f == "x86") {
            all = format!("{},x86", all);
        }
        all
    };
    println!("Features: {}", features);

    // 1. Build the kernel with the rust fork's stage1 compiler (built by
    //    `just bootstrap`); the in-tree target provides core/alloc/std from
    //    its sysroot, and the linker script comes from .cargo/config.toml.
    let stage1_rustc = find_stage1_rustc(workspace);
    assert!(
        stage1_rustc.exists(),
        "stage1 rustc not found at {} — run `just bootstrap` first",
        stage1_rustc.display()
    );
    let status = Command::new("cargo")
        .env("RUSTC", &stage1_rustc)
        .args([
            "build",
            "-p",
            "kernel-boot",
            "--target",
            "x86_64-pc-minix",
            "--features",
            &features,
            "--release",
        ])
        .status()
        .expect("cargo build failed");
    assert!(status.success());

    // 2. Extract the kmain address from the linked ELF.
    let kernel_elf = workspace
        .join("target")
        .join("x86_64-pc-minix")
        .join("release")
        .join("kernel-boot");

    let output = Command::new("rust-nm")
        .args(["-n", &kernel_elf.to_string_lossy()])
        .output()
        .expect("rust-nm failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let kmain_addr = stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[2] == "kmain" {
                Some(parts[0].to_string())
            } else {
                None
            }
        })
        .next()
        .expect("kmain symbol not found");

    println!("kmain @ 0x{}", kmain_addr);

    // 3. Build the trampoline with the correct address.
    let trampoline_s = workspace
        .join("crates")
        .join("kernel-boot")
        .join("src")
        .join("trampoline.S");
    let trampoline_ld = workspace
        .join("crates")
        .join("kernel-boot")
        .join("trampoline.ld");
    let trampoline_obj = workspace.join("target").join("trampoline_.o");
    let trampoline_name = if stem == "kernel" {
        "trampoline.elf".to_string()
    } else {
        format!("{stem}-trampoline.elf")
    };
    let trampoline_elf = workspace.join("target").join(trampoline_name);

    let status = Command::new("clang")
        .args([
            "-c",
            "-target",
            "i386-pc-none-elf",
            "-m32",
            &format!("-DKMAIN=0x{kmain_addr}"),
            "-o",
            &trampoline_obj.to_string_lossy(),
            &trampoline_s.to_string_lossy(),
        ])
        .status()
        .expect("clang failed");
    assert!(status.success());

    let status = Command::new("rust-lld")
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
        .status()
        .expect("rust-lld failed");
    assert!(status.success());

    std::fs::remove_file(&trampoline_obj).ok();
    println!("Trampoline rebuilt with kmain @ 0x{}", kmain_addr);

    // 4. objcopy to raw binary.
    let kernel_bin = workspace.join("target").join(format!("{stem}.bin"));
    let status = Command::new("rust-objcopy")
        .args([
            "-O",
            "binary",
            &kernel_elf.to_string_lossy(),
            &kernel_bin.to_string_lossy(),
        ])
        .status()
        .expect("rust-objcopy failed");
    assert!(status.success());
    println!("{} written", kernel_bin.display());
}
