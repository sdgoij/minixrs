//! The console's screen — the cells behind `/dev/console`.
//!
//! MINIX keeps these in the console driver, in a file of its own beside `tty.c`, and not in a
//! server: the console's `write` hook drops the bytes into a VGA text plane (or a framebuffer the
//! driver mapped), and the characters keep their positions because the *hardware* does. This port
//! has no such plane — its console is a byte stream to the host (the tty's own `write(1)`, the
//! kernel's serial path) — so until M5b nothing in the guest knew where a byte had landed, and the
//! front ends had to infer it (the page's `terminal.js`: one line and a cursor, CR/BS only).
//!
//! What this module adds is the model the reference's `console.c` has: a cell grid, a cursor, and
//! the handful of control characters and escape sequences the console acts on. `wserver` composes
//! it into the desktop and the host's display shows that, so the console's cells reach the page as
//! *pixels the guest composed* rather than as bytes the page renders.
//!
//! The console is a window client like any other (`minix_std::wserver`'s WS_TEXT/WS_CURSOR/
//! WS_FLUSH), which is what keeps the display's owner single: the compositor presents, the console
//! only says what its cells are. The diff is against what the window server was last told, so a
//! screenful of output that changes three cells costs three messages.
//!
//! Three things this deliberately is not:
//!   * a terminal emulator — that is `wterm`, whose client parses a wider subset (SGR among it);
//!     the console *drops* SGR the way the reference's console does, since nothing prints colour
//!     to a VGA text plane through it;
//!   * a scrollback buffer — the model is one screen, and output past the last row scrolls it;
//!   * per-line state — the active console line feeds the one screen. The reference keeps a
//!     `v_console_lines` array (one per virtual console); this port has no console switching, so
//!     the follow-up, not the model, is what is missing.

use minix_std::wserver::{ws_create, ws_cursor, ws_flush, ws_reply_status, ws_reply_wid, ws_text};

/// Console geometry in cells. 80 columns is not the desktop's width but the shell's assumption:
/// its line editor blanks a row with `SCR_COLS - 1` spaces (`userland::shell`), so a narrower model
/// would wrap where the editor believes it will not.
pub const COLS: usize = 80;
pub const ROWS: usize = 24;

/// The console's window on the desktop: centred, and exactly its cells plus a title bar.
const WIN_X: i32 = 192;
const WIN_Y: i32 = 184;
const WIN_W: i32 = COLS as i32 * 8;
const WIN_H: i32 = ROWS as i32 * 16 + 16;
const TITLE: &[u8] = b"console";

/// The parser's state: printable bytes pass through, `ESC` and `ESC [` do not.
const ST_NORMAL: u8 = 0;
const ST_ESC: u8 = 1;
const ST_CSI: u8 = 2;

