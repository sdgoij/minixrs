//! A `cc` for C projects that build *shared objects* for minix.
//!
//! [`super::minix`] links an executable against the static `minix-libc` rlib,
//! which is what bash and the smoke binaries want. Mesa and `libdrm` are the other
//! shape: their build (meson) compiles many C files with one `cc`, links some of
//! them into a `.so` that names `libc.so`, and links a program against that. A
//! static-rlib link cannot produce either, so this is a second `cc` rather than a
//! flag on the first.
//!
//! It is the `cc` half of the Wayland plan's §6.10 stage 3c-0. The pieces it
//! reuses are the ones every minix C build already shares:
//!
//! * **compile**: clang for the arch's machine, `-fPIC` (a shared object is
//!   position-independent; `cc minix`'s `-fno-pic` is for executables), with
//!   [`super::compile_flags`]'s hermetic include set, so a missing header is an
//!   error rather than the host's glibc.
//! * **link**: LLD directly ([`crate::toolchain::find_lld`]), with the
//!   [`crate::dynamic::NO_RELRO`] flags a shared object takes so it costs three
//!   VM regions and not four, against the port's `libc.so`
//!   (`target/<triple>/release/libc.so`, the object `just dynlib-<arch>`
//!   publishes). A program links with `tools/crt0-<arch>.S` and the loader's
//!   `PT_INTERP` (`/libexec/ld.so`), the same two things
//!   `cargo xtask build cdyn` supplies through rustc, here without rustc,
//!   because a build system calls this for every link.
//!
//! Point a build at it by name, naming the target first where it is not x86_64:
//!
//! ```text
//! CC="<repo>/.xtask/linux/debug/xtask cc dso"
//! CC="<repo>/.xtask/linux/debug/xtask cc dso x86"
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{compile_flags, Runner};
use crate::arch::Arch;
use crate::dynamic::NO_RELRO;
use crate::toolchain;

const LD_SCRIPT: &str = "tools/minix-user.ld";
const INTERP: &str = "/libexec/ld.so";
const LIBC: &str = "libc.so";
const WORK_ROOT: &str = "target/cc-dso";

/// A `cc` line is two commands in one, so every argument has to be classified: a
/// compile-class flag (`-I`, `-O2`, `-fPIC`) must not reach the linker, and a
/// link-class one (`-Wl,...`, `-L`, `-l`) must not reach clang. The two value
/// sets are disjoint, which is what lets one pass decide.
const COMPILE_VALUE_FLAGS: [&str; 17] = [
    "-I",
    "-D",
    "-U",
    "-isystem",
    "-include",
    "-imacros",
    "-MF",
    "-MT",
    "-MQ",
    "-x",
    "--param",
    "-std",
    "-Xclang",
    "-idirafter",
    "-iquote",
    "-isysroot",
    "--sysroot",
];
const LINK_VALUE_FLAGS: [&str; 6] = ["-L", "-l", "-Xlinker", "-u", "-z", "-T"];
/// Driver-level link flags lld does not accept as spelled; `-shared` is added
/// explicitly and the rest are dropped or translated in [`link_args`].
const LINK_FLAGS: [&str; 7] = [
    "-shared",
    "-static",
    "-rdynamic",
    "-pthread",
    "-nostdlib",
    "-nodefaultlibs",
    "-s",
];
const COMPILE_ONLY: [&str; 6] = ["-c", "-E", "-S", "-M", "-MM", "-fsyntax-only"];
/// `-Wl,` options whose value meson passes as the *next* token rather than
/// comma-joined (the version script and dynamic list are build artifacts kept as
/// separate arguments). Without this the file is classified as an input and
/// compiled.
const WL_OPT_TAKES_VALUE: [&str; 10] = [
    "--version-script",
    "--dynamic-list",
    "--script",
    "-T",
    "-Map",
    "--retain-symbols-file",
    "--defsym",
    "-e",
    "--dynamic-linker",
    "--just-symbols",
];
/// Libraries meson's platform probes picked up from the *host* (or that live in
/// the port's libc.so): the port provides libc/libm/pthread/dl/rt itself.
const SPURIOUS_LIBS: [&str; 8] = [
    "-lm", "-lpthread", "-ldl", "-lrt", "-lc", "-lws2_32", "-lgcc", "-lgcc_s",
];
const VERSION_PROBES: [&str; 7] = [
    "--version",
    "-v",
    "-V",
    "-qversion",
    "-dumpversion",
    "-dumpmachine",
    "--help",
];

