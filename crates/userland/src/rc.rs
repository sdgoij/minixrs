//! The boot's two program lists: `/etc/rc` before the shell, `/etc/session` after.
//!
//! `/bin/rc` runs what `/etc/rc` names and then becomes the interactive shell.
//! `/bin/session` is the session manager: `rc` starts it, it waits for the shell's
//! prompt, and only then starts what `/etc/session` names, for the rest of the
//! boot's life. Each line is `path [args...]`, split on spaces, and a line whose
//! first non-space byte is `#` is a comment.
//!
//! There is no shell syntax here on purpose. The format would otherwise need a
//! shell to parse it, and the whole point of the first list is to run before a
//! shell exists.
//!
//! Why two lists. A program started *before* the shell writes into a console whose
//! prompt the shell has not printed yet, and one started *while the shell is
//! printing it* appends its first line to the prompt itself (`# wlpanel: ready`),
//! which is not a prompt to anything reading the console. The session manager is
//! what knows when the prompt is out, so the long-running programs go in the second
//! list and the first list holds only the manager. Starting two programs at once
//! also wedges this port: PM and VFS serve one request each, and two `exec`s in
//! flight can leave one of them unfinished, which is why every child here is given
//! room to finish its `exec` before the next one is forked.

use minix_std::process;

/// The bootstrap list, run before the shell exists.
const RC_PATH: &[u8] = b"/etc/rc";
/// The session list, run once the prompt is out.
const SESSION_PATH: &[u8] = b"/etc/session";
/// The largest list this reads. A session's is a handful of lines.
const MAX_LIST: usize = 4096;
/// The longest line, arguments included.
const MAX_LINE: usize = 128;
/// The most arguments one line may name.
const MAX_ARGS: usize = 8;
/// How many scheduler round trips the session manager waits before it starts its
/// list. Each `getpid` is a `SENDREC` to PM, so this is a real pause, and it is
/// there for the `exec` overlap rather than for the console: `rc` forks this
/// process and then execs the shell, and a second `exec` in flight at the same
/// time can wedge PM or VFS (see `settle`).
const PROMPT_WAIT: usize = 400;

pub fn rc(_args: &[&str]) -> i32 {
    run_list(RC_PATH, b"rc");
    exec_shell();
}

/// The session manager: wait for `rc` to hand over to the shell, start the list,
/// reap for ever.
pub fn session(_args: &[&str]) -> i32 {
    for _ in 0..PROMPT_WAIT {
        let _ = process::getpid();
    }

    let started = run_list(SESSION_PATH, b"session");
    // One write, so a line a program reports cannot be split by another writer
    // sharing the console.
    let mut line = [0u8; 32];
    let mut at = 0usize;
    crate::append(&mut line, &mut at, b"session: started ");
    crate::append(
        &mut line,
        &mut at,
        crate::Decimal::of(started as u32).bytes(),
    );
    crate::append(&mut line, &mut at, b"\n");
    crate::write_out(&line[..at]);

    loop {
        // Reap whatever exits. A session's programs are long-lived, so this is
        // mostly a park, but a manager that never waits leaves zombies.
        let _ = process::waitpid(-1, 0);
    }
}

/// Start every command in `path`, and say how many were started.
fn run_list(path: &[u8], tag: &[u8]) -> usize {
    let mut text = [0u8; MAX_LIST];
    let len = read_list(path, &mut text, tag);
    let mut started = 0;
    for line in text[..len].split(|byte| *byte == b'\n') {
        let line = trim(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        start(line, tag);
        started += 1;
    }
    started
}

/// Read a list file.
///
/// A boot process starts before every server behind it is in its loop: the first
/// open of a regular file can fail or race the filesystem coming up, and the next
/// one succeed. The attempts spin and yield in between, and a file that is never
/// readable is reported rather than silently skipped, because a boot without its
/// list looks exactly like a working one.
fn read_list(path: &[u8], text: &mut [u8; MAX_LIST], tag: &[u8]) -> usize {
    for attempt in 0..32 {
        if let Ok(fd) = unsafe { minix_std::fs::open(path, minix_std::fs::O_RDONLY, 0) } {
            let n = minix_rt::read(fd, text);
            let _ = minix_std::fs::close(fd);
            return if n > 0 { n as usize } else { 0 };
        }
        for _ in 0..50_000 {
            core::hint::spin_loop();
        }
        // A syscall to another server is what lets that server run.
        if attempt % 4 == 3 {
            let _ = process::getpid();
        }
    }
    crate::write_err(tag);
    crate::write_err(b": could not read the list\n");
    0
}

fn trim(line: &[u8]) -> &[u8] {
    let start = line.iter().position(|b| *b != b' ').unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|b| *b != b' ')
        .map_or(start, |i| i + 1);
    &line[start..end]
}

