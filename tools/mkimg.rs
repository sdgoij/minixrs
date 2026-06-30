// Disk image builder for the Minix Rust port.
//
// Creates a bootable raw disk image (minix.img) containing:
//   - Sector 0: MBR bootloader (stage 1, 512 bytes)
//   - Sectors 1..40: Stage 2 bootloader (reads kernel, 32→64 transition)
//   - Sector 4096+: Kernel binary (loaded at 0x200000)
//
// Run: cargo run --manifest-path tools/Cargo.toml
// Or:  rustc tools/mkimg.rs -o target/mkimg.exe && target/mkimg.exe

use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

fn main() {
    let target_dir = Path::new("target");
    let kernel_elf = target_dir
        .join("x86_64-pc-minix")
        .join("release")
        .join("kernel-boot");
    let kernel_bin = target_dir.join("kernel.bin");
    let output = target_dir.join("minix.img");
    let mbr_bin = target_dir.join("mbr.bin");
    let stage2_bin = target_dir.join("stage2.bin");
    let stage2_elf = target_dir.join("stage2.elf");
    let tools_dir = Path::new("tools");

    // Find tools
    let clang = find_clang();
    let rust_lld = find_rust_lld();
    let rust_nm = find_rust_nm();

    // ── Build stage 1 (MBR) ──
    let mbr_src = tools_dir.join("mbr.S");
    let mbr_obj = target_dir.join("mbr.o");
    run(
        &clang,
        &[
            "-c",
            "-target",
            "i386-pc-none-elf",
            "-m32",
            "-o",
            &mbr_obj.to_string_lossy(),
            &mbr_src.to_string_lossy(),
        ],
    );
    run(
        &rust_lld,
        &[
            "-flavor",
            "gnu",
            "-m",
            "elf_i386",
            "-Ttext=0x7C00",
            "--image-base=0",
            "--oformat=binary",
            "-o",
            &mbr_bin.to_string_lossy(),
            &mbr_obj.to_string_lossy(),
        ],
    );
    // Trim/pad to exactly 512 bytes
    let mut mbr = read_binary(&mbr_bin);
    if mbr.len() > 512 {
        mbr.truncate(512);
    }
    mbr.resize(512, 0);
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
    }
    fs::write(&mbr_bin, &mbr).unwrap();
    fs::remove_file(&mbr_obj).ok();
    println!("mbr.bin: 512 bytes");

    // ── Build stage 2 ──
    let stage2_src = tools_dir.join("stage2.S");
    let stage2_obj = target_dir.join("stage2.o");
    run(
        &clang,
        &[
            "-c",
            "-target",
            "i386-pc-none-elf",
            "-m32",
            "-o",
            &stage2_obj.to_string_lossy(),
            &stage2_src.to_string_lossy(),
        ],
    );
    // Link to ELF first (for symbol extraction)
    run(
        &rust_lld,
        &[
            "-flavor",
            "gnu",
            "-m",
            "elf_i386",
            "-Ttext=0x1000",
            "--image-base=0",
            "-o",
            &stage2_elf.to_string_lossy(),
            &stage2_obj.to_string_lossy(),
        ],
    );
    run(
        &rust_lld,
        &[
            "-flavor",
            "gnu",
            "-m",
            "elf_i386",
            "-Ttext=0x1000",
            "--image-base=0",
            "--oformat=binary",
            "-o",
            &stage2_bin.to_string_lossy(),
            &stage2_obj.to_string_lossy(),
        ],
    );
    fs::remove_file(&stage2_obj).ok();
    println!(
        "stage2.bin: {} bytes",
        fs::metadata(&stage2_bin).unwrap().len()
    );

    // ── Read kernel binary ──
    let kernel = fs::read(&kernel_bin).unwrap_or_else(|_| {
        panic!("kernel.bin not found — build kernel first: just build");
    });
    let kernel_sectors = (kernel.len() + 511) / 512;
    println!(
        "kernel.bin: {} bytes ({} sectors)",
        kernel.len(),
        kernel_sectors
    );

    // ── Extract kmain address from kernel ELF ──
    let kmain_addr = extract_kmain(&rust_nm, &kernel_elf);
    println!("kmain @ 0x{kmain_addr:x}");

    // ── Find patch locations in stage2 binary ──
    let ker_entry_off = find_sym_offset(&rust_nm, &stage2_elf, "ker_entry", 0x1000);
    let ker_sectors_off = find_sym_offset(&rust_nm, &stage2_elf, "ker_sectors", 0x1000);
    // DAP sector count word is at dap_kernel + 2
    let dap_sectors_off = find_sym_offset(&rust_nm, &stage2_elf, "dap_kernel", 0x1000) + 2;
    println!("  ker_entry @ binary offset 0x{ker_entry_off:x}");
    println!("  ker_sectors @ binary offset 0x{ker_sectors_off:x}");

    // ── Patch stage2 with kernel values ──
    let mut stage2 = fs::read(&stage2_bin).unwrap();
    // ker_entry: 8-byte kmain address
    stage2[ker_entry_off..ker_entry_off + 8].copy_from_slice(&(kmain_addr as u64).to_le_bytes());
    // ker_sectors: 8-byte sector count
    stage2[ker_sectors_off..ker_sectors_off + 8]
        .copy_from_slice(&(kernel_sectors as u64).to_le_bytes());
    // DAP sector count: 2-byte word
    stage2[dap_sectors_off..dap_sectors_off + 2]
        .copy_from_slice(&(kernel_sectors as u16).to_le_bytes());
    fs::write(&stage2_bin, &stage2).unwrap();

    // ── Create disk image ──
    let mut img = File::create(&output).unwrap();
    img.set_len(8 * 1024 * 1024).ok();

    // Sector 0: MBR
    img.write_all(&mbr).unwrap();

    // Sectors 1..40: stage2
    img.write_all(&stage2).unwrap();

    // Seek to 0x200000 and write kernel
    img.seek(SeekFrom::Start(0x200000)).unwrap();
    img.write_all(&kernel).unwrap();

    println!("minix.img: 8MB, bootable via -drive format=raw,file=minix.img");
    println!("To run: qemu-system-x86_64 -nographic -drive format=raw,file=target/minix.img");
}