/// `base_cflags` with `-fno-pic` swapped for `-fPIC`.
///
/// `base_cflags` is the *executable* set (the kernel's static relocation model);
/// a shared object is the one place the port needs PIC, so the swap is explicit
/// rather than a second table. The `--target=` stays: meson's cross file names the
/// machine through no option of its own, so this argv is the only place it is
/// said.
fn pic_cflags(arch: &Arch) -> Vec<String> {
    let mut flags: Vec<String> = arch
        .base_cflags()
        .into_iter()
        .filter(|flag| flag != "-fno-pic")
        .collect();
    flags.push("-fPIC".to_string());
    flags
}

/// The libc++ headers, ahead of the port's C headers.
///
/// Order is what the `just libcxx-<arch>` note calls for: libc++, then
/// `c-include`, then the generated config. These use `-I`, not `-isystem`,
/// because clang searches every `-I` directory before every `-isystem` one: with
/// `-isystem` here the C headers would win and libc++'s `<cstring>`-style
/// wrappers would not find their own `<string.h>`. `-nostdinc++` keeps the host's
/// libstdc++ out.
fn cxx_include_flags(root: &Path) -> Vec<String> {
    let llvm = root.join("rust").join("src").join("llvm-project");
    vec![
        "-nostdinc++".to_string(),
        "-I".to_string(),
        llvm.join("libcxx").join("include").display().to_string(),
        "-I".to_string(),
        llvm.join("libcxxabi").join("include").display().to_string(),
    ]
}

fn cxx_root(root: &Path, arch: &Arch) -> PathBuf {
    root.join("target").join("cxx").join(arch.name)
}

/// The generated `__config_site` include dir, after the C headers.
fn cxx_config_flags(root: &Path, arch: &Arch) -> Vec<String> {
    vec![
        "-I".to_string(),
        cxx_root(root, arch)
            .join("libcxx-build")
            .join("include")
            .join("c++")
            .join("v1")
            .display()
            .to_string(),
    ]
}

fn libcxx_runtime(root: &Path, arch: &Arch) -> PathBuf {
    cxx_root(root, arch)
        .join("minix-runtime")
        .join("libstdc++.a")
}

fn libc_dir(root: &Path, arch: &Arch) -> PathBuf {
    root.join("target").join(arch.triple).join("release")
}

/// One classified `cc` line.
struct Split {
    compile: Vec<String>,
    link: Vec<String>,
    inputs: Vec<String>,
    output: Option<String>,
}

fn classify(argv: &[String]) -> Split {
    let mut compile = Vec::new();
    let mut link = Vec::new();
    let mut inputs = Vec::new();
    let mut output = None;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "-o" {
            i += 1;
            output = argv.get(i).cloned();
        } else if COMPILE_VALUE_FLAGS.contains(&arg.as_str()) {
            compile.push(arg.clone());
            if let Some(value) = argv.get(i + 1) {
                compile.push(value.clone());
                i += 1;
            }
        } else if LINK_VALUE_FLAGS.contains(&arg.as_str()) {
            link.push(arg.clone());
            if let Some(value) = argv.get(i + 1) {
                link.push(value.clone());
                i += 1;
            }
        } else if let Some(rest) = arg.strip_prefix("-Wl,") {
            link.push(arg.clone());
            if rest.rsplit(',').next().is_some_and(|last| WL_OPT_TAKES_VALUE.contains(&last))
                && let Some(value) = argv.get(i + 1)
            {
                link.push(value.clone());
                i += 1;
            }
        } else if LINK_FLAGS.contains(&arg.as_str()) {
            link.push(arg.clone());
        } else if arg.starts_with('-') {
            compile.push(arg.clone());
        } else {
            inputs.push(arg.clone());
        }
        i += 1;
    }
    Split {
        compile,
        link,
        inputs,
        output,
    }
}

