//! Where the fork's stage1 compiler and an LLD are, for the wrappers that invoke
//! rustc directly.
//!
//! `tools/stage1.py` and `tools/lld.py` keep their own copies for the Justfile's
//! parse-time variables and the `CARGO_TARGET_*_LINKER` export (see
//! `.notes/xtask_migration.md` §3.2), so the candidate order here has to stay
//! the same as theirs: the overrides, then the host's own build tree, then PATH.
//! Nothing else is considered, because another host's compiler and linker are
//! ones this machine cannot run.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `name` as an executable file name on this platform.
pub fn exe(name: &str) -> OsString {
    if cfg!(windows) {
        OsString::from(format!("{name}.exe"))
    } else {
        OsString::from(name)
    }
}

/// The build host triple, which is the name of x.py's build directory.
///
/// `None` when `rustc` cannot be run: the lookup then trusts `build/host`.
pub fn host_triple() -> Option<String> {
    let output = Command::new("rustc").arg("-vV").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find_map(|line| line.strip_prefix("host: ").map(|host| host.trim().to_string()))
}

/// x.py's build directory for this host, then x.py's `build/host` symlink.
fn build_dirs(root: &Path, host: Option<&str>) -> Vec<PathBuf> {
    let build = root.join("rust").join("build");
    let mut dirs = Vec::new();
    for name in host.into_iter().chain(std::iter::once("host")) {
        let path = build.join(name);
        if path.is_dir() && !dirs.contains(&path) {
            dirs.push(path);
        }
    }
    dirs
}

/// The stage1 `rustc` for this host, or `None` when there is none.
pub fn find_stage1_rustc(root: &Path) -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os("MINIXRS_STAGE1_RUSTC")
        && !override_path.is_empty()
    {
        return Some(PathBuf::from(override_path));
    }

    let build = root.join("rust").join("build");
    let host = host_triple();
    if host.is_none() && !build.is_dir() {
        return None;
    }
    let dir = build.join(host.as_deref().unwrap_or("host"));
    let rustc = dir.join("stage1").join("bin").join(exe("rustc"));
    rustc.is_file().then_some(rustc)
}

/// The lld to link minix binaries with, or `None` when there is none.
pub fn find_lld(root: &Path) -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os("MINIXRS_LLD")
        && !override_path.is_empty()
    {
        let path = PathBuf::from(&override_path);
        if !path.is_file() {
            // Not a reason to silently link with something else.
            eprintln!(
                "error: MINIXRS_LLD is {}, which is not a file",
                path.display()
            );
            return None;
        }
        return Some(path);
    }

    let host = host_triple();
    for build in build_dirs(root, host.as_deref()) {
        // The toolchain's own LLD, installed in the sysroot of the host it was
        // built on.
        let sysroot_bin = build
            .join("stage1")
            .join("lib")
            .join("rustlib")
            .join(host.as_deref().unwrap_or_else(|| {
                build
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default()
            }))
            .join("bin");
        let rust_lld = sysroot_bin.join(exe("rust-lld"));
        if rust_lld.is_file() {
            return Some(rust_lld);
        }

        // Otherwise the LLD an LLVM build left behind, under the bare `lld` name
        // rustc asks for: from-source first, then the CI artifacts.
        for bin_dir in [build.join("lld").join("bin"), build.join("ci-llvm").join("bin")] {
            let lld = bin_dir.join(exe("lld"));
            if lld.is_file() {
                return Some(lld);
            }
        }
    }

    find_on_path("lld")
}

/// `name` on PATH, the way `shutil.which` does it.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let file = exe(name);
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// The cargo that started this tool where there is one, so a wrapper re-enters
/// the same cargo rather than whatever `cargo` PATH resolves to.
pub fn cargo() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

/// The error every wrapper returns when there is no toolchain yet.
pub fn missing_stage1() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "the rust fork's stage1 compiler was not found; run `just bootstrap`, or set MINIXRS_STAGE1_RUSTC",
    )
}

/// The error every wrapper returns when there is no linker yet.
pub fn missing_lld() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "no lld to link with; run `just bootstrap` so its LLD lands in the sysroot, or set MINIXRS_LLD",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_tree_has_no_stage1() {
        let dir = std::env::temp_dir().join("minixrs-xtask-no-toolchain");
        // `find_lld` has a PATH fallback, so it is not asserted here: an
        // installed LLVM makes it `Some` on exactly the machines that can build.
        assert!(find_stage1_rustc(&dir).is_none());
    }

    #[test]
    fn an_override_wins_over_the_build_tree() {
        let dir = std::env::temp_dir().join("minixrs-xtask-override");
        std::fs::create_dir_all(dir.join("rust/build")).unwrap();
        let fake_rustc = dir.join("my-rustc");
        let fake_lld = dir.join("my-lld");
        std::fs::write(&fake_rustc, b"").unwrap();
        std::fs::write(&fake_lld, b"").unwrap();

        // SAFETY: the suite runs single-threaded (`RUST_TEST_THREADS = 1`), and
        // each variable is set, read and removed in one test.
        unsafe {
            std::env::set_var("MINIXRS_STAGE1_RUSTC", &fake_rustc);
            std::env::set_var("MINIXRS_LLD", &fake_lld);
        }
        let found_rustc = find_stage1_rustc(&dir);
        let found_lld = find_lld(&dir);
        unsafe {
            std::env::remove_var("MINIXRS_STAGE1_RUSTC");
            std::env::remove_var("MINIXRS_LLD");
        }

        assert_eq!(found_rustc, Some(fake_rustc));
        assert_eq!(found_lld, Some(fake_lld));
    }
}
