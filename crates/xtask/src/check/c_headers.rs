//! Cross-check `tools/c-include` against the C surface minix-libc exports.
//!
//! The libc is the source of truth for these headers, so the two sides have to
//! agree in both directions:
//!
//! * an export with no declaration gives a C caller the implicit-declaration
//!   path, which silently assumes `int`. That is how `fdopen` went missing from
//!   `stdio.h` and turned five call sites into "assigning to FILE * from int";
//! * a declaration with no export is a promise the link cannot keep.
//!
//! Exit status is 1 when either side has an unmatched entry.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex_lite::Regex;

use super::{files_with_extension, read_lossy, relative_posix};

/// The port's own C-ABI internals and the libc++ hooks: no C header declares
/// them, and no C library is expected to. Everything else the libc exports is
/// meant to be callable from C, and an export with no declaration gives the
/// caller the implicit `int` return type.
const INTERNALS: [&str; 5] = [
    "__cxa_atexit",
    "__cxa_finalize",
    "__cxa_thread_atexit",
    "__minix_set_environ",
    "minix_libc_tls_init",
];

/// An item the libc exports, preceded by the run of attributes and comments
/// that carries its `#[no_mangle]`.
///
/// The run is captured rather than the single attribute because the two are not
/// always adjacent: `#[unsafe(naked)]` sits between no_mangle and `pub` in
/// `c_setjmp.rs`, and `lib.rs` puts the `# Safety` doc comment between them.
///
/// The block-comment alternative is `/\*[^*]*\*+(?:[^/*][^*]*\*+)*/` rather
/// than `/\*(?:[^*]|\*(?!/))*\*/`: `regex-lite` has no lookahead, and the two
/// describe the same comments (up to the first `*/`).
fn item() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(concat!(
            r"((?:(?:#\[[^\]]*\]|//[^\n]*\n|/\*[^*]*\*+(?:[^/*][^*]*\*+)*/)\s*)*)",
            r#"pub\s+(?:unsafe\s+)?(?:extern\s+"C"\s+fn|static\s+mut)\s+"#,
            r"([A-Za-z_][A-Za-z0-9_]*)",
        ))
        .expect("the item pattern is a constant")
    })
}

/// A declaration is a prototype line: not a macro, not a typedef, and it names a
/// function. Only the identifier before the first `(` is taken.
fn decl() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^[A-Za-z_][A-Za-z0-9_ \t*]*?\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")
            .expect("the declaration pattern is a constant")
    })
}

/// An object declaration (`extern FILE *stdin;`) names the object after its
/// type.
fn decl_object() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^extern\b[^;=()]*?\b([A-Za-z_][A-Za-z0-9_]*)\s*;")
            .expect("the object pattern is a constant")
    })
}

/// The names `source` exports to C, in the order they appear.
pub fn exports_in(source: &str) -> Vec<String> {
    item()
        .captures_iter(source)
        .filter_map(|captures| {
            let attributes = captures.get(1)?.as_str();
            if !attributes.contains("no_mangle") {
                return None;
            }
            Some(captures.get(2)?.as_str().to_string())
        })
        .collect()
}

/// The names `text` puts in a C caller's scope, each with the header it came
/// from.
pub fn declarations_in(header: &str, text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut depth: i32 = 0;
    let mut pending = String::new();
    let mut macro_cont = false;

    for raw in text.lines() {
        let line = raw.trim();

        if macro_cont {
            // A continuation of a skipped `#define`: it is not a declaration,
            // even when it starts with an identifier (`__CMSG_ALIGN ...`) that
            // the matcher below would otherwise take for a function name.
            macro_cont = line.ends_with('\\');
            continue;
        }

        if depth > 0 {
            depth += count(line, '{') - count(line, '}');
            continue;
        }

        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("//")
            || line.starts_with("/*")
            || line.starts_with('*')
            || line.starts_with("typedef")
            || line.starts_with("extern \"C\"")
        {
            macro_cont = line.ends_with('\\');
            continue;
        }

        if line.contains('{') {
            // A body: a struct, a union, an enum, an inline function. Its
            // members and locals are not names a declaration puts in scope, and
            // they end in `;` like a prototype does.
            depth = count(line, '{') - count(line, '}');
            pending.clear();
            continue;
        }

        let Some(first) = line.chars().next() else {
            continue;
        };
        if pending.is_empty() && !(first.is_alphabetic() || first == '_') {
            // Not where a declaration starts: a macro's continuation, a stray
            // brace, the tail of a comment.
            continue;
        }

        pending = format!("{pending} {line}").trim().to_string();
        if !pending.ends_with(';') {
            if pending.chars().count() > 400 {
                pending.clear();
            }
            continue;
        }

        if let Some(name) = declaration_name(&pending) {
            found.push((name, header.to_string()));
        }
        pending.clear();
    }

    found
}

fn declaration_name(pending: &str) -> Option<String> {
    for pattern in [decl(), decl_object()] {
        if let Some(captures) = pattern.captures(pending) {
            return Some(captures.get(1)?.as_str().to_string());
        }
    }
    None
}

fn count(line: &str, needle: char) -> i32 {
    line.chars().filter(|&c| c == needle).count() as i32
}

/// `(undeclared, unimplemented)`, each sorted by name.
pub fn compare(
    exports: &BTreeMap<String, String>,
    declarations: &BTreeMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let undeclared = exports
        .keys()
        .filter(|name| !declarations.contains_key(*name) && !INTERNALS.contains(&name.as_str()))
        .cloned()
        .collect();
    let unimplemented = declarations
        .keys()
        .filter(|name| !exports.contains_key(*name))
        .cloned()
        .collect();
    (undeclared, unimplemented)
}

