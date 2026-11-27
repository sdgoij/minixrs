//! The stage1 release's LLVM/LLD build asset, and whether it is complete.
//!
//! `toolchain-release.yml` publishes the installed LLVM and LLD trees (and their
//! stamps) as one asset, and restores it before `just bootstrap`. `x.py` skips a
//! build on the stamp alone, so a partial tree would be restored and used rather
//! than rebuilt. This is the gate that decides whether the tree is published.
//!
//! It lives here rather than inline in the workflow so `cargo test` exercises
//! it. The check is structural on purpose: an earlier inline version ran
//! `lld --version`, but `lld` is the generic multi-driver, so it printed its
//! usage and exited 1 and a complete tree was reported incomplete. There is no
//! command to get wrong here; a build that reached this point produced the
//! binaries, and the stamps record that the build finished.

use std::io;
use std::path::{Path, PathBuf};

use crate::toolchain;

const LLVM_STAMP: &str = "llvm/.llvm-stamp";
const LLVM_LIB: &str = "llvm/lib";
const LLVM_INCLUDE: &str = "llvm/include";
const LLD_STAMP: &str = "lld/.lld-stamp";

pub fn run(root: &Path, matches: &clap::ArgMatches) -> io::Result<bool> {
    match matches.subcommand() {
        Some(("check", matches)) => {
            let value = matches
                .get_one::<String>("root")
                .expect("clap defaults the root");
            let build_root = if Path::new(value).is_absolute() {
                PathBuf::from(value)
            } else {
                root.join(value.as_str())
            };
            check(&build_root)?;
            println!(
                "ok: {} is a complete LLVM/LLD build tree",
                build_root.display()
            );
            Ok(true)
        }
        _ => unreachable!("clap requires a stage1-build subcommand"),
    }
}

#[derive(Clone, Copy)]
enum Kind {
    File,
    Dir,
    Executable,
}

impl Kind {
    fn satisfied(self, path: &Path) -> bool {
        match self {
            Kind::File => path.is_file(),
            Kind::Dir => path.is_dir(),
            Kind::Executable => is_executable(path),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "directory",
            Kind::Executable => "executable",
        }
    }
}

/// What a complete tree must contain, relative to `rust/build/<host>`.
fn required(build_root: &Path) -> Vec<(PathBuf, Kind)> {
    let llvm = build_root.join("llvm");
    let lld = build_root.join("lld");
    vec![
        (build_root.join(LLVM_STAMP), Kind::File),
        (build_root.join(LLVM_LIB), Kind::Dir),
        (build_root.join(LLVM_INCLUDE), Kind::Dir),
        (
            llvm.join("bin").join(toolchain::exe("llvm-config")),
            Kind::Executable,
        ),
        (build_root.join(LLD_STAMP), Kind::File),
        (lld.join("bin").join(toolchain::exe("lld")), Kind::Executable),
    ]
}

/// The required paths that are missing, each named with what was expected.
pub(crate) fn problems(build_root: &Path) -> Vec<String> {
    required(build_root)
        .into_iter()
        .filter(|(path, kind)| !kind.satisfied(path))
        .map(|(path, kind)| format!("missing {}: {}", kind.name(), path.display()))
        .collect()
}

pub(crate) fn check(build_root: &Path) -> io::Result<()> {
    let missing = problems(build_root);
    if missing.is_empty() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "{} is not a complete LLVM/LLD build tree:\n  {}",
            build_root.display(),
            missing.join("\n  ")
        ),
    ))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_executable(path: &Path) {
        fs::write(path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    fn complete_tree(dir: &Path) {
        fs::create_dir_all(dir.join(LLVM_LIB)).unwrap();
        fs::create_dir_all(dir.join(LLVM_INCLUDE)).unwrap();
        fs::create_dir_all(dir.join("llvm/bin")).unwrap();
        fs::create_dir_all(dir.join("lld/bin")).unwrap();
        fs::write(dir.join(LLVM_STAMP), b"").unwrap();
        fs::write(dir.join(LLD_STAMP), b"").unwrap();
        write_executable(&dir.join("llvm/bin").join(toolchain::exe("llvm-config")));
        write_executable(&dir.join("lld/bin").join(toolchain::exe("lld")));
    }

    #[test]
    fn a_complete_tree_has_no_problems() {
        let dir = temp_dir("minixrs-xtask-stage1-build-complete");
        complete_tree(&dir);
        assert!(problems(&dir).is_empty(), "{:?}", problems(&dir));
        assert!(check(&dir).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_missing_path_is_named() {
        let dir = temp_dir("minixrs-xtask-stage1-build-missing");
        for (relative, is_dir) in [
            (LLVM_STAMP, false),
            (LLVM_LIB, true),
            (LLVM_INCLUDE, true),
            (LLD_STAMP, false),
        ] {
            complete_tree(&dir);
            let path = dir.join(relative);
            if is_dir {
                fs::remove_dir_all(&path).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
            let missing = problems(&dir);
            assert_eq!(missing.len(), 1, "{relative}: {missing:?}");
            assert!(missing[0].contains(relative), "{missing:?}");
            let _ = fs::remove_dir_all(&dir);
        }

        for relative in [
            Path::new("llvm")
                .join("bin")
                .join(toolchain::exe("llvm-config")),
            Path::new("lld").join("bin").join(toolchain::exe("lld")),
        ] {
            complete_tree(&dir);
            fs::remove_file(dir.join(&relative)).unwrap();
            let missing = problems(&dir);
            assert_eq!(missing.len(), 1, "{}: {missing:?}", relative.display());
            assert!(
                missing[0].contains(&relative.display().to_string()),
                "{missing:?}"
            );
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_without_an_execute_bit_is_not_complete() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("minixrs-xtask-stage1-build-no-exec-bit");
        complete_tree(&dir);
        let lld = dir.join("lld/bin").join(toolchain::exe("lld"));
        let mut permissions = fs::metadata(&lld).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&lld, permissions).unwrap();
        let missing = problems(&dir);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("executable"), "{missing:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
