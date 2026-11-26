//! Host-side build and gate tooling for minixrs.
//!
//! Run through the `xtask` alias in `.cargo/config.toml`:
//!
//! ```text
//! cargo xtask check static
//! cargo xtask --list
//! ```
//!
//! The repository root comes from this crate's manifest directory rather than
//! the working directory, so a command behaves the same wherever it is
//! invoked. The alias adds `--target-dir .xtask` for the same class of reason:
//! `bootstrap` runs `cargo clean` on `target/`, and on Windows a build tool
//! cannot delete the executable it is running from.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod arch;
mod build;
mod cc;
mod check;
mod dynamic;
mod fetch;
mod fetch_stage1;
mod gate;
mod guest;
mod image;
mod llvm_config;
mod mirror;
mod minix_ld;
mod paths;
mod pin;
mod probe;
mod push_status;
mod qmp;
mod rust_config;
mod smoke;
mod toolchain;
mod verify_stage1;

fn main() -> ExitCode {
    let matches = command().get_matches();

    if matches.get_flag("list") {
        print_list(&command(), "");
        return ExitCode::SUCCESS;
    }

    let root = root();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    let result = match matches.subcommand() {
        Some(("check", matches)) => run_check(&root, matches, &mut out, &mut err),
        Some(("build", matches)) => run_build(&root, matches),
        Some(("cc", matches)) => run_cc(&root, matches),
        Some(("image", matches)) => run_image(&root, matches).map(|()| true),
        Some(("fetch-stage1", matches)) => {
            fetch_stage1::run(&root, matches.get_flag("force")).map(|()| true)
        }
        Some(("fetch", matches)) => run_fetch(&root, matches),
        Some(("pin", matches)) => pin::run(&root, matches).map(|()| true),
        Some(("mirror", matches)) => mirror::run(&root, matches),
        Some(("push-status", matches)) => push_status::run(
            &root,
            matches.get_flag("fetch"),
            matches.get_flag("commits"),
            matches.get_flag("table"),
        ),
        Some(("verify-stage1", _)) => verify_stage1::run(&root).map(|()| true),
        Some(("probe", matches)) => run_probe(&root, matches),
        Some(("gate", matches)) => {
            if matches.get_flag("list") {
                gate::list(&mut out)
            } else {
                let name = matches
                    .get_one::<String>("name")
                    .expect("clap requires the gate name");
                let arch = matches
                    .get_one::<String>("arch")
                    .expect("clap defaults the arch");
                let timeout = matches.get_one::<u64>("timeout").copied();
                gate::run(
                    &root,
                    name,
                    arch,
                    timeout,
                    matches.get_flag("dry-run"),
                )
            }
        }
        Some(("gate-arches", _)) => gate::run_arches(&root),
        Some(("smoke", matches)) => {
            let log = matches
                .get_one::<String>("log")
                .expect("clap requires the log");
            let seconds = *matches
                .get_one::<u64>("seconds")
                .expect("clap requires the seconds");
            smoke::run(
                &root,
                Path::new(log),
                seconds,
                &mut probe::command_from(&trailing_words(matches)),
            )
        }
        Some(("rust-config", matches)) => {
            let token = matches
                .get_one::<String>("target")
                .expect("clap defaults the target");
            rust_config::run(&root, token).map(|()| true)
        }
        Some(("minix-ld", matches)) => run_minix_ld(&root, matches),
        Some(("llvm-config", matches)) => {
            let build_root = matches
                .get_one::<String>("build-root")
                .map(String::as_str);
            let arch = matches
                .get_one::<String>("arch")
                .expect("clap defaults the arch");
            let words: Vec<String> = matches
                .get_many::<String>("query")
                .map(|values| values.cloned().collect())
                .unwrap_or_default();
            llvm_config::run(&root, build_root, arch, &words)
        }
        _ => unreachable!("clap requires a subcommand"),
    };

    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/xtask sits two levels under the repository root")
        .to_path_buf()
}

