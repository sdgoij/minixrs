//! A `cc` for x86_64-, riscv64- and aarch64-minix, for C projects that drive a
//! compiler themselves.
//!
//! bash is one: `configure` compiles and links probe programs, `make` invokes the
//! compiler once per object and again to link the shell. This is the command those
//! steps reach, assembled from the two halves the port already has:
//!
//! * **compile**: clang with the port's headers, hermetic and freestanding.
//!   [`super::compile_flags`] has why none of those flags is optional.
//! * **link**: the fork's stage1 rustc with the arch's `tools/crt0-<arch>.S` and
//!   the minix-libc rlib, which is the same pair `cargo xtask build c-hello`
//!   assembles by hand for `/bin/helloc`.
//!
//! Point a build at it by name, exactly as a cross build would, naming the target
//! first where it is not x86_64 (the default, so an existing `CC` keeps working):
//!
//! ```text
//! CC="<repo>/.xtask/linux/debug/xtask cc minix"
//! CC="<repo>/.xtask/linux/debug/xtask cc minix riscv64"
//! ```
//!
//! The build host has to be POSIX (the rlib, the stage1 and clang must be the
//! same host's): see `.notes/c_build.md`.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use super::{compile_flags, link_passthrough, Runner};
use crate::arch::Arch;
use crate::toolchain;

/// Scratch objects, per arch: two arches' objects in one directory would be
/// linked into each other's executable.
const WORK_ROOT: &str = "target/cc-minix";
const LD_SCRIPT: &str = "tools/minix-user.ld";

/// The rustc link wants a crate root; this one pulls the libc in and is otherwise
/// empty, the same stub `cargo xtask build c-hello` writes.
const STUB: &str = "#![no_std]
#![no_main]
extern crate minix_libc;
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { loop {} }
";

/// Arguments taking a separate value. Everything else that starts with `-` is a
/// flag; the inputs are what is left. A wrapper that gets this wrong either drops
/// an input or hands the compiler an argument that belongs to the link.
const VALUE_FLAGS: [&str; 17] = [
    "-o", "-I", "-D", "-U", "-L", "-l", "-isystem", "-include", "-imacros", "-MF", "-MT",
    "-MQ", "-x", "--param", "-std", "-Xlinker", "-u",
];

/// `configure` and `make` ask the compiler what it is. A wrapper that fails these
/// is rejected as "not working", so they answer without compiling anything.
const VERSION_PROBES: [&str; 7] = [
    "--version",
    "-v",
    "-V",
    "-qversion",
    "-dumpversion",
    "-dumpmachine",
    "--help",
];

const COMPILE_ONLY: [&str; 6] = ["-c", "-E", "-S", "-M", "-MM", "-fsyntax-only"];

/// One `cc` line, split for the two commands it may become.
struct Split {
    flags: Vec<String>,
    inputs: Vec<String>,
    output: Option<String>,
}

fn split_args(argv: &[String]) -> Split {
    let mut flags = Vec::new();
    let mut inputs = Vec::new();
    let mut output = None;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "-o" {
            i += 1;
            output = argv.get(i).cloned();
        } else if VALUE_FLAGS.contains(&arg.as_str()) {
            flags.push(arg.clone());
            if let Some(value) = argv.get(i + 1) {
                flags.push(value.clone());
                i += 1;
            }
        } else if arg.starts_with('-') {
            flags.push(arg.clone());
        } else {
            inputs.push(arg.clone());
        }
        i += 1;
    }
    Split {
        flags,
        inputs,
        output,
    }
}

