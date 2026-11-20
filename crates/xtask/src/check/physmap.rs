//! Reject a physical address used as a pointer without the physmap conversion.
//!
//! `.notes/physmap.md` D4/P6. A frame's physical address is a number, not a
//! virtual address the kernel may dereference. `pte_to_phys`/`pte_frame` return
//! one, a masked PTE *is* one, and so is any `*_phys` or `*_pa` binding the
//! kernel keeps for one. The only sound way from one to a pointer is the
//! conversion: the HAL's `phys_to_virt` / `PHYS_TO_VIRT`, or
//! `kernel::pagetable`'s `frame_ptr` / `table_ptr`.
//!
//! Scope: `crates/*/src/**/*.rs` whose path names `kernel` (with a `src`
//! component) or an `arch-*` crate, which are the crates the kernel's own
//! address-space rules govern. A file's test tail (`#[cfg(test)] mod tests`) is
//! out of scope, because a host fixture has no physmap and the identity is the
//! only thing that can work there (see `table_ptr`).
//!
//! A deliberate exception states itself on the line, with its reason:
//!
//! ```text
//! let ptr = pa as *mut u8; // physmap-ok: <why the identity is the right view here>
//! ```
//!
//! Exit status is 1 with one line per offender, so `just check` fails on it.

use std::io::{self, Write};
use std::path::Path;
use std::sync::OnceLock;

use regex_lite::Regex;

use super::{files_with_extension, read_lossy, relative_posix};

const CONVERSIONS: [&str; 6] = [
    "phys_to_virt",
    "PHYS_TO_VIRT",
    "frame_ptr",
    "table_ptr",
    "phys_ptr",
    "walk_phys_ptr",
];

/// The point of the rule: this is the cast that is wrong on its own.
fn cast() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"as\s*\*(?:mut|const)").expect("the cast pattern is a constant")
    })
}

/// A quantity derived from a page-table entry: the masks each arch's HAL uses,
/// and the walk helpers.
fn tables() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(pte_to_phys|pte_frame|pte_phys|pte_frame_mask|PG_FRAME|PPN_MASK|PTE_ADDR_MASK|PG_BLOCK_ADDR_MASK)",
        )
        .expect("the table pattern is a constant")
    })
}

/// A binding whose name says it holds a physical address.
fn named() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(\w+_phys|\w+_pa|phys_base|\bpa\b|\bphys\b)\s*as\s*\*(?:mut|const)")
            .expect("the named pattern is a constant")
    })
}

/// Lines (1-based) that cast a physical address to a pointer, with the line's
/// text trimmed.
pub fn offenders_in(source: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let tail = test_tail(&lines);
    let mut found = Vec::new();
    for (index, line) in lines[..tail].iter().enumerate() {
        if line.contains("physmap-ok") || !cast().is_match(line) {
            continue;
        }
        if CONVERSIONS.iter().any(|conversion| line.contains(conversion)) {
            continue;
        }
        if tables().is_match(line) || named().is_match(line) {
            found.push((index + 1, line.trim().to_string()));
        }
    }
    found
}

/// Whether a path relative to the repository root is governed by the rule.
pub fn in_scope(path: &Path) -> bool {
    let parts: Vec<&str> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    let is_rust = path.extension().is_some_and(|extension| extension == "rs");
    if parts.contains(&"kernel") {
        return is_rust && parts.contains(&"src");
    }
    is_rust && parts.iter().any(|part| part.starts_with("arch-"))
}

/// Index of the file's `#[cfg(test)] mod tests`, or `lines.len()` if it has
/// none. The `mod tests` line may be up to three lines below the attribute.
pub fn test_tail(lines: &[&str]) -> usize {
    for (index, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with("#[cfg(test)]") {
            continue;
        }
        let end = (index + 4).min(lines.len());
        for line in &lines[index + 1..end] {
            if line.trim_start().starts_with("mod tests") {
                return index;
            }
        }
    }
    lines.len()
}

pub fn run(root: &Path, out: &mut impl Write) -> io::Result<bool> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root.join("crates"))? {
        let src = entry?.path().join("src");
        if src.is_dir() {
            paths.extend(files_with_extension(&src, "rs")?);
        }
    }
    paths.sort();

    let mut found = Vec::new();
    for path in paths {
        let rel = path.strip_prefix(root).unwrap_or(&path);
        if !in_scope(rel) {
            continue;
        }
        let source = read_lossy(&path)?;
        for (line, text) in offenders_in(&source) {
            found.push(format!("{}:{line}: {text}", relative_posix(root, &path)));
        }
    }

    if found.is_empty() {
        writeln!(
            out,
            "physmap: no physical address is used as a pointer (.notes/physmap.md P6)"
        )?;
        return Ok(true);
    }

    writeln!(
        out,
        "physmap: a physical address is dereferenced without the physmap conversion:"
    )?;
    for line in &found {
        writeln!(out, "  {line}")?;
    }
    writeln!(
        out,
        "  Fix: go through phys_to_virt / frame_ptr / table_ptr. If the identity really is the right view (host fixture, boot path, the identity arm of a pointer function), say so on the line with `physmap-ok: <reason>`."
    )?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_a_table_derived_cast() {
        let source = "let p = pte_to_phys(entry) as *mut u8;\n";
        assert_eq!(
            offenders_in(source),
            vec![(1, "let p = pte_to_phys(entry) as *mut u8;".to_string())]
        );
    }

    #[test]
    fn flags_a_named_physical_binding() {
        let source = "let next = frame_pa as *const Frame;\n";
        assert_eq!(offenders_in(source).len(), 1);
    }

    #[test]
    fn ignores_a_conversion() {
        let source = "let p = phys_to_virt(frame_pa) as *mut u8;\n";
        assert!(offenders_in(source).is_empty());
    }

    #[test]
    fn ignores_a_stated_exception() {
        let source = "let p = pa as *mut u8; // physmap-ok: host fixture has no physmap\n";
        assert!(offenders_in(source).is_empty());
    }

    #[test]
    fn ignores_the_test_tail() {
        let source = "\
let ok = phys_to_virt(pa);
#[cfg(test)]
mod tests {
    fn t() {
        let _ = pa as *mut u8;
    }
}
";
        assert!(offenders_in(source).is_empty());
    }

    #[test]
    fn ignores_a_cast_with_no_physical_provenance() {
        let source = "let p = value as *mut u8;\n";
        assert!(offenders_in(source).is_empty());
    }

    #[test]
    fn reports_the_line_of_the_second_offender() {
        let source = "let a = 1;\nlet b = pte_frame(e) as *const u8;\n";
        assert_eq!(offenders_in(source)[0].0, 2);
    }

    #[test]
    fn scope_is_the_kernel_and_the_arch_crates() {
        assert!(in_scope(Path::new("crates/kernel/src/pagetable.rs")));
        assert!(in_scope(Path::new("crates/arch-x86_64/src/vmparam.rs")));
        assert!(!in_scope(Path::new("crates/fs/src/lib.rs")));
        assert!(!in_scope(Path::new("crates/kernel/build.rs")));
        assert!(!in_scope(Path::new("crates/kernel/src/pagetable.txt")));
    }

    #[test]
    fn a_test_tail_is_where_the_attribute_meets_the_module() {
        let lines = vec!["a", "#[cfg(test)]", "", "mod tests {", "}"];
        assert_eq!(test_tail(&lines), 1);
        let lines = vec!["a", "#[cfg(test)]", "mod helpers {", "}"];
        assert_eq!(test_tail(&lines), 4);
    }
}
