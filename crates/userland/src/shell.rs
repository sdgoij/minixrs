//! Shell (`sh`) with `>` redirection and `&&` operator.
//!
//! Extracted from `lib.rs` for clarity as the shell grows.

use crate::write_out;

#[cfg(target_os = "none")]
use crate::write_err;
#[cfg(target_os = "none")]
use crate::{
    cat, chmod, chown, cp, echo, errstr, fsck, ln, ls, mkdir, mknod, reboot, rm, set_redirect_fd,
    sync,
};

/// Sentinel for `exit` — must not overlap any valid exit status.
#[allow(dead_code)]
const SH_EXIT: i32 = i32::MIN;

/// A single parsed command with optional stdout redirection.
#[allow(dead_code)]
#[derive(Clone, Copy)]
struct ParsedCommand<'a> {
    tokens: [&'a str; 32],
    argc: usize,
    redirect_stdout: Option<&'a str>,
}

/// Maximum number of commands in a single `|` pipeline.
#[cfg(target_os = "none")]
const MAX_PIPELINE: usize = 8;

/// True if `cmd` is a shell builtin (run in-process).
#[cfg(target_os = "none")]
fn is_builtin_cmd(cmd: &str) -> bool {
    matches!(
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
    )
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
        // Ignore SIGINT: the tty's sigchar sends it on ^C, and the shell
        // must survive it at the prompt (read_line just gets EINTR and the
        // loop reprints the prompt). TTY.md 1C.3.
        if minix_std::time::sig_ignore(minix_std::time::SIGINT).is_err() {
            write_err(b"sh: warning: cannot ignore SIGINT\n");
        }
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

            // Split on `&&` and run each sub-command (possibly a `|`
            // pipeline) in sequence.
            let mut cmd_start = 0usize;
            let mut last_status = 0i32;
            for i in 0..raw_argc {
                if raw_tokens[i] == "&&" {
                    if i > cmd_start {
                        last_status = run_segment(&raw_tokens[cmd_start..i]);
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
                last_status = run_segment(&raw_tokens[cmd_start..raw_argc]);
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

/// Read one line from stdin into `buf`. Returns the number of bytes stored
/// (excluding the trailing newline).
///
/// The tty's canonical line discipline (ICANON) does the echoing, backspace
/// and line editing; this only accumulates bytes until the line ends. The
/// first read blocks until a line is available, so a ^C (EINTR) at an empty
/// prompt yields an empty line and the caller reprints the prompt.
#[cfg(target_os = "none")]
fn read_line(buf: &mut [u8]) -> usize {
    let mut total = 0usize;
    loop {
        let n = minix_rt::read(0, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
        if total > 0 && (buf[total - 1] == b'\n' || buf[total - 1] == b'\r') {
            break;
        }
        if total >= buf.len() {
            break;
        }
    }
    // Strip the trailing newline (canonical mode delivers it).
    while total > 0 && (buf[total - 1] == b'\n' || buf[total - 1] == b'\r') {
        total -= 1;
    }
    total
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
// Pipelines (`|`)
// ---------------------------------------------------------------------------

/// Split a token slice on `|` and run the resulting pipeline.
/// Returns the exit status of the last command, or `SH_EXIT`.
#[cfg(target_os = "none")]
fn run_segment(tokens: &[&str]) -> i32 {
    let mut commands = [ParsedCommand {
        tokens: [""; 32],
        argc: 0,
        redirect_stdout: None,
    }; MAX_PIPELINE];
    let mut ncmds = 0usize;
    let mut start = 0usize;
    for i in 0..=tokens.len() {
        if i == tokens.len() || tokens[i] == "|" {
            if i > start && ncmds < MAX_PIPELINE {
                commands[ncmds] = parse_command(&tokens[start..i], i - start);
                ncmds += 1;
            }
            start = i + 1;
        }
    }
    if ncmds == 0 {
        return 0;
    }
    if ncmds == 1 {
        return run_parsed_command(&commands[0]);
    }
    run_pipeline(&commands[..ncmds])
}

/// Move pipe ends out of the 0-2 range before wiring a pipeline.
///
/// The shell's stdio (fds 0-2) is the kernel serial console, not VFS fds,
/// so the shell's VFS fd table starts empty and `pipe()` returns fds 0 and
/// 1. dup2'ing a pipe end onto fd 1 would then be a no-op, and closing the
/// originals would destroy the pipe. Lift both ends to the top of the fd
/// space (which is free) so the dup2 wiring in the children works.
///
/// Each pipe gets its own pair (pipe i -> fds 62-2i/63-2i) so multi-stage
/// pipelines don't collide: a later pipe() would otherwise re-lift onto an
/// already-occupied fd and close the earlier pipe's end.
#[cfg(target_os = "none")]
fn lift_pipe_fds(r: i32, w: i32, pipe_index: usize) -> (i32, i32) {
    let mut r = r;
    let mut w = w;
    if r < 3 || w < 3 {
        let hi = 63 - 2 * pipe_index as i32;
        let lo = hi - 1;
        if w < 3 && minix_std::fs::dup2(w, hi).is_ok() {
            let _ = minix_std::fs::close(w);
            w = hi;
        }
        if r < 3 && minix_std::fs::dup2(r, lo).is_ok() {
            let _ = minix_std::fs::close(r);
            r = lo;
        }
    }
    (r, w)
}

/// Run a pipeline of two or more commands: each command's stdout feeds the
/// next command's stdin through a VFS pipe.
///
/// Children are forked in order (left to right) so the writer generally runs
/// before the reader; the parent closes its pipe ends and reaps each child.
#[cfg(target_os = "none")]
fn run_pipeline(commands: &[ParsedCommand]) -> i32 {
    let n = commands.len();
    if n < 2 || n > MAX_PIPELINE {
        return 1;
    }
    // pipes[i] connects commands[i] (writer) to commands[i+1] (reader).
    let mut pipes = [(-1i32, -1i32); MAX_PIPELINE - 1];
    let npipes = n - 1;
    for i in 0..npipes {
        match minix_std::fs::pipe() {
            Ok((r, w)) => pipes[i] = lift_pipe_fds(r, w, i),
            Err(_) => {
                write_err(b"sh: pipe failed\r\n");
                return 1;
            }
        }
    }

    let mut pids = [0i32; MAX_PIPELINE];
    for i in 0..n {
        let pid = minix_rt::fork();
        if pid < 0 {
            write_err(b"sh: fork failed\r\n");
            for &(r, w) in &pipes[..npipes] {
                let _ = minix_std::fs::close(r);
                let _ = minix_std::fs::close(w);
            }
            return 1;
        }
        if pid == 0 {
            // Child: wire stdin/stdout to the pipe ends, close the rest,
            // then run the command in place (no second fork).
            if i > 0 {
                let (r, _w) = pipes[i - 1];
                if minix_std::fs::dup2(r, 0).is_err() {
                    write_err(b"sh: dup2 failed\r\n");
                    minix_rt::exit(1);
                }
                unsafe { minix_rt::set_fd_vfs(0, 1) };
            }
            if i + 1 < n {
                let (_r, w) = pipes[i];
                if minix_std::fs::dup2(w, 1).is_err() {
                    write_err(b"sh: dup2 failed\r\n");
                    minix_rt::exit(1);
                }
                unsafe { minix_rt::set_fd_vfs(1, 1) };
            }
            for &(r, w) in &pipes[..npipes] {
                let _ = minix_std::fs::close(r);
                let _ = minix_std::fs::close(w);
            }
            let status = run_command_inline(&commands[i]);
            minix_rt::exit(status);
        }
        pids[i] = pid;
    }

    // Parent: close all pipe ends, then reap each child.
    for &(r, w) in &pipes[..npipes] {
        let _ = minix_std::fs::close(r);
        let _ = minix_std::fs::close(w);
    }
    let mut last_status = 0i32;
    for &pid in &pids[..n] {
        let s = minix_rt::waitpid(pid);
        if s >= 0 {
            last_status = s;
        }
    }
    last_status
}

/// Run one command in the current process (used by pipeline children, which
/// are already forked). Builtins run directly; external commands exec in
/// place. Handles an optional stdout file redirect.
#[cfg(target_os = "none")]
fn run_command_inline(parsed: &ParsedCommand) -> i32 {
    let cmd = parsed.tokens[0];
    let args = &parsed.tokens[..parsed.argc];

    // `cd` always runs in-process — redirection is meaningless for it.
    if cmd == "cd" {
        return run_cd(args);
    }
    // `exit` returns a sentinel so the main loop can break out.
    if cmd == "exit" {
        return SH_EXIT;
    }

    let is_builtin = is_builtin_cmd(cmd);

    // Optional stdout redirect to a file (typically on the last command).
    if let Some(outfile) = parsed.redirect_stdout {
        let fd = setup_redirect(outfile);
        set_redirect_fd(fd);
        if !is_builtin {
            if minix_std::fs::dup2(fd, 1).is_err() {
                write_err(b"sh: dup2 failed\r\n");
                return 1;
            }
            unsafe { minix_rt::set_fd_vfs(1, 1) };
        }
    }

    if is_builtin {
        return run_builtin(cmd, args);
    }

    // External: exec in place.
    let mut cmd_path = [0u8; 256];
    let path_len = build_path(cmd.as_bytes(), &mut cmd_path);
    if path_len == 0 {
        write_err(b"sh: '");
        write_err(cmd.as_bytes());
        write_err(b"' not found\r\n");
        return 1;
    }
    try_exec(args, &mut cmd_path);
    // If we get here, exec failed.
    write_err(b"sh: '");
    write_err(cmd.as_bytes());
    write_err(b"' not found\r\n");
    1
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

    let is_builtin = is_builtin_cmd(cmd);

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
                // The fd avoids the kernel's serial shortcut for fd 1/2,
                // so writes go through VFS to the filesystem.
                let redirect_fd = setup_redirect(outfile);
                set_redirect_fd(redirect_fd);
                let status = if is_builtin {
                    run_builtin(cmd, args)
                } else {
                    // External commands: make fd 1 the redirect file at the
                    // VFS level, mark fd 1 VFS-owned, then exec in place.
                    // The redirect child is already a throwaway fork, so no
                    // second fork is needed (and the exec'd image inherits
                    // both the fd table and the kernel flag).
                    if let Err(_) = minix_std::fs::dup2(redirect_fd, 1) {
                        write_err(b"sh: dup2 failed\r\n");
                        minix_rt::exit(1);
                    }
                    unsafe { minix_rt::set_fd_vfs(1, 1) };
                    let mut cmd_path = [0u8; 256];
                    let path_len = build_path(cmd.as_bytes(), &mut cmd_path);
                    if path_len == 0 {
                        write_err(b"sh: '");
                        write_err(cmd.as_bytes());
                        write_err(b"' not found\r\n");
                        minix_rt::exit(1);
                    }
                    try_exec(args, &mut cmd_path);
                    // If we get here, exec failed.
                    write_err(b"sh: '");
                    write_err(cmd.as_bytes());
                    write_err(b"' not found\r\n");
                    minix_rt::exit(1);
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

    let r = unsafe {
        minix_rt::execve(
            cmd_path.as_ptr(),
            child_path_len,
            argv_buf.as_ptr(),
            core::ptr::null(),
        )
    };

    // Try /sbin/<cmd> as fallback.
    if r < 0 && cmd_start > 1 && 6 + cmd_name_len < cmd_path.len() {
        cmd_path[..6].copy_from_slice(b"/sbin/");
        cmd_path[6..6 + cmd_name_len].copy_from_slice(&cmd_name[..cmd_name_len]);
        cmd_path[6 + cmd_name_len] = 0;
        let _ = unsafe {
            minix_rt::execve(
                cmd_path.as_ptr(),
                6 + cmd_name_len + 1,
                argv_buf.as_ptr(),
                core::ptr::null(),
            )
        };
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
