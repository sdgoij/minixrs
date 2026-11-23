//! Shell (`sh`) with `>` redirection and `&&` operator.
//!
//! Extracted from `lib.rs` for clarity as the shell grows.

use crate::write_out;

#[cfg(target_os = "minix")]
use crate::write_err;
#[cfg(target_os = "minix")]
use crate::{
    cat, chmod, chown, cp, echo, errstr, for_each_dir_entry, fsck, hangdump, ln, ls, memstat,
    mkdir, mknod, reboot, regions, rm, set_redirect_fd, sync,
};
#[cfg(target_os = "minix")]
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};

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

/// Commands `run_builtin` implements in-process.
#[cfg(target_os = "minix")]
const BUILTINS: &[&str] = &[
    "echo", "cat", "cp", "ls", "mkdir", "rm", "ln", "chmod", "chown", "sync", "mknod", "reboot",
    "fsck", "memstat", "regions", "hangdump", "help", "clear",
];

/// Commands the prompt handles before `is_builtin_cmd`: `cd` moves the shell's directory and
/// `exit` ends its loop, so neither has a `run_builtin` arm.
#[cfg(target_os = "minix")]
const INTERNAL_COMMANDS: &[&str] = &["cd", "exit"];

/// True if `cmd` is a shell builtin (run in-process).
#[cfg(target_os = "minix")]
fn is_builtin_cmd(cmd: &str) -> bool {
    BUILTINS.contains(&cmd)
}

// ---------------------------------------------------------------------------
// Public entry point — replaces the old inline `sh` in lib.rs
// ---------------------------------------------------------------------------

pub fn sh(args: &[&str]) -> i32 {
    // init recorded this process as the console session (`record_session` in
    // `lib.rs`), and the pid survives both execs, so this is the entry it owns.
    #[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
    {
        let pid = minix_rt::getpid();
        let status = sh_loop(args);
        // A missing session file (an image built before init wrote one) is not
        // worth a line; anything else means the file is there and wrong.
        match minix_std::utmpx::retire_record(minix_std::utmpx::DEFAULT_FILE, pid) {
            Ok(()) => {}
            Err(e) if e.0 == 2 => {}
            Err(_) => write_err(b"sh: cannot update /var/run/utmpx\n"),
        }
        status
    }
    #[cfg(not(all(target_os = "minix", not(target_arch = "wasm32"))))]
    {
        sh_loop(args)
    }
}