/// One screen of console cells and the cursor in it.
///
/// `sent` is the diff base: what the window server currently holds. It starts as spaces to match a
/// freshly created window, so the first `sync` after `attach` sends every non-blank cell and no
/// message is spent on the blank ones.
pub struct Screen {
    grid: [[u8; COLS]; ROWS],
    sent: [[u8; COLS]; ROWS],
    row: usize,
    col: usize,
    sent_cursor: (usize, usize),
    state: u8,
    /// CSI parameters, in order; only `H`/`f` reads the second.
    params: [u32; 2],
    nparams: usize,
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen {
    pub const fn new() -> Self {
        Self {
            grid: [[b' '; COLS]; ROWS],
            sent: [[b' '; COLS]; ROWS],
            row: 0,
            col: 0,
            sent_cursor: (0, 0),
            state: ST_NORMAL,
            params: [0; 2],
            nparams: 0,
        }
    }

    /// The cell at a body position, or a space outside the screen. The console's reader is the
    /// display's check (`tools/wasm-servers/boot.cjs` reads this grid through the report below).
    pub fn cell(&self, row: usize, col: usize) -> u8 {
        self.grid
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or(b' ')
    }

    /// Where the cursor is (row, col). The console's cursor is where the *next* byte lands, which
    /// is the shell editor's cursor: it moves with CR and BS.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// Feed one console byte.
    pub fn feed(&mut self, ch: u8) {
        match self.state {
            ST_NORMAL => self.feed_normal(ch),
            ST_ESC => {
                self.state = ST_NORMAL;
                if ch == b'[' {
                    self.state = ST_CSI;
                    self.params = [0; 2];
                    self.nparams = 0;
                }
                // Anything else is a two-byte sequence this console does not act on
                // (`ESC 7`/`ESC 8` among them); the escape is consumed, the byte is not printed.
            }
            ST_CSI => match ch {
                b'0'..=b'9' => {
                    // Past the second parameter the rest are ignored, which is all a console
                    // needs and all it has room to remember.
                    if let Some(p) = self.params.get_mut(self.nparams) {
                        *p = p.saturating_mul(10).saturating_add((ch - b'0') as u32);
                    }
                }
                b';' => self.nparams = (self.nparams + 1).min(self.params.len()),
                _ => {
                    self.state = ST_NORMAL;
                    let params = self.params;
                    self.csi_final(ch, params);
                }
            },
            _ => {}
        }
    }

    /// A byte outside an escape sequence: the console's own vocabulary — CR, LF, BS, TAB, and
    /// whatever prints.
    fn feed_normal(&mut self, ch: u8) {
        match ch {
            b'\x1b' => self.state = ST_ESC,
            b'\r' => self.col = 0,
            b'\n' => self.newline(),
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => {
                // Tab stops every 8 columns, as the console's hardware had them.
                self.col = ((self.col + 8) & !7).min(COLS - 1);
            }
            0x07 => {} // BEL: the console has no bell
            c if c >= 0x20 => self.put(c),
            // The remaining C0 controls (NUL, vertical tab, form feed, and the escapes that
            // arrived without an `ESC`) draw nothing on a text plane.
            _ => {}
        }
    }

    /// Act on a CSI sequence's final byte. Everything not named here is dropped, which is what a
    /// console does with a sequence it has no attribute for.
    fn csi_final(&mut self, ch: u8, params: [u32; 2]) {
        // A missing or zero parameter means 1, the convention the sequences share.
        let one = |p: u32| if p == 0 { 1 } else { p as usize };
        match ch {
            b'H' | b'f' => {
                self.row = one(params[0]).saturating_sub(1).min(ROWS - 1);
                self.col = one(params[1]).saturating_sub(1).min(COLS - 1);
            }
            b'A' => self.row = self.row.saturating_sub(one(params[0])).min(ROWS - 1),
            b'B' => self.row = (self.row + one(params[0])).min(ROWS - 1),
            b'C' => self.col = (self.col + one(params[0])).min(COLS - 1),
            b'D' => self.col = self.col.saturating_sub(one(params[0])),
            b'J' => match params[0] {
                // Clear from the cursor to the end of the screen — what `clear` leaves, since it
                // sends `ESC[2J` followed by `ESC[H` and the second sequence re-homes the cursor.
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
                _ => {
                    self.grid = [[b' '; COLS]; ROWS];
                    self.row = 0;
                    self.col = 0;
                }
            },
            b'K' => match params[0] {
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
                _ => self.grid[self.row] = [b' '; COLS],
            },
            _ => {}
        }
    }

    /// Print one character, wrapping at the last column.
    fn put(&mut self, ch: u8) {
        self.grid[self.row][self.col] = ch;
        self.col += 1;
        if self.col >= COLS {
            self.newline();
        }
    }

    /// Carriage return plus line feed's effect: the next row, wrapping by scrolling.
    fn newline(&mut self) {
        self.col = 0;
        self.row += 1;
        if self.row >= ROWS {
            // Scroll: the top row is gone, so the screen moves up one and the last row is blank.
            self.grid.copy_within(1..ROWS, 0);
            self.grid[ROWS - 1] = [b' '; COLS];
            self.row = ROWS - 1;
        }
    }

    /// The next cell that differs from what the window server holds, if any.
    ///
    /// One at a time rather than as a batch, because the caller has to *send* it: a message that
    /// did not arrive must not be marked as sent, or the window server keeps a stale cell forever.
    pub fn take_damage(&self) -> Option<(usize, usize, u8)> {
        for r in 0..ROWS {
            for c in 0..COLS {
                let ch = self.grid[r][c];
                if ch != self.sent[r][c] {
                    return Some((r, c, ch));
                }
            }
        }
        None
    }

    /// Record that the window server now holds the cell `take_damage` returned.
    pub fn confirm(&mut self, row: usize, col: usize) {
        if let (Some(g), Some(s)) = (self.grid.get(row), self.sent.get_mut(row))
            && let Some(ch) = g.get(col)
            && let Some(dst) = s.get_mut(col)
        {
            *dst = *ch;
        }
    }

    /// Whether the window server's cursor is somewhere other than this screen's.
    pub fn cursor_stale(&self) -> bool {
        self.cursor() != self.sent_cursor
    }

    /// Record that the window server's cursor is now at `cursor`.
    pub fn confirm_cursor(&mut self) {
        self.sent_cursor = self.cursor();
    }

    /// The raw cells, for a report a front end reads (see `report_ptr`).
    pub fn grid(&self) -> &[[u8; COLS]; ROWS] {
        &self.grid
    }
}

/// The console: its screen, and the window that screen is shown in.
pub struct Console {
    screen: Screen,
    /// The window server's id for the console's window, or `-1` when there is none (no window
    /// server, a refused create, or the tests). The screen runs either way — the model is the
    /// console's own — but nothing is shown.
    wid: i32,
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}

/// Send one request to the window server; returns the reply status.
fn ws_call(msg: &mut [u8; 64]) -> i32 {
    if unsafe { minix_std::sendrec(minix_std::WS_PROC_NR, msg) }.is_err() {
        return -71; // EPROTO
    }
    ws_reply_status(msg)
}

impl Console {
    pub const fn new() -> Self {
        Self {
            screen: Screen::new(),
            wid: -1,
        }
    }

