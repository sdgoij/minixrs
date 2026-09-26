//! Where a `DT_NEEDED` name is looked for.
//!
//! The reference's `libexec/ld.elf_so/search.c`, reduced to the two rules the port
//! needs: a name with a slash in it is a path, opened as it stands, and only a name
//! without one is looked for under the search path. Both are pure name policy, so
//! they live here rather than in the loader's own module and are checked on the host
//! (`cargo test -p ldso`).

/// The directories a bare `DT_NEEDED` name is looked for under, in order.
///
/// Each entry carries its own trailing separator, so joining a name to one is a
/// concatenation ([`search_path_join`]) — which is what keeps `PATH_MAX` the only
/// bound in play.
pub const SEARCH_PATH: &[&[u8]] = &[b"/lib/", b"/usr/lib/"];

/// The longest path a `DT_NEEDED` name can resolve to.
///
/// The bound on the loader's path buffer, and on a loaded object's recorded name.
/// The scratch buffers are stack-sized because the loader has no allocator.
pub const PATH_MAX: usize = 128;

/// Whether a `DT_NEEDED` name is a path to open as it stands.
///
/// A slash is what makes it one — `search.c`'s `strchr(name, '/') != NULL`. The rule
/// is not a convenience: joining a path onto a directory produces a name that cannot
/// exist (`/lib/` beside `/lib/x.so`), so the object such a `DT_NEEDED` names would
/// never load at all.
pub fn names_a_path(name: &[u8]) -> bool {
    name.contains(&b'/')
}

/// The path to open for a bare `name` found under `dir`, or `None` when it does not
/// fit in `out`.
///
/// `dir` carries its own trailing separator (see [`SEARCH_PATH`]), so this
/// concatenates rather than joins.
pub fn search_path_join(dir: &[u8], name: &[u8], out: &mut [u8]) -> Option<usize> {
    let total = dir.len() + name.len();
    if total > out.len() {
        return None;
    }
    out[..dir.len()].copy_from_slice(dir);
    out[dir.len()..total].copy_from_slice(name);
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_makes_a_name_a_path() {
        assert!(!names_a_path(b"libdyn2.so"));
        assert!(names_a_path(b"/lib/libdyn2.so"));
        // Anywhere, not just at the front: a relative path is a path too.
        assert!(names_a_path(b"lib/libdyn2.so"));
        assert!(names_a_path(b"/lib/./libdyn2.so"));
        // The separator is the rule; a dot is not.
        assert!(!names_a_path(b"..so"));
    }

    #[test]
    fn a_bare_name_is_appended_to_the_directory() {
        let mut out = [0u8; PATH_MAX];
        let n = search_path_join(b"/lib/", b"libdyn2.so", &mut out).expect("fits");
        assert_eq!(&out[..n], b"/lib/libdyn2.so");

        // The directory's own trailing separator is part of it, so there is exactly
        // one: joining `/lib` and `/libx` the same way would produce `/liblibx`.
        let n = search_path_join(SEARCH_PATH[1], b"libc.so", &mut out).expect("fits");
        assert_eq!(&out[..n], b"/usr/lib/libc.so");
    }

    #[test]
    fn a_path_that_does_not_fit_is_refused_rather_than_truncated() {
        let mut out = [0u8; 16];
        // 9 + 8 = 17 > 16. A truncated path would be a *different*, plausible name.
        assert_eq!(
            search_path_join(b"/usr/lib/", b"libdyn2.so", &mut out),
            None
        );
        assert_eq!(out, [0u8; 16], "a refusal must not write a partial path");
        assert_eq!(search_path_join(b"/usr/lib/", b"x.so", &mut out), Some(13));
    }

    #[test]
    fn the_search_path_ends_its_entries_with_the_separator_join_expects() {
        // `search_path_join` relies on this, so a path entry added without it would
        // silently produce a name that cannot exist.
        for dir in SEARCH_PATH {
            assert!(dir.ends_with(b"/"), "{dir:?} has no trailing separator");
        }
    }
}
