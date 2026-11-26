//! Build GNU bash for a minix target from the pinned upstream commit.
//!
//! bash is the port's largest C consumer and its broadest test of the C surface:
//! 209 objects, its own configure, and everything from termios to globbing. It is
//! also the reason several of the C headers and libc entry points exist at all
//! (`.notes/c_build.md` records which). This is the build that keeps them honest:
//! it fetches bash at the pinned commit, builds minix-libc with the *same* stage1
//! the link uses, configures bash against the port's headers, links it, and leaves
//! the result at `target/bash/<arch>/bash`.
//!
//! The `cc` those steps call is [`crate::cc::minix`], named as a binary
//! (`<this xtask> cc minix <arch>`), so bash and `just coreutils-aarch64` cannot
//! drift apart. The build tree, the artifact and the logs are per arch: two arches
//! sharing a build directory would configure and link each other's objects.
//!
//! The host has to be POSIX: the rlib, the stage1 and clang must all be the same
//! host's (`.notes/c_build.md`). On Windows this re-enters WSL and runs the Linux
//! build of this same binary there; `MINIX_WSL_DISTRO` picks the distribution.
//!
//! Deliberately not a `BOOT_BINS` entry: an image that always carries a 1.4 MB
//! bash would build only where bash had been built, and every other recipe would
//! inherit the dependency. `just test-bash` injects it through `MINIXFS_EXTRA`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::arch::{self, Arch};
use crate::cc;
use crate::pin;
use crate::toolchain;

use super::{git_output, have_commit, run_git};

const SRC: &str = "target/bash-src";
/// Two configure knobs a *static* libc makes necessary, both of which upstream
/// never sees because a shared libc interposes instead (`.notes/c_build.md` has
/// the link errors each one fixes): `--without-bash-malloc` keeps bash's
/// `lib/malloc` and the libc's malloc out of the same link (lld refuses the
/// duplicate), and `-DNEED_EXTERN_PC` tells readline that bash's bundled termcap
/// declares `PC`/`BC`/`UP`.
const CFLAGS: &str = "-DNEED_EXTERN_PC";
/// The host tools (`mkbuiltins`, `mksignames`, ...) are built by `CC_FOR_BUILD`
/// against `buildconf.h`, which has no `HAVE_STDBOOL_H`: `bashansi.h` then writes
/// `typedef unsigned char bool`, which a C23 compiler rejects (gcc 16 defaults to
/// it, clang to gnu17). Either compiler works; both need to be told C17.
const CC_FOR_BUILD_COMPILERS: [&str; 2] = ["gcc", "clang"];
const CC_FOR_BUILD_STD: &str = "-std=gnu17";
/// A shell below this is not the shell: the artifact is ~1.4 MB.
const MIN_SIZE: u64 = 200_000;
const LOG_TAIL: usize = 25;
const LOG_ERRORS: usize = 20;

