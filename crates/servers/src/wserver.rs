//! Window server (K5) — an in-house compositor on the framebuffer.
//!
//! Boot proc 18. Opens `/dev/fb` and mmaps it (K3), registers with the
//! input server as the keyboard consumer (so its receive loop wakes on key
//! events), and serves the `minix_std::wserver` protocol: create/text/fill/
//! close windows, and route key presses to the focused window's client.
//! Draws a desktop background + window chrome (title bars) with the shared
//! 8×16 font; redraws the whole desktop on every change (K5 scale; damage
//! tracking is perf follow-up).

#[cfg(target_os = "minix")]
use arch_common::ipc::Message;
#[cfg(target_os = "minix")]
use minix_std::font::FONT_8X16;
#[cfg(target_os = "minix")]
use minix_std::wserver::{WS_CLOSE, WS_CREATE, WS_FILL, WS_INPUT, WS_KEY, WS_TEXT};

const TITLE_H: usize = 16;
const MAX_WINDOWS: usize = 4;
const MAX_TEXT_COLS: usize = 40;
const MAX_TEXT_ROWS: usize = 24;
const MAX_RECTS: usize = 8;

// Drawing constants — the rasterizer only runs on the MINIX target (the
// host tests cover the window-state protocol ops; pixels are probe-verified).
#[cfg(target_os = "minix")]
const XRES: usize = 1024;
#[cfg(target_os = "minix")]
const YRES: usize = 768;
#[cfg(target_os = "minix")]
const PITCH: usize = XRES * 4;
#[cfg(target_os = "minix")]
const MAP_LEN: usize = 4 * 1024 * 1024;
#[cfg(target_os = "minix")]
const KEY_PAGE: u16 = 0x0007;

/// XRGB8888 colors (u32 0x00RRGGBB, LE bytes B,G,R,0).
#[cfg(target_os = "minix")]
const COLOR_DESKTOP: u32 = 0x00282828;
#[cfg(target_os = "minix")]
const COLOR_BODY: u32 = 0x00181820;
#[cfg(target_os = "minix")]
const COLOR_TITLE_FOCUSED: u32 = 0x004080C0;
#[cfg(target_os = "minix")]
const COLOR_TITLE_UNFOCUSED: u32 = 0x00202040;
#[cfg(target_os = "minix")]
const COLOR_TEXT: u32 = 0x00FFFFFF;

/// A filled pixel rect inside a window's body (window-local coords).
#[derive(Clone, Copy)]
pub struct WsRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub color: u32,
}

/// One window: position/size, title, body text cells, and filled rects.
#[derive(Clone, Copy)]
pub struct Window {
    pub used: bool,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub title: [u8; 24],
    pub text: [[u8; MAX_TEXT_COLS]; MAX_TEXT_ROWS],
    pub nrects: usize,
    pub rects: [WsRect; MAX_RECTS],
}

impl Window {
    const fn new() -> Self {
        Self {
            used: false,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            title: [0; 24],
            text: [[b' '; MAX_TEXT_COLS]; MAX_TEXT_ROWS],
            nrects: 0,
            rects: [WsRect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
                color: 0,
            }; MAX_RECTS],
        }
    }

    /// Number of text columns (w / 8, capped).
    fn cols(&self) -> usize {
        ((self.w as usize) / 8).min(MAX_TEXT_COLS)
    }

    /// Number of text rows (body h / 16, capped).
    fn rows(&self) -> usize {
        ((self.h as usize - TITLE_H) / 16).min(MAX_TEXT_ROWS)
    }
}

/// Window-server state (host-testable; the server's statics are one of
/// these).
pub struct WsState {
    pub windows: [Window; MAX_WINDOWS],
    /// Focused window index, or `MAX_WINDOWS` when none.
    pub focus: usize,
    /// Client blocked in WS_INPUT for a window: (endpoint, wid).
    pub waiter: (i32, usize),
}

