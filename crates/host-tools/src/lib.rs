//! Toolchain tools for the x86 post-link steps, from the build tree first and
//! `PATH` only as a fallback.
//!
//! `x.py` installs the tools under names that differ from the ones on `PATH`:
//! the stage1 sysroot ships LLD as `rust-lld` and copies `llvm-objcopy` in as
//! `rust-objcopy`, and a CI image has none of them on `PATH` at all. So both
//! consumers, `crates/kernel-boot/build.rs` (the fallback trampoline) and the
//! xtask's `image` module (the real post-link steps), look in the tree the stage1
//! compiler lives in first.
//!
//! This is a crate rather than a `#[path]` include because rust-analyzer refuses
//! a `#[path]` that leaves the crate directory, so an include in either consumer
//! made the editor report the module as missing even though the build was fine.
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
        .expect("rustc -vV failed: a Rust toolchain has to be on PATH");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_first_directory_and_the_first_name_that_exist_win() {
        let dir = temp_dir("minixrs-host-tools-find");
        let first = dir.join("first");
        let second = dir.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let preferred = first.join(exe("second-name"));
        std::fs::write(&preferred, b"").unwrap();

        let found = find_tool(&[first, second], &["first-name", "second-name"]);
        assert_eq!(found, Some(preferred));

        assert_eq!(find_tool(&[], &["no-such-tool-minixrs"]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_build_tree_directories_are_tried_in_order() {
        let dirs = tool_dirs(Path::new("tree"), "host-triple");
        assert_eq!(dirs.len(), 4);
        assert!(dirs[0].ends_with("stage1/lib/rustlib/host-triple/bin"));
        assert!(dirs[1].ends_with("llvm/bin"));
        assert!(dirs[2].ends_with("lld/bin"));
        assert!(dirs[3].ends_with("ci-llvm/bin"));
    }

    #[test]
    fn the_host_triple_names_the_build_directory() {
        assert!(host_triple().contains('-'));
    }
}