fn run_check(
    root: &Path,
    matches: &clap::ArgMatches,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<bool> {
    match matches.subcommand() {
        Some(("static", _)) => check::run_static(root, out, err),
        Some(("headers", matches)) => {
            check::c_headers::run(root, matches.get_flag("verbose"), out)
        }
        Some(("physmap", _)) => check::physmap::run(root, out),
        Some(("justfile", _)) => check::justfile::run(root, out, err),
        _ => unreachable!("clap requires a check subcommand"),
    }
}

fn run_build(root: &Path, matches: &clap::ArgMatches) -> io::Result<bool> {
    match matches.subcommand() {
        Some(("bash", matches)) => build::bash::run(
            root,
            target(matches),
            matches.get_flag("force"),
            matches.get_one::<u64>("jobs").copied(),
        ),
        Some(("c-hello", matches)) => build::c_hello::run(root, target(matches)),
        Some(("c-direct", matches)) => build::c_direct::run(root, target(matches)),
        Some(("doom", matches)) => build::doom::run(root, target(matches)),
        Some(("libc-static", matches)) => build::libc_static::run(root, target(matches)),
        Some(("std-hello", matches)) => build::std_hello::run(root, target(matches)),
        Some(("dynlibc", matches)) => build::dynlibc::run(root, target(matches)),
        Some(("dynlink", matches)) => build::dynlink::run(root, target(matches)),
        Some(("libcxx", matches)) => build::libcxx::run(root, target(matches)).map(|()| true),
        Some(("libcxx-toolchain", matches)) => {
            build::libcxx::toolchain_command(root, target(matches)).map(|()| true)
        }
        Some(("llvm-host-tools", _)) => build::llvm_host::run(root).map(|()| true),
        Some(("llvm-minix", matches)) => {
            build::llvm_minix::run(root, target(matches)).map(|()| true)
        }
        Some(("rustc-minix", matches)) => build::rustc_minix::run(root, target(matches)),
        Some(("mesa", matches)) => build::mesa_tree::run(
            root,
            &build::mesa_tree::Options {
                target: target(matches),
                force: matches.get_flag("force"),
                build: matches.get_flag("build"),
                stage_only: matches.get_flag("stage"),
            },
        ),
        Some(("gltriangle", matches)) => build::mesa::gltriangle(root, target(matches)),
        Some(("gui", matches)) => build::mesa::gui(root, target(matches)),
        Some(("gbmtest", matches)) => build::mesa::gbmtest(root, target(matches)),
        Some(("wlgl", matches)) => build::mesa::wlgl(root, target(matches)),
        Some(("wlcompose", matches)) => build::mesa::wlcompose(root, target(matches)),
        Some(("wldmabufgl", matches)) => build::mesa::wldmabufgl(root, target(matches)),
        Some(("cdyn", matches)) => {
            let Some(source) = matches.get_one::<String>("source") else {
                unreachable!("clap requires a source")
            };
            let name = matches.get_one::<String>("name").map(String::as_str);
            build::cdyn::run(root, target(matches), Path::new(source), name)
        }
        _ => unreachable!("clap requires a build subcommand"),
    }
}

fn target(matches: &clap::ArgMatches) -> Option<&str> {
    matches.get_one::<String>("target").map(String::as_str)
}

fn run_fetch(root: &Path, matches: &clap::ArgMatches) -> io::Result<bool> {
    match matches.subcommand() {
        Some(("bash", matches)) => {
            fetch::bash(root, target(matches), matches.get_flag("force"))
        }
        Some(("mesa", matches)) => {
            fetch::mesa(root, target(matches), matches.get_flag("force"))
        }
        _ => unreachable!("clap requires a fetch subcommand"),
    }
}

fn run_image(root: &Path, matches: &clap::ArgMatches) -> io::Result<()> {
    match matches.subcommand() {
        Some(("mkboot", matches)) => image::mkboot(
            root,
            matches.get_one::<String>("features").map(String::as_str),
            matches.get_one::<String>("stem").map(String::as_str),
        ),
        Some(("mkfs", matches)) => image::mkfs(root, target(matches)),
        _ => unreachable!("clap requires an image subcommand"),
    }
}

/// The link rule a foreign CMake passes `<OBJECTS>` and `<LINK_LIBRARIES>` to.
fn run_minix_ld(_root: &Path, matches: &clap::ArgMatches) -> io::Result<bool> {
    let text = |name: &str| -> &str {
        matches
            .get_one::<String>(name)
            .map(String::as_str)
            .unwrap_or_else(|| panic!("clap requires {name}"))
    };
    let words: Vec<String> = matches
        .get_many::<String>("args")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    minix_ld::run(
        &minix_ld::Link {
            arch: text("arch"),
            out: Path::new(text("out")),
            crt0: Path::new(text("crt0")),
            libc: Path::new(text("libc")),
            libstdcxx: Path::new(text("libstdcxx")),
            script: Path::new(text("script")),
            lld: Path::new(text("lld")),
        },
        &words,
    )
}

fn run_cc(root: &Path, matches: &clap::ArgMatches) -> io::Result<bool> {
    let Some((name, matches)) = matches.subcommand() else {
        unreachable!("clap requires a cc subcommand")
    };
    let words: Vec<String> = matches
        .get_many::<String>("args")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    let dry_run = matches.get_flag("dry-run");
    let (arch, args) = cc_arch(&words);
    match name {
        "minix" => cc::minix::run(root, arch, &args, dry_run),
        "dso" => cc::dso::run(root, arch, &args, dry_run),
        _ => unreachable!("clap requires a cc subcommand"),
    }
}

/// The optional leading target of a `cc` line: a bare word, so it cannot be
/// confused with a flag or an input file. With none, the line is the whole line
/// and the target is the default.
fn cc_arch(words: &[String]) -> (&'static arch::Arch, Vec<String>) {
    if let Some(first) = words.first()
        && let Ok(arch) = arch::resolve(first)
    {
        return (arch, words[1..].to_vec());
    }
    (arch::DEFAULT, words.to_vec())
}

fn dry_run_arg() -> clap::Arg {
    clap::Arg::new("dry-run")
        .long("dry-run")
        .action(clap::ArgAction::SetTrue)
        .help("Print the commands instead of running them")
}

/// The compiler command line, as the foreign build gives it.
fn cc_args() -> clap::Arg {
    clap::Arg::new("args")
        .value_name("ARG")
        .required(true)
        .num_args(1..)
        .trailing_var_arg(true)
        .allow_hyphen_values(true)
        .help("The compiler command line, an optional target first")
}

fn run_probe(root: &Path, matches: &clap::ArgMatches) -> io::Result<bool> {
    match matches.subcommand() {
        Some(("read-hist", matches)) => {
            let trace = matches
                .get_one::<String>("trace")
                .expect("clap requires the trace");
            probe::read_hist::read_hist(Path::new(trace), matches.get_one::<u64>("max").copied())
        }
        Some(("rc", _)) => probe::rc::run(root),
        Some(("alloc", matches)) => probe::alloc::run(
            root,
            matches.get_one::<String>("arch").expect("clap defaults the arch"),
            matches
                .get_one::<String>("memory")
                .expect("clap defaults the memory"),
        ),
        Some(("fb", _)) => probe::fb::run(root),
        Some(("exec-loop", matches)) => probe::exec_loop::run(
            root,
            &probe::exec_loop::Options {
                arch: matches.get_one::<String>("arch").expect("clap defaults the arch"),
                memory: matches
                    .get_one::<String>("memory")
                    .expect("clap defaults the memory"),
                execs: *matches.get_one::<u64>("execs").expect("clap defaults the count"),
                sample_every: *matches
                    .get_one::<u64>("sample")
                    .expect("clap defaults the interval"),
                command: matches
                    .get_one::<String>("command")
                    .expect("clap defaults the command"),
                marker: matches
                    .get_one::<String>("marker")
                    .expect("clap defaults the marker"),
                complete: matches
                    .get_one::<String>("complete")
                    .expect("clap defaults the completion marker"),
            },
        ),
        Some(("dso-share", matches)) => probe::dso_share::run(
            root,
            matches.get_one::<String>("arch").expect("clap defaults the arch"),
            matches
                .get_one::<String>("memory")
                .expect("clap defaults the memory"),
            matches
                .get_one::<String>("dynclib")
                .expect("clap defaults the dynclib"),
        ),
        Some(("interactive-bash", matches)) => probe::interactive_bash::run(
            Path::new(
                matches
                    .get_one::<String>("log")
                    .expect("clap requires the log"),
            ),
            *matches
                .get_one::<u64>("seconds")
                .expect("clap requires the seconds"),
            &mut probe::command_from(&trailing_words(matches)),
        ),
        Some(("wldemo", _)) => probe::wldemo::run(root),
        Some(("doom", matches)) => probe::doom::run(root, target(matches).unwrap_or("x86")),
        Some(("doom-save", matches)) => {
            probe::doom::run_save(root, target(matches).unwrap_or("x86"))
        }
        Some(("wldmabuf", matches)) => probe::wldmabuf::run(root, &trailing_words(matches)),
        Some(("wldmabufgl", matches)) => probe::wldmabufgl::run(root, &trailing_words(matches)),
        Some(("wlfull", matches)) => probe::wlfull::run(root, &trailing_words(matches)),
        Some(("wlmotion", _)) => probe::wlmotion::run(root),
        Some(("wlpanel", _)) => probe::wlpanel::run(root),
        Some(("wlcompose", matches)) => probe::wlcompose::run(root, &trailing_words(matches)),
        Some(("wlscene", matches)) => probe::wlscene::run(root, &trailing_words(matches)),
        Some(("wlkey", _)) => probe::wlkey::run(root),
        Some(("wlfocus", _)) => probe::wlfocus::run(root),
        Some(("wlpointer", _)) => probe::wlpointer::run(root),
        _ => unreachable!("clap requires a probe subcommand"),
    }
}

/// The trailing words a recipe passed after `--`.
fn trailing_words(matches: &clap::ArgMatches) -> Vec<String> {
    matches
        .get_many::<String>("qemu")
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

/// The arch a probe boots, named the way the recipes and images are.
fn probe_arch_arg() -> clap::Arg {
    clap::Arg::new("arch")
        .long("arch")
        .value_name("ARCH")
        .value_parser(["x86", "riscv64", "aarch64"])
        .default_value("x86")
        .help("Which arch's image to boot")
}

fn probe_memory_arg() -> clap::Arg {
    clap::Arg::new("memory")
        .value_name("MEM")
        .default_value("256M")
        .help("The guest's RAM")
}

/// A gate that boots the image with a QEMU line the recipe owns, because the
/// devices and the disk image differ per arch and per gate.
fn trailing_command_arg() -> clap::Arg {
    clap::Arg::new("qemu")
        .value_name("QEMU")
        .required(true)
        .num_args(1..)
        .trailing_var_arg(true)
        .allow_hyphen_values(true)
        .help("The QEMU command line to boot, as the recipe gives it")
}

fn command() -> clap::Command {
    clap::Command::new("xtask")
        .about("Host-side build and gate tooling for minixrs")
        .arg_required_else_help(true)
        .disable_help_subcommand(true)
        .arg(
            clap::Arg::new("list")
                .long("list")
                .action(clap::ArgAction::SetTrue)
                .help("List the commands instead of running one"),
        )
        .subcommand(
            clap::Command::new("check")
                .about("Static gates over the tree, the toolchain-free half of `just check`")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("static")
                        .about("Every static gate: the C headers, the physmap rule, the Justfile"),
                )
                .subcommand(
                    clap::Command::new("headers")
                        .about("tools/c-include against the C surface minix-libc exports")
                        .arg(
                            clap::Arg::new("verbose")
                                .long("verbose")
                                .action(clap::ArgAction::SetTrue)
                                .help("Also print the number of declarations per header"),
                        ),
                )
                .subcommand(
                    clap::Command::new("physmap")
                        .about("No physical address is cast to a pointer without the conversion"),
                )
                .subcommand(
                    clap::Command::new("justfile")
                        .about("A recipe that builds kernel-boot depends on every image input"),
                ),
        )
        .subcommand(
            clap::Command::new("build")
                .about("Build a target program with the fork toolchain")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("bash")
                        .about("GNU bash for a minix target, from the pinned upstream commit")
                        .arg(target_arg("x86"))
                        .arg(
                            clap::Arg::new("force")
                                .long("force")
                                .action(clap::ArgAction::SetTrue)
                                .help("Refetch the source and reconfigure"),
                        )
                        .arg(
                            clap::Arg::new("jobs")
                                .long("jobs")
                                .value_name("N")
                                .value_parser(clap::value_parser!(u64).range(1..))
                                .help("Parallel make jobs (default: the host's cores)"),
                        ),
                )
                .subcommand(
                    clap::Command::new("mesa")
                        .about("Fetch and build Mesa + libdrm, and stage the DSOs")
                        .arg(target_arg("x86"))
                        .arg(
                            clap::Arg::new("force")
                                .long("force")
                                .action(clap::ArgAction::SetTrue)
                                .help("Refetch the sources and set the build trees up again"),
                        )
                        .arg(
                            clap::Arg::new("build")
                                .long("build")
                                .action(clap::ArgAction::SetTrue)
                                .help("Run ninja as well as meson setup"),
                        )
                        .arg(
                            clap::Arg::new("stage")
                                .long("stage")
                                .action(clap::ArgAction::SetTrue)
                                .help("Only copy the built DSOs into target/mesa"),
                        ),
                )
                .subcommand(
                    clap::Command::new("c-hello")
                        .about("The C smoke binaries: /bin/helloc, /bin/ctest, /bin/cabi")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("c-direct")
                        .about("A C program linked by clang + LLD against libc.a, with no rustc")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("doom")
                        .about("doomgeneric against /dev/fb and /dev/kbd, with the shareware IWAD")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("libc-static")
                        .about("The C library as libc.a, for a C toolchain's own linker driver")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("std-hello")
                        .about("The std smoke binary: /bin/hello")
                        .arg(target_arg("x86_64-pc-minix")),
                )
                .subcommand(
                    clap::Command::new("dynlibc")
                        .about("The shared C library and the program linked against it")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("dynlink")
                        .about("The loader gate's objects and programs under target/dynlink")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("libcxx")
                        .about("The C++ runtime: libc++ + libc++abi as one libstdc++.a")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("libcxx-toolchain")
                        .about("Write target/cxx/toolchain-<machine>.cmake")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("llvm-host-tools")
                        .about("The LLVM tablegen programs a cross build of clang runs on the host"),
                )
                .subcommand(
                    clap::Command::new("llvm-minix")
                        .about("Cross-build clang and lld for a minix target and stage the toolchain root")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("rustc-minix")
                        .about("Cross-build the in-guest rustc and stage it into the toolchain root")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("gltriangle")
                        .about("The surfaceless-EGL GLES2 client against Mesa's DSOs")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("gui")
                        .about("The in-house renderer client, linked against Mesa's DSOs")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("gbmtest")
                        .about("The GBM create/export probe against libgbm and libdrm")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("wlgl")
                        .about("The Wayland GL present client against Mesa's DSOs")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("wlcompose")
                        .about("The GPU-composition helper and its test peer")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("wldmabufgl")
                        .about("The linux-dmabuf GL present client against Mesa's DSOs")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("cdyn")
                        .about("Link a C program of your own against /lib/libc.so")
                        .arg(
                            clap::Arg::new("source")
                                .value_name("SOURCE")
                                .required(true)
                                .help("The C file to compile"),
                        )
                        .arg(
                            clap::Arg::new("name")
                                .value_name("NAME")
                                .help("The program's name (default: the source's stem)"),
                        )
                        .arg(target_option("x86")),
                ),
        )
        .subcommand(
            clap::Command::new("cc")
                .about("The `cc` a foreign C build calls: an executable, or a shared object")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("minix")
                        .about("Compile for minix, and link an executable against the static libc")
                        .arg(dry_run_arg())
                        .arg(cc_args()),
                )
                .subcommand(
                    clap::Command::new("dso")
                        .about("Build a shared object, and a program against the port's libc.so")
                        .arg(dry_run_arg())
                        .arg(cc_args()),
                ),
        )
        .subcommand(
            clap::Command::new("minix-ld")
                .about("A CMake link rule's driver: LLD + crt0 + libc.a + libstdc++.a")
                .arg(
                    clap::Arg::new("arch")
                        .long("arch")
                        .value_name("ARCH")
                        .value_parser(["x86", "riscv64", "aarch64"])
                        .default_value("x86"),
                )
                .arg(clap::Arg::new("out").long("out").value_name("FILE").required(true))
                .arg(clap::Arg::new("crt0").long("crt0").value_name("FILE").required(true))
                .arg(clap::Arg::new("libc").long("libc").value_name("FILE").required(true))
                .arg(
                    clap::Arg::new("libstdcxx")
                        .long("libstdcxx")
                        .value_name("FILE")
                        .required(true),
                )
                .arg(clap::Arg::new("script").long("script").value_name("FILE").required(true))
                .arg(clap::Arg::new("lld").long("lld").value_name("FILE").required(true))
                .arg(cc_args()),
        )
        .subcommand(
            clap::Command::new("llvm-config")
                .about("Answer llvm-config queries for the minix LLVM cross-build, on the host")
                .arg(
                    clap::Arg::new("build-root")
                        .long("build-root")
                        .value_name("DIR")
                        .help("The LLVM build tree (default: target/llvm-<arch>/build)"),
                )
                .arg(
                    clap::Arg::new("arch")
                        .long("arch")
                        .value_name("ARCH")
                        .value_parser(["x86", "riscv64", "aarch64"])
                        .default_value("x86")
                        .help("Which arch's LLVM build tree to answer for"),
                )
                .arg(
                    clap::Arg::new("query")
                        .value_name("QUERY")
                        .num_args(0..)
                        .trailing_var_arg(true)
                        .allow_hyphen_values(true)
                        .help("llvm-config output flags, and the components a --libs query names"),
                ),
        )
        .subcommand(
            clap::Command::new("image")
                .about("Post-link image steps: the x86 trampoline and the raw disk image")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("mkboot")
                        .about("Build kernel-boot, then the trampoline with its kmain address")
                        .arg(
                            clap::Arg::new("features")
                                .value_name("FEATURES")
                                .help("Comma-joined kernel-boot features (default: embed_initramfs,x86)"),
                        )
                        .arg(
                            clap::Arg::new("stem")
                                .value_name("STEM")
                                .help("Output name: <stem>.bin and <stem>-trampoline.elf (default: kernel)"),
                        ),
                )
                .subcommand(
                    clap::Command::new("mkfs")
                        .about("Copy the kernel's MinixFS blob to the arch's raw disk image")
                        .arg(target_arg("x86")),
                ),
        )
        .subcommand(
            clap::Command::new("fetch")
                .about("Install a prebuilt third-party artifact for its pinned sources")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("bash")
                        .about("The prebuilt GNU bash for an arch")
                        .arg(target_arg("x86"))
                        .arg(force_arg()),
                )
                .subcommand(
                    clap::Command::new("mesa")
                        .about("The prebuilt Mesa and libdrm DSOs for an arch")
                        .arg(target_arg("x86"))
                        .arg(force_arg()),
                ),
        )
        .subcommand(
            clap::Command::new("pin")
                .about("Print a pinned third-party release tag or source field")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("bash")
                        .about("The bash release tag, or a source field")
                        .arg(
                            clap::Arg::new("field")
                                .value_name("FIELD")
                                .value_parser(["tag", "describe", "commit"])
                                .default_value("tag")
                                .help("tag, describe or commit"),
                        ),
                )
                .subcommand(
                    clap::Command::new("mesa")
                        .about("The Mesa release tag, or a source field")
                        .arg(
                            clap::Arg::new("field")
                                .value_name("FIELD")
                                .value_parser([
                                    "tag",
                                    "mesa-tag",
                                    "mesa-commit",
                                    "drm-tag",
                                    "drm-commit",
                                    "build-config",
                                ])
                                .default_value("tag")
                                .help("tag or one of the pinned source fields"),
                        ),
                ),
        )
        .subcommand(
            clap::Command::new("push-status")
                .about("List repositories whose commits are not on their remote")
                .arg(
                    clap::Arg::new("fetch")
                        .long("fetch")
                        .action(clap::ArgAction::SetTrue)
                        .help("Fetch every repository first"),
                )
                .arg(
                    clap::Arg::new("commits")
                        .long("commits")
                        .action(clap::ArgAction::SetTrue)
                        .help("List the unpushed commits"),
                )
                .arg(
                    clap::Arg::new("table")
                        .long("table")
                        .action(clap::ArgAction::SetTrue)
                        .help("Full table of every repository"),
                ),
        )
        .subcommand(
            clap::Command::new("mirror")
                .about("Create or refresh a third-party mirror the build clones from")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("bash")
                        .about("Refresh sdgoij/bash from savannah and push it")
                        .arg(
                            clap::Arg::new("check")
                                .long("check")
                                .action(clap::ArgAction::SetTrue)
                                .help("Only verify the pinned commit is present"),
                        ),
                ),
        )
        .subcommand(
            clap::Command::new("fetch-stage1")
                .about("Install the prebuilt stage1 toolchain for the pinned rust commit")
                .arg(
                    clap::Arg::new("force")
                        .long("force")
                        .action(clap::ArgAction::SetTrue)
                        .help("Replace an existing toolchain, even one built from source"),
                ),
        )
        .subcommand(
            clap::Command::new("verify-stage1")
                .about("Install the published stage1, then link the probe binaries with it"),
        )
        .subcommand(
            clap::Command::new("gate")
                .about("Run one QEMU gate: its scenario, its markers and its verdict")
                .arg(
                    clap::Arg::new("name")
                        .value_name("GATE")
                        .required_unless_present("list")
                        .help("The gate name, e.g. fork or coreutils-minix"),
                )
                .arg(
                    clap::Arg::new("list")
                        .long("list")
                        .action(clap::ArgAction::SetTrue)
                        .help("List the gates instead of running one"),
                )
                .arg(probe_arch_arg())
                .arg(
                    clap::Arg::new("timeout")
                        .long("timeout")
                        .value_name("SECONDS")
                        .value_parser(clap::value_parser!(u64))
                        .help("How long the guest may run (default: the gate's own)"),
                )
                .arg(
                    clap::Arg::new("dry-run")
                        .long("dry-run")
                        .action(clap::ArgAction::SetTrue)
                        .help("Print the QEMU command instead of running it"),
                ),
        )
        .subcommand(
            clap::Command::new("gate-arches")
                .about("Every arch-gated QEMU gate, in the order the arch suites use"),
        )
        .subcommand(
            clap::Command::new("smoke")
                .about("Drive a booted guest through a .tsv smoke scenario")
                .arg(
                    clap::Arg::new("log")
                        .value_name("LOG")
                        .required(true)
                        .help("Where to write the guest's console"),
                )
                .arg(
                    clap::Arg::new("seconds")
                        .value_name("SECONDS")
                        .required(true)
                        .value_parser(clap::value_parser!(u64))
                        .help("How long the guest may run"),
                )
                .arg(trailing_command_arg()),
        )
        .subcommand(
            clap::Command::new("probe")
                .about("Gate probes over a running guest and the data it produced")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("read-hist")
                        .about("Summarise a virtio_blk_handle_read trace, failing above --max")
                        .arg(
                            clap::Arg::new("trace")
                                .value_name("TRACE")
                                .required(true)
                                .help("The trace file QEMU wrote"),
                        )
                        .arg(
                            clap::Arg::new("max")
                                .long("max")
                                .value_name("REQUESTS")
                                .value_parser(clap::value_parser!(u64))
                                .help("Fail when the request count is above this ceiling"),
                        ),
                )
                .subcommand(
                    clap::Command::new("rc")
                        .about("The /etc/rc chain: the session manager, a prompt of its own, the panel"),
                )
                .subcommand(
                    clap::Command::new("alloc")
                        .about("The rt allocator's region count is flat under alloc/free churn")
                        .arg(probe_arch_arg())
                        .arg(probe_memory_arg()),
                )
                .subcommand(
                    clap::Command::new("fb")
                        .about("The framebuffer carries the red/green/blue boot pattern"),
                )
                .subcommand(
                    clap::Command::new("exec-loop")
                        .about("N execs leave the free-page and region counts flat")
                        .arg(probe_arch_arg())
                        .arg(probe_memory_arg())
                        .arg(
                            clap::Arg::new("execs")
                                .long("execs")
                                .value_name("N")
                                .value_parser(clap::value_parser!(u64).range(1..))
                                .default_value("100")
                                .help("How many execs to run"),
                        )
                        .arg(
                            clap::Arg::new("sample")
                                .long("sample")
                                .value_name("K")
                                .value_parser(clap::value_parser!(u64).range(1..))
                                .default_value("10")
                                .help("Sample the counts every K execs"),
                        )
                        .arg(
                            clap::Arg::new("command")
                                .long("command")
                                .value_name("CMD")
                                .default_value("hello")
                                .help("The line typed for each exec"),
                        )
                        .arg(
                            clap::Arg::new("marker")
                                .long("marker")
                                .value_name("TEXT")
                                .default_value("pid=")
                                .help("Output that proves the exec started"),
                        )
                        .arg(
                            clap::Arg::new("complete")
                                .long("complete")
                                .value_name("TEXT")
                                .default_value("threadstd")
                                .help("Output that proves the exec finished"),
                        ),
                )
                .subcommand(
                    clap::Command::new("dso-share")
                        .about("Two processes mapping one shared object share its read-only frames")
                        .arg(
                            clap::Arg::new("arch")
                                .long("arch")
                                .value_name("ARCH")
                                .value_parser(["x86", "riscv64"])
                                .default_value("x86")
                                .help("The page-table walk to use"),
                        )
                        .arg(
                            clap::Arg::new("memory")
                                .value_name("MEM")
                                .default_value("256M")
                                .help("The guest's RAM"),
                        )
                        .arg(
                            clap::Arg::new("dynclib")
                                .value_name("DYNCLIB")
                                .default_value("/bin/dynclib")
                                .help("The dynamic client the pipeline runs twice"),
                        ),
                )
                .subcommand(
                    clap::Command::new("interactive-bash")
                        .about("An interactive bash accepts a typed line")
                        .arg(
                            clap::Arg::new("log")
                                .value_name("LOG")
                                .required(true)
                                .help("Where to write the guest's console"),
                        )
                        .arg(
                            clap::Arg::new("seconds")
                                .value_name("SECONDS")
                                .required(true)
                                .value_parser(clap::value_parser!(u64))
                                .help("How long each wait may take"),
                        )
                        .arg(trailing_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wldemo")
                        .about("The widget layer: a field, a list and a button take their keys"),
                )
                .subcommand(
                    clap::Command::new("wldmabuf")
                        .about("A render-node object handed over as a linux-dmabuf buffer and scanned out")
                        .arg(trailing_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wldmabufgl")
                        .about("A GL client whose render target is the linux-dmabuf buffer scanned out")
                        .arg(trailing_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wlfull")
                        .about("A whole-output wl_shm frame presented by re-pointing the scanout")
                        .arg(trailing_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wlmotion")
                        .about("The pointer is interpolated across a tick, not jumped"),
                )
                .subcommand(
                    clap::Command::new("wlpanel")
                        .about("The panel is a layer surface, and its launcher takes a click"),
                )
                .subcommand(
                    clap::Command::new("wlcompose")
                        .about("Two GL surfaces composed on the render node by a helper in another process")
                        .arg(trailing_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wlscene")
                        .about("Two dma-buf surfaces composed on the GPU and scanned out")
                        .arg(trailing_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wlkey")
                        .about("A host key reaches a Wayland client, and the keymap arrives"),
                )
                .subcommand(
                    clap::Command::new("doom")
                        .about("A host key reaches Doom through the input server's /dev/kbd")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("doom-save")
                        .about("A saved game reaches the disk and survives a reboot")
                        .arg(target_arg("x86")),
                )
                .subcommand(
                    clap::Command::new("wlfocus")
                        .about("Two clients, one focus, and the keys that follow it"),
                )
                .subcommand(
                    clap::Command::new("wlpointer")
                        .about("The focus follows the window under the pointer"),
                ),
        )
        .subcommand(
            clap::Command::new("rust-config")
                .about("Regenerate the rust fork's rust/config.toml for an arch")
                .arg(
                    clap::Arg::new("target")
                        .value_name("TARGET")
                        .default_value("all")
                        .help("all, or one of x86, riscv64, aarch64"),
                ),
        )
}

fn target_option(default: &'static str) -> clap::Arg {
    clap::Arg::new("target")
        .long("target")
        .value_name("TARGET")
        .default_value(default)
        .help("An arch name or a minix triple")
}

fn force_arg() -> clap::Arg {
    clap::Arg::new("force")
        .long("force")
        .action(clap::ArgAction::SetTrue)
        .help("Replace an existing artifact, even one built from source")
}

fn target_arg(default: &'static str) -> clap::Arg {
    clap::Arg::new("target")
        .value_name("TARGET")
        .default_value(default)
        .help("An arch name or a minix triple, or `all`")
}

/// `--list` prints the tree the way `just --list` prints recipes: the command,
/// then its one-line description, indented by depth.
fn print_list(command: &clap::Command, prefix: &str) {
    for subcommand in command.get_subcommands() {
        let name = format!("{prefix}{}", subcommand.get_name());
        let about = subcommand
            .get_about()
            .map(|about| about.to_string())
            .unwrap_or_default();
        println!("{name:<28} {about}");
        print_list(subcommand, &format!("{prefix}    "));
    }
}
