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
mod fetch_stage1;
mod guest;
mod image;
mod paths;
mod probe;
mod qmp;
mod rust_config;
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
        Some(("image", matches)) => run_image(&root, matches).map(|()| true),
        Some(("fetch-stage1", matches)) => {
            fetch_stage1::run(&root, matches.get_flag("force")).map(|()| true)
        }
        Some(("verify-stage1", _)) => verify_stage1::run(&root).map(|()| true),
        Some(("probe", matches)) => run_probe(&root, matches),
        Some(("rust-config", matches)) => {
            let token = matches
                .get_one::<String>("target")
                .expect("clap defaults the target");
            rust_config::run(&root, token).map(|()| true)
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
        Some(("c-hello", matches)) => build::c_hello::run(root, target(matches)),
        Some(("std-hello", matches)) => build::std_hello::run(root, target(matches)),
        Some(("dynlibc", matches)) => build::dynlibc::run(root, target(matches)),
        Some(("dynlink", matches)) => build::dynlink::run(root, target(matches)),
        Some(("libcxx", matches)) => build::libcxx::run(root, target(matches)).map(|()| true),
        Some(("libcxx-toolchain", matches)) => {
            build::libcxx::toolchain_command(root, target(matches)).map(|()| true)
        }
        Some(("gltriangle", matches)) => build::mesa::gltriangle(root, target(matches)),
        Some(("gui", matches)) => build::mesa::gui(root, target(matches)),
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

fn run_probe(root: &Path, matches: &clap::ArgMatches) -> io::Result<bool> {
    match matches.subcommand() {
        Some(("read-hist", matches)) => {
            let trace = matches
                .get_one::<String>("trace")
                .expect("clap requires the trace");
            probe::read_hist::read_hist(Path::new(trace), matches.get_one::<u64>("max").copied())
        }
        Some(("rc", _)) => probe::rc::run(root),
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
        Some(("wldemo", _)) => probe::wldemo::run(root),
        Some(("wldmabuf", matches)) => probe::wldmabuf::run(root, &probe_words(matches)),
        Some(("wldmabufgl", matches)) => probe::wldmabufgl::run(root, &probe_words(matches)),
        Some(("wlfull", matches)) => probe::wlfull::run(root, &probe_words(matches)),
        Some(("wlmotion", _)) => probe::wlmotion::run(root),
        Some(("wlpanel", _)) => probe::wlpanel::run(root),
        Some(("wlcompose", matches)) => probe::wlcompose::run(root, &probe_words(matches)),
        Some(("wlscene", matches)) => probe::wlscene::run(root, &probe_words(matches)),
        Some(("wlkey", _)) => probe::wlkey::run(root),
        Some(("wlfocus", _)) => probe::wlfocus::run(root),
        Some(("wlpointer", _)) => probe::wlpointer::run(root),
        _ => unreachable!("clap requires a probe subcommand"),
    }
}

/// The trailing QEMU words a recipe passed after `--`.
fn probe_words(matches: &clap::ArgMatches) -> Vec<String> {
    matches
        .get_many::<String>("qemu")
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

/// A gate that boots the image with a QEMU line the recipe owns, because the
/// devices and the disk image differ per arch and per gate.
fn guest_command_arg() -> clap::Arg {
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
                    clap::Command::new("c-hello")
                        .about("The C smoke binaries: /bin/helloc, /bin/ctest, /bin/cabi")
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
                    clap::Command::new("wldemo")
                        .about("The widget layer: a field, a list and a button take their keys"),
                )
                .subcommand(
                    clap::Command::new("wldmabuf")
                        .about("A render-node object handed over as a linux-dmabuf buffer and scanned out")
                        .arg(guest_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wldmabufgl")
                        .about("A GL client whose render target is the linux-dmabuf buffer scanned out")
                        .arg(guest_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wlfull")
                        .about("A whole-output wl_shm frame presented by re-pointing the scanout")
                        .arg(guest_command_arg()),
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
                        .arg(guest_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wlscene")
                        .about("Two dma-buf surfaces composed on the GPU and scanned out")
                        .arg(guest_command_arg()),
                )
                .subcommand(
                    clap::Command::new("wlkey")
                        .about("A host key reaches a Wayland client, and the keymap arrives"),
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
