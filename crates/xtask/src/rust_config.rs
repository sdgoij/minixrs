//! Regenerate the rust fork's `rust/config.toml` for the given minix arch.
//!
//! The fork's `config.toml` is gitignored, so `just bootstrap` calls this to
//! (re)create it deterministically before running x.py. The minix targets are in
//! `STAGE0_MISSING_TARGETS`, so bootstrap builds a stage1 compiler from source,
//! and that compiler needs LLVM, built from source as well (see the `[llvm]`
//! block this writes).
//!
//! The build host triple is detected and always included in the target list:
//! x.py prunes std for any target not listed, and the host std is required to run
//! build scripts (for example the minixrs crates' `build.rs`) when compiling for a
//! minix target.
//!
//! Each minix triple is listed with its `-elf` form: the position-independent
//! target a shared object is built for (`cargo xtask build dynlibc`). Those, and
//! `wasm32-minix` (the target `tools/wasm-servers/build.sh` builds the wasm
//! kernel, servers and program modules for), are the targets whose sysroot the
//! `no-std` key narrows to `core` and `alloc`, because neither a shared object
//! nor a wasm module links anything else.
//!
//! The output is the text `tools/rust-config.py` wrote, except for its header
//! line, which named that script. The rest is byte-for-byte the same so a tree
//! configured by either produces the same x.py build.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::arch;
use crate::toolchain;

/// The position-independent form of each minix triple, for code a loader maps
/// as a shared object.
///
/// It is a target of its own and not a `-C relocation-model` because the `core`
/// and `alloc` it links against are what have to be built PIC; its spec lives
/// beside the executable one in the fork
/// (`compiler/rustc_target/src/spec/targets/*_minix_elf.rs`).
///
/// The `-elf` suffix is not decoration. `bootstrap` asks cc-rs for a C compiler
/// for *every* configured target, and cc-rs rejects a fourth component it does
/// not recognise as an environment or ABI ("unknown environment/ABI `dyn`");
/// `-elf` is one it maps to neither, so the name is the executable triple plus a
/// distinguishing suffix and nothing else.
const DYN_TARGETS: [(&str, &str); 3] = [
    ("x86_64-pc-minix", "x86_64-pc-minix-elf"),
    ("riscv64gc-unknown-minix", "riscv64gc-unknown-minix-elf"),
    ("aarch64-unknown-minix", "aarch64-unknown-minix-elf"),
];

/// A target that is none of the port's three arches, so it has no short name and
/// no `-elf` form: `all` lists it, and a wasm front end builds for it
/// (`tools/wasm-servers/build.sh`). Its spec is `targets/wasm32_minix.rs` in the
/// fork.
const EXTRA_TARGETS: [&str; 1] = ["wasm32-minix"];

/// The generated file and the target list that produced it.
#[derive(Debug)]
struct Plan {
    text: String,
    /// The `[build] target` list, spelled the way the old script printed it.
    targets: String,
}

pub fn run(root: &Path, token: &str) -> io::Result<()> {
    let plan = write(root, token)?;
    println!(
        "wrote {} with targets {}",
        config_path(root).display(),
        plan.targets
    );
    Ok(())
}

fn config_path(root: &Path) -> PathBuf {
    root.join("rust").join("config.toml")
}

fn write(root: &Path, token: &str) -> io::Result<Plan> {
    // `rustc` may be missing on a tree that has never been bootstrapped, so a
    // failed detection is not an error: the host is then left out, which is what
    // the old script did too.
    let plan = plan(token, toolchain::host_triple().as_deref())?;
    let out = config_path(root);
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    // Explicit UTF-8: x.py reads config.toml as UTF-8, and the locale encoding
    // (cp1252 on Windows) would mangle non-ASCII bytes.
    write_text(&out, &plan.text)?;
    Ok(plan)
}

/// `text` as UTF-8 with LF line endings.
///
/// x.py accepts either, but LF is the repository's convention and what every
/// non-Windows host already wrote; a CRLF file makes editors and `git diff` see
/// a whole-file change on a checkout whose `core.autocrlf` is off.
fn write_text(path: &Path, text: &str) -> io::Result<()> {
    fs::write(path, text)
}

/// The config text for `token` (`all`, or one arch's short name or triple) and
/// `host`, the build host triple when it could be detected.
fn plan(token: &str, host: Option<&str>) -> io::Result<Plan> {
    let (minix, with_extra) = if token == "all" {
        (arch::ALL.iter().map(|arch| arch.triple).collect::<Vec<_>>(), true)
    } else {
        (vec![arch::resolve(token)?.triple], false)
    };

    let mut selected = minix.clone();
    if with_extra {
        selected.extend(EXTRA_TARGETS);
    }
    let dyn_targets: Vec<&str> = minix
        .iter()
        .copied()
        .filter_map(dyn_target)
        .collect();

    // The host first, then the minix targets, then their PIC forms: the order
    // x.py sees, and the order the old script wrote.
    let mut targets: Vec<&str> = Vec::new();
    if let Some(host) = host {
        targets.push(host);
    }
    targets.extend(selected.iter().copied());
    targets.extend(dyn_targets.iter().copied());

    // `no-std` makes bootstrap keep `core` and `alloc` for the target and build
    // no std into its sysroot.
    let no_std: String = targets
        .iter()
        .copied()
        .filter(|target| is_no_std(target))
        .map(|target| format!("\n[target.{target}]\nno-std = true\n"))
        .collect();

    let list = python_list(&targets);
    let text = format!(
        "# Generated by `cargo xtask rust-config` ({token}); rerun `just bootstrap` to
# regenerate, or hand-edit (subsequent bootstrap runs overwrite it).
# The host ({host}) is always listed so its std survives x.py's pruning.
# A position-independent (`-elf`) triple is listed so its PIC `core`/`alloc`
# exist at all, and `wasm32-minix` because the wasm front ends build for it; the
# `no-std` sections below keep std out of their sysroots.

[llvm]
# Built from source. The CI LLVM artifacts are keyed to the upstream base
# commit and get purged upstream once that base ages out; when the lookup
# misses, bootstrap exits with \"failed to download llvm from CI\" instead of
# falling back. `if-unchanged` does not avoid that either - this tree is
# unmodified relative to upstream, so it takes the download path too.
download-ci-llvm = false
# Only the backends the host and the minix targets need. The default is 15
# targets plus 4 experimental ones and dominates a from-source build, so
# extend this list when a port adds an arch (the minix triples map to X86,
# RISCV and AArch64, and `wasm32-minix` to WebAssembly). Changing this line
# also needs `rm rust/build/<host>/llvm/.llvm-stamp`, or x.py reuses the LLVM
# built with the old list - the `rust-fork-rebase` skill has the symptom.
targets = \"AArch64;RISCV;X86;WebAssembly\"

[build]
target = {list}
{no_std}",
        token = token,
        host = host.unwrap_or(""),
        list = list,
        no_std = no_std,
    );
    Ok(Plan { text, targets: list })
}

