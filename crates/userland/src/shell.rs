//! Shell (`sh`) with `>` redirection and `&&` operator.
//!
//! Extracted from `lib.rs` for clarity as the shell grows.

use crate::write_out;

#[cfg(target_os = "minix")]
use crate::write_err;
#[cfg(target_os = "minix")]
use crate::{
    cat, chmod, chown, cp, echo, errstr, fsck, hangdump, ln, ls, memstat, mkdir, mknod, reboot,
    regions, rm, set_redirect_fd, sync,
};
#[cfg(target_os = "minix")]
use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

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
#[cfg(target_os = "minix")]
const MAX_PIPELINE: usize = 8;

/// True if `cmd` is a shell builtin (run in-process).
#[cfg(target_os = "minix")]
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
            | "memstat"
            | "regions"
            | "hangdump"
            | "help"
            | "clear"
    )
}

// ---------------------------------------------------------------------------
// Public entry point — replaces the old inline `sh` in lib.rs
// ---------------------------------------------------------------------------

pub fn sh(_args: &[&str]) -> i32 {
    #[cfg(not(target_os = "minix"))]
    {
        write_out(b"sh: stub (no MINIX syscall ABI on host)\n");
        0
    }
    #[cfg(target_os = "minix")]
    {
        // Ignore SIGINT: the tty's sigchar sends it on ^C, and the shell
        // must survive it at the prompt (the editor's read just gets EINTR
        // and the loop reprints the prompt). TTY.md 1C.3.
        if minix_std::time::sig_ignore(minix_std::time::SIGINT).is_err() {
            write_err(b"sh: warning: cannot ignore SIGINT\n");
        }
        // The line editor drives echo/editing itself (arrows, history), so
        // the tty runs raw at the prompt and canonical for commands. If fd 0
        // is not a terminal, termios fails and the editor still reads bytes.
        let editor_ok = tty_set_raw(true);
        let mut ed = Editor::new();
        let mut buf = [0u8; 256];
        loop {
            // Reap finished background jobs before each prompt so `[pid]
            // done` / `[pid] terminated (signal N)` reports appear in
            // order. SIGNALS.md 3.4 — prompt-time reaping is the pattern.
            reap_jobs();
            write_out(b"# ");
            let line_len = read_line(&mut ed, &mut buf);
            if line_len == LINE_EOF {
                // ^D at an empty prompt: exit the shell.
                if editor_ok {
                    tty_set_raw(false);
                }
                return 0;
            }
            if line_len == 0 {
                continue;
            }
            ed.push_history(&buf[..line_len]);
            // Commands run with the tty back in canonical mode (children
            // expect normal echo/line editing).
            if editor_ok {
                tty_set_raw(false);
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
                continue;
            }

            // Background job: a trailing `&` runs the command chain in a
            // forked child without waiting; the pid is recorded for
            // prompt-time reaping.
            let mut background = false;
            if raw_tokens[raw_argc - 1] == "&" {
                background = true;
                raw_argc -= 1;
                if raw_argc == 0 {
                    continue;
                }
            }

            if background {
                run_background(&raw_tokens[..raw_argc]);
                if editor_ok {
                    tty_set_raw(true);
                }
                continue;
            }

            // Run the command chain (`&&` splitting) in the foreground.
            let status = run_chain(&raw_tokens, raw_argc);
            if editor_ok {
                tty_set_raw(true);
            }
            if status == SH_EXIT {
                return 0;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Command chaining and background jobs
// ---------------------------------------------------------------------------

/// Run a token slice, splitting on `&&`. Each sub-command may be a `|`
/// pipeline. Returns the last status, or `SH_EXIT` for `exit`.
#[cfg(target_os = "minix")]
fn run_chain(raw_tokens: &[&str; 32], raw_argc: usize) -> i32 {
    let mut cmd_start = 0usize;
    let mut last_status = 0i32;
    for i in 0..raw_argc {
        if raw_tokens[i] == "&&" {
            if i > cmd_start {
                last_status = run_segment(&raw_tokens[cmd_start..i]);
                if last_status == SH_EXIT {
                    return SH_EXIT;
                }
                if last_status != 0 {
                    return last_status;
                }
            }
            cmd_start = i + 1;
        }
    }
    if last_status == 0 && cmd_start < raw_argc {
        last_status = run_segment(&raw_tokens[cmd_start..raw_argc]);
    }
    last_status
}

/// Background job pids — a fixed table the shell reaps at each prompt.
/// The child (post-fork) copy is inert: it runs one command and exits.
#[cfg(target_os = "minix")]
const MAX_JOBS: usize = 16;
#[cfg(target_os = "minix")]
static JOB_PIDS: [AtomicI32; MAX_JOBS] = [const { AtomicI32::new(0) }; MAX_JOBS];
#[cfg(target_os = "minix")]
static JOB_NEXT: AtomicUsize = AtomicUsize::new(0);

/// Record a background job's pid.
#[cfg(target_os = "minix")]
fn add_job(pid: i32) {
    let idx = JOB_NEXT.fetch_add(1, Ordering::Relaxed) % MAX_JOBS;
    JOB_PIDS[idx].store(pid, Ordering::Relaxed);
}

/// Write an unsigned decimal to stdout.
#[cfg(target_os = "minix")]
fn write_dec(mut n: u32) {
    let mut buf = [0u8; 12];
    let mut i = 12;
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    write_out(&buf[i..]);
}

/// Run a command chain in the background: fork, record the pid, return
/// immediately. The child runs the chain and exits.
#[cfg(target_os = "minix")]
fn run_background(tokens: &[&str]) {
    let pid = minix_rt::fork();
    if pid < 0 {
        write_err(b"sh: fork failed\r\n");
        return;
    }
    if pid == 0 {
        // Child: run the chain (single commands exec in place so the job
        // pid IS the command's process — `kill` targets it directly), then
        // exit. The `&` token was already stripped by the caller.
        let status = run_segment_bg(tokens);
        minix_rt::exit(status);
    }
    add_job(pid);
    write_out(b"[");
    write_dec(pid as u32);
    write_out(b"]\r\n");
}

/// Like `run_segment`, but a single command runs in place (exec for
/// externals, inline for builtins) instead of forking a grandchild, so the
/// background child itself is the command's process.
#[cfg(target_os = "minix")]
fn run_segment_bg(tokens: &[&str]) -> i32 {
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
        return run_command_inline(&commands[0]);
    }
    run_pipeline(&commands[..ncmds])
}

/// Reap finished background jobs and report them at the prompt. A status
/// >= 128 means the job died by signal N = status - 128 (sig_proc_exit
/// encodes deaths as 0x80 | signo).
#[cfg(target_os = "minix")]
fn reap_jobs() {
    loop {
        let (pid, status) = minix_rt::waitpid(-1, minix_std::process::WNOHANG);
        if pid <= 0 {
            break; // EAGAIN — nothing more to reap
        }
        // Report only jobs we launched; ignore stray reaped pids.
        let mut found = false;
        for slot in &JOB_PIDS {
            if slot.load(Ordering::Relaxed) == pid {
                slot.store(0, Ordering::Relaxed);
                found = true;
                break;
            }
        }
        if found {
            write_out(b"[");
            write_dec(pid as u32);
            write_out(b"] ");
            if status >= 128 && status < 256 {
                write_out(b"terminated (signal ");
                write_dec((status - 128) as u32);
                write_out(b")\r\n");
            } else {
                write_out(b"done\r\n");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Line reading — raw-mode editor with history (arrows, ^A/E/U/W/K, ^C/^D)
// ---------------------------------------------------------------------------

/// Maximum line length the editor holds.
#[cfg(any(target_os = "minix", test))]
const EDIT_MAX: usize = 256;
/// Number of history entries kept.
#[cfg(any(target_os = "minix", test))]
const HIST_MAX: usize = 16;
/// Assumed terminal width for full-row blanks (the pty is 80 cols;
/// the console has no winsize).
#[cfg(any(target_os = "minix", test))]
const SCR_COLS: usize = 80;
/// Sentinel returned by `read_line` on EOF (^D at an empty prompt).
pub const LINE_EOF: usize = usize::MAX;

/// Write to stdout (the tty) with EAGAIN retry — the pty slave's output
/// buffer reports EAGAIN when full and the reader (wterm) drains it.
#[cfg(target_os = "minix")]
fn tty_write(s: &[u8]) {
    let mut off = 0;
    while off < s.len() {
        match unsafe { minix_std::fs::write(1, &s[off..]) } {
            Ok(n) => off += n as usize,
            Err(e) if e.0 == -minix_std::EAGAIN => {}
            Err(_) => break,
        }
    }
}

/// Host stub: the editor's buffer logic is unit-tested without a tty.
#[cfg(all(not(target_os = "minix"), test))]
fn tty_write(_s: &[u8]) {}

/// Raw-mode line editor state (the shell's own echo/editing — the tty's
/// canonical mode cannot do arrows or history).
#[cfg(any(target_os = "minix", test))]
struct Editor {
    line: [u8; EDIT_MAX],
    len: usize,
    cur: usize,
    hist: [[u8; EDIT_MAX]; HIST_MAX],
    hist_len: [usize; HIST_MAX],
    hist_n: usize,
    hist_pos: usize,
    draft: [u8; EDIT_MAX],
    draft_len: usize,
}

#[cfg(any(target_os = "minix", test))]
impl Editor {
    fn new() -> Self {
        Self {
            line: [0; EDIT_MAX],
            len: 0,
            cur: 0,
            hist: [[0; EDIT_MAX]; HIST_MAX],
            hist_len: [0; HIST_MAX],
            hist_n: 0,
            hist_pos: 0,
            draft: [0; EDIT_MAX],
            draft_len: 0,
        }
    }

    fn set_line(&mut self, src: &[u8]) {
        let n = src.len().min(EDIT_MAX);
        self.line[..n].copy_from_slice(&src[..n]);
        self.len = n;
        self.cur = n;
    }

    /// Blank the current row and redraw the prompt + line, positioning the
    /// display cursor at the editor cursor. The blank is `SCR_COLS - 1` wide
    /// so it never wraps in wterm (exactly 80 spaces would push the 80th
    /// cell onto the next row).
    fn redraw_line(&mut self) {
        let spaces = [b' '; SCR_COLS - 1];
        tty_write(b"\r");
        tty_write(&spaces);
        tty_write(b"\r");
        tty_write(b"# ");
        tty_write(&self.line[..self.len]);
        for _ in 0..(self.len - self.cur) {
            tty_write(b"\x08");
        }
    }

    /// The display cursor sits at `display_at`; rewrite the line tail from
    /// there (plus a blank to clear any leftover char) and position the
    /// display cursor at the editor cursor (which may be left or right of
    /// `display_at`).
    fn sync_from(&mut self, display_at: usize) {
        let written = self.len - display_at + 1;
        tty_write(&self.line[display_at..self.len]);
        tty_write(b" ");
        for _ in 0..written {
            tty_write(b"\x08");
        }
        if self.cur > display_at {
            let mut p = display_at;
            while p < self.cur {
                tty_write(&[self.line[p]]);
                p += 1;
            }
        } else {
            for _ in self.cur..display_at {
                tty_write(b"\x08");
            }
        }
    }

    fn insert(&mut self, ch: u8) {
        if self.len >= EDIT_MAX {
            return;
        }
        let at = self.cur;
        for i in (at..self.len).rev() {
            self.line[i + 1] = self.line[i];
        }
        self.line[at] = ch;
        self.len += 1;
        self.cur += 1;
        if at + 1 == self.len {
            // Append at the end: just echo the char.
            tty_write(&[ch]);
        } else {
            self.sync_from(at);
        }
    }

    fn backspace(&mut self) {
        if self.cur == 0 {
            return;
        }
        let at = self.cur - 1;
        for i in at..self.len - 1 {
            self.line[i] = self.line[i + 1];
        }
        self.len -= 1;
        self.cur = at;
        if at == self.len {
            // Erase at the end: backspace, blank, backspace.
            tty_write(b"\x08 \x08");
        } else {
            // The display cursor is one past `at` (the deleted char); move
            // it back onto the deletion point, then sync the tail.
            tty_write(b"\x08");
            self.sync_from(at);
        }
    }

    fn kill_to_end(&mut self) {
        let killed = self.len - self.cur;
        for _ in 0..killed {
            tty_write(b" \x08");
        }
        self.len = self.cur;
    }

    fn kill_word(&mut self) {
        let old_cur = self.cur;
        let mut at = old_cur;
        while at > 0 && self.line[at - 1] == b' ' {
            at -= 1;
        }
        while at > 0 && self.line[at - 1] != b' ' {
            at -= 1;
        }
        let shift = old_cur - at;
        for i in at..self.len - shift {
            self.line[i] = self.line[i + shift];
        }
        self.len -= shift;
        self.cur = at;
        // Move the display cursor back to `at`, then sync from there.
        for _ in 0..shift {
            tty_write(b"\x08");
        }
        self.sync_from(at);
    }

    #[cfg(target_os = "minix")]
    fn cursor_left(&mut self) {
        if self.cur > 0 {
            self.cur -= 1;
            tty_write(b"\x08");
        }
    }

    #[cfg(target_os = "minix")]
    fn cursor_right(&mut self) {
        if self.cur < self.len {
            tty_write(&self.line[self.cur..self.cur + 1]);
            self.cur += 1;
        }
    }

    #[cfg(target_os = "minix")]
    fn cursor_home(&mut self) {
        while self.cur > 0 {
            self.cur -= 1;
            tty_write(b"\x08");
        }
    }

    #[cfg(target_os = "minix")]
    fn cursor_end(&mut self) {
        while self.cur < self.len {
            tty_write(&self.line[self.cur..self.cur + 1]);
            self.cur += 1;
        }
    }

    #[cfg(target_os = "minix")]
    fn kill_line(&mut self) {
        let spaces = [b' '; SCR_COLS - 1];
        tty_write(b"\r");
        tty_write(&spaces);
        tty_write(b"\r");
        tty_write(b"# ");
        self.len = 0;
        self.cur = 0;
    }

    fn history_up(&mut self) {
        if self.hist_n == 0 {
            return;
        }
        if self.hist_pos == 0 {
            self.draft[..self.len].copy_from_slice(&self.line[..self.len]);
            self.draft_len = self.len;
        }
        if self.hist_pos < self.hist_n {
            self.hist_pos += 1;
            let idx = self.hist_n - self.hist_pos;
            let mut tmp = [0u8; EDIT_MAX];
            let n = self.hist_len[idx];
            tmp[..n].copy_from_slice(&self.hist[idx][..n]);
            self.set_line(&tmp[..n]);
            self.redraw_line();
        }
    }

    fn history_down(&mut self) {
        if self.hist_pos == 0 {
            return;
        }
        self.hist_pos -= 1;
        if self.hist_pos == 0 {
            let mut tmp = [0u8; EDIT_MAX];
            let n = self.draft_len;
            tmp[..n].copy_from_slice(&self.draft[..n]);
            self.set_line(&tmp[..n]);
        } else {
            let idx = self.hist_n - self.hist_pos;
            let mut tmp = [0u8; EDIT_MAX];
            let n = self.hist_len[idx];
            tmp[..n].copy_from_slice(&self.hist[idx][..n]);
            self.set_line(&tmp[..n]);
        }
        self.redraw_line();
    }

    fn push_history(&mut self, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        if self.hist_n == HIST_MAX {
            for i in 1..HIST_MAX {
                let mut tmp = [0u8; EDIT_MAX];
                let n = self.hist_len[i];
                tmp[..n].copy_from_slice(&self.hist[i][..n]);
                self.hist[i - 1][..n].copy_from_slice(&tmp[..n]);
                self.hist_len[i - 1] = n;
            }
            self.hist_n = HIST_MAX - 1;
        }
        let n = line.len().min(EDIT_MAX);
        self.hist[self.hist_n][..n].copy_from_slice(&line[..n]);
        self.hist_len[self.hist_n] = n;
        self.hist_n += 1;
    }
}

/// Set the tty's line mode: raw (the editor drives echo/editing — ISIG off
/// so ^C arrives as data and the editor aborts the line itself) or
/// canonical (normal echo + ISIG for foreground commands). Returns false
/// when fd 0 is not a terminal (the caller then falls back to a plain read
/// loop).
#[cfg(target_os = "minix")]
fn tty_set_raw(raw: bool) -> bool {
    use minix_std::termios::{ECHO, ECHOCTL, ECHOE, ECHOK, ECHONL, ICANON, ISIG, TIOCSETA};
    let mut t = minix_std::termios::Termios::zeroed();
    if unsafe { minix_std::termios::tcgetattr(0, &mut t) }.is_err() {
        return false;
    }
    if raw {
        t.c_lflag &=
            !(ICANON | ECHO | ECHOE | ECHOK | ECHONL | ECHOCTL | ISIG | minix_std::termios::IEXTEN);
    } else {
        t.c_lflag |= ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ISIG | minix_std::termios::IEXTEN;
    }
    unsafe { minix_std::termios::tcsetattr(0, TIOCSETA, &t) }.is_ok()
}

/// Read one line from stdin into `buf` with the raw-mode editor. Returns
/// the number of bytes stored (excluding the newline), `0` for an aborted
/// or empty line, or `LINE_EOF` for ^D at an empty prompt. The editor
/// (history) persists across calls.
#[cfg(target_os = "minix")]
fn read_line(ed: &mut Editor, buf: &mut [u8]) -> usize {
    // The editor persists across calls (history); start each line fresh.
    ed.len = 0;
    ed.cur = 0;
    ed.hist_pos = 0;
    // ESC sequence state: 0 none, 1 saw ESC, 2 saw ESC[.
    let mut esc = 0u8;
    loop {
        let mut b = [0u8; 1];
        let n = minix_rt::read(0, &mut b);
        if n == minix_std::EAGAIN as i64 {
            continue; // pty slave: no byte yet — retry
        }
        if n == minix_std::EINTR as i64 {
            // ^C: ISIG consumed it and signaled; the shell ignores SIGINT.
            tty_write(b"^C\n");
            return 0;
        }
        if n <= 0 {
            break;
        }
        let ch = b[0];
        if esc == 1 {
            esc = if ch == b'[' { 2 } else { 0 };
            continue;
        }
        if esc == 2 {
            esc = 0;
            match ch {
                b'A' => ed.history_up(),
                b'B' => ed.history_down(),
                b'C' => ed.cursor_right(),
                b'D' => ed.cursor_left(),
                _ => {}
            }
            continue;
        }
        match ch {
            0x1B => esc = 1,
            b'\n' | b'\r' => {
                tty_write(b"\n");
                let n = ed.len.min(buf.len());
                buf[..n].copy_from_slice(&ed.line[..n]);
                return n;
            }
            0x08 | 0x7F => ed.backspace(),
            0x01 => ed.cursor_home(),
            0x05 => ed.cursor_end(),
            0x0B => ed.kill_to_end(),
            0x0C => { /* ^L: no ANSI clear on the console — ignore */ }
            0x15 => ed.kill_line(),
            0x17 => ed.kill_word(),
            0x03 => {
                // ^C with ISIG off (shouldn't happen) — abort the line.
                tty_write(b"^C\n");
                return 0;
            }
            0x04 => {
                if ed.len == 0 {
                    return LINE_EOF;
                }
                // ^D on a non-empty line: delete the char under the cursor.
                if ed.cur < ed.len {
                    for i in ed.cur..ed.len - 1 {
                        ed.line[i] = ed.line[i + 1];
                    }
                    ed.len -= 1;
                    ed.sync_from(ed.cur);
                }
            }
            c if (0x20..=0x7E).contains(&c) => ed.insert(c),
            _ => {}
        }
    }
    ed.len
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Scan `raw_tokens[0..raw_argc]` for `>` and split off the redirect filename.
/// The returned `ParsedCommand` contains only the non-redirect tokens.
#[cfg(target_os = "minix")]
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
#[cfg(target_os = "minix")]
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
#[cfg(target_os = "minix")]
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
#[cfg(target_os = "minix")]
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
        let s = minix_rt::waitpid_status(pid);
        if s >= 0 {
            last_status = s;
        }
    }
    last_status
}

/// Run one command in the current process (used by pipeline children, which
/// are already forked). Builtins run directly; external commands exec in
/// place. Handles an optional stdout file redirect.
#[cfg(target_os = "minix")]
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
#[cfg(target_os = "minix")]
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
            let status = minix_rt::waitpid_status(pid);
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

#[cfg(target_os = "minix")]
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

#[cfg(target_os = "minix")]
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
        "memstat" => memstat(args),
        "regions" => regions(args),
        "hangdump" => hangdump(args),
        "help" => {
            write_out(b"available commands: echo cat cp ls mkdir rm ln");
            write_out(b" chmod chown sync mknod reboot fsck memstat regions help clear\r\n");
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

#[cfg(target_os = "minix")]
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
    let status = minix_rt::waitpid_status(pid);
    if status < 0 {
        write_err(b"sh: waitpid failed\r\n");
        1
    } else {
        status
    }
}

/// Build the first candidate path for `cmd_bytes` into `cmd_path`.
/// Returns the length (including null terminator), or 0 on overflow.
#[cfg(target_os = "minix")]
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
#[cfg(target_os = "minix")]
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
#[cfg(target_os = "minix")]
fn setup_redirect(outfile: &str) -> i32 {
    // Open the file WITHOUT closing fd 1.
    // The returned fd (>= 3) avoids the kernel's serial shortcut
    // for fd 1/2, so writes go through VFS to the filesystem.
    match unsafe {
        minix_std::fs::open(
            outfile.as_bytes(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_insert_at_end_and_middle() {
        let mut ed = Editor::new();
        ed.insert(b'e');
        ed.insert(b'c');
        ed.insert(b'h');
        assert_eq!(&ed.line[..ed.len], b"ech");
        assert_eq!(ed.cur, 3);
        // Move to the middle and insert.
        ed.cur = 1;
        ed.insert(b'X');
        assert_eq!(&ed.line[..ed.len], b"eXch");
        assert_eq!(ed.cur, 2);
    }

    #[test]
    fn editor_backspace_mid_line() {
        let mut ed = Editor::new();
        for c in b"abcd".iter() {
            ed.insert(*c);
        }
        ed.cur = 3; // between c and d
        ed.backspace(); // remove c
        assert_eq!(&ed.line[..ed.len], b"abd");
        assert_eq!(ed.cur, 2);
    }

    #[test]
    fn editor_kill_word_and_to_end() {
        let mut ed = Editor::new();
        for c in b"rm -rf /".iter() {
            ed.insert(*c);
        }
        ed.kill_word(); // removes " /" ... actually "rm -rf " + word
        assert_eq!(&ed.line[..ed.len], b"rm -rf ");
        ed.cur = ed.len;
        ed.insert(b't');
        ed.insert(b'm');
        ed.insert(b'p');
        ed.cur = 3; // after "rm "
        ed.kill_to_end();
        assert_eq!(&ed.line[..ed.len], b"rm ");
    }

    #[test]
    fn editor_history_navigation() {
        let mut ed = Editor::new();
        ed.push_history(b"echo one");
        ed.push_history(b"echo two");
        assert_eq!(ed.hist_n, 2);
        ed.history_up();
        assert_eq!(&ed.line[..ed.len], b"echo two");
        ed.history_up();
        assert_eq!(&ed.line[..ed.len], b"echo one");
        ed.history_down();
        assert_eq!(&ed.line[..ed.len], b"echo two");
        ed.history_down();
        // Back to the (empty) draft.
        assert_eq!(ed.len, 0);
    }

    #[test]
    fn editor_history_keeps_draft() {
        let mut ed = Editor::new();
        ed.push_history(b"ls");
        for c in b"cat ".iter() {
            ed.insert(*c);
        }
        // Type "cat " then Up: the draft "cat " must be restored on Down.
        ed.history_up();
        assert_eq!(&ed.line[..ed.len], b"ls");
        ed.history_down();
        assert_eq!(&ed.line[..ed.len], b"cat ");
    }

    #[test]
    fn editor_overwrite_then_backspace() {
        // The user's scenario: type `arsrst`, move back, overwrite, then
        // backspace — the deleted char must come out and the cursor track.
        let mut ed = Editor::new();
        for c in b"arsrst".iter() {
            ed.insert(*c);
        }
        ed.cur = 3; // move back three (cursor_left on the real tty)
        assert_eq!(ed.cur, 3);
        ed.insert(b'X'); // overwrite: "arsXrst", cur 4
        assert_eq!(&ed.line[..ed.len], b"arsXrst");
        ed.backspace(); // delete the X: "arsrst", cur 3
        assert_eq!(&ed.line[..ed.len], b"arsrst");
        assert_eq!(ed.cur, 3);
        // And a second backspace deletes the 's' (index 2).
        ed.backspace();
        assert_eq!(&ed.line[..ed.len], b"arrst");
        assert_eq!(ed.cur, 2);
    }

    #[test]
    fn editor_history_ring_drops_oldest() {
        let mut ed = Editor::new();
        for i in 0..(HIST_MAX + 5) {
            let mut s = [0u8; 4];
            let n = write_dec(&mut s, i as u32);
            ed.push_history(&s[s.len() - n..]);
        }
        assert_eq!(ed.hist_n, HIST_MAX);
        // The oldest entries (0..5) were dropped.
        ed.history_up();
        assert_eq!(&ed.line[..ed.len], b"20");
    }

    fn write_dec(buf: &mut [u8], mut v: u32) -> usize {
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 || i == 0 {
                break;
            }
        }
        buf.len() - i
    }
}