fn libc_exports(root: &Path) -> io::Result<BTreeMap<String, String>> {
    let libc = root.join("crates").join("minix-libc").join("src");
    let mut sources: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&libc)? {
        let path = entry?.path();
        if path.extension() == Some(OsStr::new("rs")) {
            sources.push(path);
        }
    }
    sources.sort();

    let mut exports = BTreeMap::new();
    for path in sources {
        let file = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        for symbol in exports_in(&read_lossy(&path)?) {
            exports.insert(symbol, file.clone());
        }
    }
    Ok(exports)
}

fn header_declarations(root: &Path) -> io::Result<BTreeMap<String, String>> {
    let include = root.join("tools").join("c-include");
    let mut declarations = BTreeMap::new();
    for path in files_with_extension(&include, "h")? {
        let from = relative_posix(&include, &path);
        for (name, header) in declarations_in(&from, &read_lossy(&path)?) {
            declarations.entry(name).or_insert(header);
        }
    }
    Ok(declarations)
}

pub fn run(root: &Path, verbose: bool, out: &mut impl Write) -> io::Result<bool> {
    let exports = libc_exports(root)?;
    let declarations = header_declarations(root)?;
    let (undeclared, unimplemented) = compare(&exports, &declarations);

    writeln!(
        out,
        "exports: {}   declarations: {}",
        exports.len(),
        declarations.len()
    )?;
    writeln!(
        out,
        "\nexported but not declared anywhere ({}):",
        undeclared.len()
    )?;
    for name in &undeclared {
        writeln!(out, "  {name:<22} ({})", exports[name.as_str()])?;
    }
    writeln!(out, "\ndeclared but not exported ({}):", unimplemented.len())?;
    for name in &unimplemented {
        writeln!(out, "  {name:<22} ({})", declarations[name.as_str()])?;
    }

    if verbose {
        writeln!(out, "\ndeclarations per header:")?;
        let mut per: BTreeMap<&str, usize> = BTreeMap::new();
        for header in declarations.values() {
            *per.entry(header.as_str()).or_insert(0) += 1;
        }
        for (header, count) in per {
            writeln!(out, "  {count:>4}  {header}")?;
        }
    }

    Ok(undeclared.is_empty() && unimplemented.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, file)| (name.to_string(), file.to_string()))
            .collect()
    }

    #[test]
    fn an_export_is_an_item_with_no_mangle() {
        let source = "#[unsafe(no_mangle)]\npub extern \"C\" fn foo() {}\n";
        assert_eq!(exports_in(source), vec!["foo"]);
    }

    #[test]
    fn a_doc_comment_between_the_attribute_and_the_item_keeps_no_mangle() {
        let source = "/// # Safety\n#[no_mangle]\npub unsafe extern \"C\" fn bar() {}\n";
        assert_eq!(exports_in(source), vec!["bar"]);
    }

    #[test]
    fn a_block_comment_between_them_keeps_no_mangle() {
        let source = "#[no_mangle]\n/* the doc */\npub extern \"C\" fn baz() {}\n";
        assert_eq!(exports_in(source), vec!["baz"]);
    }

    #[test]
    fn a_static_mut_export_counts() {
        let source = "#[unsafe(no_mangle)]\npub static mut errno: i32 = 0;\n";
        assert_eq!(exports_in(source), vec!["errno"]);
    }

    #[test]
    fn a_plain_pub_fn_is_not_an_export() {
        let source = "pub fn helper() {}\n";
        assert!(exports_in(source).is_empty());
    }

    /// The item pattern alone is not the rule: `no_mangle` is what puts the symbol
    /// in the C namespace.
    #[test]
    fn a_pub_extern_c_fn_without_no_mangle_is_not_an_export() {
        let source = "pub extern \"C\" fn helper() {}\n";
        assert!(exports_in(source).is_empty());
    }

    #[test]
    fn a_prototype_wrapped_over_lines_is_one_declaration() {
        let text = "\
int pthread_create(pthread_t *,
    const pthread_attr_t *,
    void *(*)(void *),
    void *);
";
        assert_eq!(
            declarations_in("pthread.h", text),
            vec![("pthread_create".to_string(), "pthread.h".to_string())]
        );
    }

    #[test]
    fn an_object_declaration_names_the_object() {
        let text = "extern FILE *stdin;\n";
        assert_eq!(
            declarations_in("stdio.h", text),
            vec![("stdin".to_string(), "stdio.h".to_string())]
        );
    }

    #[test]
    fn a_struct_body_is_not_a_repeated_declaration() {
        let text = "\
struct s {
    int member;
};
";
        assert!(declarations_in("s.h", text).is_empty());
    }

    #[test]
    fn a_macro_continuation_is_not_a_declaration() {
        let text = "\
#define A \\
    __CMSG_ALIGN(x)
";
        assert!(declarations_in("sys.h", text).is_empty());
    }

    #[test]
    fn a_typedef_is_not_a_declaration() {
        let text = "typedef unsigned int uint;\n";
        assert!(declarations_in("types.h", text).is_empty());
    }

    #[test]
    fn the_two_sides_are_compared_both_ways() {
        let exports = map(&[("declared", "lib.rs"), ("missing", "lib.rs")]);
        let declarations = map(&[("declared", "a.h"), ("orphan", "b.h")]);
        let (undeclared, unimplemented) = compare(&exports, &declarations);
        assert_eq!(undeclared, vec!["missing"]);
        assert_eq!(unimplemented, vec!["orphan"]);
    }

    #[test]
    fn an_internal_hook_needs_no_declaration() {
        let exports = map(&[("__cxa_atexit", "lib.rs")]);
        let declarations = map(&[]);
        let (undeclared, unimplemented) = compare(&exports, &declarations);
        assert!(undeclared.is_empty());
        assert!(unimplemented.is_empty());
    }
}
