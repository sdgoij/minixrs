//! The build wrappers that used to be `tools/build-*.py`.
//!
//! Each one is a sequence of compiles: clang for the C half, the fork's stage1
//! rustc for the link, and `cargo` only for the `minix-libc` rlib they link
//! against. The argv is the contract, so every command is echoed before it runs.

pub mod bash;
pub mod c_direct;
pub mod c_hello;
pub mod cdyn;
pub mod doom;
pub mod dynlibc;
pub mod dynlink;
pub mod libc_static;
pub mod libcxx;
pub mod llvm_host;
pub mod llvm_minix;
pub mod mesa;
pub mod mesa_tree;
pub mod rustc_minix;
pub mod std_hello;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::cc;

/// The `git` every source checkout runs with: the line endings git stores, not
/// the ones this host would write. A Windows checkout with autocrlf writes
/// `configure` with CRLF, and `/bin/sh^M` is not a shell (`.notes/c_build.md`,
/// trap 1).
const GIT_CONFIG: [&str; 4] = ["-c", "core.autocrlf=false", "-c", "core.eol=lf"];

pub(crate) fn git(args: &[String]) -> Command {
    let mut command = Command::new("git");
    command.args(GIT_CONFIG).args(args);
    command
}

pub(crate) fn run_git(args: &[String]) -> io::Result<bool> {
    cc::run(&mut git(args))
}

pub(crate) fn git_output(args: &[String]) -> io::Result<String> {
    let output = git(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn have_commit(src: &Path, commit: &str) -> io::Result<bool> {
    Ok(git(&[
        "-C".to_string(),
        src.to_string_lossy().into_owned(),
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        format!("{commit}^{{commit}}"),
    ])
    .output()?
    .status
    .success())
}

/// The newest `libminix_libc-*.rlib` under a target's `release/deps`.
///
/// Newest because the directory holds every rlib the previous builds left: the
/// link has to name the one the cargo build just wrote, not an older one with
/// the same crate name.
pub(crate) fn newest_libc_rlib(deps: &Path) -> io::Result<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    let entries = fs::read_dir(deps).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "reading {}: {error} (did `cargo build -p minix-libc` succeed for this target?)",
                deps.display()
            ),
        )
    })?;
    for entry in entries {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !(name.starts_with("libminix_libc-") && name.ends_with(".rlib")) {
            continue;
        }
        let modified = fs::metadata(&path)?.modified()?;
        if newest.as_ref().is_none_or(|(newest, _)| modified > *newest) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no libminix_libc-*.rlib under {}; did `cargo build -p minix-libc` succeed for this target?",
                deps.display()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_of_two_rlibs_wins() {
        let dir = std::env::temp_dir().join("minixrs-xtask-rlibs");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("libminix_libc-old.rlib");
        let new = dir.join("libminix_libc-new.rlib");
        fs::write(&old, b"").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&new, b"").unwrap();
        assert_eq!(newest_libc_rlib(&dir).unwrap(), new);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_rlib_says_which_build_failed() {
        let dir = std::env::temp_dir().join("minixrs-xtask-rlibs-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let error = newest_libc_rlib(&dir).unwrap_err();
        assert!(error.to_string().contains("cargo build -p minix-libc"));
        let _ = fs::remove_dir_all(&dir);
    }
}
