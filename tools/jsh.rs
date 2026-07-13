//! Platform-agnostic shell and build tool for MINIX/Rust.
//!
//! Two modes:
//!   jsh build|run|debug [target]  — Build mode: runs build steps directly.
//!   jsh -c "<command>"            — Shell mode (used by `just`): runs a command
//!                                   with built-in detection + path resolution.
//!
//! Target defaults to "x86" for riscv64 use "riscv64".

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root_dir() -> PathBuf {
    let exe = env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn target_dir() -> PathBuf {
    root_dir().join("target")
}

fn trampoline() -> PathBuf {
    target_dir().join("trampoline.elf")
}

fn kernel_bin() -> PathBuf {
    target_dir().join("kernel.bin")
}

fn mkboot_src() -> PathBuf {
    root_dir().join("tools").join("mkboot.rs")
}

fn mkboot_exe() -> PathBuf {
    target_dir().join(exe_name("mkboot"))
}

/// Append .exe on Windows.
fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", name)
    } else {
        name.to_string()
    }
}

// ── Build steps ───────────────────────────────────────────────────────
fn build_mkboot() {
    println!("[jsh] compile {}", mkboot_src().display());
    let mut cmd = Command::new("rustc");
    cmd.arg(mkboot_src())
        .arg("--edition")
        .arg("2024")
        .arg("-o")
        .arg(&mkboot_exe());
    let status = cmd.status().expect("rustc failed");
    assert!(status.success(), "mkboot compilation failed");
}

fn run_mkboot() {
    println!("[jsh] run {}", mkboot_exe().display());
    let status = Command::new(&mkboot_exe())
        .current_dir(root_dir())
        .status()
        .expect("mkboot failed");
    assert!(status.success(), "mkboot failed");
}

fn run_qemu_x86(gdb: bool) {
    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args([
        "-nographic",
        "-m",
        "256M",
        "-no-reboot",
        "-kernel",
        &trampoline().to_string_lossy(),
        "-device",
        &format!(
            "loader,file={},addr=0x200000",
            kernel_bin().to_string_lossy()
        ),
    ]);
    if gdb {
        cmd.args(["-s", "-S"]);
        println!("[jsh] QEMU waiting for GDB on port 1234");
        println!(
            "[jsh]   lldb {}",
            target_dir()
                .join("x86_64-pc-minix/release/kernel-boot")
                .display()
        );
        println!("[jsh]   (lldb) gdb-remote 127.0.0.1:1234");
    }
    let status = cmd.status().expect("QEMU failed");
    std::process::exit(status.code().unwrap_or(1));
}

fn run_qemu_riscv() {
    let status = Command::new("qemu-system-riscv64")
        .args([
            "-machine",
            "virt",
            "-m",
            "256M",
            "-nographic",
            "-kernel",
            &target_dir()
                .join("riscv64gc-unknown-none-elf/release/kernel-boot-riscv64")
                .to_string_lossy(),
        ])
        .status()
        .expect("QEMU failed");
    std::process::exit(status.code().unwrap_or(1));
}

// ── Built-in commands ─────────────────────────────────────────────────
fn cmd_build(target: &str) {
    match target {
        "x86" => {
            build_mkboot();
            run_mkboot();
        }
        "riscv64" => {
            // Build initramfs tools then the kernel
            let status = Command::new("rustc")
                .args([
                    "tools/mkinitramfs.rs",
                    "--edition",
                    "2024",
                    "-o",
                    "target/mkinitramfs",
                ])
                .status()
                .expect("rustc failed");
            assert!(status.success());
            let status = Command::new(&target_dir().join(exe_name("mkinitramfs")))
                .arg("riscv64")
                .status()
                .expect("mkinitramfs failed");
            assert!(status.success());
            let status = Command::new("rustc")
                .args([
                    "tools/mkminixfs.rs",
                    "--edition",
                    "2021",
                    "-o",
                    "target/mkminixfs",
                ])
                .status()
                .expect("rustc failed");
            assert!(status.success());
            let _ = Command::new(&target_dir().join(exe_name("mkminixfs")))
                .arg("riscv64")
                .status();
            let status = Command::new("rustup")
                .args([
                    "run",
                    "nightly",
                    "cargo",
                    "build",
                    "-p",
                    "kernel-boot",
                    "--bin",
                    "kernel-boot-riscv64",
                    "--target",
                    "riscv64gc-unknown-none-elf",
                    "--features",
                    "embed_initramfs,embed_minixfs,riscv64",
                    "-Zbuild-std=core,alloc",
                    "-Zbuild-std-features=compiler-builtins-mem",
                    "--release",
                ])
                .status()
                .expect("cargo failed");
            assert!(status.success());
        }
        _ => {
            eprintln!("jsh: unknown target '{target}' (use x86 or riscv64)");
            std::process::exit(1);
        }
    }
}

