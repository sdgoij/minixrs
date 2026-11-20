//! The static gates `just check` runs before it needs a toolchain.
//!
//! Each is a scan of the tree: no compiler, no network, no QEMU. The rule in
//! each module is a function over text, so a test states a case without a
//! fixture tree; the walker around it only names the files.

pub mod c_headers;
pub mod justfile;
pub mod physmap;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Every gate, in the order the `check-static` recipe ran them, stopping at the
/// first failure. `just` aborted on the first failing `python` line, and a later
/// gate reading a tree the first one just rejected is not more useful.
pub fn run_static(root: &Path, out: &mut impl Write, err: &mut impl Write) -> io::Result<bool> {
    if !c_headers::run(root, false, out)? {
        return Ok(false);
    }
    if !physmap::run(root, out)? {
        return Ok(false);
    }
    justfile::run(root, out, err)
}

/// Read a source file as text, replacing invalid UTF-8. A gate reports a rule
/// violation; it does not die on a stray byte.
pub(crate) fn read_lossy(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path).map_err(|error| {
        io::Error::new(error.kind(), format!("{}: {error}", path.display()))
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Every file under `dir` whose extension is `extension`, sorted by path.
pub(crate) fn files_with_extension(dir: &Path, extension: &str) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    walk(dir, extension, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk(dir: &Path, extension: &str, found: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(&path, extension, found)?;
        } else if path.extension() == Some(std::ffi::OsStr::new(extension)) {
            found.push(path);
        }
    }
    Ok(())
}

/// `path` relative to `base` with `/` separators, so a message reads the same
/// on Windows as it does in CI.
pub(crate) fn relative_posix(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}