fn dyn_target(triple: &str) -> Option<&'static str> {
    DYN_TARGETS
        .iter()
        .find(|(executable, _)| *executable == triple)
        .map(|(_, dyn_triple)| *dyn_triple)
}

fn is_no_std(target: &str) -> bool {
    DYN_TARGETS.iter().any(|(_, dyn_triple)| *dyn_triple == target)
        || EXTRA_TARGETS.contains(&target)
}

/// A list of strings the way Python's `repr` spells it: square brackets, single
/// quotes, comma and space between items.
fn python_list(items: &[&str]) -> String {
    let mut list = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            list.push_str(", ");
        }
        list.push('\'');
        list.push_str(item);
        list.push('\'');
    }
    list.push(']');
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_plan() -> Plan {
        plan("all", Some("x86_64-unknown-test")).unwrap()
    }

    #[test]
    fn all_lists_the_host_the_minix_targets_their_pic_forms_and_wasm() {
        let plan = all_plan();
        assert!(plan.text.contains(
            "target = ['x86_64-unknown-test', 'x86_64-pc-minix', \
             'riscv64gc-unknown-minix', 'aarch64-unknown-minix', 'wasm32-minix', \
             'x86_64-pc-minix-elf', 'riscv64gc-unknown-minix-elf', \
             'aarch64-unknown-minix-elf']"
        ));
    }

    #[test]
    fn one_arch_lists_only_itself_and_its_pic_form() {
        let plan = plan("riscv64", Some("host-triple")).unwrap();
        assert!(plan
            .text
            .contains("target = ['host-triple', 'riscv64gc-unknown-minix', 'riscv64gc-unknown-minix-elf']"));
        assert!(!plan.text.contains("[target.wasm32-minix]"));
        assert!(!plan.text.contains("[target.x86_64-pc-minix-elf]"));
    }

    #[test]
    fn the_no_std_sections_are_the_pic_and_wasm_targets_only() {
        let plan = all_plan();
        for target in [
            "x86_64-pc-minix-elf",
            "riscv64gc-unknown-minix-elf",
            "aarch64-unknown-minix-elf",
            "wasm32-minix",
        ] {
            assert!(
                plan.text
                    .contains(&format!("[target.{target}]\nno-std = true")),
                "no no-std section for {target}"
            );
        }
        assert!(!plan.text.contains("[target.x86_64-pc-minix]\nno-std"));
        assert!(!plan.text.contains("[target.x86_64-unknown-test]\nno-std"));
    }

    #[test]
    fn a_host_that_cannot_be_detected_is_left_out() {
        let plan = plan("x86", None).unwrap();
        assert!(plan
            .text
            .contains("target = ['x86_64-pc-minix', 'x86_64-pc-minix-elf']"));
        assert!(plan.text.contains("# The host () is always listed"));
    }

    #[test]
    fn the_llvm_block_keeps_the_from_source_build() {
        let plan = all_plan();
        assert!(plan.text.contains("download-ci-llvm = false"));
        assert!(plan
            .text
            .contains("targets = \"AArch64;RISCV;X86;WebAssembly\""));
    }

    #[test]
    fn an_unknown_arch_names_the_ones_that_exist() {
        let error = plan("mips", None).unwrap_err();
        assert!(error.to_string().contains("mips"));
        assert!(error.to_string().contains("riscv64"));
    }

    #[test]
    fn a_list_is_spelled_the_way_python_spelled_it() {
        assert_eq!(python_list(&[]), "[]");
        assert_eq!(python_list(&["one"]), "['one']");
        assert_eq!(python_list(&["one", "two"]), "['one', 'two']");
    }

    #[test]
    fn the_config_sits_in_the_fork() {
        assert!(config_path(Path::new("root")).ends_with("rust/config.toml"));
    }

    #[test]
    fn a_write_creates_the_fork_directory_and_reports_its_targets() {
        let dir = std::env::temp_dir().join("minixrs-xtask-rust-config");
        let _ = fs::remove_dir_all(&dir);
        let plan = write(&dir, "x86").unwrap();
        let written = fs::read_to_string(config_path(&dir)).unwrap();
        assert_eq!(written, plan.text);
        assert!(!written.contains('\r'), "the generated config must be LF");
        assert!(plan.targets.contains("'x86_64-pc-minix'"));
        let _ = fs::remove_dir_all(&dir);
    }
}
