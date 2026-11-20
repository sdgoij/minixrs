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
mod toolchain;

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
        _ => unreachable!("clap requires a build subcommand"),
    }
}

fn target(matches: &clap::ArgMatches) -> Option<&str> {
    matches.get_one::<String>("target").map(String::as_str)
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
                ),
        )
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
