//! Shell (`sh`) with `>` redirection and `&&` operator.
//!
//! Extracted from `lib.rs` for clarity as the shell grows.

use crate::write_out;

#[cfg(target_os = "none")]
use crate::write_err;
#[cfg(target_os = "none")]
use crate::{cat, chmod, chown, cp, echo, errstr, fsck, ln, ls, mkdir, mknod, reboot, rm, sync};

/// Sentinel for `exit` — must not overlap any valid exit status.
#[allow(dead_code)]
const SH_EXIT: i32 = i32::MIN;

/// A single parsed command with optional stdout redirection.
#[allow(dead_code)]
struct ParsedCommand<'a> {
    tokens: [&'a str; 32],
    argc: usize,
    redirect_stdout: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Public entry point — replaces the old inline `sh` in lib.rs
// ---------------------------------------------------------------------------

pub fn sh(_args: &[&str]) -> i32 {
    #[cfg(not(target_os = "none"))]
    {
        write_out(b"sh: stub (no MINIX syscall ABI on host)\n");
        0
    }
    #[cfg(target_os = "none")]
    {
        write_out(b"# ");
        let mut buf = [0u8; 256];
        loop {
            let line_len = read_line(&mut buf);
            if line_len == 0 {
                write_out(b"# ");
                continue;
            }

            let line_str = core::str::from_utf8(&buf[..line_len]).unwrap_or("");

            // Split into raw tokens by whitespace.
            let mut raw_tokens = [""; 32];
            let mut raw_argc = 0usize;
            for token in line_str.split_whitespace() {
                if raw_argc < raw_tokens.len() {
                    raw_tokens[raw_argc] = token;
                    raw_argc += 1;
                }
            }

            if raw_argc == 0 {
                write_out(b"# ");
                continue;
            }

            // Split on `&&` and run each sub-command in sequence.
            let mut cmd_start = 0usize;
            let mut last_status = 0i32;
            for i in 0..raw_argc {
                if raw_tokens[i] == "&&" {
                    if i > cmd_start {
                        let parsed = parse_command(&raw_tokens[cmd_start..i], i - cmd_start);
                        last_status = run_parsed_command(&parsed);
                        if last_status == SH_EXIT {
                            return 0;
                        }
                        if last_status != 0 {
                            break;
                        }
                    }
                    cmd_start = i + 1;
                }
            }

            // Run the final (or only) sub-command.
            if last_status == 0 && cmd_start < raw_argc {
                let parsed = parse_command(&raw_tokens[cmd_start..raw_argc], raw_argc - cmd_start);
                last_status = run_parsed_command(&parsed);
                if last_status == SH_EXIT {
                    return 0;
                }
            }

            // Print the next prompt.
            write_out(b"# ");
        }
    }
}

// ---------------------------------------------------------------------------
// Line reading
// ---------------------------------------------------------------------------

/// Read one line from stdin into `buf`.  Returns the number of bytes stored
/// (excluding the trailing null / cr / lf).
#[cfg(target_os = "none")]
fn read_line(buf: &mut [u8]) -> usize {
    let mut pos = 0usize;
    while pos < buf.len() - 1 {
        let n = minix_rt::read(0, &mut buf[pos..pos + 1]);
        if n <= 0 {
            break;
        }
        let c = buf[pos];
        // Enter (\r from QEMU terminal) ends the line.
        if c == b'\r' || c == b'\n' {
            write_out(b"\r\n");
            break;
        }
        // Backspace (DEL 0x7F or BS 0x08) erases previous char.
        if c == 0x7F || c == 0x08 {
            if pos > 0 {
                pos -= 1;
                write_out(b"\x08 \x08");
            }
            continue;
        }
        // Echo printable character and store it.
        write_out(&[c]);
        pos += 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Scan `raw_tokens[0..raw_argc]` for `>` and split off the redirect filename.
/// The returned `ParsedCommand` contains only the non-redirect tokens.
#[cfg(target_os = "none")]
fn parse_command<'a>(raw_tokens: &[&'a str], raw_argc: usize) -> ParsedCommand<'a> {
    let mut tokens = [""; 32];
    let mut argc = 0;
    let mut redirect_stdout = None;
    let mut i = 0;
    while i < raw_argc {
        if raw_tokens[i] == ">" && i + 1 < raw_argc {
            redirect_stdout = Some(raw_tokens[i + 1]);
            i += 2;
        } else {
            if argc < tokens.len() {
                tokens[argc] = raw_tokens[i];
                argc += 1;
            }
            i += 1;
        }
    }
    ParsedCommand {
        tokens,
        argc,
        redirect_stdout,
    }
}

// ---------------------------------------------------------------------------
// Command execution
// ---------------------------------------------------------------------------

