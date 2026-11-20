//! Path helpers shared by the gates and the build wrappers.

use std::path::Path;

/// `path` relative to `base` with `/` separators, so a message reads the same on
/// Windows as it does in CI.
pub fn relative_posix(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_reads_with_forward_slashes() {
        let base = Path::new("root");
        let path = base.join("target").join("x86").join("prog");
        assert_eq!(relative_posix(base, &path), "target/x86/prog");
    }
}