/// `link` as LLD arguments, expanding `-Wl,` and dropping RELRO.
///
/// meson spells a shared object's soname `-Wl,-soname,libX.so`, so a `-Wl,a,b`
/// becomes `-a -b`; `-z relro` is dropped because [`NO_RELRO`] is the port's rule,
/// a `GNU_RELRO` segment is one VM region per object and nothing reads it.
/// Driver-level flags (`-pthread`, `-static`) are dropped, because lld is not the
/// compiler driver and rejects them.
fn link_args(link: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < link.len() {
        let arg = &link[i];
        if LINK_FLAGS.contains(&arg.as_str()) {
            i += 1;
            continue;
        }
        if arg == "-rdynamic" {
            out.push("--export-dynamic".to_string());
            i += 1;
            continue;
        }
        let parts: Vec<String> = if arg == "-Wl" && i + 1 < link.len() {
            let parts = link[i + 1].split(',').map(str::to_string).collect();
            i += 2;
            parts
        } else if let Some(rest) = arg.strip_prefix("-Wl,") {
            let parts = rest.split(',').map(str::to_string).collect();
            i += 1;
            parts
        } else {
            if arg == "-z" && link.get(i + 1).is_some_and(|next| next == "relro") {
                i += 2;
                continue;
            }
            out.push(arg.clone());
            i += 1;
            continue;
        };
        let mut j = 0;
        while j < parts.len() {
            if parts[j] == "-z" && parts.get(j + 1).is_some_and(|next| next == "relro") {
                j += 2;
                continue;
            }
            out.push(parts[j].clone());
            j += 1;
        }
    }
    out
}

/// Named views of `libc.so` for the libraries it already contains.
///
/// libc++'s objects record a `-lpthread` dependency specifier (a `.deplibs`
/// entry from its own build); lld follows it and fails when no `libpthread`
/// exists. The same is true of `-lm`/`-ldl`/`-lrt`. A one-line linker script
/// named for each forwards to `libc.so` rather than duplicating it.
fn ensure_libc_shims(dir: &Path) -> io::Result<()> {
    for name in ["libpthread.so", "libm.so", "libdl.so", "librt.so"] {
        let shim = dir.join(name);
        if !shim.exists() {
            fs::write(&shim, "INPUT ( libc.so )\n")?;
        }
    }
    Ok(())
}

/// The compiler half of a line: the program, the arch's PIC flags and, for C++,
/// the libc++ include set.
struct Compiler<'a> {
    root: &'a Path,
    arch: &'a Arch,
    runner: &'a Runner,
    program: &'a str,
    cxx: bool,
}

impl Compiler<'_> {
    fn object(&self, src: &Path, out: &Path, flags: &[String]) -> io::Result<bool> {
        let mut command = Command::new(self.program);
        command.args(pic_cflags(self.arch));
        if self.cxx {
            command.args(cxx_include_flags(self.root));
        }
        command.args(compile_flags(self.root)?);
        if self.cxx {
            command.args(cxx_config_flags(self.root, self.arch));
        }
        command
            .args(flags)
            .arg("-c")
            .arg(src)
            .arg("-o")
            .arg(out);
        self.runner.call(&mut command)
    }
}

/// Everything one link needs, named so the argument list stays readable.
struct Link<'a> {
    root: &'a Path,
    arch: &'a Arch,
    runner: &'a Runner,
    lld: &'a Path,
    cc: &'a str,
    /// The compile-class flags, for a source that is reached mid-link.
    compile: &'a [String],
    argv: &'a [String],
    output: &'a Path,
    work: &'a Path,
    cxx: bool,
    dry_run: bool,
}