impl Default for WsState {
    fn default() -> Self {
        Self::new()
    }
}

impl WsState {
    pub const fn new() -> Self {
        Self {
            windows: [Window::new(); MAX_WINDOWS],
            focus: MAX_WINDOWS,
            waiter: (-1, usize::MAX),
        }
    }

    /// Allocate a window; returns its wid or a negative error.
    pub fn create(&mut self, x: i32, y: i32, w: i32, h: i32) -> Result<usize, i32> {
        if w <= 0 || h <= 0 || (w as usize) / 8 > MAX_TEXT_COLS || (h as usize) / 16 > MAX_TEXT_ROWS
        {
            return Err(-22); // EINVAL
        }
        for (i, win) in self.windows.iter_mut().enumerate() {
            if !win.used {
                win.used = true;
                win.x = x;
                win.y = y;
                win.w = w;
                win.h = h;
                win.title = [0; 24];
                win.text = [[b' '; MAX_TEXT_COLS]; MAX_TEXT_ROWS];
                win.nrects = 0;
                self.focus = i;
                return Ok(i);
            }
        }
        Err(-28) // ENOSPC
    }

    /// Write one char into a window's body text buffer.
    pub fn text(&mut self, wid: usize, row: i32, col: i32, ch: u8) -> Result<(), i32> {
        let win = self.windows.get_mut(wid).ok_or(-9)?; // EBADF
        if !win.used {
            return Err(-9);
        }
        let (rows, cols) = (win.rows(), win.cols());
        if row < 0 || col < 0 || row as usize >= rows || col as usize >= cols {
            return Err(-34); // ERANGE
        }
        win.text[row as usize][col as usize] = ch;
        Ok(())
    }

    /// Add a filled rect to a window's body (clamped to the body).
    pub fn fill(
        &mut self,
        wid: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: u32,
    ) -> Result<(), i32> {
        let win = self.windows.get_mut(wid).ok_or(-9)?;
        if !win.used {
            return Err(-9);
        }
        if x1 <= x0 || y1 <= y0 || win.nrects >= MAX_RECTS {
            return Err(-22); // EINVAL
        }
        let (bw, bh) = (win.w, win.h - TITLE_H as i32);
        if x0 < 0 || y0 < 0 || x1 > bw || y1 > bh {
            return Err(-22);
        }
        win.rects[win.nrects] = WsRect {
            x0,
            y0,
            x1,
            y1,
            color,
        };
        win.nrects += 1;
        Ok(())
    }

    /// Close a window; focus falls back to the highest open window.
    pub fn close(&mut self, wid: usize) -> Result<(), i32> {
        let win = self.windows.get_mut(wid).ok_or(-9)?;
        if !win.used {
            return Err(-9);
        }
        win.used = false;
        if self.waiter.1 == wid {
            self.waiter = (-1, usize::MAX);
        }
        if self.focus == wid {
            self.focus = (0..MAX_WINDOWS)
                .rev()
                .find(|&i| self.windows[i].used)
                .unwrap_or(MAX_WINDOWS);
        }
        Ok(())
    }

    /// Route a key press to the focused window's waiter; returns the
    /// waiter's endpoint + char when one is blocked in WS_INPUT.
    pub fn route_key(&mut self, ch: u8) -> Option<(i32, u8)> {
        if self.focus < MAX_WINDOWS && self.waiter.0 >= 0 && self.waiter.1 == self.focus {
            let (ep, wid) = self.waiter;
            self.waiter = (-1, usize::MAX);
            let _ = wid;
            return Some((ep, ch));
        }
        None
    }
}

/// Server globals.
#[cfg(target_os = "minix")]
static mut WS_STATE: WsState = WsState::new();
#[cfg(target_os = "minix")]
static mut WS_FB: u64 = 0;
#[cfg(target_os = "minix")]
static mut WS_FB_FD: i32 = -1;
#[cfg(target_os = "minix")]
static mut WS_KBD_FD: i32 = -1;
/// Keyboard shift state across drains (a press and its letter can land in
/// different batches).
#[cfg(target_os = "minix")]
static mut WS_SHIFT: bool = false;