    /// Create the console's window. Returns the window id, or a negative error — the caller
    /// reports it rather than the console going silently window-less.
    pub fn attach(&mut self) -> i32 {
        let mut msg = ws_create(
            WIN_X,
            WIN_Y,
            WIN_W,
            WIN_H,
            TITLE.as_ptr() as u64,
            TITLE.len() as i32,
        );
        if ws_call(&mut msg) != 0 {
            return -1;
        }
        self.wid = ws_reply_wid(&msg);
        self.wid
    }

    /// Feed console output and show what changed.
    pub fn output(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.screen.feed(b);
        }
        self.sync();
    }

    /// Push the screen's damage to the window server, then ask it to repaint if anything moved.
    fn sync(&mut self) {
        if self.wid < 0 {
            return;
        }
        let mut dirty = false;
        // Bounded by the screen's size: each round trip either confirms a cell (so the next
        // `take_damage` moves past it) or gives up.
        for _ in 0..ROWS * COLS {
            let Some((row, col, ch)) = self.screen.take_damage() else {
                break;
            };
            let mut msg = ws_text(self.wid, row as i32, col as i32, ch);
            if ws_call(&mut msg) != 0 {
                break;
            }
            self.screen.confirm(row, col);
            dirty = true;
        }
        if self.screen.cursor_stale() {
            let (row, col) = self.screen.cursor();
            let mut msg = ws_cursor(self.wid, row as i32, col as i32);
            if ws_call(&mut msg) == 0 {
                self.screen.confirm_cursor();
                dirty = true;
            }
        }
        if dirty {
            // The repaint is what reaches the display: the window server paints its surface and
            // presents it, so a batch of cells costs one frame rather than one per cell.
            let mut msg = ws_flush();
            let _ = ws_call(&mut msg);
        }
    }
}

/// The console's screen and window. One console, as the port has one active console line.
static mut CONSOLE: Console = Console::new();

/// Create the console's window; returns the window id or a negative error.
pub fn attach() -> i32 {
    // SAFETY: the console's state is this server's own, and the server is single-threaded.
    unsafe { (*core::ptr::addr_of_mut!(CONSOLE)).attach() }
}

/// Feed console output (what the tty's `console_write` sent to the kernel) and show it.
pub fn output(bytes: &[u8]) {
    // SAFETY: as `attach`.
    unsafe { (*core::ptr::addr_of_mut!(CONSOLE)).output(bytes) };
}

/// Address of the screen's cell grid, for a front end that reads what the console holds.
///
/// A report, in the sense the DS and INIT reports are: the harness reads the cells the *guest*
/// composed rather than inferring them from the bytes it saw on the console, which is the claim
/// M5b makes.
pub fn report_ptr() -> u32 {
    // SAFETY: as `attach`. The pointer is to a static the caller is reading, not writing.
    unsafe { (*core::ptr::addr_of!(CONSOLE)).screen.grid().as_ptr() as u32 }
}

