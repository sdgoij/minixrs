//! Reject a Justfile recipe that builds `kernel-boot` without its image inputs.
//!
//! `crates/kernel/build.rs` reads *every* `BOOT_BINS` entry out of
//! `target/<triple>/release/` and panics if one is missing:
//!
//! ```text
//! kernel build.rs: boot binary .../release/coreutils missing (...)
//!     the uutils multicall is built from the coreutils submodule
//! ```
//!
//! So a recipe that builds `kernel-boot` has to depend on the recipes that
//! produce them: `userland-<arch>`, `coreutils-<arch>` and `dynlib-<arch>`. A
//! developer's `target/` usually has them all from an earlier run, so a missing
//! prerequisite is invisible until a *clean* runner builds that one recipe. That
//! is how CI's `gltriangle (aarch64)` job failed: `boot-image-aarch64` never
//! depended on `coreutils-aarch64` after aarch64 started embedding the multicall,
//! while every local gate had the binary already built by `build-aarch64`.
//!
//! Exit status is 1 with one line per offender, so `just check` fails on it.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;
use std::sync::OnceLock;

use regex_lite::Regex;

/// Cargo's `--target` value to the arch suffix the recipes use.
fn arch_for(target: &str) -> Option<&'static str> {
    match target {
        "x86_64-unknown-minix" => Some("x86"),
        "riscv64gc-unknown-minix" => Some("riscv64"),
        "aarch64-unknown-minix" => Some("aarch64"),
        _ => None,
    }
}

/// The producers of `manifest::BOOT_BINS`, named `<producer>-<arch>` in the
/// Justfile. `BOOT_BINS` itself is a list of (dest, binary) pairs and does not
/// name the recipe that builds each binary, so the mapping lives here.
const PREREQUISITES: [&str; 3] = ["userland", "coreutils", "dynlib"];

fn recipe_line() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^([A-Za-z0-9_-]+)(?:[ \t][^:]*)?:(.*)$")
            .expect("the recipe pattern is a constant")
    })
}

fn builds_kernel_boot() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"cargo build -p kernel-boot\b").expect("the build pattern is a constant")
    })
}

fn target() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"--target (\S+)").expect("the target pattern is a constant"))
}

struct Recipe<'a> {
    name: &'a str,
    number: usize,
    deps: Vec<&'a str>,
    body: Vec<&'a str>,
}

/// Every recipe in `text`, each with the line it starts on and its body lines.
///
/// A recipe starts on a non-blank line that is not indented; everything else
/// belongs to the recipe above it, blank lines included.
fn recipes(text: &str) -> Vec<Recipe<'_>> {
    let mut found = Vec::new();
    let mut current: Option<Recipe<'_>> = None;

    for (index, line) in text.lines().enumerate() {
        if !line.trim().is_empty() && !line.starts_with(char::is_whitespace) {
            if let Some(recipe) = current.take() {
                found.push(recipe);
            }
            let Some(captures) = recipe_line().captures(line) else {
                continue;
            };
            current = Some(Recipe {
                name: captures.get(1).expect("group 1 is the name").as_str(),
                number: index + 1,
                deps: captures
                    .get(2)
                    .map(|deps| deps.as_str().split_whitespace().collect())
                    .unwrap_or_default(),
                body: Vec::new(),
            });
        } else if let Some(recipe) = current.as_mut() {
            recipe.body.push(line);
        }
    }

    if let Some(recipe) = current {
        found.push(recipe);
    }
    found
}

/// One line per offender, in recipe order.
pub fn violations(text: &str) -> Vec<String> {
    let mut problems = Vec::new();

    for recipe in recipes(text) {
        let mut arches = BTreeSet::new();
        for line in &recipe.body {
            if !builds_kernel_boot().is_match(line) {
                continue;
            }
            for captures in target().captures_iter(line) {
                let target = captures.get(1).expect("group 1 is the target").as_str();
                match arch_for(target) {
                    Some(arch) => {
                        arches.insert(arch);
                    }
                    None => problems.push(format!(
                        "Justfile:{}: recipe `{}` builds kernel-boot for unknown target `{target}`; add it to this checker",
                        recipe.number, recipe.name
                    )),
                }
            }
        }

        for arch in arches {
            for producer in PREREQUISITES {
                let want = format!("{producer}-{arch}");
                if !recipe.deps.contains(&want.as_str()) {
                    problems.push(format!(
                        "Justfile:{}: recipe `{}` builds kernel-boot for {arch} but does not depend on `{want}` (build.rs embeds it)",
                        recipe.number, recipe.name
                    ));
                }
            }
        }
    }

    problems
}

pub fn run(root: &Path, out: &mut impl Write, err: &mut impl Write) -> io::Result<bool> {
    let path = root.join("Justfile");
    let text = super::read_lossy(&path)?;
    let problems = violations(&text);

    for problem in &problems {
        writeln!(err, "{problem}")?;
    }
    if !problems.is_empty() {
        return Ok(false);
    }

    writeln!(
        out,
        "justfile: every recipe that builds kernel-boot depends on its image inputs"
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KERNEL_BOOT: &str =
        "    RUSTC=$RUSTC cargo build -p kernel-boot --target x86_64-unknown-minix --release\n";

    #[test]
    fn a_recipe_with_every_producer_is_fine() {
        let text = format!(
            "boot-image-x86: userland-x86 coreutils-x86 dynlib-x86\n{KERNEL_BOOT}"
        );
        assert!(violations(&text).is_empty());
    }

    #[test]
    fn a_missing_producer_is_reported_with_its_line() {
        let text = format!("boot-image-x86: userland-x86 dynlib-x86\n{KERNEL_BOOT}");
        let problems = violations(&text);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("Justfile:1: recipe `boot-image-x86`"));
        assert!(problems[0].contains("coreutils-x86"));
    }

    #[test]
    fn a_missing_producer_is_never_inherited_from_the_recipe_above() {
        let text = format!(
            "build-x86: userland-x86 coreutils-x86 dynlib-x86\n{KERNEL_BOOT}\
             boot-image-x86: userland-x86\n{KERNEL_BOOT}"
        );
        let problems = violations(&text);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().all(|problem| problem.contains("boot-image-x86")));
    }

    #[test]
    fn an_unknown_target_is_named() {
        let text = "\
x: userland-x86 coreutils-x86 dynlib-x86
    cargo build -p kernel-boot --target mips-unknown-minix
";
        let problems = violations(text);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("unknown target `mips-unknown-minix`"));
    }

    #[test]
    fn a_cargo_check_is_not_a_build() {
        let text = "\
x:
    cargo check -p kernel-boot --target x86_64-unknown-minix --release
";
        assert!(violations(text).is_empty());
    }

    #[test]
    fn parameters_are_not_dependencies() {
        let text = "\
run-x86 memory: boot-image-x86
    qemu-system-x86_64
";
        let recipes = recipes(text);
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes[0].deps, vec!["boot-image-x86"]);
        assert_eq!(recipes[0].number, 1);
    }

    #[test]
    fn a_comment_line_closes_the_recipe_above_it() {
        let text = "\
boot-image-x86: userland-x86
    RUSTC=x cargo build -p kernel-boot --target x86_64-unknown-minix
# a comment
";
        assert_eq!(violations(text).len(), 2);
    }
}