#[cfg(target_os = "minix")]
fn put_pixel(fb: u64, x: usize, y: usize, color: u32) {
    if x < XRES && y < YRES {
        let off = (y * PITCH + x * 4) as u64;
        unsafe {
            core::ptr::write_volatile((fb + off) as *mut u32, color);
        }
    }
}

#[cfg(target_os = "minix")]
fn fill_rect(fb: u64, x0: usize, y0: usize, x1: usize, y1: usize, color: u32) {
    for y in y0..y1 {
        for x in x0..x1 {
            put_pixel(fb, x, y, color);
        }
    }
}

#[cfg(target_os = "minix")]
fn draw_char(fb: u64, x: usize, y: usize, ch: u8, fg: u32, bg: u32) {
    if !(0x20..=0x7E).contains(&ch) {
        return;
    }
    let glyph = FONT_8X16[(ch - 0x20) as usize];
    for r in 0..16 {
        let bits = glyph[r];
        for c in 0..8 {
            let color = if bits & (0x80 >> c) != 0 { fg } else { bg };
            put_pixel(fb, x + c, y + r, color);
        }
    }
}

#[cfg(target_os = "minix")]
fn draw_text(fb: u64, x: usize, y: usize, s: &[u8], fg: u32, bg: u32) {
    for (i, &ch) in s.iter().enumerate() {
        draw_char(fb, x + i * 8, y, ch, fg, bg);
    }
}

/// Redraw the whole desktop: background, then windows in creation order.
#[cfg(target_os = "minix")]
fn redraw(fb: u64) {
    fill_rect(fb, 0, 0, XRES, YRES, COLOR_DESKTOP);
    let state = unsafe { &*core::ptr::addr_of!(WS_STATE) };
    for (i, win) in state.windows.iter().enumerate() {
        if !win.used {
            continue;
        }
        let (x, y) = (win.x as usize, win.y as usize);
        let (w, h) = (win.w as usize, win.h as usize);
        let body_y = y + TITLE_H;
        let body_h = h - TITLE_H;
        // Body background.
        fill_rect(fb, x, body_y, x + w, body_y + body_h, COLOR_BODY);
        // Filled rects (body-local).
        for r in win.rects[..win.nrects].iter() {
            fill_rect(
                fb,
                x + r.x0 as usize,
                body_y + r.y0 as usize,
                x + r.x1 as usize,
                body_y + r.y1 as usize,
                r.color,
            );
        }
        // Title bar chrome + title text.
        let title_color = if state.focus == i {
            COLOR_TITLE_FOCUSED
        } else {
            COLOR_TITLE_UNFOCUSED
        };
        fill_rect(fb, x, y, x + w, y + TITLE_H, title_color);
        let title_len = win
            .title
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(win.title.len());
        draw_text(
            fb,
            x + 4,
            y,
            &win.title[..title_len],
            COLOR_TEXT,
            title_color,
        );
        // Body text.
        let (cols, rows) = (win.cols(), win.rows());
        for r in 0..rows {
            for c in 0..cols {
                let ch = win.text[r][c];
                if ch != b' ' {
                    draw_char(fb, x + c * 8, body_y + r * 16, ch, COLOR_TEXT, COLOR_BODY);
                }
            }
        }
    }
    // virtio-gpu is explicit-flush: push the new frame to the display.
    // No-op for VGA-style backends.
    let fd = unsafe { WS_FB_FD };
    if fd >= 0 {
        let _ =
            unsafe { minix_std::fs::ioctl(fd, minix_std::fs::FBIOFLUSH, core::ptr::null_mut()) };
    }
}