fn sh_loop(_args: &[&str]) -> i32 {
    #[cfg(not(target_os = "minix"))]
    {
        write_out(b"sh: stub (no MINIX syscall ABI on host)\n");
        0
    }
    #[cfg(target_os = "minix")]
    {
        // Ignore SIGINT: the tty's sigchar sends it on ^C, and the shell
        // must survive it at the prompt (the editor's read just gets EINTR
        // and the loop reprints the prompt).
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
            // order — prompt-time reaping is the pattern.
            reap_jobs();
            // The count goes with the prompt: anything another writer takes the line for after
            // this is a prompt the editor has to reprint.
            LINE_TAKEN.store(line_taken(), Ordering::Relaxed);
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

            // Split into words, removing quoting. `buf` is rewritten in place
            // (quotes and the bytes they protect are compacted out), so the
            // words borrow it; history already holds the line as typed.
            let mut spans = [(0usize, 0usize); 32];
            let mut raw_argc = match tokenize(&mut buf, line_len, &mut spans) {
                Ok(n) => n,
                Err(TokenizeError::UnmatchedQuote(q)) => {
                    write_err(b"sh: unexpected EOF while looking for matching `");
                    write_err(&[q]);
                    write_err(b"'\n");
                    if editor_ok {
                        tty_set_raw(true);
                    }
                    continue;
                }
            };
            let mut raw_tokens = [""; 32];
            for (i, &(start, len)) in spans[..raw_argc].iter().enumerate() {
                raw_tokens[i] = core::str::from_utf8(&buf[start..start + len]).unwrap_or("");
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
        write_err(b"sh: fork failed\n");
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
    write_out(b"]\n");
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
                write_out(b")\n");
            } else {
                write_out(b"done\n");
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

/// How many times another writer had taken the console line when the prompt was drawn. The
/// editor compares it with the tty's count before acting on a byte: a program printing over the
/// prompt pushes it up, and the prompt then has to be reprinted on the line the typing is on.
#[cfg(target_os = "minix")]
static LINE_TAKEN: AtomicU32 = AtomicU32::new(0);

/// The tty's count of how many times another writer has taken the console line.
///
/// Zero when the request is refused, which is what a line that is not a console does (a pty
/// slave never has its line taken): the editor then never redraws, which is right there.
#[cfg(target_os = "minix")]
fn line_taken() -> u32 {
    let mut count: u32 = 0;
    let arg = &mut count as *mut u32 as *mut u8;
    if unsafe { minix_std::fs::ioctl(0, minix_std::termios::TIOCGTAKEN, arg) }.is_err() {
        return 0;
    }
    count
}

/// Characters that end a completion word. `>` and `<` are redirections, so the word after one
/// names a file; `|`, `&` and `;` begin another command.
#[cfg(any(target_os = "minix", test))]
fn is_word_sep(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'|' | b'>' | b'<' | b'&' | b';')
}

/// The word around `cur`: from the previous separator to the next one. The word is empty when
/// the cursor sits on a separator or in the blank after one.
#[cfg(any(target_os = "minix", test))]
fn word_at(line: &[u8], cur: usize) -> (usize, usize) {
    let cur = cur.min(line.len());
    let mut start = cur;
    while start > 0 && !is_word_sep(line[start - 1]) {
        start -= 1;
    }
    let mut end = cur;
    while end < line.len() && !is_word_sep(line[end]) {
        end += 1;
    }
    (start, end)
}

/// Whether a word beginning at `start` is a command. The preceding non-blank byte decides:
/// nothing, `|`, `&` or `;` begins a command; a word or a redirection does not.
#[cfg(any(target_os = "minix", test))]
fn at_command_position(line: &[u8], start: usize) -> bool {
    let mut i = start;
    while i > 0 && (line[i - 1] == b' ' || line[i - 1] == b'\t') {
        i -= 1;
    }
    if i == 0 {
        return true;
    }
    matches!(line[i - 1], b'|' | b'&' | b';')
}

/// How many names a completion menu holds. The command set is under 200; the rest is room for
/// a directory with more entries than the prompt will usefully show.
#[cfg(any(target_os = "minix", test))]
const MENU_MAX: usize = 256;
/// Bytes the menu's names are packed into (a MinixFS name is at most 60 bytes).
#[cfg(any(target_os = "minix", test))]
const MENU_BYTES: usize = 8 * 1024;
/// Room for the rendered list: every name, a space between them, one newline per row, and the
/// overflow marker.
#[cfg(target_os = "minix")]
const MENU_RENDER_MAX: usize = MENU_BYTES + 2 * MENU_MAX + 16;

#[cfg(any(target_os = "minix", test))]
fn menu_name(arena: &[u8], span: (usize, usize)) -> &[u8] {
    &arena[span.0..span.0 + span.1]
}

/// The candidates the prompt is cycling through, and the span of the line the current one
/// occupies.
#[cfg(any(target_os = "minix", test))]
struct Menu {
    #[cfg(target_os = "minix")]
    active: bool,
    #[cfg(target_os = "minix")]
    start: usize,
    #[cfg(target_os = "minix")]
    end: usize,
    #[cfg(target_os = "minix")]
    current: usize,
    count: usize,
    used: usize,
    overflow: bool,
    arena: [u8; MENU_BYTES],
    spans: [(usize, usize); MENU_MAX],
}

#[cfg(any(target_os = "minix", test))]
impl Menu {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "minix")]
            active: false,
            #[cfg(target_os = "minix")]
            start: 0,
            #[cfg(target_os = "minix")]
            end: 0,
            #[cfg(target_os = "minix")]
            current: 0,
            count: 0,
            used: 0,
            overflow: false,
            arena: [0; MENU_BYTES],
            spans: [(0, 0); MENU_MAX],
        }
    }

    /// Start a new set of candidates. `offer` fills it, then `sort` orders it.
    #[cfg(target_os = "minix")]
    fn clear(&mut self) {
        self.active = false;
        self.count = 0;
        self.used = 0;
        self.overflow = false;
    }

    /// Add `name` unless an equal one is already there. The first spelling wins, so the scan's
    /// source order does not matter for a duplicate.
    fn offer(&mut self, name: &[u8]) {
        if name.is_empty() {
            return;
        }
        if self.count == MENU_MAX || self.used + name.len() > MENU_BYTES {
            self.overflow = true;
            return;
        }
        let duplicate = self.spans[..self.count]
            .iter()
            .any(|&span| menu_name(&self.arena, span) == name);
        if duplicate {
            return;
        }
        let off = self.used;
        self.arena[off..off + name.len()].copy_from_slice(name);
        self.spans[self.count] = (off, name.len());
        self.used += name.len();
        self.count += 1;
    }

    /// Sort bytewise: the list and the cycle both use this order.
    fn sort(&mut self) {
        let arena = &self.arena;
        self.spans[..self.count]
            .sort_unstable_by(|a, b| menu_name(arena, *a).cmp(menu_name(arena, *b)));
    }

    fn name(&self, index: usize) -> &[u8] {
        menu_name(&self.arena, self.spans[index])
    }

    /// Lay the names out for the screen: space-separated, wrapped at `SCR_COLS`, with `...`
    /// when the menu overflowed. Returns the number of bytes written into `out`.
    fn render(&self, out: &mut [u8]) -> usize {
        let mut n = 0usize;
        let mut col = 0usize;
        for i in 0..self.count {
            let name = self.name(i);
            if col > 0 && col + name.len() + 1 > SCR_COLS {
                n = menu_put(out, n, b"\n");
                col = 0;
            }
            n = menu_put(out, n, name);
            col += name.len();
            if i + 1 < self.count {
                n = menu_put(out, n, b" ");
                col += 1;
            }
        }
        if self.overflow {
            if col > 0 {
                if col + 4 > SCR_COLS {
                    n = menu_put(out, n, b"\n");
                    col = 0;
                } else {
                    n = menu_put(out, n, b" ");
                    col += 1;
                }
            }
            n = menu_put(out, n, b"...");
            col += 3;
        }
        if col > 0 {
            n = menu_put(out, n, b"\n");
        }
        n
    }

    /// Clear the prompt's row and print the list on it. The caller redraws the prompt and line
    /// below the list afterwards.
    #[cfg(target_os = "minix")]
    fn print(&self) {
        let spaces = [b' '; SCR_COLS - 1];
        tty_write(b"\r");
        tty_write(&spaces);
        tty_write(b"\r");
        let mut out = [0u8; MENU_RENDER_MAX];
        let n = self.render(&mut out);
        tty_write(&out[..n]);
    }
}

/// Append `bytes` at `n`, stopping at the buffer's end.
#[cfg(any(target_os = "minix", test))]
fn menu_put(out: &mut [u8], n: usize, bytes: &[u8]) -> usize {
    let room = out.len() - n;
    let copy = bytes.len().min(room);
    out[n..n + copy].copy_from_slice(&bytes[..copy]);
    n + copy
}

/// The byte that follows a unique path completion: `/` when it names a directory, a space when
/// it names a file.
#[cfg(target_os = "minix")]
fn directory_suffix(dir: &[u8], name: &[u8]) -> u8 {
    let mut path = [0u8; EDIT_MAX + 64];
    let len = dir.len() + name.len();
    if len > path.len() {
        return b' ';
    }
    path[..dir.len()].copy_from_slice(dir);
    path[dir.len()..len].copy_from_slice(name);
    let is_dir = minix_std::fs::stat_bytes(&path[..len])
        .map(|st| st.st_mode & minix_std::fs::S_IFMT == minix_std::fs::S_IFDIR)
        .unwrap_or(false);
    if is_dir { b'/' } else { b' ' }
}

/// Room for the applet names `coreutils --list` prints. The `feat_minix` set is about 100
/// names, under 1 KiB; the rest is headroom.
#[cfg(target_os = "minix")]
const APPLETS_MAX: usize = 2048;

/// The multicall's applet names, read once from `coreutils --list`.
///
/// Command completion otherwise knows only names that are files in `/bin` and `/sbin`, and the
/// applets are not: they are dispatch targets inside `/bin/coreutils`. The read is lazy so a
/// boot that never completes a command does not pay the 6 MiB multicall's first-run pre-fault,
/// and the cache keeps later Tabs from paying it again. A failed read stays an empty ready
/// cache, so a missing multicall is not retried on every Tab.
#[cfg(target_os = "minix")]
struct AppletCache {
    names: [u8; APPLETS_MAX],
    len: usize,
    ready: bool,
}

