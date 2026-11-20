//! The flags that make a C compile for minix hermetic, and the command echo the
//! build wrappers share.
//!
//! Without `-nostdinc`, `clang --target=x86_64-unknown-none -I tools/c-include`
//! also searches the host's `/usr/include`, so **any header the port does not
//! provide is silently satisfied by glibc** and any question asked about it is
//! answered by the host: configure reported `HAVE_TERMIOS_H`, `HAVE_SYS_WAIT_H`
//! and `HAVE_UNION_WAIT` for a target that has none of them, and bash then
//! compiled paths no real system has. A missing header should be an error, not a
//! guess. `.notes/c_build.md` has the write-up.

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

fn describe(command: &Command) -> String {
    let mut line = format!("running: {}", command.get_program().to_string_lossy());
    for argument in command.get_args() {
        line.push(' ');
        line.push_str(&argument.to_string_lossy());
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_echo_names_the_program_and_every_argument() {
        let mut command = Command::new("clang");
        command.args(["-c", "hello.c"]);
        assert_eq!(describe(&command), "running: clang -c hello.c");
    }

    #[test]
    fn a_missing_compiler_names_the_command_that_failed() {
        let error = resource_dir("no-such-clang-anywhere").unwrap_err();
        assert!(error.to_string().contains("-print-resource-dir"));
    }
}