/// HID keyboard usage → ASCII (unshifted).
#[cfg_attr(not(target_os = "minix"), allow(dead_code))]
fn usage_to_ascii(code: u16) -> Option<u8> {
    let c = match code {
        0x04..=0x1D => b'a' + (code - 0x04) as u8,
        0x1E..=0x26 => b'1' + (code - 0x1E) as u8,
        0x27 => b'0',
        0x28 => return Some(b'\n'),
        0x2A => return Some(0x08),
        0x2C => b' ',
        0x2D => b'-',
        0x2E => b'=',
        0x2F => b'[',
        0x30 => b']',
        0x31 => b'\\',
        0x33 => b';',
        0x34 => b'\'',
        0x35 => b'`',
        0x36 => b',',
        0x37 => b'.',
        0x38 => b'/',
        _ => return None,
    };
    Some(c)
}

#[cfg_attr(not(target_os = "minix"), allow(dead_code))]
fn shifted(c: u8) -> u8 {
    match c {
        b'1' => b'!',
        b'2' => b'@',
        b'3' => b'#',
        b'4' => b'$',
        b'5' => b'%',
        b'6' => b'^',
        b'7' => b'&',
        b'8' => b'*',
        b'9' => b'(',
        b'0' => b')',
        b'-' => b'_',
        b'=' => b'+',
        b'[' => b'{',
        b']' => b'}',
        b'\\' => b'|',
        b';' => b':',
        b'\'' => b'"',
        b'`' => b'~',
        b',' => b'<',
        b'.' => b'>',
        b'/' => b'?',
        c => c,
    }
}

/// Drain /dev/kbd and route key presses to the focused window's waiter.
/// Only drains when the focused window has a blocked WS_INPUT client —
/// otherwise the events stay queued for other consumers (e.g. keytest).
#[cfg(target_os = "minix")]
fn drain_kbd() {
    let state = unsafe { &mut *core::ptr::addr_of_mut!(WS_STATE) };
    if state.waiter.0 < 0 || state.waiter.1 != state.focus {
        return;
    }
    let mut buf = [0u8; 16];
    loop {
        let n = unsafe { minix_rt::read(WS_KBD_FD, &mut buf) };
        if n < 8 {
            break;
        }
        let n = n as usize;
        let mut off = 0;
        while off + 8 <= n {
            let page = u16::from_le_bytes([buf[off], buf[off + 1]]);
            let code = u16::from_le_bytes([buf[off + 2], buf[off + 3]]);
            let press =
                i32::from_le_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]);
            off += 8;
            if page != KEY_PAGE {
                continue;
            }
            match code {
                0x00E1 | 0x00E5 => {
                    unsafe { WS_SHIFT = press == 1 };
                    continue;
                }
                _ => {}
            }
            if press != 1 {
                continue;
            }
            let Some(mut ch) = usage_to_ascii(code) else {
                continue;
            };
            if ch.is_ascii_alphabetic() {
                if unsafe { WS_SHIFT } {
                    ch = ch.to_ascii_uppercase();
                }
            } else if unsafe { WS_SHIFT } {
                ch = shifted(ch);
            }
            let state = unsafe { &mut *core::ptr::addr_of_mut!(WS_STATE) };
            if let Some((ep, ch)) = state.route_key(ch) {
                let mut key_msg = Message {
                    m_source: 0,
                    m_type: WS_KEY as i32,
                    m_payload: unsafe { core::mem::zeroed() },
                };
                // The key char rides in m2l1 (minix_std::wserver::ws_key_char).
                unsafe {
                    key_msg.m_payload.m2.m2l1 = ch as i64;
                    minix_rt::syscall2(
                        minix_rt::SEND_CALL,
                        ep as u64,
                        &mut key_msg as *mut Message as u64,
                    );
                }
            }
        }
    }
}