/// The cursor cell, as (row, col), for the same report.
pub fn report_cursor() -> u32 {
    // SAFETY: as `report_ptr`.
    let (row, col) = unsafe { (*core::ptr::addr_of!(CONSOLE)).screen.cursor() };
    ((row as u32) << 16) | col as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row's bytes.
    fn row(s: &Screen, r: usize) -> [u8; COLS] {
        core::array::from_fn(|c| s.cell(r, c))
    }

    /// The row's length without its trailing blanks.
    fn row_len(s: &Screen, r: usize) -> usize {
        let mut len = 0;
        for c in 0..COLS {
            if s.cell(r, c) != b' ' {
                len = c + 1;
            }
        }
        len
    }

    /// Assert a row's text, trailing blanks trimmed.
    fn assert_row(s: &Screen, r: usize, want: &[u8]) {
        let got = row(s, r);
        assert_eq!(&got[..row_len(s, r)], want, "row {r}");
    }

    #[test]
    fn test_print_advances_the_cursor() {
        let mut s = Screen::new();
        for &b in b"# ls" {
            s.feed(b);
        }
        assert_row(&s, 0, b"# ls");
        assert_eq!(s.cursor(), (0, 4));
    }

    #[test]
    fn test_cr_returns_and_bs_steps_back() {
        let mut s = Screen::new();
        for &b in b"abc\rXY" {
            s.feed(b);
        }
        // `\r` put the cursor back at the first column, so the overwrite lands on `a`,`b`.
        assert_row(&s, 0, b"XYc");
        let mut s = Screen::new();
        for &b in b"abc\x08\x08Z" {
            s.feed(b);
        }
        assert_row(&s, 0, b"aZc");
    }

    #[test]
    fn test_lf_starts_a_new_line() {
        let mut s = Screen::new();
        for &b in b"ab\ncd" {
            s.feed(b);
        }
        // LF is a new line at column 0, not a bare line feed: the kernel's own serial path maps
        // it to CR+LF on the way out, so the screen agrees with the host's terminal pane.
        assert_row(&s, 0, b"ab");
        assert_row(&s, 1, b"cd");
        assert_eq!(s.cursor(), (1, 2));
    }

    #[test]
    fn test_tab_expands_to_the_next_stop() {
        let mut s = Screen::new();
        for &b in b"a\tb" {
            s.feed(b);
        }
        assert_row(&s, 0, b"a       b");
    }

    #[test]
    fn test_wrap_at_the_last_column() {
        let mut s = Screen::new();
        for _ in 0..COLS {
            s.feed(b'x');
        }
        // The 80th cell filled the row; the cursor wrapped to the next one.
        assert_eq!(s.cursor(), (1, 0));
        assert_eq!(row_len(&s, 0), COLS);
        assert_eq!(row_len(&s, 1), 0);
    }

    #[test]
    fn test_scroll_when_the_last_row_fills() {
        let mut s = Screen::new();
        // One character per row, then a newline past the last row: the top row must be gone.
        for i in 0..ROWS {
            s.feed(b'a' + (i % 26) as u8);
            s.feed(b'\n');
        }
        assert_row(&s, 0, b"b");
        assert_row(&s, ROWS - 2, b"x");
        assert_eq!(row_len(&s, ROWS - 1), 0, "the last row is the blank one");
        assert_eq!(s.cursor(), (ROWS - 1, 0));
    }

    #[test]
    fn test_clear_sequence_blanks_the_screen() {
        let mut s = Screen::new();
        for &b in b"left over" {
            s.feed(b);
        }
        // What the shell's `clear` builtin sends: `ESC[2J` then `ESC[H`.
        for &b in b"\x1b[2J\x1b[H" {
            s.feed(b);
        }
        assert_eq!(row_len(&s, 0), 0);
        assert_eq!(s.cursor(), (0, 0));
    }

    #[test]
    fn test_erase_to_end_of_line() {
        let mut s = Screen::new();
        for &b in b"keep this\rKEEP\x1b[K" {
            s.feed(b);
        }
        assert_row(&s, 0, b"KEEP");
    }

    #[test]
    fn test_cursor_position_sequence() {
        let mut s = Screen::new();
        for &b in b"\x1b[3;5Hx" {
            s.feed(b);
        }
        // 1-based, and the character lands at the position it names.
        assert_eq!(s.cursor(), (2, 5));
        assert_eq!(s.cell(2, 4), b'x');
    }

    #[test]
    fn test_unknown_escape_is_consumed_not_printed() {
        let mut s = Screen::new();
        // SGR: the console drops it rather than printing `[1m`.
        for &b in b"a\x1b[1mb" {
            s.feed(b);
        }
        assert_row(&s, 0, b"ab");
    }

    #[test]
    fn test_damage_is_every_non_blank_cell_first_then_only_changes() {
        let mut s = Screen::new();
        for &b in b"ab" {
            s.feed(b);
        }
        let mut first = 0;
        while let Some((r, c, ch)) = s.take_damage() {
            assert_eq!(r, 0);
            assert_eq!(c, first);
            assert_eq!(ch, if first == 0 { b'a' } else { b'b' });
            s.confirm(r, c);
            first += 1;
        }
        assert_eq!(first, 2);
        // Nothing changed, so there is nothing to send — which is what keeps a repeated write from
        // costing a frame.
        assert!(s.take_damage().is_none());
        // A cell that goes back to a space is damage too, even though a fresh window shows a
        // space there.
        for &b in b"\r " {
            s.feed(b);
        }
        assert_eq!(s.take_damage(), Some((0, 0, b' ')));
    }

    #[test]
    fn test_cursor_damage_tracks_the_sequence_the_editor_uses() {
        let mut s = Screen::new();
        // The shell's line editor redraw: CR, blank, CR, prompt, then BS back to the cursor.
        for &b in b"\r# one\x08\x08" {
            s.feed(b);
        }
        assert!(s.cursor_stale());
        s.confirm_cursor();
        assert!(!s.cursor_stale());
        for &b in b"\x08" {
            s.feed(b);
        }
        assert!(s.cursor_stale());
    }
}