pub fn run(root: &Path, arch: &Arch, argv: &[String], dry_run: bool) -> io::Result<bool> {
    if argv.is_empty() {
        eprintln!("cc-minix: no arguments");
        return Ok(false);
    }
    if argv.len() == 1 && VERSION_PROBES.contains(&argv[0].as_str()) {
        println!(
            "cc-minix: clang for {}, linked by the minix fork's stage1 rustc",
            arch.clang_target()
        );
        return Ok(true);
    }

    let Some(rustc) = toolchain::find_stage1_rustc(root) else {
        let host = toolchain::host_triple().unwrap_or_else(|| "<unknown>".to_string());
        eprintln!(
            "cc-minix: no stage1 rustc at rust/build/{host}/stage1/bin - run `just fetch-stage1` (or bootstrap) on this host"
        );
        return Ok(false);
    };
    let Some(lld) = toolchain::find_lld(root) else {
        eprintln!("cc-minix: no lld to link with (see `cargo xtask check static`)");
        return Ok(false);
    };

    let split = split_args(argv);
    let cflags = arch.base_cflags();
    let include_flags = compile_flags(root)?;
    let work = root.join(WORK_ROOT).join(arch.name);
    let runner = Runner::new(dry_run);

    if argv.iter().any(|arg| COMPILE_ONLY.contains(&arg.as_str())) {
        let mut command = Command::new("clang");
        command.args(&cflags).args(&include_flags).args(&split.flags);
        command.args(&split.inputs);
        if let Some(output) = &split.output {
            command.arg("-o").arg(output);
        }
        return runner.call(&mut command);
    }

    if split.inputs.is_empty() {
        eprintln!("cc-minix: no input files");
        return Ok(false);
    }

    if !dry_run {
        fs::create_dir_all(&work)?;
    }

    // Link: compile the C/C++ inputs first, keep archives and objects as they
    // are, then hand the whole set to rustc along with crt0 and the rlib.
    let mut objects = Vec::new();
    for source in &split.inputs {
        if source.ends_with(".o") || source.ends_with(".a") {
            objects.push(source.clone());
            continue;
        }
        let stem = Path::new(source)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("input");
        let object = work.join(format!("{stem}.o"));
        let mut clang = Command::new("clang");
        clang
            .args(&cflags)
            .args(&include_flags)
            .args(&split.flags)
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object);
        if !runner.call(&mut clang)? {
            return Ok(false);
        }
        objects.push(object.display().to_string());
    }

    let crt0 = work.join("crt0.o");
    let mut clang = Command::new("clang");
    clang
        .args(&cflags)
        .arg("-c")
        .arg(arch.crt0(root))
        .arg("-o")
        .arg(&crt0);
    if !runner.call(&mut clang)? {
        return Ok(false);
    }

    let deps = arch.libc_deps(root);
    let rlib = match crate::build::newest_libc_rlib(&deps) {
        Ok(rlib) => rlib,
        Err(_) => {
            eprintln!(
                "cc-minix: no minix-libc rlib for {} - run `just build-bash {}` (it builds one with this host's stage1) or `cargo build -p minix-libc --target {} --release`",
                arch.triple, arch.name, arch.triple
            );
            return Ok(false);
        }
    };

    let stub = work.join("link_stub.rs");
    if !dry_run {
        fs::write(&stub, STUB)?;
    }

    let mut command = Command::new(&rustc);
    command
        .args(["--crate-type", "bin", "--target", arch.triple, "--edition", "2024"])
        .arg("-C")
        .arg(format!("link-arg=-T{}", root.join(LD_SCRIPT).display()))
        .arg("-C")
        .arg(format!("linker={}", lld.display()))
        .arg("-C")
        .arg(format!("link-arg={}", crt0.display()));
    for object in &objects {
        command.arg("-C").arg(format!("link-arg={object}"));
    }
    // `-l`/`-L`/`-Wl,` belong to the link, not the compile: without them the
    // executable has no archives in it and every symbol in them is undefined.
    for arg in link_passthrough(argv) {
        command.arg("-C").arg(format!("link-arg={arg}"));
    }
    let output = split.output.clone().unwrap_or_else(|| "a.out".to_string());
    command
        .arg("--extern")
        .arg(format!("minix_libc={}", rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(&output)
        .arg(&stub);
    runner.call(&mut command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_compile_line_keeps_its_flags_and_its_output() {
        let split = split_args(&words("-c -O2 -I include -o obj.o src.c"));
        assert_eq!(split.flags, vec!["-c", "-O2", "-I", "include"]);
        assert_eq!(split.inputs, vec!["src.c"]);
        assert_eq!(split.output.as_deref(), Some("obj.o"));
    }

    #[test]
    fn an_argument_that_takes_a_value_is_not_an_input() {
        let split = split_args(&words("-MF dep.d -MT target -u main -Xlinker --as-needed foo.c"));
        assert_eq!(split.inputs, vec!["foo.c"]);
        assert!(split.flags.contains(&"dep.d".to_string()));
        assert!(split.flags.contains(&"--as-needed".to_string()));
    }

    #[test]
    fn a_missing_output_value_is_not_an_output() {
        let split = split_args(&words("-c src.c -o"));
        assert!(split.output.is_none());
        assert_eq!(split.inputs, vec!["src.c"]);
    }

    #[test]
    fn the_stub_is_the_one_c_hello_writes() {
        assert!(STUB.contains("extern crate minix_libc;"));
        assert!(STUB.contains("#![no_std]"));
    }
}
