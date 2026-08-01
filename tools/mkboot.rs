// x86 post-link build helper: compiles the kernel, extracts the kmain
// address, rebuilds the trampoline with it, and produces target/kernel.bin.
//
// The initramfs and MinixFS images are assembled by crates/kernel/build.rs;
// this tool only does the steps that must run AFTER the kernel ELF links
// (a build script cannot do post-link work).
//
// Usage: rustc tools/mkboot.rs --edition 2024 -o target/mkboot
//        target/mkboot [features]     (default: embed_initramfs,embed_minixfs)

use std::path::Path;
use std::process::Command;

fn main() {
    let workspace = Path::new(".");

    // Parse optional --features argument (e.g. "embed_initramfs,integration-tests")
    let extra_features: Vec<String> = std::env::args().skip(1).collect();
    let features = if extra_features.is_empty() {
        "embed_initramfs,embed_minixfs".to_string()
    } else {
        let mut all = extra_features.join(",");
        if !all.contains("embed_initramfs") {
            all = format!("embed_initramfs,{}", all);
        }
        if !all.contains("embed_minixfs") {
            all = format!("{},embed_minixfs", all);
        }
        all
    };
    println!("Features: {}", features);

    // 1. Build the kernel (the linker script comes from .cargo/config.toml).
    let status = Command::new("rustup")
        .args([
            "run",
            "nightly",
            "cargo",
            "build",
            "-p",
            "kernel-boot",
            "--target",
            "x86_64-pc-minix.json",
            "-Zunstable-options",
            "-Zjson-target-spec",
            "-Zbuild-std=core,alloc",
            "-Zbuild-std-features=compiler-builtins-mem",
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
    let trampoline_elf = workspace.join("target").join("trampoline.elf");

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
    let kernel_bin = workspace.join("target").join("kernel.bin");
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
    println!("kernel.bin written");
}