#[cfg(target_os = "minix")]
impl AppletCache {
    fn new() -> Self {
        Self {
            names: [0; APPLETS_MAX],
            len: 0,
            ready: false,
        }
    }

    fn ensure(&mut self) {
        if self.ready {
            return;
        }
        self.ready = true;
        self.len = 0;
        let (r, w) = match minix_std::fs::pipe() {
            Ok(pair) => pair,
            Err(_) => return,
        };
        let (r, w) = lift_pipe_fds(r, w, 0);
        // A pipe end still below 3 could not be lifted, and reading it would read the console.
        if r < 3 || w < 3 {
            let _ = minix_std::fs::close(r);
            let _ = minix_std::fs::close(w);
            return;
        }
        let pid = minix_rt::fork();
        if pid < 0 {
            let _ = minix_std::fs::close(r);
            let _ = minix_std::fs::close(w);
            return;
        }
        if pid == 0 {
            if minix_std::fs::dup2(w, 1).is_err() {
                minix_rt::exit(127);
            }
            unsafe { minix_rt::set_fd_vfs(1, 1) };
            let _ = minix_std::fs::close(r);
            let _ = minix_std::fs::close(w);
            let argv0 = b"/bin/coreutils\0";
            let argv1 = b"--list\0";
            let argv = [argv0.as_ptr(), argv1.as_ptr(), core::ptr::null()];
            unsafe {
                minix_rt::execve(
                    COREUTILS_PATH.as_ptr(),
                    COREUTILS_PATH.len(),
                    argv.as_ptr(),
                    core::ptr::null(),
                )
            };
            minix_rt::exit(127);
        }
        let _ = minix_std::fs::close(w);
        let mut scratch = [0u8; 256];
        loop {
            match unsafe { minix_std::fs::read(r, &mut scratch) } {
                Ok(0) => break,
                Ok(n) => {
                    let n = n as usize;
                    let room = APPLETS_MAX - self.len;
                    let copy = n.min(room);
                    self.names[self.len..self.len + copy].copy_from_slice(&scratch[..copy]);
                    self.len += copy;
                }
                Err(e) if e.0 == -minix_std::EAGAIN => continue,
                Err(_) => break,
            }
        }
        let _ = minix_std::fs::close(r);
        if minix_rt::waitpid_status(pid) != 0 {
            self.len = 0;
        } else if self.len == APPLETS_MAX {
            // Keep the last complete name: a partial one would complete to something that is
            // not an applet.
            self.len = match self.names[..self.len].iter().rposition(|&b| b == b'\n') {
                Some(pos) => pos + 1,
                None => 0,
            };
        }
    }
}

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
    #[cfg(target_os = "minix")]
    menu: Menu,
    #[cfg(target_os = "minix")]
    applets: AppletCache,
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
            #[cfg(target_os = "minix")]
            menu: Menu::new(),
            #[cfg(target_os = "minix")]
            applets: AppletCache::new(),
        }
    }

    fn set_line(&mut self, src: &[u8]) {
        let n = src.len().min(EDIT_MAX);
        self.line[..n].copy_from_slice(&src[..n]);
        self.len = n;
        self.cur = n;
    }

    /// Replace `line[start..end]` with `text`, leaving the cursor after it. Returns false when
    /// the result does not fit.
    #[cfg(any(target_os = "minix", test))]
    fn set_span(&mut self, start: usize, end: usize, text: &[u8]) -> bool {
        if start > end || end > self.len {
            return false;
        }
        let old = end - start;
        let new_len = self.len - old + text.len();
        if new_len > EDIT_MAX {
            return false;
        }
        if text.len() > old {
            let grow = text.len() - old;
            for i in (end..self.len).rev() {
                self.line[i + grow] = self.line[i];
            }
        } else {
            let shrink = old - text.len();
            for i in end..self.len {
                self.line[i - shrink] = self.line[i];
            }
        }
        self.line[start..start + text.len()].copy_from_slice(text);
        self.len = new_len;
        self.cur = start + text.len();
        true
    }

    /// Insert the rest of the single candidate after the `base_len` bytes already typed, then
    /// the suffix when the word ends the line. Returns false when nothing was added.
    #[cfg(any(target_os = "minix", test))]
    fn apply_unique(
        &mut self,
        word_end: usize,
        base_len: usize,
        name: &[u8],
        suffix: Option<u8>,
    ) -> bool {
        if base_len > name.len() {
            return false;
        }
        let extra = &name[base_len..];
        // A space or slash joins the completed word to what follows, so it only goes on when
        // the word is the last thing on the line.
        let suffix = if self.len == word_end { suffix } else { None };
        let add = extra.len() + if suffix.is_some() { 1 } else { 0 };
        if add == 0 || self.len + add > EDIT_MAX {
            return false;
        }
        while self.cur < word_end {
            self.cursor_right();
        }
        for &b in extra {
            self.insert(b);
        }
        if let Some(b) = suffix {
            self.insert(b);
        }
        true
    }

    /// Advance the active menu to the next candidate, wrapping, and replace the current one.
    #[cfg(target_os = "minix")]
    fn cycle_menu(&mut self) {
        if self.menu.count == 0 {
            self.menu.active = false;
            return;
        }
        let next = (self.menu.current + 1) % self.menu.count;
        let n = self.menu.name(next).len();
        let mut name = [0u8; EDIT_MAX];
        if n > name.len() {
            self.menu.active = false;
            tty_write(b"\x07");
            return;
        }
        name[..n].copy_from_slice(self.menu.name(next));
        if !self.set_span(self.menu.start, self.menu.end, &name[..n]) {
            self.menu.active = false;
            tty_write(b"\x07");
            return;
        }
        self.menu.current = next;
        self.menu.end = self.menu.start + n;
        self.redraw_line();
    }

    /// Complete the word at the cursor: command names at a command position, directory entries
    /// anywhere else. One match is completed and followed by a space (or `/` for a directory);
    /// several are listed and the first replaces the word, and each further Tab cycles to the
    /// next. A beep means there was nothing to offer.
    #[cfg(target_os = "minix")]
    fn complete(&mut self) {
        if self.menu.active {
            self.cycle_menu();
            return;
        }
        let command = {
            let line = &self.line[..self.len];
            let (start, end) = word_at(line, self.cur);
            at_command_position(line, start) && !line[start..end].contains(&b'/')
        };
        if command {
            self.applets.ensure();
        }
        self.menu.clear();
        let (span_start, word_end, base_len, suffix) = {
            let line = &self.line[..self.len];
            let (start, end) = word_at(line, self.cur);
            let word = &line[start..end];
            let menu = &mut self.menu;
            if command {
                for cmd in BUILTINS.iter().chain(INTERNAL_COMMANDS.iter()) {
                    let name = cmd.as_bytes();
                    if name.starts_with(word) {
                        menu.offer(name);
                    }
                }
                let applets = &self.applets.names[..self.applets.len];
                for applet in applets.split(|&b| b == b'\n') {
                    if !applet.is_empty() && applet.starts_with(word) {
                        menu.offer(applet);
                    }
                }
                for dir in [&b"/bin"[..], &b"/sbin"[..]] {
                    if let Ok(fd) = unsafe { minix_std::fs::open(dir, 0, 0) } {
                        for_each_dir_entry(fd, |name| {
                            if name != b"." && name != b".." && name.starts_with(word) {
                                menu.offer(name);
                            }
                        });
                        let _ = minix_std::fs::close(fd);
                    }
                }
                menu.sort();
                (start, end, word.len(), Some(b' '))
            } else {
                let slash = word.iter().rposition(|&b| b == b'/');
                let (dir, base) = match slash {
                    Some(i) => (&word[..=i], &word[i + 1..]),
                    None => (&b"."[..], word),
                };
                if let Ok(fd) = unsafe { minix_std::fs::open(dir, 0, 0) } {
                    for_each_dir_entry(fd, |name| {
                        if name == b"." || name == b".." {
                            if base.starts_with(b".") && name.starts_with(base) {
                                menu.offer(name);
                            }
                            return;
                        }
                        if name.starts_with(b".") && !base.starts_with(b".") {
                            return;
                        }
                        if name.starts_with(base) {
                            menu.offer(name);
                        }
                    });
                    let _ = minix_std::fs::close(fd);
                }
                menu.sort();
                let suffix = if menu.count == 1 {
                    Some(directory_suffix(dir, menu.name(0)))
                } else {
                    None
                };
                let dir_len = slash.map(|i| i + 1).unwrap_or(0);
                (start + dir_len, end, base.len(), suffix)
            }
        };
        match self.menu.count {
            0 => {
                self.menu.active = false;
                tty_write(b"\x07");
            }
            1 => {
                let n = self.menu.name(0).len();
                let mut name = [0u8; EDIT_MAX];
                name[..n].copy_from_slice(self.menu.name(0));
                if !self.apply_unique(word_end, base_len, &name[..n], suffix) {
                    tty_write(b"\x07");
                }
            }
            _ => {
                let n = self.menu.name(0).len();
                let mut name = [0u8; EDIT_MAX];
                name[..n].copy_from_slice(self.menu.name(0));
                self.menu.active = true;
                self.menu.start = span_start;
                self.menu.end = word_end;
                self.menu.current = 0;
                if !self.set_span(span_start, word_end, &name[..n]) {
                    self.menu.active = false;
                    tty_write(b"\x07");
                    return;
                }
                self.menu.end = span_start + n;
                self.menu.print();
                self.redraw_line();
            }
        }
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

    /// Reprint the prompt and the line on a fresh row, for after another writer took the row
    /// this one was on. The newline rather than a blank-and-return: the row the cursor is on
    /// belongs to whatever was written there, and a fresh one needs no clearing.
    #[cfg(target_os = "minix")]
    fn reprint_line(&mut self) {
        tty_write(b"\n# ");
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

    #[cfg(any(target_os = "minix", test))]
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
    ed.menu.clear();
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
        // Any key other than Tab ends a menu cycle; the next Tab starts a new completion.
        if ch != 0x09 {
            ed.menu.active = false;
        }
        // Another writer may have taken the line since the prompt was drawn (the session's
        // programs do, once at boot): the prompt has been pushed up and the cursor sits on a
        // blank line, so reprint it with what has been typed before acting on this byte.
        let taken = line_taken();
        if taken != LINE_TAKEN.load(Ordering::Relaxed) {
            LINE_TAKEN.store(taken, Ordering::Relaxed);
            ed.reprint_line();
        }
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
            0x09 => ed.complete(),
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

/// The quote a tokenizer pass stopped inside.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
enum TokenizeError {
    /// A `'` or `"` with no matching close — `bash -c 'echo hi`.
    UnmatchedQuote(u8),
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// Where a word's bytes are being written in `tokenize`'s line buffer, and how
/// many words are already complete. Exists so the five sites that emit a byte
/// (unquoted, either quote, either escape) do not each repeat the "start a word
/// if this byte is its first" step.
struct WordSink<'a> {
    line: &'a mut [u8],
    write: usize,
    start: Option<usize>,
    count: usize,
    spans: &'a mut [(usize, usize)],
}

#[allow(dead_code)]
impl WordSink<'_> {
    /// Begin a word at the current write position, if one is not open.
    fn begin_word(&mut self) {
        if self.start.is_none() {
            self.start = Some(self.write);
        }
    }

    fn push(&mut self, byte: u8) {
        self.begin_word();
        self.line[self.write] = byte;
        self.write += 1;
    }

    /// Close the open word, if any. Words past `spans` are dropped: the
    /// caller's token table is fixed.
    fn end_word(&mut self) {
        if let Some(start) = self.start.take()
            && self.count < self.spans.len()
        {
            self.spans[self.count] = (start, self.write - start);
            self.count += 1;
        }
    }
}

/// Split `line[..len]` into words with quoting removed, returning how many
/// there are. Word `i` is left as `spans[i]`, a byte range into `line`, whose
/// bytes have been compacted in place — quote characters and the escapes that
/// protected a byte are gone from the word's own text.
///
/// The words cannot be `split_whitespace()` results because whitespace inside
/// quotes is part of the word: `/bin/bash -c 'echo BASH-OK'` is three words,
/// and the third is `echo BASH-OK`, not two more.
///
/// That in-place compaction is why `line` is mutable and why the spans must be
/// read after the call: each byte is written at or before the position it was
/// read from, so no word's text is damaged by a later one.
#[allow(dead_code)]
fn tokenize(
    line: &mut [u8],
    len: usize,
    spans: &mut [(usize, usize)],
) -> Result<usize, TokenizeError> {
    let mut sink = WordSink {
        line,
        write: 0,
        start: None,
        count: 0,
        spans,
    };
    let mut quote = Quote::None;
    let mut read = 0usize;
    while read < len {
        let byte = sink.line[read];
        match quote {
            Quote::Single if byte == b'\'' => {
                quote = Quote::None;
                read += 1;
            }
            Quote::Single => {
                sink.push(byte);
                read += 1;
            }
            Quote::Double if byte == b'"' => {
                quote = Quote::None;
                read += 1;
            }
            Quote::Double
                if byte == b'\\'
                    && read + 1 < len
                    && matches!(sink.line[read + 1], b'"' | b'\\' | b'$' | b'`') =>
            {
                let protected = sink.line[read + 1];
                sink.push(protected);
                read += 2;
            }
            Quote::Double => {
                sink.push(byte);
                read += 1;
            }
            Quote::None if byte == b'\'' => {
                // Opening a quote begins a word even if the quotes hold
                // nothing: `echo ''` passes one empty argument.
                sink.begin_word();
                quote = Quote::Single;
                read += 1;
            }
            Quote::None if byte == b'"' => {
                sink.begin_word();
                quote = Quote::Double;
                read += 1;
            }
            Quote::None if byte.is_ascii_whitespace() => {
                sink.end_word();
                read += 1;
            }
            Quote::None if byte == b'\\' && read + 1 < len => {
                let protected = sink.line[read + 1];
                sink.push(protected);
                read += 2;
            }
            Quote::None => {
                sink.push(byte);
                read += 1;
            }
        }
    }
    if quote != Quote::None {
        let byte = if quote == Quote::Single { b'\'' } else { b'"' };
        return Err(TokenizeError::UnmatchedQuote(byte));
    }
    sink.end_word();
    Ok(sink.count)
}

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
                write_err(b"sh: pipe failed\n");
                return 1;
            }
        }
    }

    let mut pids = [0i32; MAX_PIPELINE];
    for i in 0..n {
        let pid = minix_rt::fork();
        if pid < 0 {
            write_err(b"sh: fork failed\n");
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
                    write_err(b"sh: dup2 failed\n");
                    minix_rt::exit(1);
                }
                unsafe { minix_rt::set_fd_vfs(0, 1) };
            }
            if i + 1 < n {
                let (_r, w) = pipes[i];
                if minix_std::fs::dup2(w, 1).is_err() {
                    write_err(b"sh: dup2 failed\n");
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
                write_err(b"sh: dup2 failed\n");
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
        write_err(b"' not found\n");
        return 1;
    }
    // If this returns, exec failed; say what the kernel said.
    let error = try_exec(args, &mut cmd_path);
    report_exec_failure(cmd, &cmd_path, error);
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
        write_out(b"\n");
        return SH_EXIT;
    }

    let is_builtin = is_builtin_cmd(cmd);

    match parsed.redirect_stdout {
        Some(outfile) => {
            // Redirection requested — always fork so the shell's stdio
            // is never affected.
            let pid = minix_rt::fork();
            if pid < 0 {
                write_err(b"sh: fork failed\n");
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
                        write_err(b"sh: dup2 failed\n");
                        minix_rt::exit(1);
                    }
                    unsafe { minix_rt::set_fd_vfs(1, 1) };
                    let mut cmd_path = [0u8; 256];
                    let path_len = build_path(cmd.as_bytes(), &mut cmd_path);
                    if path_len == 0 {
                        write_err(b"sh: '");
                        write_err(cmd.as_bytes());
                        write_err(b"' not found\n");
                        minix_rt::exit(1);
                    }
                    // If this returns, exec failed; say what the kernel said.
                    let error = try_exec(args, &mut cmd_path);
                    report_exec_failure(cmd, &cmd_path, error);
                    minix_rt::exit(1);
                };
                minix_rt::exit(status);
            }
            // Parent: wait.
            let status = minix_rt::waitpid_status(pid);
            if status < 0 {
                write_err(b"sh: waitpid failed\n");
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
        write_err(b"sh: cd: missing argument\n");
        return 1;
    }
    let path = args[1].as_bytes();
    let r = minix_rt::chdir(path);
    if r < 0 {
        write_err(b"sh: cd: ");
        write_err(path);
        write_err(b": ");
        write_err(errstr(r as i32));
        write_err(b"\n");
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
            write_out(b" chmod chown sync mknod reboot fsck memstat regions help clear\n");
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
        write_err(b"' not found\n");
        return 1;
    }

    let pid = minix_rt::fork();
    if pid < 0 {
        write_err(b"sh: fork failed\n");
        return 1;
    }
    if pid == 0 {
        // If this returns, exec failed; say what the kernel said.
        let error = try_exec(args, &mut cmd_path);
        report_exec_failure(cmd, &cmd_path, error);
        minix_rt::exit(1);
    }

    // Parent: wait.
    let status = minix_rt::waitpid_status(pid);
    if status < 0 {
        write_err(b"sh: waitpid failed\n");
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

/// Pack `args` into `buf` as NUL-terminated strings, 8-byte aligned, and write each string's
/// address into `argv[1..]`; `argv[0]` is the caller's.
///
/// False when an argument does not fit, with the ones before it packed. The shell's arguments come
/// from a line no longer than `EDIT_MAX`, so the buffer the caller passes cannot run out.
#[cfg(any(target_os = "minix", test))]
fn pack_args(buf: &mut [u8], args: &[&str], argv: &mut [*const u8]) -> bool {
    let mut off = 0usize;
    for (i, arg) in args.iter().enumerate() {
        let Some(slot) = argv.get_mut(i + 1) else {
            return false;
        };
        let tok = arg.as_bytes();
        if off + tok.len() + 1 > buf.len() {
            return false;
        }
        buf[off..off + tok.len()].copy_from_slice(tok);
        buf[off + tok.len()] = 0;
        *slot = buf.as_ptr().wrapping_add(off);
        off = (off + tok.len() + 1 + 7) & !7;
    }
    true
}

/// The uutils multicall in every hardware image (`crates/boot-image/src/manifest.rs`). A bare
/// name that `/bin`, `/sbin` and `/usr/bin` all lack is looked up there as an applet.
#[cfg(target_os = "minix")]
const COREUTILS_PATH: &[u8] = b"/bin/coreutils";

/// In the child process, try to exec the command.
///
/// Returns `None` when the argument frame could not be built (the caller's line is bounded, so
/// that is a bug rather than a full argv), otherwise the kernel's error for the last path tried:
/// `Some(errno)`, which is never 0 because a successful exec does not return.
#[cfg(target_os = "minix")]
fn try_exec(args: &[&str], cmd_path: &mut [u8; 256]) -> Option<i32> {
    let child_path_len = cmd_path.iter().position(|&b| b == 0).unwrap_or(255) + 1;
    let cmd_end = child_path_len - 1;
    let cmd_start = (0..cmd_end)
        .rev()
        .find(|&i| cmd_path[i] == b'/')
        .map(|i| i + 1)
        .unwrap_or(0);
    let cmd_len = cmd_end - cmd_start;

    // Save the command name for the directory fallbacks.
    let mut cmd_name = [0u8; 56];
    let cmd_name_len = cmd_len.min(56);
    cmd_name[..cmd_name_len].copy_from_slice(&cmd_path[cmd_start..cmd_start + cmd_name_len]);

    let mut argv_buf: [*const u8; 32] = [core::ptr::null(); 32];
    argv_buf[0] = cmd_path.as_ptr();
    // The arguments get a buffer of their own. They used to be packed into `cmd_path` behind the
    // path, where one argument could occupy at most 55 bytes and a longer one was silently cut to
    // a prefix (measured: `/bin/echo` with a 61-byte argument printed 55). The line they came
    // from is at most `EDIT_MAX` bytes, so this buffer holds all of them with room to spare.
    let mut arg_buf = [0u8; 4 * EDIT_MAX];
    let argc = args.len().min(32);
    if !pack_args(&mut arg_buf, &args[1..argc], &mut argv_buf[..argc]) {
        return None;
    }

    let r = unsafe {
        minix_rt::execve(
            cmd_path.as_ptr(),
            child_path_len,
            argv_buf.as_ptr(),
            core::ptr::null(),
        )
    };

    // Try /sbin/<cmd>, then /usr/bin/<cmd>, but only for a bare name and only when the
    // previous path was simply not there. Any other error is about the command the user
    // asked for, and retrying a path that cannot exist would replace it with the fallback's
    // own ENOENT (item 50). `/usr/bin` is where the system image's toolchain lives (clang,
    // clang++, ld.lld); the boot image's `/bin` and `/sbin` do not carry it.
    let cmd = args.first().copied().unwrap_or("");
    let bare = !cmd.contains('/');
    let mut error = r;
    if bare && should_try_next_dir(error) && cmd_start > 1 && 6 + cmd_name_len < cmd_path.len() {
        cmd_path[..6].copy_from_slice(b"/sbin/");
        cmd_path[6..6 + cmd_name_len].copy_from_slice(&cmd_name[..cmd_name_len]);
        cmd_path[6 + cmd_name_len] = 0;
        error = unsafe {
            minix_rt::execve(
                cmd_path.as_ptr(),
                6 + cmd_name_len + 1,
                argv_buf.as_ptr(),
                core::ptr::null(),
            )
        };
    }
    if bare && should_try_next_dir(error) && cmd_start > 1 && 9 + cmd_name_len < cmd_path.len() {
        cmd_path[..9].copy_from_slice(b"/usr/bin/");
        cmd_path[9..9 + cmd_name_len].copy_from_slice(&cmd_name[..cmd_name_len]);
        cmd_path[9 + cmd_name_len] = 0;
        error = unsafe {
            minix_rt::execve(
                cmd_path.as_ptr(),
                9 + cmd_name_len + 1,
                argv_buf.as_ptr(),
                core::ptr::null(),
            )
        };
    }

    // Try /bin/coreutils as the multicall. It selects its applet from `argv[0]`, so the
    // command name goes there and the arguments stay where `pack_args` put them. A name that
    // did not fit with a terminator cannot be an applet, and `should_try_multicall` refuses a
    // path or the multicall's own name.
    if should_try_multicall(cmd, error) && cmd_name_len < cmd_name.len() {
        argv_buf[0] = cmd_name.as_ptr();
        let r2 = unsafe {
            minix_rt::execve(
                COREUTILS_PATH.as_ptr(),
                COREUTILS_PATH.len(),
                argv_buf.as_ptr(),
                core::ptr::null(),
            )
        };
        if r2 == ENOENT {
            // Nothing answered to the multicall's path (the wasm image carries no coreutils),
            // so the command's own error is the one to report.
            return Some(error);
        }
        cmd_path[..COREUTILS_PATH.len()].copy_from_slice(COREUTILS_PATH);
        cmd_path[COREUTILS_PATH.len()] = 0;
        return Some(r2);
    }
    Some(error)
}

/// `ENOENT` as the kernel reports it. The shell compares against no other error, so this is not
/// an errno table: `minix-rt` hands back the kernel's negative numbers as they are.
#[cfg(any(target_os = "minix", test))]
const ENOENT: i32 = -2;

/// Whether another search directory is worth trying after the previous one failed with
/// `primary`.
///
/// Only the file's absence makes a second directory meaningful. Every other error (the heap,
/// the loader, a page it cannot read) is about the command the user asked for, and retrying a
/// path that cannot exist replaced it with the fallback's own `ENOENT`: the random `/bin/bash`
/// failure was reported as `/sbin/bash: -2` for exactly that reason, and the errno owed to
/// item 50 was never the real one.
#[cfg(any(target_os = "minix", test))]
fn should_try_next_dir(primary: i32) -> bool {
    primary == ENOENT
}

/// Whether a command that `/bin`, `/sbin` and `/usr/bin` all lack is an applet of `/bin/coreutils`.
///
/// The multicall dispatches on `argv[0]`, so a bare name can be retried with that name as
/// `argv[0]`. An explicit path names a file, and silently running a different program for it
/// would be a surprise; `coreutils` itself is skipped so a missing multicall is not retried as
/// one of its own applets. Like the `/sbin` fallback, only an absent file is worth a retry:
/// every other error is about the command the user asked for.
#[cfg(any(target_os = "minix", test))]
fn should_try_multicall(cmd: &str, last_error: i32) -> bool {
    last_error == ENOENT && !cmd.is_empty() && !cmd.contains('/') && cmd != "coreutils"
}

/// Say why an external command did not run.
///
/// Every failure used to read `not found`, which is true only for `ENOENT`. `execve` can also
/// fail on the heap (`minix-rt` builds the argument frame with `sbrk` before the syscall), on an
/// ELF the loader rejects, or on a page it cannot read, and a gate that cannot tell those apart
/// cannot be chased: the aarch64 interactive-bash flake reported `not found` for a `/bin/bash`
/// that was on the disk. The path is printed because the last thing tried may be `/usr/bin/<cmd>`.
#[cfg(target_os = "minix")]
fn report_exec_failure(cmd: &str, cmd_path: &[u8; 256], error: Option<i32>) {
    write_err(b"sh: '");
    write_err(cmd.as_bytes());
    write_err(b"' not found");
    match error {
        None => write_err(b" (the arguments did not fit)\n"),
        Some(errno) => {
            let path_end = cmd_path.iter().position(|&b| b == 0).unwrap_or(0);
            write_err(b" (");
            write_err(&cmd_path[..path_end]);
            write_err(b": ");
            let mut buf = [0u8; 12];
            write_err(format_errno(&mut buf, errno));
            write_err(b")\n");
        }
    }
}

/// Write `errno` into `buf` as its signed decimal, returning the slice it occupies.
///
/// The kernel's errors are negative, so the sign is part of the message; `unsigned_abs` keeps
/// `i32::MIN` from overflowing.
#[cfg(any(target_os = "minix", test))]
fn format_errno(buf: &mut [u8; 12], errno: i32) -> &[u8] {
    let mut v = errno.unsigned_abs();
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    if errno < 0 {
        i -= 1;
        buf[i] = b'-';
    }
    &buf[i..]
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
            write_err(b"\n");
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

    /// The words `tokenize` found in a line, read back the way `sh` reads
    /// them. A struct because the crate has no `Vec` to return them in.
    struct Words {
        buf: [u8; 256],
        spans: [(usize, usize); 32],
        count: usize,
    }

    impl Words {
        fn new(line: &[u8]) -> Self {
            let mut buf = [0u8; 256];
            buf[..line.len()].copy_from_slice(line);
            let mut spans = [(0usize, 0usize); 32];
            let count = tokenize(&mut buf, line.len(), &mut spans).expect("no unmatched quote");
            Words { buf, spans, count }
        }

        fn get(&self, i: usize) -> &[u8] {
            let (start, len) = self.spans[i];
            &self.buf[start..start + len]
        }
    }

    #[test]
    fn tokenize_splits_on_unquoted_whitespace() {
        let w = Words::new(b"/bin/echo  hello\tworld");
        assert_eq!(w.count, 3);
        assert_eq!(w.get(0), b"/bin/echo");
        assert_eq!(w.get(1), b"hello");
        assert_eq!(w.get(2), b"world");
    }

    #[test]
    fn tokenize_keeps_a_quoted_argument_in_one_word() {
        // The case that exposed the gap: a shell that splits on whitespace
        // alone hands bash `'echo` and `BASH-OK'`, and bash reports an
        // unmatched quote instead of running the command.
        for line in [
            b"/bin/bash -c 'echo BASH-OK'".as_slice(),
            b"/bin/bash -c \"echo BASH-OK\"".as_slice(),
        ] {
            let w = Words::new(line);
            assert_eq!(w.count, 3);
            assert_eq!(w.get(0), b"/bin/bash");
            assert_eq!(w.get(1), b"-c");
            assert_eq!(w.get(2), b"echo BASH-OK");
        }
    }

    #[test]
    fn tokenize_strips_quotes_around_and_inside_a_word() {
        // Quotes may open mid-word, and the word continues after them.
        let w = Words::new(b"a'b c'd");
        assert_eq!(w.count, 1);
        assert_eq!(w.get(0), b"ab cd");
        // An empty quoted word is still a word.
        let w = Words::new(b"echo ''");
        assert_eq!(w.count, 2);
        assert_eq!(w.get(0), b"echo");
        assert_eq!(w.get(1), b"");
    }

    #[test]
    fn tokenize_escapes_outside_quotes() {
        // A backslash protects the byte that would otherwise separate words.
        let w = Words::new(b"a\\ b");
        assert_eq!(w.count, 1);
        assert_eq!(w.get(0), b"a b");
        // Inside double quotes it protects a double quote or a backslash;
        // before anything else the backslash is literal and both bytes stay.
        let w = Words::new(b"\"a\\\"b\"");
        assert_eq!(w.count, 1);
        assert_eq!(w.get(0), b"a\"b");
        let w = Words::new(b"\"a\\nb\"");
        assert_eq!(w.count, 1);
        assert_eq!(w.get(0), b"a\\nb");
    }

    #[test]
    fn tokenize_reports_an_unmatched_quote() {
        // `bash -c 'echo hi` must be refused rather than run with the quote
        // carried through as text.
        let line = b"/bin/bash -c 'echo hi";
        let mut buf = [0u8; 32];
        buf[..line.len()].copy_from_slice(line);
        let mut spans = [(0usize, 0usize); 32];
        assert_eq!(
            tokenize(&mut buf, line.len(), &mut spans),
            Err(TokenizeError::UnmatchedQuote(b'\''))
        );
    }

    /// An argument longer than 55 bytes used to be cut to a prefix while the shell packed it into
    /// the command path's buffer (measured in a guest: `/bin/echo` with a 61-byte argument printed
    /// 55). Every argument now has its own buffer and reaches `argv` whole.
    #[test]
    fn an_argument_longer_than_the_old_path_buffer_reaches_argv_whole() {
        let long = [b'd'; 61];
        let arg = core::str::from_utf8(&long).unwrap();
        let mut buf = [0u8; 256];
        let mut argv = [core::ptr::null(); 2];
        assert!(pack_args(&mut buf, &[arg], &mut argv));
        let got = unsafe { core::ffi::CStr::from_ptr(argv[1] as *const core::ffi::c_char) };
        assert_eq!(got.to_bytes(), &long);
    }

    #[test]
    fn packed_arguments_are_separate_nul_terminated_strings() {
        let mut buf = [0u8; 256];
        let mut argv = [core::ptr::null(); 4];
        let args = ["one", "two", "three"];
        assert!(pack_args(&mut buf, &args, &mut argv));
        for (i, want) in args.iter().enumerate() {
            let got = unsafe { core::ffi::CStr::from_ptr(argv[i + 1] as *const core::ffi::c_char) };
            assert_eq!(got.to_bytes(), want.as_bytes());
        }
        assert!(argv[0].is_null(), "argv[0] belongs to the caller");
    }

    /// The kernel's errors are negative and `i32::MIN` must not overflow, so the message carries
    /// the sign and the formatter has room for ten digits plus it.
    #[test]
    fn a_kernel_error_formats_with_its_sign() {
        let mut buf = [0u8; 12];
        assert_eq!(format_errno(&mut buf, 0), b"0");
        assert_eq!(format_errno(&mut buf, -2), b"-2");
        assert_eq!(format_errno(&mut buf, 12345), b"12345");
        assert_eq!(format_errno(&mut buf, i32::MIN), b"-2147483648");
    }

    /// Only a missing file makes another directory worth a try. Every other error is the
    /// command's own, and the fallback's `ENOENT` used to replace it in the message, which is why
    /// the intermittent `/bin/bash` failure was reported as a `/sbin/bash` that never existed.
    #[test]
    fn only_a_missing_file_tries_another_directory() {
        assert!(should_try_next_dir(ENOENT));
        assert!(!should_try_next_dir(-12), "ENOMEM from the argument frame is the command's");
        assert!(!should_try_next_dir(-13), "EACCES is the command's");
        assert!(!should_try_next_dir(-1), "the exec-never-returned sentinel is not an errno");
        assert!(!should_try_next_dir(0));
    }

    /// A bare name in neither `/bin` nor `/sbin` is retried as a multicall applet with its own
    /// name as `argv[0]`, which is how the multicall dispatches. An explicit path names a file,
    /// and the multicall itself is not one of its own applets.
    #[test]
    fn only_a_bare_missing_name_is_retried_as_a_multicall_applet() {
        assert!(should_try_multicall("wc", ENOENT));
        assert!(!should_try_multicall("wc", -13), "EACCES is the command's");
        assert!(
            !should_try_multicall("/bin/wc", ENOENT),
            "an explicit path names a file"
        );
        assert!(
            !should_try_multicall("coreutils", ENOENT),
            "the multicall is not one of its own applets"
        );
        assert!(!should_try_multicall("", ENOENT));
    }

    fn menu_of(names: &[&str]) -> Menu {
        let mut menu = Menu::new();
        for name in names {
            menu.offer(name.as_bytes());
        }
        menu.sort();
        menu
    }

    #[test]
    fn a_completion_word_ends_at_a_separator() {
        assert_eq!(word_at(b"ls fi", 5), (3, 5));
        assert_eq!(word_at(b"ls fi", 4), (3, 5));
        assert_eq!(word_at(b"ls ", 3), (3, 3));
        assert_eq!(word_at(b"echo a | wc", 11), (9, 11));
        assert_eq!(word_at(b"ls fi|wc", 6), (6, 8));
    }

    #[test]
    fn a_command_position_follows_a_pipeline_and_a_logical_and() {
        assert!(at_command_position(b"ls", 0));
        assert!(!at_command_position(b"echo fi", 5));
        assert!(at_command_position(b"echo a | wc", 9));
        assert!(at_command_position(b"echo a && wc", 10));
        assert!(!at_command_position(b"echo a > out", 9));
        assert!(at_command_position(b"   ls", 3));
    }

    #[test]
    fn a_menu_dedupes_and_sorts_names() {
        let menu = menu_of(&["wc", "cat", "wc", "printf"]);
        assert_eq!(menu.count, 3);
        assert_eq!(menu.name(0), &b"cat"[..]);
        assert_eq!(menu.name(1), &b"printf"[..]);
        assert_eq!(menu.name(2), &b"wc"[..]);
    }

    #[test]
    fn a_menu_renders_names_in_screen_rows() {
        let menu = menu_of(&["aaaa", "bbbb", "cccc"]);
        let mut out = [0u8; 64];
        let n = menu.render(&mut out);
        assert_eq!(&out[..n], &b"aaaa bbbb cccc\n"[..]);
    }

    #[test]
    fn a_menu_wraps_a_row_that_would_not_fit() {
        let a = [b'a'; 50];
        let b = [b'b'; 50];
        let mut menu = Menu::new();
        menu.offer(&a);
        menu.offer(&b);
        menu.sort();
        let mut out = [0u8; 128];
        let n = menu.render(&mut out);
        assert_eq!(n, 103);
        assert_eq!(out[50], b' ');
        assert_eq!(out[51], b'\n');
        assert_eq!(&out[52..102], &b);
        assert_eq!(out[102], b'\n');
    }

    #[test]
    fn a_menu_marks_a_list_it_could_not_hold() {
        let mut menu = menu_of(&["cat"]);
        menu.overflow = true;
        let mut out = [0u8; 32];
        let n = menu.render(&mut out);
        assert_eq!(&out[..n], &b"cat ...\n"[..]);
    }

    #[test]
    fn a_unique_completion_adds_the_rest_and_a_separator() {
        let mut ed = Editor::new();
        ed.set_line(b"ech");
        assert!(ed.apply_unique(3, 3, b"echo", Some(b' ')));
        assert_eq!(&ed.line[..ed.len], &b"echo "[..]);
        assert_eq!(ed.cur, 5);
    }

    /// A path completion is measured against the basename, so the directory the user typed
    /// stays as it was.
    #[test]
    fn a_path_completion_adds_only_the_basename_suffix() {
        let mut ed = Editor::new();
        ed.set_line(b"cat src/li");
        assert!(ed.apply_unique(10, 2, b"lib.rs", Some(b' ')));
        assert_eq!(&ed.line[..ed.len], &b"cat src/lib.rs "[..]);
    }

    #[test]
    fn replacing_a_span_keeps_the_text_after_it() {
        let mut ed = Editor::new();
        ed.set_line(b"ls fi | wc");
        assert!(ed.set_span(3, 5, b"file1"));
        assert_eq!(&ed.line[..ed.len], &b"ls file1 | wc"[..]);
        assert_eq!(ed.cur, 8);
        assert!(ed.set_span(3, 8, b"fi"));
        assert_eq!(&ed.line[..ed.len], &b"ls fi | wc"[..]);
        assert_eq!(ed.cur, 5);
    }

    #[test]
    fn a_completion_in_the_middle_of_a_line_keeps_the_text_after_it() {
        let mut ed = Editor::new();
        ed.set_line(b"ls fi | wc");
        ed.cur = 5;
        assert!(ed.apply_unique(5, 2, b"file1", Some(b' ')));
        assert_eq!(&ed.line[..ed.len], &b"ls file1 | wc"[..]);
        assert_eq!(ed.cur, 8);
    }
}