/// Start one command in the background.
///
/// The tokens are copied into a local buffer and each is NUL-terminated in
/// place, because that is the form `exec` hands to PM. The buffer is on this
/// frame and `exec` copies what it needs before replacing the child.
fn start(line: &[u8], tag: &[u8]) {
    let mut buf = [0u8; MAX_LINE];
    let n = line.len().min(MAX_LINE - 1);
    buf[..n].copy_from_slice(&line[..n]);

    let mut argv = [core::ptr::null(); MAX_ARGS];
    let (argc, path_start, path_end) = tokens(&mut buf, n, &mut argv);
    if argc == 0 {
        return;
    }

    let path = &buf[path_start..path_end];
    match unsafe { process::fork() } {
        Ok(0) => {
            if process::exec(path, &argv[..argc], &[]).is_err() {
                crate::write_err(tag);
                crate::write_err(b": exec failed: ");
                crate::write_err(path);
                crate::write_err(b"\n");
            }
            process::exit(127);
        }
        Ok(pid) => settle(pid),
        Err(_) => {
            crate::write_err(tag);
            crate::write_err(b": fork failed\n");
        }
    }
}

/// Let a child finish its `exec` before the next line, or before this process
/// execs the shell.
///
/// PM and VFS each serve one request at a time, and two `exec`s in flight can
/// wedge one of them: with one child in `/etc/rc` the child's line appeared and
/// the shell prompt sometimes did not; with an empty list every boot reached the
/// prompt. A short program is reaped here, which removes the overlap outright. A
/// long-running one does not exit, so it gets the same handful of yields and is
/// left running; the session manager is started first and delays its own work, so
/// its execs never overlap the shell's.
fn settle(pid: i32) {
    for _ in 0..64 {
        if process::waitpid(pid, process::WNOHANG).is_ok() {
            return;
        }
        let _ = process::getpid();
    }
}

/// Let a child finish its `exec` before the next line, or before this process
/// Split the first `n` bytes into space-separated tokens, each NUL-terminated in
/// place, and record where each starts. Returns how many there are and the range
/// of the first one (the path needs a length, not a terminator).
fn tokens(
    buf: &mut [u8; MAX_LINE],
    n: usize,
    argv: &mut [*const u8; MAX_ARGS],
) -> (usize, usize, usize) {
    let mut count = 0;
    let mut path_start = 0;
    let mut path_end = 0;
    let mut i = 0;
    while i < n && count < MAX_ARGS {
        while i < n && (buf[i] == b' ' || buf[i] == 0) {
            i += 1;
        }
        if i >= n {
            break;
        }
        if count == 0 {
            path_start = i;
        }
        argv[count] = buf.as_ptr().wrapping_add(i);
        while i < n && buf[i] != b' ' && buf[i] != 0 {
            i += 1;
        }
        if i < buf.len() {
            buf[i] = 0;
        }
        if count == 0 {
            path_end = i;
        }
        count += 1;
    }
    (count, path_start, path_end)
}

/// Replace this process with the interactive shell.
fn exec_shell() -> ! {
    #[cfg(target_os = "minix")]
    {
        let _ = unsafe { exec_one(c"/bin/sh") };
        crate::write_err(b"rc: /bin/sh did not start\n");
    }
    process::exit(127)
}

#[cfg(target_os = "minix")]
unsafe fn exec_one(path: &core::ffi::CStr) -> i64 {
    let argv: [*const u8; 2] = [path.as_ptr().cast(), core::ptr::null()];
    unsafe {
        minix_rt::execve(
            path.as_ptr().cast(),
            path.to_bytes_with_nul().len(),
            argv.as_ptr(),
            core::ptr::null(),
        ) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_ARGS, MAX_LINE, tokens, trim};

    #[test]
    fn comments_and_blank_lines_are_skipped_by_the_trimmed_line() {
        assert_eq!(trim(b"   "), b"");
        assert_eq!(trim(b"  /bin/wlpanel  "), b"/bin/wlpanel");
        assert_eq!(trim(b"# a note")[0], b'#');
    }

    #[test]
    fn a_line_splits_into_a_path_and_its_arguments() {
        let mut buf = [0u8; MAX_LINE];
        let line = b"  /bin/echo rc: session ";
        buf[..line.len()].copy_from_slice(line);

        let mut argv = [core::ptr::null(); MAX_ARGS];
        let (count, path_start, path_end) = tokens(&mut buf, line.len(), &mut argv);
        assert_eq!(count, 3);
        // The path is the first token, terminated in place.
        assert_eq!(&buf[path_start..path_end], b"/bin/echo");
        assert_eq!(buf[path_end], 0);
        // The remaining tokens follow it, NUL-separated.
        assert_eq!(&buf[path_end + 1..path_end + 4], b"rc:");
        assert_eq!(buf[path_end + 4], 0);
        // And each recorded pointer names its token.
        assert_eq!(&buf[path_end + 5..path_end + 12], b"session");
    }

    #[test]
    fn a_line_of_spaces_names_nothing() {
        let mut buf = [0u8; MAX_LINE];
        buf[..4].copy_from_slice(b"    ");
        let mut argv = [core::ptr::null(); MAX_ARGS];
        assert_eq!(tokens(&mut buf, 4, &mut argv), (0, 0, 0));
    }
}