pub fn run(
    root: &Path,
    target: Option<&str>,
    force: bool,
    jobs: Option<u64>,
) -> io::Result<bool> {
    let arches: Vec<&Arch> = match target {
        None => vec![arch::DEFAULT],
        Some("all") => arch::ALL.to_vec(),
        Some(token) => vec![arch::resolve(token)?],
    };

    if cfg!(windows) {
        return reenter(root, target, force, jobs);
    }

    let jobs = jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|count| count.get() as u64)
            .unwrap_or(4)
    });
    for arch in arches {
        if !build(root, arch, force, jobs)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn build(root: &Path, arch: &Arch, force: bool, jobs: u64) -> io::Result<bool> {
    let commit = fetch_source(root, force)?;
    build_libc(root, arch)?;
    let cc = cc_command(arch)?;
    configure(root, arch, force, &commit, &cc)?;
    make(root, arch, jobs, &cc)?;
    publish(root, arch)
}

// ------------------------------------------------------------------ Windows host

/// Re-enter WSL and run this command there: the build needs a POSIX host.
///
/// The source, the build tree and the artifact all live under `target/` in the
/// repository, which WSL reaches as a mounted path, so the inner run works on the
/// same files, and its `rust/build/<host>/stage1` is the Linux one.
///
/// The inner shell is a *login* shell, which is what puts `~/.cargo/bin` on PATH:
/// a non-login `cargo` straight from `wsl.exe` has no toolchain. `.xtask/linux`
/// and not `.xtask`: a Windows `cargo clean` cannot delete a running binary, and
/// the two hosts' fingerprints in one directory would keep invalidating each
/// other.
fn reenter(root: &Path, target: Option<&str>, force: bool, jobs: Option<u64>) -> io::Result<bool> {
    let Some(wsl) = toolchain::find_on_path("wsl") else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "this host is Windows and has no `wsl` to build in; bash's build needs a POSIX host (see .notes/c_build.md)",
        ));
    };
    let distro = std::env::var("MINIX_WSL_DISTRO").unwrap_or_else(|_| "FedoraLinux-44".to_string());
    let mut inner = format!(
        "cd {} && cargo run --quiet --package xtask --target-dir .xtask/linux -- build bash",
        quote(&wsl_path(root))
    );
    if let Some(token) = target {
        inner.push(' ');
        inner.push_str(&quote(token));
    }
    if force {
        inner.push_str(" --force");
    }
    if let Some(jobs) = jobs {
        inner.push_str(&format!(" --jobs {jobs}"));
    }

    println!(
        "[bash] Windows host: running the build in WSL ({distro}). Set MINIX_WSL_DISTRO to use another distribution."
    );
    let status = Command::new(wsl)
        .args(["-d", &distro, "--", "bash", "-lc", &inner])
        .status()?;
    if !status.success() {
        println!(
            "[bash] the WSL build failed ({status}). If the distribution is not installed, set MINIX_WSL_DISTRO."
        );
    }
    Ok(status.success())
}

/// `path` as WSL sees it: `C:\a\b` is `/mnt/c/a/b`.
pub(crate) fn wsl_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let bytes = text.as_bytes();
    if bytes.len() > 1 && bytes[1] == b':' {
        return format!("/mnt/{}{}", text[..1].to_lowercase(), &text[2..]);
    }
    text
}

/// One word for a POSIX shell, in single quotes.
pub(crate) fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

// ------------------------------------------------------------------ source

fn src(root: &Path) -> PathBuf {
    root.join(SRC)
}

fn bash_dir(root: &Path, arch: &Arch) -> PathBuf {
    root.join("target").join("bash").join(arch.name)
}

fn build_dir(root: &Path, arch: &Arch) -> PathBuf {
    bash_dir(root, arch).join("build")
}

/// What `publish` writes and `just test-bash` injects.
fn artifact(root: &Path, arch: &Arch) -> PathBuf {
    bash_dir(root, arch).join("bash")
}

fn configure_log(root: &Path, arch: &Arch) -> PathBuf {
    bash_dir(root, arch).join("configure.log")
}

fn make_log(root: &Path, arch: &Arch) -> PathBuf {
    bash_dir(root, arch).join("make.log")
}

/// Which commit the tree was configured for: a pin change means the tree on disk
/// is not the tree the config describes.
fn build_marker(root: &Path, arch: &Arch) -> PathBuf {
    build_dir(root, arch).join(".minixrs-commit")
}

