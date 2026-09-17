//! Toolchain tools for the x86 post-link steps, from the build tree first and
//! `PATH` only as a fallback.
//!
//! `x.py` installs the tools under names that differ from the ones on `PATH` —
//! the stage1 sysroot ships LLD as `rust-lld` and copies `llvm-objcopy` in as
//! `rust-objcopy` — and a CI image has none of them on `PATH` at all. So both
//! consumers, `build.rs` (the fallback trampoline) and `tools/mkboot.rs` (the
//! real post-link steps), look in the tree the stage1 compiler lives in first.
//!
//! Shared by `mod toolchain_tools;` in the build script and
//! `#[path = "../crates/kernel-boot/toolchain_tools.rs"]` in `mkboot.rs`, which
//! is why the items are `pub`.
//!
//! rustc's own linker for the minix targets is resolved separately, by
//! `tools/lld.py`, which the Justfile and the smoke-test scripts use.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The host triple, which is what `x.py` names its build directory after.
pub fn host_triple() -> String {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("rustc -vV failed — a Rust toolchain has to be on PATH");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("no host triple in rustc -vV")
        .to_string()
}

/// An executable name for this platform.
pub fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Where a build of the fork leaves its tools, most specific first: the stage1
/// sysroot, a from-source LLVM, a from-source LLD, the downloaded CI LLVM.
pub fn tool_dirs(workspace: &Path, host: &str) -> Vec<PathBuf> {
    let build = workspace.join("rust").join("build").join(host);
    vec![
        build
            .join("stage1")
            .join("lib")
            .join("rustlib")
            .join(host)
            .join("bin"),
        build.join("llvm").join("bin"),
        build.join("lld").join("bin"),
        build.join("ci-llvm").join("bin"),
    ]
}

/// `names` in preference order, from `dirs` and then from `PATH`. A `PATH` hit
/// has to answer `--version`, so a stub or a broken install is not mistaken for
/// the tool.
pub fn find_tool(dirs: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    for dir in dirs {
        for name in names {
            let candidate = dir.join(exe(name));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for name in names {
        let runs = Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if runs {
            return Some(PathBuf::from(name));
        }
    }
    None
}