/// One link, walking the original `cc` line so flag/input order survives.
///
/// A `-Wl,--whole-archive ... -Wl,--no-whole-archive` pair only means anything if
/// the archives stay between the two flags, so the link step cannot hoist all
/// flags ahead of all inputs (which would leave `libdri.a` unextracted and the
/// version script with nothing to export). Source inputs are compiled in place as
/// they are reached.
fn link(cx: &Link) -> io::Result<bool> {
    let Link {
        root,
        arch,
        runner,
        lld,
        cc,
        compile,
        argv,
        output,
        work,
        cxx,
        dry_run,
    } = *cx;
    let dir = libc_dir(root, arch);
    if !dir.join(LIBC).is_file() {
        eprintln!(
            "cc-dso-minix: no {} - run `just dynlib-{}` first",
            dir.join(LIBC).display(),
            arch.name
        );
        return Ok(false);
    }
    if !dry_run {
        ensure_libc_shims(&dir)?;
    }

    let mut shared = false;
    let mut memo: BTreeMap<String, String> = BTreeMap::new();
    let mut args: Vec<String> = Vec::new();
    let compiler = Compiler {
        root,
        arch,
        runner,
        program: cc,
        cxx,
    };
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "-o" {
            i += 2;
            continue;
        }
        if COMPILE_VALUE_FLAGS.contains(&arg.as_str()) {
            i += 2;
            continue;
        }
        if COMPILE_ONLY.contains(&arg.as_str()) || VERSION_PROBES.contains(&arg.as_str()) {
            i += 1;
            continue;
        }
        if arg == "-Wl" || arg.starts_with("-Wl,") {
            let parts: Vec<String> = if arg == "-Wl" {
                let parts = argv
                    .get(i + 1)
                    .map(|next| next.split(',').map(str::to_string).collect())
                    .unwrap_or_default();
                i += 2;
                parts
            } else {
                let parts = arg[4..].split(',').map(str::to_string).collect();
                i += 1;
                parts
            };
            let mut j = 0;
            while j < parts.len() {
                let part = &parts[j];
                if part == "-z" && parts.get(j + 1).is_some_and(|next| next == "relro") {
                    j += 2;
                    continue;
                }
                args.push(part.clone());
                // meson passes a version script / dynamic list as the next token
                if WL_OPT_TAKES_VALUE.contains(&part.as_str())
                    && j == parts.len() - 1
                    && let Some(value) = argv.get(i)
                {
                    args.push(value.clone());
                    i += 1;
                }
                j += 1;
            }
            continue;
        }
        if LINK_VALUE_FLAGS.contains(&arg.as_str()) {
            if arg == "-z" && argv.get(i + 1).is_some_and(|next| next == "relro") {
                i += 2;
                continue;
            }
            args.push(arg.clone());
            if let Some(value) = argv.get(i + 1) {
                args.push(value.clone());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if LINK_FLAGS.contains(&arg.as_str()) {
            if arg == "-shared" {
                shared = true;
            }
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            // meson passes linker flags through `-Wl,`, so a bare flag here is a
            // compiler flag (e.g. `-fPIC`) unless it is a path/library option.
            // `-L`/`-l` pass through; the spurious host libraries do not.
            if arg.starts_with("-L")
                || (arg.starts_with("-l") && !SPURIOUS_LIBS.contains(&arg.as_str()))
            {
                args.push(arg.clone());
            }
            i += 1;
            continue;
        }
        // An input: an existing object/archive/shared object passes through;
        // anything else is a source to compile now.
        if arg.ends_with(".o") || arg.ends_with(".a") || arg.ends_with(".so") || arg.contains(".so.")
        {
            args.push(arg.clone());
        } else if let Some(object) = memo.get(arg) {
            args.push(object.clone());
        } else {
            let stem = Path::new(arg)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("input");
            let object = work.join(format!("{stem}.o"));
            if !compiler.object(Path::new(arg), &object, compile)? {
                return Ok(false);
            }
            let object = object.display().to_string();
            memo.insert(arg.clone(), object.clone());
            args.push(object);
        }
        i += 1;
    }

    let mut command = Command::new(lld);
    command.args(["-flavor", "gnu"]);
    if shared {
        // `--no-undefined` cannot hold for a shared object the port loads: it
        // references `__tls_get_addr` (the loader) and libc symbols resolved at
        // runtime. `--allow-shlib-undefined` below is the port's rule.
        args.retain(|arg| arg != "--no-undefined");
        command.arg("-shared").args(NO_RELRO);
    } else {
        let crt0 = work.join("crt0.o");
        let mut clang = Command::new(cc);
        clang
            .args(pic_cflags(arch))
            .arg("-c")
            .arg(arch.crt0(root))
            .arg("-o")
            .arg(&crt0);
        if !runner.call(&mut clang)? {
            return Ok(false);
        }
        command
            .arg(format!("-T{}", root.join(LD_SCRIPT).display()))
            .arg(format!("--dynamic-linker={INTERP}"))
            .arg(&crt0);
    }
    command.arg("-o").arg(output).args(&args);

    let runtime = libcxx_runtime(root, arch);
    if runtime.is_file() {
        command.arg(&runtime);
    }
    command
        .arg(format!("-L{}", dir.display()))
        .arg(format!("-l:{LIBC}"))
        .arg("--allow-shlib-undefined");
    runner.call(&mut command)
}

pub fn run(root: &Path, arch: &Arch, argv: &[String], dry_run: bool) -> io::Result<bool> {
    // `--cxx` selects clang++ for the whole line: meson's cross file names this
    // wrapper once for C and once for C++, and the mode is what separates them.
    let cxx = argv.iter().any(|arg| arg == "--cxx");
    let argv: Vec<String> = argv.iter().filter(|arg| *arg != "--cxx").cloned().collect();
    let cc = if cxx { "clang++" } else { "clang" };
    if argv.is_empty() {
        eprintln!("cc-dso-minix: no arguments");
        return Ok(false);
    }
    if argv.len() == 1 && VERSION_PROBES.contains(&argv[0].as_str()) {
        println!(
            "cc-dso-minix: {cc} for {}, linked by lld against {LIBC}",
            arch.clang_target()
        );
        return Ok(true);
    }

    let Some(lld) = toolchain::find_lld(root) else {
        eprintln!("cc-dso-minix: no lld to link with (see `cargo xtask check static`)");
        return Ok(false);
    };

    let split = classify(&argv);
    let work = root.join(WORK_ROOT).join(arch.name);
    let runner = Runner::new(dry_run);
    if !dry_run {
        fs::create_dir_all(&work)?;
    }

    if argv.iter().any(|arg| COMPILE_ONLY.contains(&arg.as_str())) {
        let mut command = Command::new(cc);
        command.args(pic_cflags(arch));
        if cxx {
            command.args(cxx_include_flags(root));
        }
        command.args(compile_flags(root)?);
        if cxx {
            command.args(cxx_config_flags(root, arch));
        }
        command.args(&split.compile).args(&split.inputs);
        if let Some(output) = &split.output {
            command.arg("-o").arg(output);
        }
        return runner.call(&mut command);
    }

    if split.inputs.is_empty() {
        // A linker probe (`cc -Wl,--version`, which meson runs to identify the
        // linker) has flags and no input file; forward it to lld, which is the
        // linker being asked about.
        if !split.link.is_empty() {
            let mut command = Command::new(&lld);
            command.args(["-flavor", "gnu"]).args(link_args(&split.link));
            return runner.call(&mut command);
        }
        eprintln!("cc-dso-minix: no input files");
        return Ok(false);
    }
    let Some(output) = split.output.clone() else {
        eprintln!("cc-dso-minix: no -o for a link");
        return Ok(false);
    };

    link(
        &Link {
            root,
            arch,
            runner: &runner,
            lld: &lld,
            cc,
            compile: &split.compile,
            argv: &argv,
            output: Path::new(&output),
            work: &work,
            cxx,
            dry_run,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_source_goes_to_the_compile_half_and_a_library_to_the_link_half() {
        // A joined `-lfoo` is not one of the value-taking spellings, so it is a
        // compile-class token; the link walk in `link` still passes it through.
        let split = classify(&words("-O2 -I include -c src.c -L lib -lfoo -o out"));
        assert_eq!(split.compile, vec!["-O2", "-I", "include", "-c", "-lfoo"]);
        assert_eq!(split.link, vec!["-L", "lib"]);
        assert_eq!(split.inputs, vec!["src.c"]);
        assert_eq!(split.output.as_deref(), Some("out"));
    }

    #[test]
    fn a_wl_option_taking_a_value_takes_the_next_token() {
        let split = classify(&words("-Wl,--version-script -Wl,script.map foo.c"));
        // meson keeps the script as the next argument, not comma-joined.
        let split2 = classify(&words("-Wl,--version-script script.map foo.c"));
        assert_eq!(split.inputs, vec!["foo.c"]);
        assert_eq!(split2.inputs, vec!["foo.c"]);
        assert_eq!(split2.link, vec!["-Wl,--version-script", "script.map"]);
        assert!(!split.link.is_empty());
    }

    #[test]
    fn relro_is_dropped_and_wl_is_expanded() {
        assert_eq!(
            link_args(&words("-Wl,-soname,libx.so -z relro -shared -lm")),
            vec!["-soname", "libx.so", "-lm"]
        );
        assert_eq!(link_args(&words("-Wl,-z,relro,-z,now")), vec!["-z", "now"]);
    }

    #[test]
    fn a_pic_compile_keeps_the_target_and_adds_fpic() {
        let flags = pic_cflags(crate::arch::DEFAULT);
        assert!(flags.iter().any(|flag| flag == "--target=x86_64-unknown-none"));
        assert!(flags.iter().any(|flag| flag == "-fPIC"));
        assert!(!flags.iter().any(|flag| flag == "-fno-pic"));
        assert!(flags.iter().any(|flag| flag == "-fno-stack-protector"));
    }

    #[test]
    fn cxx_headers_come_before_the_generated_config() {
        let root = Path::new("root");
        let includes = cxx_include_flags(root);
        assert_eq!(includes.first().map(String::as_str), Some("-nostdinc++"));
        let config = cxx_config_flags(root, crate::arch::DEFAULT);
        assert!(config[1].contains("libcxx-build"));
        assert!(config[1].ends_with("v1"));
    }
}