/// Check bash out at the pinned commit, under `target/bash-src`.
///
/// Cloned under `target/` rather than at the repository root so a build is
/// reproducible from a clean checkout, and with `core.autocrlf=false` so the line
/// endings are the ones git stores: a Windows checkout with autocrlf writes
/// `configure` with CRLF, and `/bin/sh^M` is not a shell (`.notes/c_build.md`,
/// trap 1).
///
/// The returned string is the commit that was checked out.
fn fetch_source(root: &Path, force: bool) -> io::Result<String> {
    let pins = pin::load(root)?;
    let commit = std::env::var("MINIXRS_BASH_COMMIT")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or(pins.get("bash.commit")?.to_string());
    let url = std::env::var("MINIXRS_BASH_GIT")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or(pins.get("bash.git")?.to_string());
    let describe = pins.get("bash.describe")?.to_string();

    let src = src(root);

    if force && src.is_dir() {
        fs::remove_dir_all(&src)?;
    }
    if !src.join(".git").is_dir() {
        if let Some(parent) = src.parent() {
            fs::create_dir_all(parent)?;
        }
        if !run_git(&[
            "clone".to_string(),
            url.clone(),
            src.to_string_lossy().into_owned(),
        ])? {
            return Err(io::Error::other(format!("cloning {url} failed")));
        }
    } else if !have_commit(&src, &commit)? {
        println!("[bash] the pinned commit is not in the local clone; fetching");
        if !run_git(&[
            "-C".to_string(),
            src.to_string_lossy().into_owned(),
            "fetch".to_string(),
            "origin".to_string(),
        ])? {
            return Err(io::Error::other("fetching bash failed"));
        }
    }

    if !run_git(&[
        "-C".to_string(),
        src.to_string_lossy().into_owned(),
        "checkout".to_string(),
        "--force".to_string(),
        commit.clone(),
    ])? {
        return Err(io::Error::other(format!(
            "checking out {commit} in {} failed (is the commit published?)",
            src.display()
        )));
    }

    let head = git_output(&[
        "-C".to_string(),
        src.to_string_lossy().into_owned(),
        "rev-parse".to_string(),
        "HEAD".to_string(),
    ])?;
    if head.trim() != commit {
        return Err(io::Error::other(format!(
            "{} is at {}, not the pinned {commit}",
            src.display(),
            head.trim()
        )));
    }

    // make regenerates what is older than its input. Git writes a checkout in
    // index order, which happens to leave `configure` newer than configure.ac, but
    // that is luck, and it costs a build host autoconf (or bison) when it goes the
    // other way. Both pairs are generated files that ship in the tree, so their
    // mtimes are the only thing to fix.
    for generated in ["configure", "y.tab.c"] {
        touch(&src.join(generated))?;
    }
    println!("[bash] source at {describe} ({}), {}", &commit[..12], src.display());
    Ok(commit)
}

/// Set a file's time slightly ahead of now, so make does not regenerate it.
///
/// The mtime is the only thing that stops make from running autoconf (or bison)
/// on a checkout whose generated files happen to be older than their inputs. A
/// `drvfs` mount (a Windows path seen from WSL) truncates a set mtime down to the
/// whole second, so "now" can come out *older* than a file git wrote later in the
/// same second, which is how `--force` found make asking for autoconf. Two seconds
/// of margin is invisible to everything else and cannot be truncated into the
/// past.
fn touch(path: &Path) -> io::Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let file = fs::File::options().write(true).open(path)?;
    let when = SystemTime::now() + std::time::Duration::from_secs(2);
    file.set_times(fs::FileTimes::new().set_accessed(when).set_modified(when))
}

// ------------------------------------------------------------------ libc

/// Build minix-libc with this host's stage1.
///
/// The rlib records the `core` it was built against, so a host that links with a
/// different stage1 gets E0460 ("found possibly newer version of crate core"), and
/// cargo cannot tell: the other host's build fingerprint says its rlib is current
/// while this host's link needs a different one. So the rlibs are deleted first and
/// rebuilt, in this order, with no build from another host in between.
fn build_libc(root: &Path, arch: &Arch) -> io::Result<()> {
    let rustc = toolchain::find_stage1_rustc(root).ok_or_else(toolchain::missing_stage1)?;
    let deps = arch.libc_deps(root);
    if deps.is_dir() {
        for entry in fs::read_dir(&deps)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.starts_with("libminix_libc-") && name.ends_with(".rlib") {
                fs::remove_file(&path)?;
            }
        }
    }
    println!("[bash] minix-libc for {} with {}", arch.triple, rustc.display());
    let mut command = Command::new(toolchain::cargo());
    command
        .args(["build", "-p", "minix-libc", "--target", arch.triple, "--release"])
        .current_dir(root)
        .env("RUSTC", &rustc);
    if !cc::run(&mut command)? {
        return Err(io::Error::other("building minix-libc failed"));
    }
    println!("[bash] minix-libc rebuilt with this host's stage1");
    Ok(())
}

