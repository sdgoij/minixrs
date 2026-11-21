//! The flags that make a C compile for minix hermetic, the command echo the
//! build wrappers share, and the two wrappers themselves.
//!
//! Without `-nostdinc`, `clang --target=x86_64-unknown-none -I tools/c-include`
//! also searches the host's `/usr/include`, so **any header the port does not
//! provide is silently satisfied by glibc** and any question asked about it is
//! answered by the host: configure reported `HAVE_TERMIOS_H`, `HAVE_SYS_WAIT_H`
//! and `HAVE_UNION_WAIT` for a target that has none of them, and bash then
//! compiled paths no real system has. A missing header should be an error, not a
//! guess. `.notes/c_build.md` has the write-up.
//!
//! [`minix`] is the `cc` a project links an *executable* with (bash, and anything
//! else that wants the static libc), and [`dso`] the one that builds a *shared
//! object* and a program against the port's `libc.so` (Mesa, libdrm). Both are
//! process-shaped because a foreign build system calls them once per object.

pub mod dso;
pub mod minix;

use std::io;
use std::path::Path;
use std::process::Command;

/// clang's own include dir, which holds the headers a freestanding compile is
/// entitled to (`stddef.h`, `stdint.h`, `stdarg.h`, `limits.h`, ...).
///
/// Asked of the compiler rather than hardcoded: the path names its version and
/// differs between the Windows and Linux installs, which are two different clang
/// builds.
pub fn resource_dir(clang: &str) -> io::Result<String> {
    let output = Command::new(clang)
        .arg("-print-resource-dir")
        .output()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("running `{clang} -print-resource-dir`: {error} (is clang on PATH?)"),
            )
        })?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "`{clang} -print-resource-dir` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `-nostdinc`, the compiler's own headers, and the port's.
///
/// Order matters: `-I` is searched before `-isystem`, so the port's `stddef.h`,
/// `stdint.h` and the rest win over clang's where both define them, and clang's
/// are the fallback for what the port has no opinion about.
pub fn compile_flags(root: &Path) -> io::Result<Vec<String>> {
    let resource = resource_dir("clang")?;
    Ok(vec![
        "-nostdinc".to_string(),
        "-isystem".to_string(),
        format!("{resource}/include"),
        format!("-I{}", root.join("tools").join("c-include").display()),
    ])
}

/// Run `command`, echoing it the way the scripts did, and report its success.
///
/// The echo is the contract a `--dry-run` will print later: these wrappers exist
/// to pass a specific argv to clang and rustc, and that argv is the thing to
/// review.
pub fn run(command: &mut Command) -> io::Result<bool> {
    println!("{}", describe(command));
    Ok(command.status()?.success())
}

/// Run `command` with the caller's stdio, echoing it to stderr for the build log.
///
/// A `cc` wrapper is called by `make` and by `configure`, which read the
/// compiler's own stdout: the echo cannot go there. The child's status comes back
/// as the wrapper's, so a failed compile is a failed compile to the caller.
pub fn run_passthrough(command: &mut Command) -> io::Result<bool> {
    eprintln!("+ {}", command_line(command));
    Ok(command.status()?.success())
}

/// The program and its arguments, as one line.
pub fn command_line(command: &Command) -> String {
    let mut line = command.get_program().to_string_lossy().into_owned();
    for argument in command.get_args() {
        line.push(' ');
        line.push_str(&argument.to_string_lossy());
    }
    line
}

fn describe(command: &Command) -> String {
    format!("running: {}", command_line(command))
}

/// The link-step arguments of a `cc` command line, in order.
///
/// The arguments in a `cc` line that the *link* step needs, as opposed to the
/// compile step. A wrapper that forwards `-l`/`-L` to nothing links an executable
/// with no archives in it, which shows up as undefined symbols for whole
/// libraries: bash's link line carries `-lbuiltins -lglob -lsh -lreadline ...`.
pub fn link_passthrough(argv: &[String]) -> Vec<String> {
    const FLAGS: [&str; 6] = ["-L", "-l", "-Wl,", "-static", "-rdynamic", "-pthread"];
    const WITH_VALUE: [&str; 3] = ["-Xlinker", "-u", "-z"];

    let mut out = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if WITH_VALUE.contains(&arg.as_str()) {
            if let Some(value) = argv.get(i + 1) {
                out.push(arg.clone());
                out.push(value.clone());
                i += 2;
                continue;
            }
        } else if FLAGS.iter().any(|flag| arg.starts_with(flag)) {
            out.push(arg.clone());
        }
        i += 1;
    }
    out
}

/// One command, either run or printed.
///
/// `--dry-run` is how the wrapper's argv is reviewed without compiling 209
/// objects: the commands are the whole contract, so printing them is the test.
pub struct Runner {
    dry_run: bool,
}

impl Runner {
    pub fn new(dry_run: bool) -> Runner {
        Runner { dry_run }
    }

    pub fn call(&self, command: &mut Command) -> io::Result<bool> {
        if self.dry_run {
            println!("{}", command_line(command));
            return Ok(true);
        }
        run_passthrough(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_echo_names_the_program_and_every_argument() {
        let mut command = Command::new("clang");
        command.args(["-c", "hello.c"]);
        assert_eq!(describe(&command), "running: clang -c hello.c");
        assert_eq!(command_line(&command), "clang -c hello.c");
    }

    #[test]
    fn link_flags_keep_their_values_and_drop_compile_flags() {
        let argv: Vec<String> = [
            "-c", "-O2", "-I", "include", "-L", "lib", "-lm", "-static", "-Wl,-soname,x",
            "-Xlinker", "--as-needed", "-u", "main", "foo.c",
        ]
        .iter()
        .map(|word| word.to_string())
        .collect();
        assert_eq!(
            link_passthrough(&argv),
            vec![
                "-L", "-lm", "-static", "-Wl,-soname,x", "-Xlinker", "--as-needed", "-u",
                "main"
            ]
        );
    }

    #[test]
    fn a_missing_compiler_names_the_command_that_failed() {
        let error = resource_dir("no-such-clang-anywhere").unwrap_err();
        assert!(error.to_string().contains("-print-resource-dir"));
    }
}