fn cmd_run(target: &str) {
    cmd_build(target);
    match target {
        "x86" => run_qemu_x86(false),
        "riscv64" => run_qemu_riscv(),
        _ => std::process::exit(1),
    }
}

fn cmd_debug(target: &str) {
    cmd_build(target);
    run_qemu_x86(true);
}

fn cmd_test(target: &str) {
    match target {
        "riscv64" => {
            let status = Command::new("rustup")
                .args([
                    "run",
                    "nightly",
                    "cargo",
                    "build",
                    "-p",
                    "kernel-boot",
                    "--bin",
                    "kernel-boot-riscv64",
                    "--target",
                    "riscv64gc-unknown-none-elf",
                    "--features",
                    "riscv64,integration-tests",
                    "-Zbuild-std=core,alloc",
                    "-Zbuild-std-features=compiler-builtins-mem",
                    "--release",
                ])
                .status()
                .expect("cargo failed");
            assert!(status.success());
            run_qemu_riscv();
        }
        _ => {
            eprintln!("jsh: test target '{target}' not supported (use riscv64)");
            std::process::exit(1);
        }
    }
}

// ── Shell mode ────────────────────────────────────────────────────────
fn run_shell_command(cmd_str: &str) {
    let trimmed = cmd_str.trim();
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }

    match parts[0] {
        "build" => return cmd_build(parts.get(1).copied().unwrap_or("x86")),
        "run" => return cmd_run(parts.get(1).copied().unwrap_or("x86")),
        "debug" => return cmd_debug(parts.get(1).copied().unwrap_or("x86")),
        "test" | "test-qemu" => return cmd_test(parts.get(1).copied().unwrap_or("x86")),
        _ => {}
    }

    // Not a built-in — execute as an external command.
    let program = resolve(parts[0]);
    let args = &parts[1..];
    let mut cmd = Command::new(&program);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::inherit());
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());
    let status = cmd.status().expect("jsh: failed to execute command");
    std::process::exit(status.code().unwrap_or(1));
}

/// Resolve a program path: / → \, try .exe on Windows.
fn resolve(program: &str) -> String {
    #[cfg(windows)]
    {
        let converted = program.replace('/', "\\");
        let path = Path::new(&converted);
        if path.extension().is_none() && !is_builtin(program) {
            let with_exe = format!("{}.exe", converted);
            if Path::new(&with_exe).exists() {
                return with_exe;
            }
        }
        converted
    }
    #[cfg(not(windows))]
    {
        program.to_string()
    }
}

#[cfg(windows)]
fn is_builtin(program: &str) -> bool {
    matches!(
        program.to_lowercase().as_str(),
        "echo" | "cd" | "dir" | "copy" | "del" | "type"
    )
}

// ── Entry ─────────────────────────────────────────────────────────────
fn main() {
    let args: Vec<String> = env::args().collect();

    // jsh build|run|debug|test [target]  —  built-in commands
    if args.len() >= 2 {
        let cmd = args[1].as_str();
        let target = args.get(2).map(|s| s.as_str()).unwrap_or("x86");
        match cmd {
            "build" => return cmd_build(target),
            "run" => return cmd_run(target),
            "debug" => return cmd_debug(target),
            "test" | "test-qemu" => return cmd_test(target),
            _ => {} // fall through to shell mode
        }
    }

    // jsh -c "<command>"  —  shell mode
    if args.len() >= 3 && args[1] == "-c" {
        return run_shell_command(&args[2]);
    }

    eprintln!("jsh: usage: jsh build|run|debug|test [target]  |  jsh -c <command>");
    std::process::exit(1);
}