// ------------------------------------------------------------------ configure + make

fn configured_for(root: &Path, arch: &Arch) -> io::Result<String> {
    let marker = build_marker(root, arch);
    Ok(match fs::read_to_string(&marker) {
        Ok(text) => text.trim().to_string(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    })
}

/// Run bash's configure out-of-tree, if the tree is not configured for the pin.
fn configure(
    root: &Path,
    arch: &Arch,
    force: bool,
    commit: &str,
    cc: &str,
) -> io::Result<()> {
    let build = build_dir(root, arch);
    if !force && build.join("config.status").is_file() && configured_for(root, arch)? == commit {
        println!("[bash] configure: reusing the existing config.status");
        return Ok(());
    }
    if force && build.is_dir() {
        fs::remove_dir_all(&build)?;
    }
    fs::create_dir_all(&build)?;

    let host = cc_for_build()?;
    let mut command = Command::new(src(root).join("configure"));
    command
        .arg(format!("--host={}", arch.clang_target()))
        .args(["--disable-nls", "--without-bash-malloc"])
        .current_dir(&build)
        .env("CC", cc)
        .env("CC_FOR_BUILD", &host)
        .env("CFLAGS", CFLAGS);
    if let Some(lld) = toolchain::find_lld(root) {
        command.env("LD", lld);
    }
    println!("[bash] CC={cc}  CC_FOR_BUILD={host}");
    let log = configure_log(root, arch);
    if !run_logged(&mut command, &log)? {
        return Err(io::Error::other(format!(
            "configure failed; {}",
            log_failure(&log)
        )));
    }
    fs::write(build_marker(root, arch), commit)?;
    println!("[bash] configure ok (log: {})", log.display());
    Ok(())
}

/// Link bash. The relink is forced: make compares the shell against its objects,
/// and the rlib is a prerequisite of none of them, so a libc change alone leaves
/// every object newer and make reports success without relinking
/// (`.notes/c_build.md`, trap 2).
fn make(root: &Path, arch: &Arch, jobs: u64, cc: &str) -> io::Result<()> {
    let build = build_dir(root, arch);
    let previous = build.join("bash");
    if previous.exists() {
        fs::remove_file(&previous)?;
    }
    let host = cc_for_build()?;
    let mut command = Command::new("make");
    command
        .arg(format!("-j{jobs}"))
        .arg(format!("CC_FOR_BUILD={host}"))
        .current_dir(&build)
        .env("CC", cc);
    println!("[bash] make -j{jobs}");
    let log = make_log(root, arch);
    if !run_logged(&mut command, &log)? {
        return Err(io::Error::other(format!("make failed; {}", log_failure(&log))));
    }
    Ok(())
}

fn cc_for_build() -> io::Result<String> {
    for name in CC_FOR_BUILD_COMPILERS {
        if toolchain::find_on_path(name).is_some() {
            return Ok(format!("{name} {CC_FOR_BUILD_STD}"));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "no {} or {} on PATH for bash's host tools (CC_FOR_BUILD)",
            CC_FOR_BUILD_COMPILERS[0], CC_FOR_BUILD_COMPILERS[1]
        ),
    ))
}

/// Run `command` with its output in `log`, echoing the command line first.
fn run_logged(command: &mut Command, log: &Path) -> io::Result<bool> {
    println!("[bash] {}", cc::command_line(command));
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(log)?;
    let copy = file.try_clone()?;
    let status = command
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(copy))
        .status()?;
    Ok(status.success())
}