/// Handle one client request; returns `Some(status)` to reply, or `None`
/// for WS_INPUT (the client is left blocked until a key routes).
#[cfg(target_os = "minix")]
fn handle_request(msg: &mut Message, src: i32) -> Option<i32> {
    let state = unsafe { &mut *core::ptr::addr_of_mut!(WS_STATE) };
    let m = unsafe { &msg.m_payload.m2 };
    match msg.m_type as u32 {
        WS_CREATE => {
            let x = m.m2i1;
            let y = m.m2i2;
            let w = m.m2i3;
            let h = m.m2l1 as i32;
            let title_ptr = m.m2l2 as u64;
            let title_len = m.m2l3 as usize;
            match state.create(x, y, w, h) {
                Ok(wid) => {
                    if title_len > 0 {
                        let mut title = [0u8; 24];
                        let n = title_len.min(23);
                        let _ = minix_rt::sys_vircopy(
                            src,
                            title_ptr,
                            minix_rt::SELF,
                            title.as_mut_ptr() as u64,
                            n,
                        );
                        state.windows[wid].title[..n].copy_from_slice(&title[..n]);
                    }
                    redraw(unsafe { WS_FB });
                    // Union field writes of Copy types are safe.
                    msg.m_payload.m2.m2i1 = wid as i32;
                    Some(0)
                }
                Err(e) => Some(e),
            }
        }
        WS_TEXT => {
            let wid = m.m2i1 as usize;
            let row = m.m2i2;
            let col = m.m2i3;
            let ch = m.m2l1 as u8;
            let r = state.text(wid, row, col, ch);
            if r.is_ok() {
                redraw(unsafe { WS_FB });
                Some(0)
            } else {
                Some(r.unwrap_err())
            }
        }
        WS_FILL => {
            let wid = m.m2i1 as usize;
            let x0 = m.m2i2;
            let y0 = m.m2i3;
            let x1 = m.m2l1 as i32;
            let y1 = m.m2l2 as i32;
            let color = m.m2l3 as u32;
            let r = state.fill(wid, x0, y0, x1, y1, color);
            if r.is_ok() {
                redraw(unsafe { WS_FB });
                Some(0)
            } else {
                Some(r.unwrap_err())
            }
        }
        WS_CLOSE => {
            let wid = m.m2i1 as usize;
            let r = state.close(wid);
            if r.is_ok() {
                redraw(unsafe { WS_FB });
                Some(0)
            } else {
                Some(r.unwrap_err())
            }
        }
        WS_INPUT => {
            let wid = m.m2i1 as usize;
            if wid >= MAX_WINDOWS || !state.windows[wid].used {
                return Some(-9); // EBADF
            }
            state.waiter = (src, wid);
            // A key may already be queued (the input server notifies on
            // IRQ, not on waiter registration); catch it immediately.
            drain_kbd();
            None
        }
        _ => Some(-38), // ENOSYS
    }
}

