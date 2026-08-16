#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

use core::ptr;

use minix_std::wserver::{
    ws_cursor, ws_flush, ws_input, ws_key_char, ws_key_usage, ws_reply_status, ws_reply_wid,
    ws_text,
};

const COLS: usize = 80;
const ROWS: usize = 24;
const WIN_X: i32 = 120;
const WIN_Y: i32 = 60;
const WIN_W: i32 = 640;
const WIN_H: i32 = 400; // 24 * 16 body + 16 title

/// VT parser state.
enum PState {
    Normal,
    Esc,
    Csi,
}

/// Terminal grid + incremental VT parser (Phase M3 subset).
///
/// The wserver protocol carries plain chars (no per-cell color), so SGR
/// sequences are parsed and dropped — the terminal renders monochrome.
struct Term {
    grid: [[u8; COLS]; ROWS],
    /// What the window server's text buffer holds (diff base).
    sent: [[u8; COLS]; ROWS],
    /// What the window server's cursor shows (diff base for WS_CURSOR).
    sent_cursor: (usize, usize),
    row: usize,
    col: usize,
    saved: Option<(usize, usize)>,
    state: PState,
    params: [u32; 8],
    nparams: usize,
    param_val: u32,
}

impl Term {
    const fn new() -> Self {
        Self {
            grid: [[b' '; COLS]; ROWS],
            sent: [[b' '; COLS]; ROWS],
            sent_cursor: (0, 0),
            row: 0,
            col: 0,
            saved: None,
            state: PState::Normal,
            params: [0; 8],
            nparams: 0,
            param_val: 0,
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        self.row += 1;
        if self.row >= ROWS {
            self.row = ROWS - 1;
            for r in 1..ROWS {
                self.grid[r - 1] = self.grid[r];
            }
            self.grid[ROWS - 1] = [b' '; COLS];
        }
    }

    fn put(&mut self, ch: u8) {
        self.grid[self.row][self.col] = ch;
        self.col += 1;
        if self.col >= COLS {
            self.newline();
        }
    }

    fn push_param(&mut self) {
        if self.nparams < self.params.len() {
            self.params[self.nparams] = self.param_val;
            self.nparams += 1;
        }
        self.param_val = 0;
    }

    fn param(&self, i: usize) -> u32 {
        self.params.get(i).copied().unwrap_or(0)
    }

    fn csi_final(&mut self, ch: u8) {
        let n = {
            let p = self.param(0);
            if p == 0 { 1 } else { p }
        } as usize;
        match ch {
            b'H' | b'f' => {
                let r = {
                    let p = self.param(0);
                    if p == 0 { 1 } else { p }
                } as usize;
                let c = {
                    let p = self.param(1);
                    if p == 0 { 1 } else { p }
                } as usize;
                self.row = (r - 1).min(ROWS - 1);
                self.col = (c - 1).min(COLS - 1);
            }
            b'A' => self.row = self.row.saturating_sub(n),
            b'B' => self.row = (self.row + n).min(ROWS - 1),
            b'C' => self.col = (self.col + n).min(COLS - 1),
            b'D' => self.col = self.col.saturating_sub(n),
            b'G' => {
                let c = {
                    let p = self.param(0);
                    if p == 0 { 1 } else { p }
                } as usize;
                self.col = (c - 1).min(COLS - 1);
            }
            b'J' => match self.param(0) {
                0 => {
                    for r in self.row..ROWS {
                        let from = if r == self.row { self.col } else { 0 };
                        for c in from..COLS {
                            self.grid[r][c] = b' ';
                        }
                    }
                }
                1 => {
                    for r in 0..=self.row {
                        let to = if r == self.row { self.col } else { COLS - 1 };
                        for c in 0..=to {
                            self.grid[r][c] = b' ';
                        }
                    }
                }
                2 => {
                    self.grid = [[b' '; COLS]; ROWS];
                    self.row = 0;
                    self.col = 0;
                }
                _ => {}
            },
            b'K' => match self.param(0) {
                0 => {
                    for c in self.col..COLS {
                        self.grid[self.row][c] = b' ';
                    }
                }
                1 => {
                    for c in 0..=self.col {
                        self.grid[self.row][c] = b' ';
                    }
                }
                2 => self.grid[self.row] = [b' '; COLS],
                _ => {}
            },
            // SGR: parsed but not rendered (monochrome window server).
            b'm' => {}
            b's' => self.saved = Some((self.row, self.col)),
            b'u' => {
                if let Some((r, c)) = self.saved {
                    self.row = r;
                    self.col = c;
                }
            }
            _ => {}
        }
    }

    /// Feed one byte through the VT parser.
    fn feed(&mut self, ch: u8) {
        match self.state {
            PState::Normal => match ch {
                b'\x1b' => self.state = PState::Esc,
                b'\r' => self.col = 0,
                b'\n' => self.newline(),
                b'\t' => {
                    self.col = (self.col + 8) & !7;
                    if self.col >= COLS {
                        self.col = COLS - 1;
                    }
                }
                0x08 => self.col = self.col.saturating_sub(1),
                0x07 => {}
                c if c >= 0x20 => self.put(c),
                _ => {}
            },
            PState::Esc => {
                self.state = PState::Normal;
                match ch {
                    b'[' => {
                        self.state = PState::Csi;
                        self.params = [0; 8];
                        self.nparams = 0;
                        self.param_val = 0;
                    }
                    b'7' => self.saved = Some((self.row, self.col)),
                    b'8' => {
                        if let Some((r, c)) = self.saved {
                            self.row = r;
                            self.col = c;
                        }
                    }
                    _ => {}
                }
            }
            PState::Csi => match ch {
                b'?' => {}
                b'0'..=b'9' => {
                    self.param_val = self
                        .param_val
                        .saturating_mul(10)
                        .saturating_add((ch - b'0') as u32);
                }
                b';' => self.push_param(),
                _ => {
                    self.push_param();
                    self.csi_final(ch);
                    self.state = PState::Normal;
                }
            },
        }
    }
}

/// Send one message to the window server; returns the reply status.
fn ws_call(msg: &mut [u8; 64]) -> i32 {
    if unsafe { minix_std::sendrec(minix_std::WS_PROC_NR, msg) }.is_err() {
        return -71;
    }
    ws_reply_status(msg)
}

/// Push the grid diff to the window server (WS_TEXT per changed cell), the
/// block cursor position (WS_CURSOR), and repaint once with WS_FLUSH.
/// `sent`/`sent_cursor` track what the window server holds, so a cell that
/// goes char → space (a backspace erase) is re-sent rather than skipped
/// because its grid value matches the initial all-space grid.
fn flush(t: &mut Term, wid: i32) {
    let mut dirty = false;
    for r in 0..ROWS {
        for c in 0..COLS {
            if t.grid[r][c] != t.sent[r][c] {
                let mut msg = ws_text(wid, r as i32, c as i32, t.grid[r][c]);
                if ws_call(&mut msg) == 0 {
                    t.sent[r][c] = t.grid[r][c];
                }
                dirty = true;
            }
        }
    }
    let cur = (t.row, t.col);
    if dirty || cur != t.sent_cursor {
        let mut msg = ws_cursor(wid, cur.0 as i32, cur.1 as i32);
        if ws_call(&mut msg) == 0 {
            t.sent_cursor = cur;
        }
        dirty = true;
    }
    if dirty {
        let mut msg = ws_flush();
        ws_call(&mut msg);
    }
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // 1. Create the window.
    let title = b"wterm";
    let mut msg = minix_std::wserver::ws_create(
        WIN_X,
        WIN_Y,
        WIN_W,
        WIN_H,
        title.as_ptr() as u64,
        title.len() as i32,
    );
    if ws_call(&mut msg) != 0 {
        userland::write_err(b"wterm: create window failed\n");
        return 1;
    }
    let wid = ws_reply_wid(&msg);

    // 2. Open the pty master (before forking so the child's slave open has
    //    a live pair).
    let master = match unsafe { minix_std::fs::open(b"/dev/ptyp0", minix_std::fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            userland::write_err(b"wterm: open /dev/ptyp0 failed\n");
            return 1;
        }
    };
    // Report the terminal size (24 rows x 80 cols) to the slave so
    // TIOCGWINSZ / `stty size` see the real grid. The master's winsize
    // ioctl forwards to the slave line.
    let mut size = minix_std::termios::WinSize {
        ws_row: ROWS as u16,
        ws_col: COLS as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let _ = unsafe {
        minix_std::fs::ioctl(
            master,
            minix_std::termios::TIOCSWINSZ,
            &mut size as *mut _ as *mut u8,
        )
    };

    // 3. Key pipe + helper: WS_INPUT blocks, so a helper process forwards
    //    routed keys through a pipe the main loop polls alongside the pty.
    let (key_r, key_w) = match minix_std::fs::pipe() {
        Ok(p) => p,
        Err(_) => {
            userland::write_err(b"wterm: pipe failed\n");
            return 1;
        }
    };
    let helper_pid = match unsafe { minix_std::process::fork() } {
        Ok(0) => {
            // Helper: block on the window's key queue, forward to the pipe.
            let _ = minix_std::fs::close(key_r);
            let _ = minix_std::fs::close(master);
            loop {
                let mut msg = ws_input(wid);
                if unsafe { minix_std::sendrec(minix_std::WS_PROC_NR, &mut msg) }.is_err() {
                    minix_rt::exit(0);
                }
                let ch = ws_key_char(&msg);
                let usage = ws_key_usage(&msg);
                // Arrows carry no ASCII char; encode them as the terminal
                // escape sequences the shell's line editor expects.
                let seq: &[u8] = match usage {
                    0x52 => b"\x1b[A", // Up
                    0x51 => b"\x1b[B", // Down
                    0x50 => b"\x1b[D", // Left
                    0x4F => b"\x1b[C", // Right
                    _ => &[ch],
                };
                for &b in seq {
                    loop {
                        match unsafe { minix_std::fs::write(key_w, &[b]) } {
                            Ok(_) => break,
                            Err(e) if e.0 == -minix_std::EAGAIN => {}
                            Err(_) => minix_rt::exit(0),
                        }
                    }
                }
            }
        }
        Ok(pid) => pid,
        Err(_) => {
            userland::write_err(b"wterm: helper fork failed\n");
            return 1;
        }
    };
    let _ = minix_std::fs::close(key_w);

    // 4. Shell child: the pty slave as stdio (the init pattern), then exec.
    let _shell_pid = match unsafe { minix_std::process::fork() } {
        Ok(0) => {
            let _ = minix_std::fs::close(master);
            let _ = minix_std::fs::close(key_r);
            let fd = minix_rt::open(b"/dev/ttyp0", 0o2) as i32; // O_RDWR
            if fd >= 0
                && minix_std::fs::dup2(fd, 0).is_ok()
                && minix_std::fs::dup2(fd, 1).is_ok()
                && minix_std::fs::dup2(fd, 2).is_ok()
            {
                unsafe {
                    minix_rt::set_fd_vfs(0, 1);
                    minix_rt::set_fd_vfs(1, 1);
                    minix_rt::set_fd_vfs(2, 1);
                }
                // The fork's stage1 compiler has no C-string literals, so
                // build argv[0] from a NUL-terminated byte string (clippy's
                // manual_c_str_literals fires on the host toolchain).
                #[allow(clippy::manual_c_str_literals)]
                let argv = [b"/bin/sh\0".as_ptr(), ptr::null()];
                let _ = minix_std::process::exec(b"/bin/sh", &argv);
            }
            minix_rt::exit(1);
        }
        Ok(pid) => pid,
        Err(_) => {
            userland::write_err(b"wterm: shell fork failed\n");
            return 1;
        }
    };

    // 5. Main loop: poll the pty master (output) and the key pipe (input).
    let mut term = Term::new();
    let mut rbuf = [0u8; 64];
    let mut kbuf = [0u8; 8];
    let mut gone = false;
    while !gone {
        // Drain shell output.
        loop {
            match unsafe { minix_std::fs::read(master, &mut rbuf) } {
                Ok(0) => {
                    // EOF: the slave closed — the shell is gone.
                    gone = true;
                    break;
                }
                Ok(n) => {
                    for &b in &rbuf[..n as usize] {
                        term.feed(b);
                    }
                    flush(&mut term, wid);
                }
                Err(e) if e.0 == -minix_std::EAGAIN => break,
                Err(_) => {
                    gone = true;
                    break;
                }
            }
        }
        if gone {
            break;
        }
        // Drain keys into the pty.
        loop {
            match unsafe { minix_std::fs::read(key_r, &mut kbuf) } {
                Ok(0) => {
                    gone = true;
                    break;
                }
                Ok(n) => {
                    for &ch in &kbuf[..n as usize] {
                        loop {
                            match unsafe { minix_std::fs::write(master, &[ch]) } {
                                Ok(_) => break,
                                Err(e) if e.0 == -minix_std::EAGAIN => {}
                                Err(_) => {
                                    gone = true;
                                    break;
                                }
                            }
                        }
                        if gone {
                            break;
                        }
                    }
                }
                Err(e) if e.0 == -minix_std::EAGAIN => break,
                Err(_) => {
                    gone = true;
                    break;
                }
            }
        }
    }

    // Cleanup: close the window, kill the key helper, exit.
    let mut msg = minix_std::wserver::ws_close(wid);
    let _ = ws_call(&mut msg);
    let _ = minix_std::time::kill(helper_pid, minix_std::time::SIGTERM);
    let _ = minix_std::fs::close(master);
    let _ = minix_std::fs::close(key_r);
    0
}