/// A failed build's tail, plus the error lines the tail scrolled past.
///
/// `make -j` prints the failing object's diagnostics and only then the warnings
/// from the jobs it waits for, so a 25-line tail can hide every error: four of
/// `bashline.c`'s did exactly that in CI, and the run read as warnings that
/// happened to stop the build. The tail is still printed first, because the error
/// lines alone lose the source excerpt under them.
fn log_failure(log: &Path) -> String {
    let text = match fs::read_to_string(log) {
        Ok(text) => text,
        Err(_) => return format!("tail of {}:\n({} was not written)", log.display(), log.display()),
    };
    let lines: Vec<&str> = text.lines().collect();
    let tail_start = lines.len().saturating_sub(LOG_TAIL);
    let tail = &lines[tail_start..];
    let mut report = format!("tail of {}:\n{}", log.display(), tail.join("\n"));
    let errors: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.contains(": error: ") || line.contains(": fatal error: "))
        .filter(|line| !tail.contains(line))
        .take(LOG_ERRORS)
        .collect();
    if !errors.is_empty() {
        report.push_str("\nerror lines above the tail:\n");
        report.push_str(&errors.join("\n"));
    }
    report
}

// ------------------------------------------------------------------ publish

fn publish(root: &Path, arch: &Arch) -> io::Result<bool> {
    let built = build_dir(root, arch).join("bash");
    if !built.is_file() {
        return Err(io::Error::other(format!(
            "{} was not produced: make ran but linked nothing",
            built.display()
        )));
    }
    let data = fs::read(&built)?;
    if data.len() < 20 || &data[..4] != b"\x7fELF" {
        return Err(io::Error::other(format!(
            "{} is not an ELF ({:?})",
            built.display(),
            &data[..4.min(data.len())]
        )));
    }
    let machine = u16::from_le_bytes([data[18], data[19]]);
    if machine != arch.e_machine {
        return Err(io::Error::other(format!(
            "{} is ELF machine 0x{machine:x}, not {} (0x{:x}): the build did not use the target's flags",
            built.display(),
            arch.triple,
            arch.e_machine
        )));
    }
    let size = data.len() as u64;
    if size < MIN_SIZE {
        return Err(io::Error::other(format!(
            "{} is {size} bytes, which is too small to be the shell",
            built.display()
        )));
    }
    let out = artifact(root, arch);
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&built, &out)?;
    println!("[bash] wrote {} ({size} bytes, {})", out.display(), arch.triple);
    Ok(true)
}

/// `<this xtask> cc minix <arch>`, the `cc` configure and make call.
fn cc_command(arch: &Arch) -> io::Result<String> {
    let exe = std::env::current_exe()?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    Ok(format!("{} cc minix {}", exe.display(), arch.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_path_becomes_the_mount_wsl_sees() {
        assert_eq!(
            wsl_path(Path::new("C:\\Users\\T\\src\\repo")),
            "/mnt/c/Users/T/src/repo"
        );
        assert_eq!(wsl_path(Path::new("/home/t/repo")), "/home/t/repo");
    }

    #[test]
    fn a_quoted_word_survives_a_single_quote() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn the_arch_directories_are_per_arch() {
        let root = Path::new("root");
        assert!(artifact(root, arch::DEFAULT).ends_with("target/bash/x86/bash"));
        assert!(build_dir(root, &arch::AARCH64).ends_with("target/bash/aarch64/build"));
        assert_eq!(
            configure_log(root, &arch::RISCV64).file_name().and_then(|name| name.to_str()),
            Some("configure.log")
        );
    }

    #[test]
    fn a_missing_log_says_so_rather_than_printing_nothing() {
        let missing = std::env::temp_dir().join("minixrs-xtask-no-such-make.log");
        let _ = fs::remove_file(&missing);
        let report = log_failure(&missing);
        assert!(report.contains("was not written"));
    }
}