/// Main loop: receive client requests and input notifications.
pub fn wserver_main() {
    #[cfg(target_os = "minix")]
    {
        const ANY: i32 = 0x0000_ffff;

        // Map the framebuffer (K3) and open the keyboard. Failures are
        // reported (not silent) — the server cannot serve without them.
        let fb_fd = match unsafe { minix_std::fs::open(b"/dev/fb", minix_std::fs::O_RDWR, 0) } {
            Ok(fd) => fd,
            Err(e) => {
                let mut msg_buf = [0u8; 64];
                let n = e.0 as u32;
                let mut i = 0;
                for b in b"wserver: open /dev/fb err=".iter() {
                    msg_buf[i] = *b;
                    i += 1;
                }
                if n == 0 {
                    msg_buf[i] = b'0';
                    i += 1;
                } else {
                    let mut tmp = [0u8; 10];
                    let mut j = 10;
                    let mut v = n;
                    while v > 0 {
                        j -= 1;
                        tmp[j] = b'0' + (v % 10) as u8;
                        v /= 10;
                    }
                    while j < 10 {
                        msg_buf[i] = tmp[j];
                        i += 1;
                        j += 1;
                    }
                }
                msg_buf[i] = b'\n';
                i += 1;
                unsafe {
                    minix_rt::write(2, msg_buf.as_ptr(), i);
                }
                return;
            }
        };
        let fb = unsafe {
            minix_std::vmem::mmap(
                core::ptr::null_mut(),
                MAP_LEN,
                minix_std::vmem::PROT_READ | minix_std::vmem::PROT_WRITE,
                minix_std::vmem::MAP_SHARED,
                fb_fd,
                0,
            )
        };
        if fb == minix_std::vmem::MAP_FAILED {
            unsafe {
                minix_rt::write(2, b"wserver: mmap /dev/fb failed\n".as_ptr(), 29);
            }
            return;
        }
        unsafe {
            WS_FB = fb as u64;
            WS_FB_FD = fb_fd;
        }
        let kbd_fd = match unsafe { minix_std::fs::open(b"/dev/kbd", minix_std::fs::O_RDONLY, 0) } {
            Ok(fd) => fd,
            Err(_) => {
                unsafe {
                    minix_rt::write(2, b"wserver: open /dev/kbd failed\n".as_ptr(), 30);
                }
                return;
            }
        };
        unsafe {
            WS_KBD_FD = kbd_fd;
        }

        // Register as the input server's consumer so key events wake the
        // receive loop.
        let mut reg = Message {
            m_source: 0,
            m_type: arch_common::com::INPUT_REG_CONSUMER as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let reg_r = unsafe {
            minix_rt::syscall2(
                minix_rt::SENDREC_CALL,
                arch_common::com::INPUT_PROC_NR as u64,
                &mut reg as *mut Message as u64,
            )
        };
        if reg_r < 0 {
            unsafe {
                minix_rt::write(2, b"wserver: input consumer register failed\n".as_ptr(), 40);
            }
        }

        unsafe {
            minix_rt::write(1, b"wserver: ready\n".as_ptr(), 14);
        }

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };
            let src = unsafe {
                minix_rt::syscall2(
                    minix_rt::RECEIVE_CALL,
                    ANY as u64,
                    &mut msg as *mut Message as u64,
                )
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            // Input notification: the keyboard has queued events.
            let is_notify =
                (msg.m_type as u32).wrapping_sub(arch_common::com::NOTIFY_MESSAGE) < 0x100;
            if is_notify && src_ep == arch_common::com::INPUT_PROC_NR {
                drain_kbd();
                continue;
            }

            if let Some(status) = handle_request(&mut msg, src_ep) {
                if status == -38 {
                    // Report a stray message (a notification with the wrong
                    // source or an unknown request type) — servers should
                    // surface protocol anomalies, not swallow them.
                    let mut buf = [0u8; 64];
                    let mut i = 0;
                    for b in b"wserver: stray msg src=".iter() {
                        buf[i] = *b;
                        i += 1;
                    }
                    let n = src_ep as u32;
                    if n == 0 {
                        buf[i] = b'0';
                        i += 1;
                    } else {
                        let mut tmp = [0u8; 10];
                        let mut j = 10;
                        let mut v = n;
                        while v > 0 {
                            j -= 1;
                            tmp[j] = b'0' + (v % 10) as u8;
                            v /= 10;
                        }
                        while j < 10 {
                            buf[i] = tmp[j];
                            i += 1;
                            j += 1;
                        }
                    }
                    for b in b" type=".iter() {
                        buf[i] = *b;
                        i += 1;
                    }
                    let t = msg.m_type as u32;
                    let mut tmp = [0u8; 10];
                    let mut j = 10;
                    let mut v = t;
                    if t == 0 {
                        buf[i] = b'0';
                        i += 1;
                    } else {
                        while v > 0 {
                            j -= 1;
                            tmp[j] = b'0' + (v % 10) as u8;
                            v /= 10;
                        }
                        while j < 10 {
                            buf[i] = tmp[j];
                            i += 1;
                            j += 1;
                        }
                    }
                    buf[i] = b'\n';
                    i += 1;
                    unsafe {
                        minix_rt::write(2, buf.as_ptr(), i);
                    }
                }
                msg.m_type = status;
                unsafe {
                    minix_rt::syscall2(
                        minix_rt::SEND_CALL,
                        src_ep as u64,
                        &mut msg as *mut Message as u64,
                    );
                }
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // Host stub — the server loop cannot run outside the MINIX target.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_text_fill_roundtrip() {
        let mut st = WsState::new();
        let wid = st.create(40, 40, 320, 200).unwrap();
        assert_eq!(st.focus, wid);
        assert_eq!(st.windows[wid].rows(), 11); // (200-16)/16
        assert_eq!(st.windows[wid].cols(), 40);

        st.text(wid, 0, 0, b'>').unwrap();
        assert_eq!(st.windows[wid].text[0][0], b'>');
        st.text(wid, 0, 1, b' ').unwrap();
        // Out-of-bounds cell rejected.
        assert!(st.text(wid, 0, 40, b'x').is_err());
        assert!(st.text(wid, 11, 0, b'x').is_err());

        st.fill(wid, 0, 150, 320, 158, 0x00FF0000).unwrap();
        assert_eq!(st.windows[wid].nrects, 1);
        assert_eq!(st.windows[wid].rects[0].color, 0x00FF0000);
        // Fill beyond the body rejected.
        assert!(st.fill(wid, 0, 0, 320, 200, 0).is_err());
        // Body height is h - TITLE_H = 184.
        assert!(st.fill(wid, 0, 0, 320, 184, 0).is_ok());
    }

    #[test]
    fn test_create_bounds() {
        let mut st = WsState::new();
        assert!(st.create(0, 0, 0, 100).is_err());
        assert!(st.create(0, 0, 3200, 200).is_err()); // > 40 cols
        assert!(st.create(0, 0, 320, 0).is_err());
        let wid = st.create(0, 0, 320, 200).unwrap();
        assert_eq!(wid, 0);
    }

    #[test]
    fn test_close_focus_fallback() {
        let mut st = WsState::new();
        let a = st.create(0, 0, 320, 200).unwrap();
        let b = st.create(440, 40, 320, 200).unwrap();
        assert_eq!(st.focus, b);
        st.close(b).unwrap();
        assert_eq!(st.focus, a);
        assert!(!st.windows[b].used);
        st.close(b).unwrap_err();
    }

    #[test]
    fn test_route_key_to_waiter() {
        let mut st = WsState::new();
        let wid = st.create(0, 0, 320, 200).unwrap();
        // No waiter yet → no route.
        assert!(st.route_key(b'a').is_none());
        st.waiter = (42, wid);
        assert_eq!(st.route_key(b'a'), Some((42, b'a')));
        // Waiter consumed.
        assert!(st.route_key(b'b').is_none());
    }

    #[test]
    fn test_usage_to_ascii_codes() {
        assert_eq!(usage_to_ascii(0x04), Some(b'a'));
        assert_eq!(usage_to_ascii(0x1D), Some(b'z'));
        assert_eq!(usage_to_ascii(0x1E), Some(b'1'));
        assert_eq!(usage_to_ascii(0x27), Some(b'0'));
        assert_eq!(usage_to_ascii(0x2C), Some(b' '));
        assert_eq!(usage_to_ascii(0x28), Some(b'\n'));
        assert_eq!(usage_to_ascii(0x2A), Some(0x08));
        assert_eq!(usage_to_ascii(0x2D), Some(b'-'));
        assert!(usage_to_ascii(0x00E1).is_none());
        assert_eq!(shifted(b'1'), b'!');
        assert_eq!(shifted(b'.'), b'>');
    }
}