/// Run a fully-parsed command.
///
/// Returns the exit status (0 = success), or `SH_EXIT` for the `exit` builtin.
#[cfg(target_os = "none")]
fn run_parsed_command(parsed: &ParsedCommand) -> i32 {
    if parsed.argc == 0 {
        return 0;
    }

    let cmd = parsed.tokens[0];
    let args = &parsed.tokens[..parsed.argc];

    // `cd` always runs in-process — redirection is meaningless for it.
    if cmd == "cd" {
        return run_cd(args);
    }

    // `exit` returns a sentinel so the main loop can break out.
    if cmd == "exit" {
        write_out(b"\r\n");
        return SH_EXIT;
    }

    let is_builtin = matches!(
        cmd,
        "echo"
            | "cat"
            | "cp"
            | "ls"
            | "mkdir"
            | "rm"
            | "ln"
            | "chmod"
            | "chown"
            | "sync"
            | "mknod"
            | "reboot"
            | "fsck"
            | "help"
            | "clear"
    );

    match parsed.redirect_stdout {
        Some(outfile) => {
            // Redirection requested — always fork so the shell's stdio
            // is never affected.
            let pid = minix_rt::fork();
            if pid < 0 {
                write_err(b"sh: fork failed\r\n");
                return 1;
            }
            if pid == 0 {
                // Child: open redirect file, run command, exit.
                // Use fd >= 3 to avoid the kernel's serial shortcut
                // for fd 1/2.
                let redirect_fd = setup_redirect(outfile);
                let status = if is_builtin {
                    run_builtin_out(cmd, args, redirect_fd)
                } else {
                    run_external(cmd, args)
                };
                minix_rt::exit(status);
            }
            // Parent: wait.
            let status = minix_rt::waitpid(pid);
            if status < 0 {
                write_err(b"sh: waitpid failed\r\n");
                1
            } else {
                status
            }
        }
        None => {
            // No redirection.
            if is_builtin {
                run_builtin(cmd, args)
            } else {
                // External commands always fork.
                run_external(cmd, args)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cd (always in-process)
// ---------------------------------------------------------------------------

#[cfg(target_os = "none")]
fn run_cd(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_err(b"sh: cd: missing argument\r\n");
        return 1;
    }
    let path = args[1].as_bytes();
    let r = minix_rt::chdir(path);
    if r < 0 {
        write_err(b"sh: cd: ");
        write_err(path);
        write_err(b": ");
        write_err(errstr(r as i32));
        write_err(b"\r\n");
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

#[cfg(target_os = "none")]
fn run_builtin(cmd: &str, args: &[&str]) -> i32 {
    match cmd {
        "echo" => echo(args),
        "cat" => cat(args),
        "cp" => cp(args),
        "ls" => ls(args),
        "mkdir" => mkdir(args),
        "rm" => rm(args),
        "ln" => ln(args),
        "chmod" => chmod(args),
        "chown" => chown(args),
        "sync" => sync(args),
        "mknod" => mknod(args),
        "reboot" => reboot(args),
        "fsck" => fsck(args),
        "help" => {
            write_out(b"available commands: echo cat cp ls mkdir rm ln");
            write_out(b" chmod chown sync mknod reboot fsck help clear\r\n");
            0
        }
        "clear" => {
            write_out(b"\x1b[2J\x1b[H");
            0
        }
        _ => 127,
    }
}

/// Like `run_builtin`, but routes stdout through `redirect_fd`
/// (via VFS) so file redirects work correctly.
#[cfg(target_os = "none")]
fn run_builtin_out(cmd: &str, args: &[&str], redirect_fd: i32) -> i32 {
    match cmd {
        "echo" => crate::echo_fd(args, redirect_fd),
        _ => run_builtin(cmd, args),
    }
}

// ---------------------------------------------------------------------------
// External commands (fork + exec)
// ---------------------------------------------------------------------------

#[cfg(target_os = "none")]
fn run_external(cmd: &str, args: &[&str]) -> i32 {
    // Use stack buffer (NOT static mut) to avoid COW page faults after fork
    // and potential aliasing issues.
    let mut cmd_path = [0u8; 256];
    let cmd_bytes = cmd.as_bytes();

    let path_len = build_path(cmd_bytes, &mut cmd_path);
    if path_len == 0 {
        write_err(b"sh: '");
        write_err(cmd.as_bytes());
        write_err(b"' not found\r\n");
        return 1;
    }

    let pid = minix_rt::fork();
    if pid < 0 {
        write_err(b"sh: fork failed\r\n");
        return 1;
    }
    if pid == 0 {
        try_exec(args, &mut cmd_path);
        // If we get here, exec failed.
        write_err(b"sh: '");
        write_err(cmd.as_bytes());
        write_err(b"' not found\r\n");
        minix_rt::exit(1);
    }

    // Parent: wait.
    let status = minix_rt::waitpid(pid);
    if status < 0 {
        write_err(b"sh: waitpid failed\r\n");
        1
    } else {
        status
    }
}

/// Build the first candidate path for `cmd_bytes` into `cmd_path`.
/// Returns the length (including null terminator), or 0 on overflow.
#[cfg(target_os = "none")]
fn build_path(cmd_bytes: &[u8], cmd_path: &mut [u8; 256]) -> usize {
    if cmd_bytes.starts_with(b"/") {
        let len = (cmd_bytes.len() + 1).min(cmd_path.len());
        cmd_path[..len - 1].copy_from_slice(cmd_bytes);
        cmd_path[len - 1] = 0;
        len
    } else if 5 + cmd_bytes.len() < cmd_path.len() {
        cmd_path[..5].copy_from_slice(b"/bin/");
        cmd_path[5..5 + cmd_bytes.len()].copy_from_slice(cmd_bytes);
        cmd_path[5 + cmd_bytes.len()] = 0;
        5 + cmd_bytes.len() + 1
    } else {
        0
    }
}

/// In the child process, try to exec the command.
/// Falls through if exec fails, so the caller can retry `/sbin/<cmd>`.
#[cfg(target_os = "none")]
fn try_exec(args: &[&str], cmd_path: &mut [u8; 256]) {
    let child_path_len = cmd_path.iter().position(|&b| b == 0).unwrap_or(255) + 1;
    let cmd_end = child_path_len - 1;
    let cmd_start = (0..cmd_end)
        .rev()
        .find(|&i| cmd_path[i] == b'/')
        .map(|i| i + 1)
        .unwrap_or(0);
    let cmd_len = cmd_end - cmd_start;

    // Save command name for the /sbin/ fallback.
    let mut cmd_name = [0u8; 56];
    let cmd_name_len = cmd_len.min(56);
    cmd_name[..cmd_name_len].copy_from_slice(&cmd_path[cmd_start..cmd_start + cmd_name_len]);

    let mut argv_buf: [*const u8; 32] = [core::ptr::null(); 32];
    argv_buf[0] = cmd_path.as_ptr();
    let mut arg_off = child_path_len;
    for i in 1..args.len().min(32) {
        let tok = args[i].as_bytes();
        let len = tok.len().min(55);
        if arg_off + len + 1 >= cmd_path.len() {
            break;
        }
        cmd_path[arg_off..arg_off + len].copy_from_slice(tok);
        cmd_path[arg_off + len] = 0;
        argv_buf[i] = unsafe { cmd_path.as_ptr().add(arg_off) };
        arg_off += len + 1;
        arg_off = (arg_off + 7) & !7;
    }

    let r = unsafe { minix_rt::exec_replace(&cmd_path[..child_path_len], argv_buf.as_ptr()) };

    // Try /sbin/<cmd> as fallback.
    if r < 0 && cmd_start > 1 && 6 + cmd_name_len < cmd_path.len() {
        cmd_path[..6].copy_from_slice(b"/sbin/");
        cmd_path[6..6 + cmd_name_len].copy_from_slice(&cmd_name[..cmd_name_len]);
        cmd_path[6 + cmd_name_len] = 0;
        let _ =
            unsafe { minix_rt::exec_replace(&cmd_path[..6 + cmd_name_len + 1], argv_buf.as_ptr()) };
    }
}

// ---------------------------------------------------------------------------
// Redirection helper (called in child process)
// ---------------------------------------------------------------------------

/// In the child process: close fd 1, open `outfile` for writing on fd 1.
/// Exits the child on failure.
#[cfg(target_os = "none")]
fn setup_redirect(outfile: &str) -> i32 {
    // Open the file WITHOUT closing fd 1.
    // The returned fd (>= 3) avoids the kernel's serial shortcut
    // for fd 1/2, so writes go through VFS to the filesystem.
    match unsafe {
        minix_std::fs::open(
            outfile,
            minix_std::fs::O_WRONLY | minix_std::fs::O_CREAT | minix_std::fs::O_TRUNC,
            0o644,
        )
    } {
        Ok(fd) => fd,
        Err(e) => {
            let pos = if e.0 < 0 { -e.0 } else { e.0 } as u32;
            write_err(b"sh: cannot create ");
            write_err(outfile.as_bytes());
            write_err(b": err=");
            write_err(&[b'0' + (pos / 10) as u8, b'0' + (pos % 10) as u8]);
            write_err(b"\r\n");
            minix_rt::exit(1);
        }
    }
}
