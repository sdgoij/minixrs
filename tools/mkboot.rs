// Build helper: extracts kmain address from kernel ELF and rebuilds the trampoline.
//
// Usage: rustc tools/build.rs --edition 2024 -o target/build_tramp.exe
//   or:  rustup run nightly cargo run --manifest-path tools/Cargo.toml
//
// Actually just run as a standalone script:
//   rustc tools/mkboot.rs --edition 2018 -o target/mkboot.exe && target/mkboot.exe

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

    // 1. Build initramfs first (kernel build needs initramfs.cpio via include_bytes!)
    println!("Building initramfs...");
    let mkinitramfs = workspace.join("target").join("mkinitramfs.exe");
    // Always rebuild mkinitramfs to pick up source changes.
    std::fs::remove_file(&mkinitramfs).ok();
    let status = Command::new("rustc")
        .args([
            workspace.join("tools/mkinitramfs.rs").to_str().unwrap(),
            "--edition",
            "2024",
            "-o",
            &mkinitramfs.to_string_lossy(),
        ])
        .status()
        .expect("rustc mkinitramfs failed");
    assert!(status.success());
    let status = Command::new(&mkinitramfs)
        .status()
        .expect("mkinitramfs failed");
    assert!(status.success());
    println!("initramfs built.");

    // 1b. Build the Minix FS image (needs binaries from initramfs)
    println!("Building Minix FS image...");
    let mkminixfs = workspace.join("target").join("mkminixfs.exe");
    std::fs::remove_file(&mkminixfs).ok();
    let status = Command::new("rustc")
        .args([
            workspace.join("tools/mkminixfs.rs").to_str().unwrap(),
            "--edition",
            "2021",
            "-o",
            &mkminixfs.to_string_lossy(),
        ])
        .status()
        .expect("rustc mkminixfs failed");
    assert!(status.success());
    let status = Command::new(&mkminixfs).status().expect("mkminixfs failed");
    assert!(status.success());
    println!("Minix FS image built.");

    // 2. Build the kernel with cargo
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
            "-Zjson-target-spec",
            "-Zbuild-std=core,alloc",
            "-Zbuild-std-features=compiler-builtins-mem",
            "--features",
            &features,
            "--release",
        ])
        .env("RUSTFLAGS", "-C link-arg=-Ttools/minix-raw.ld")
        .status()
        .expect("cargo build failed");
    assert!(status.success());

    // 2. Extract kmain address
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

    // 3. Build trampoline with correct address
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

    // 4. objcopy to raw binary
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
