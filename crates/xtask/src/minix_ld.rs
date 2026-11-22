//! The link driver a foreign build's CMake uses for a minix program.
//!
//! CMake's link rule hands a compiler driver compiler-driver syntax:
//! `-Wl,-rpath,...`, `-lpthreads`, `-static`, and often the whole line in a
//! response file. The port has no such driver; it has LLD, `crt0.o`, `libc.a`,
//! `libstdc++.a` and `tools/minix-user.ld`. This translates the first into the
//! second, which is the same link `cargo xtask build c-direct` performs.
//!
//! It exists because the cross build must not let the host's clang choose the
//! host's linker: for `x86_64-unknown-none` on Windows that is Strawberry GCC and
//! its libc, and the result would be a program for the wrong system.
//!
//! Called as:
//!
//! ```text
//! cargo xtask minix-ld --arch x86 --out <file> --crt0 <obj> --libc <a> \
//!     --libstdcxx <a> --script <ld> --lld <ld.lld> <link args…>
//! ```

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::{self, Arch};
use crate::cc;

/// Link flags that name a host library the port does not have as a separate
/// object: every one of them is a symbol in `libc.a`.
const HOST_LIBS: [&str; 10] = [
    "-lpthreads", "-lpthread", "-ldl", "-lm", "-lrt", "-lc", "-lstdc++", "-lunwind",
    "-lgcc", "-lgcc_s",
];

/// Compiler-driver flags that mean nothing to LLD or to a static minix program.
const DROP_FLAGS: [&str; 10] = [
    "-pthread",
    "-static",
    "-static-libgcc",
    "-static-libstdc++",
    "-shared",
    "-pie",
    "-no-pie",
    "-fno-pie",
    "-rdynamic",
    "-stdlib=libc++",
];

/// The paths one link needs. A CMake link rule supplies all but the arguments.
pub struct Link<'a> {
    pub arch: &'a str,
    pub out: &'a Path,
    pub crt0: &'a Path,
    pub libc: &'a Path,
    pub libstdcxx: &'a Path,
    pub script: &'a Path,
    pub lld: &'a Path,
}

pub fn run(link: &Link<'_>, argv: &[String]) -> io::Result<bool> {
    let arch = arch::resolve(link.arch)?;
    let args = expand_response_files(argv)?;
    let mut command = Command::new(link.lld);
    // The host `lld` this runs may be the generic driver (`lld.exe`), not a
    // flavour-named copy; `-flavor gnu` is what makes it link ELF. It also has
    // to be the first argument, so it precedes everything else.
    command.args(["-flavor", "gnu"]);
    command.args(link_args(arch, link.out, link.crt0, link.script));
    command.args(translate(&args));
    // The C++ runtime before the C library: the runtime calls into it.
    command.arg(link.libstdcxx).arg(link.libc);
    cc::run(&mut command)
}

/// The fixed prefix of the LLD line. The translated arguments follow it, and the
/// two archives come last so a library they pull can still resolve.
fn link_args(arch: &Arch, out: &Path, crt0: &Path, script: &Path) -> Vec<String> {
    vec![
        "-o".to_string(),
        out.display().to_string(),
        format!("--image-base={}", arch.image_base),
        format!("--defsym=BASE_ADDRESS={}", arch.image_base),
        format!("--defsym=LOADER_BASE={}", arch.loader_base),
        "-T".to_string(),
        script.display().to_string(),
        "-z".to_string(),
        "norelro".to_string(),
        "--no-eh-frame-hdr".to_string(),
        // libc++'s objects name `pthread` and `rt` in `.deplibs`; the port has
        // one C archive and no separate libraries to find.
        "--no-dependent-libraries".to_string(),
        crt0.display().to_string(),
    ]
}

/// A response file is a compiler driver's, so its contents go through
/// [`translate`] like any other argument.
fn expand_response_files(argv: &[String]) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    expand_into(argv, &mut out, &mut seen)?;
    Ok(out)
}

fn expand_into(
    argv: &[String],
    out: &mut Vec<String>,
    seen: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    for arg in argv {
        if let Some(path) = response_path(arg) {
            let path = path.canonicalize().unwrap_or(path);
            if !seen.insert(path.clone()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("response file {} includes itself", path.display()),
                ));
            }
            let text = fs::read_to_string(&path).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("reading response file {}: {error}", path.display()),
                )
            })?;
            let words: Vec<String> = text.split_whitespace().map(String::from).collect();
            expand_into(&words, out, seen)?;
            seen.remove(&path);
        } else {
            out.push(arg.clone());
        }
    }
    Ok(())
}

fn response_path(arg: &str) -> Option<PathBuf> {
    let name = arg.strip_prefix('@')?;
    Some(PathBuf::from(name))
}

/// The link arguments LLD should see.
///
/// `-Wl,a,b` is split at its commas, because it is clang's spelling for two
/// arguments; an rpath is dropped (there is no dynamic loader here), and a
/// `-z` pair is reassembled for LLD.
fn translate(argv: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        index += 1;
        if arg == "-o" {
            // The output is `--out`; drop its value with it.
            index += 1;
        } else if let Some(rest) = arg.strip_prefix("-Wl,") {
            let parts: Vec<&str> = rest.split(',').collect();
            if parts
                .iter()
                .any(|part| part.starts_with("-rpath") || *part == "-soname")
            {
                continue;
            }
            if parts.len() == 2 && parts[0] == "-z" {
                out.push("-z".to_string());
                out.push(parts[1].to_string());
            } else if parts.len() == 1 {
                out.push(parts[0].to_string());
            }
            // A multi-part flag LLD does not know (an rpath is already gone, a
            // `-z` pair is rebuilt): keeping half of it would be worse.
        } else if HOST_LIBS.contains(&arg.as_str()) || DROP_FLAGS.contains(&arg.as_str()) {
            continue;
        } else {
            out.push(arg.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn a_compiler_drivers_flags_become_linker_flags() {
        let args = translate(&words(
            "-Wl,-z,noexecstack -Wl,--gc-sections -Wl,-rpath,$ORIGIN/lib -lpthreads -static obj.o lib/libLLVMCore.a",
        ));
        assert_eq!(
            args,
            vec![
                "-z",
                "noexecstack",
                "--gc-sections",
                "obj.o",
                "lib/libLLVMCore.a"
            ]
        );
    }

    #[test]
    fn a_host_library_is_not_named() {
        for lib in HOST_LIBS {
            assert!(translate(&[lib.to_string()]).is_empty(), "{lib} survived");
        }
    }

    #[test]
    fn the_fixed_prefix_pins_the_arch_and_names_the_script_and_crt0() {
        let args = link_args(
            &arch::X86_64,
            Path::new("lld"),
            Path::new("crt0.o"),
            Path::new("minix-user.ld"),
        );
        assert!(args.contains(&"--image-base=0x4000000".to_string()));
        assert!(args.contains(&"--defsym=LOADER_BASE=0x8000000".to_string()));
        assert!(args.contains(&"minix-user.ld".to_string()));
        assert!(args.contains(&"crt0.o".to_string()));
    }

    #[test]
    fn a_response_file_is_read_not_passed_through() {
        let dir = std::env::temp_dir().join("minixrs-xtask-response-file");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("link.rsp");
        fs::write(&file, "a.o -Wl,--gc-sections -lpthreads").unwrap();
        let args = expand_response_files(&[format!("@{}", file.display())]).unwrap();
        assert_eq!(args, vec!["a.o", "-Wl,--gc-sections", "-lpthreads"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