fn run(prog: &str, args: &[&str]) {
    let status = Command::new(prog)
        .args(args)
        .status()
        .unwrap_or_else(|_| panic!("failed to execute command"));
    assert!(status.success(), "command failed");
}

fn read_binary(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_default()
}

fn find_clang() -> String {
    for name in &["clang", "clang-cl"] {
        if Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return name.to_string();
        }
    }
    panic!("clang not found in PATH");
}

fn find_rust_lld() -> String {
    if Command::new("rust-lld")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        return "rust-lld".to_string();
    }
    // Fallback: look in rustup toolchain
    if let Ok(home) = std::env::var("RUSTUP_HOME") {
        let candidates = [
            format!("{home}/toolchains/nightly-x86_64-pc-windows-msvc/bin/rust-lld.exe"),
            format!("{home}/toolchains/nightly-x86_64-pc-windows-msvc/bin/rust-lld"),
        ];
        for c in &candidates {
            if std::path::Path::new(c).exists() {
                return c.clone();
            }
        }
    }
    panic!("rust-lld not found");
}

fn find_rust_nm() -> String {
    if Command::new("rust-nm")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        return "rust-nm".to_string();
    }
    // Fallback
    if let Ok(home) = std::env::var("RUSTUP_HOME") {
        let candidates = [
            format!("{home}/toolchains/nightly-x86_64-pc-windows-msvc/bin/rust-nm.exe"),
            format!("{home}/toolchains/nightly-x86_64-pc-windows-msvc/bin/rust-nm"),
        ];
        for c in &candidates {
            if std::path::Path::new(c).exists() {
                return c.clone();
            }
        }
    }
    panic!("rust-nm not found");
}

/// Extract the kmain symbol address from the kernel ELF.
fn extract_kmain(rust_nm: &str, kernel_elf: &Path) -> u64 {
    let output = Command::new(rust_nm)
        .args(["-n", &kernel_elf.to_string_lossy()])
        .output()
        .unwrap_or_else(|_| panic!("failed to run rust-nm"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // Format: "00000000002002b0 T kmain"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[2] == "kmain" {
            return u64::from_str_radix(parts[0], 16).unwrap();
        }
    }
    panic!("kmain symbol not found in {}", kernel_elf.display());
}

/// Find the file offset of a symbol in a flat binary.
/// `symbol_vma` = symbol address in the ELF
/// `base_vma` = load address (0x1000 for stage2)
/// Returns the offset within the binary file.
fn find_sym_offset(rust_nm: &str, elf: &Path, sym: &str, base_vma: u64) -> usize {
    let output = Command::new(rust_nm)
        .args(["-n", &elf.to_string_lossy()])
        .output()
        .unwrap_or_else(|_| panic!("failed to run rust-nm"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[2] == sym {
            let addr = u64::from_str_radix(parts[0], 16).unwrap();
            return (addr - base_vma) as usize;
        }
    }
    panic!("symbol `{sym}` not found in {}", elf.display());
}
