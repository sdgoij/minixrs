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
use minix_std::wserver::{
    WS_CLOSE, WS_CREATE, WS_CURSOR, WS_FILL, WS_FLUSH, WS_INPUT, WS_KEY, WS_PTR, WS_PTRMODE,
    WS_TEXT,
};

const TITLE_H: usize = 16;
const MAX_WINDOWS: usize = 4;
const MAX_TEXT_COLS: usize = 80;
const MAX_TEXT_ROWS: usize = 24;
const MAX_RECTS: usize = 8;

// Screen geometry — unconditional so the host tests exercise the
// pointer/drag math with the real desktop size.
const XRES: usize = 1024;
const YRES: usize = 768;

// Window chrome geometry.
/// Width of the title-bar close button (px).
const CLOSE_W: i32 = 16;
/// Edge grip for resize hit-testing (px).
const RESIZE_GRIP: i32 = 4;
/// Minimum window size (px). Height includes the title bar, so MIN_H
/// keeps at least one body row.
const MIN_W: i32 = 80;
const MIN_H: i32 = TITLE_H as i32 + 16;

// Resize edge bitmask (N/S/E/W).
const EDGE_N: u8 = 0x01;
const EDGE_S: u8 = 0x02;
const EDGE_E: u8 = 0x04;
const EDGE_W: u8 = 0x08;

/// Mouse pointer bitmap (8x12 arrow), drawn on top of everything.
#[cfg(target_os = "minix")]
const POINTER_BITMAP: [u8; 12] = [
    0b10000000, 0b11000000, 0b11100000, 0b11110000, 0b11111000, 0b11111100, 0b11111110, 0b11111100,
    0b11101000, 0b11011000, 0b10011000, 0b00011000,
];
/// Pointer overlay size (px) — matches the bitmap (8 wide, 12 tall).
#[cfg(target_os = "minix")]
const POINTER_W: usize = 8;
#[cfg(target_os = "minix")]
const POINTER_H: usize = 12;
/// Number of pixels in the pointer overlay.
#[cfg(target_os = "minix")]
const POINTER_CELLS: usize = POINTER_W * POINTER_H;

// Drawing constants — the rasterizer only runs on the MINIX target (the
// host tests cover the window-state protocol ops; pixels are probe-verified).
#[cfg(target_os = "minix")]
const PITCH: usize = XRES * 4;
#[cfg(target_os = "minix")]
const MAP_LEN: usize = 4 * 1024 * 1024;
#[cfg(target_os = "minix")]
const KEY_PAGE: u16 = 0x0007;
#[cfg(target_os = "minix")]
const POINTER_PAGE_GD: u16 = 0x0001;
#[cfg(target_os = "minix")]
const POINTER_PAGE_ABS: u16 = 0x00FD;
#[cfg(target_os = "minix")]
const POINTER_PAGE_BTN: u16 = 0x0009;
#[cfg(target_os = "minix")]
const POINTER_GD_X: u16 = 0x0030;
#[cfg(target_os = "minix")]
const POINTER_GD_Y: u16 = 0x0031;
#[cfg(target_os = "minix")]
const POINTER_BTN_1: u16 = 0x0001;

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
#[cfg(target_os = "minix")]
const COLOR_POINTER: u32 = 0x00FFFFFF;

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
    /// Inverse-video block cell (body row, col), if the client set one.
    pub cursor: Option<(usize, usize)>,
    /// Window opted into WS_PTR pointer-event delivery (WS_PTRMODE).
    pub want_ptr: bool,
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
            cursor: None,
            want_ptr: false,
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

/// An in-flight title-bar drag or edge/corner resize.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Drag {
    pub wid: usize,
    pub mode: DragMode,
    /// Pointer position when the drag started (screen px).
    pub grab: (i32, i32),
    /// Window geometry (x, y, w, h) when the drag started.
    pub start: (i32, i32, i32, i32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    Move,
    Resize { edges: u8 },
}

/// What a pointer button transition did, for the server to act on.
pub enum PtrAction {
    /// Deliver a WS_PTR to `wid`'s client (window-local x/y).
    Deliver { wid: usize, x: i32, y: i32 },
    /// No client delivery (desktop, chrome, or drag end).
    None,
}

/// Window-server state (host-testable; the server's statics are one of
/// these).
pub struct WsState {
    pub windows: [Window; MAX_WINDOWS],
    /// Focused window index, or `MAX_WINDOWS` when none (== z-order top).
    pub focus: usize,
    /// Client blocked in WS_INPUT for a window: (endpoint, wid).
    pub waiter: (i32, usize),
    /// Stacking order: window indices bottom → top (used windows only).
    pub zorder: [usize; MAX_WINDOWS],
    pub nz: usize,
    /// Pointer position (screen px), clamped to the framebuffer.
    pub pointer: (i32, i32),
    /// In-flight chrome operation (title drag / edge resize), if any.
    pub drag: Option<Drag>,
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
            zorder: [0; MAX_WINDOWS],
            nz: 0,
            pointer: ((XRES / 2) as i32, (YRES / 2) as i32),
            drag: None,
        }
    }

    /// Allocate a window; returns its wid or a negative error.
    pub fn create(&mut self, x: i32, y: i32, w: i32, h: i32) -> Result<usize, i32> {
        // The text grid holds body rows, so the height check excludes the
        // title bar (a 24-row terminal is 400 px: 384 body + 16 title).
        if w <= 0
            || h <= (TITLE_H as i32)
            || (w as usize) / 8 > MAX_TEXT_COLS
            || (h as usize - TITLE_H) / 16 > MAX_TEXT_ROWS
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
                win.cursor = None;
                win.want_ptr = false;
                self.push_z(i);
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

    /// Place the inverse-video block cursor at a body cell.
    pub fn set_cursor(&mut self, wid: usize, row: i32, col: i32) -> Result<(), i32> {
        let win = self.windows.get_mut(wid).ok_or(-9)?; // EBADF
        if !win.used {
            return Err(-9);
        }
        let (rows, cols) = (win.rows(), win.cols());
        if row < 0 || col < 0 || row as usize >= rows || col as usize >= cols {
            return Err(-34); // ERANGE
        }
        win.cursor = Some((row as usize, col as usize));
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

    /// Close a window; focus falls back to the top of the z-order.
    pub fn close(&mut self, wid: usize) -> Result<(), i32> {
        let win = self.windows.get_mut(wid).ok_or(-9)?;
        if !win.used {
            return Err(-9);
        }
        win.used = false;
        self.remove_z(wid);
        if self.waiter.1 == wid {
            self.waiter = (-1, usize::MAX);
        }
        if let Some(d) = self.drag
            && d.wid == wid
        {
            self.drag = None;
        }
        if self.focus == wid {
            self.focus = if self.nz > 0 {
                self.zorder[self.nz - 1]
            } else {
                MAX_WINDOWS
            };
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

    // --- stacking order ---

    /// Position of `wid` in the z-order (bottom = 0), if used.
    fn zpos(&self, wid: usize) -> Option<usize> {
        self.zorder[..self.nz].iter().position(|&w| w == wid)
    }

    /// Append a window to the top of the z-order and focus it.
    fn push_z(&mut self, wid: usize) {
        self.zorder[self.nz] = wid;
        self.nz += 1;
        self.focus = wid;
    }

    /// Remove a window from the z-order (does not touch focus).
    fn remove_z(&mut self, wid: usize) {
        if let Some(pos) = self.zpos(wid) {
            for i in pos..self.nz - 1 {
                self.zorder[i] = self.zorder[i + 1];
            }
            self.nz -= 1;
        }
    }

    /// Raise a window to the top of the stacking order and focus it.
    pub fn raise(&mut self, wid: usize) {
        let Some(pos) = self.zpos(wid) else {
            return;
        };
        for i in pos..self.nz - 1 {
            self.zorder[i] = self.zorder[i + 1];
        }
        self.zorder[self.nz - 1] = wid;
        self.focus = wid;
    }

    /// Topmost window whose rect contains (x, y); `None` on the desktop.
    pub fn window_at(&self, x: i32, y: i32) -> Option<usize> {
        for &wid in self.zorder[..self.nz].iter().rev() {
            let w = &self.windows[wid];
            if x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h {
                return Some(wid);
            }
        }
        None
    }

    /// Which resize edges a press at (x, y) hits on `wid` (bitmask), or 0.
    pub fn resize_edges(&self, wid: usize, x: i32, y: i32) -> u8 {
        let win = &self.windows[wid];
        let (wx, wy, ww, wh) = (win.x, win.y, win.w, win.h);
        let mut e = 0;
        if y >= wy && y < wy + RESIZE_GRIP {
            e |= EDGE_N;
        }
        if y >= wy + wh - RESIZE_GRIP && y < wy + wh {
            e |= EDGE_S;
        }
        if x >= wx && x < wx + RESIZE_GRIP {
            e |= EDGE_W;
        }
        if x >= wx + ww - RESIZE_GRIP && x < wx + ww {
            e |= EDGE_E;
        }
        e
    }

    /// Move the pointer by a relative delta (a GD X/Y event) and apply
    /// any in-flight drag/resize. Returns true when the pointer moved
    /// (and thus the desktop changed — the caller repaints accordingly).
    pub fn pointer_rel(&mut self, dx: i32, dy: i32) -> bool {
        let (oldx, oldy) = self.pointer;
        self.pointer.0 = (self.pointer.0 + dx).clamp(0, XRES as i32 - 1);
        self.pointer.1 = (self.pointer.1 + dy).clamp(0, YRES as i32 - 1);
        if self.pointer.0 == oldx && self.pointer.1 == oldy {
            return false;
        }
        let (px, py) = self.pointer;
        match self.drag {
            Some(Drag {
                wid,
                mode: DragMode::Move,
                grab,
                start: _,
            }) => {
                let win = &mut self.windows[wid];
                // Keep at least 8 px of the window on-screen horizontally
                // and the title bar visible vertically.
                win.x = (px - grab.0).clamp(-(win.w - 8), XRES as i32 - 8);
                win.y = (py - grab.1).clamp(0, YRES as i32 - 16);
            }
            Some(Drag {
                wid,
                mode: DragMode::Resize { edges },
                grab: _,
                start: (sx, sy, sw, sh),
            }) => {
                let win = &mut self.windows[wid];
                let (mut x, mut y, mut w, mut h) = (sx, sy, sw, sh);
                if edges & EDGE_W != 0 {
                    x = px.min(sx + sw - MIN_W);
                    w = sx + sw - x;
                }
                if edges & EDGE_E != 0 {
                    w = (px - sx).max(MIN_W);
                }
                if edges & EDGE_N != 0 {
                    y = py.min(sy + sh - MIN_H);
                    h = sy + sh - y;
                }
                if edges & EDGE_S != 0 {
                    h = (py - sy).max(MIN_H);
                }
                // Snap to the cell grid so the text grid stays well-defined.
                w = (w / 8) * 8;
                h = TITLE_H as i32 + ((h - TITLE_H as i32) / 16) * 16;
                // Re-anchor after snapping: the opposite edge stays put.
                if edges & EDGE_W != 0 {
                    x = sx + sw - w;
                }
                if edges & EDGE_N != 0 {
                    y = sy + sh - h;
                }
                win.w = w.max(MIN_W);
                win.h = h.max(MIN_H);
                win.x = x.clamp(-(win.w - 8), XRES as i32 - 8);
                win.y = y.clamp(0, YRES as i32 - 16);
            }
            None => {}
        }
        true
    }

    /// A button press/release (1 = left, 2 = right, 3 = middle). Applies
    /// the chrome ops (close button, raise/focus, title drag, edge resize)
    /// and returns a pointer delivery for the window under the pointer
    /// when it opted in, else `None`.
    pub fn pointer_button(&mut self, which: u8, down: bool) -> PtrAction {
        let (px, py) = self.pointer;
        if down {
            let Some(wid) = self.window_at(px, py) else {
                self.drag = None;
                return PtrAction::None;
            };
            // The close button (top-right of the title bar) closes.
            if which == 1 {
                let win = &self.windows[wid];
                let (cx0, cy0, cx1, cy1) = (
                    win.x + win.w - CLOSE_W,
                    win.y,
                    win.x + win.w,
                    win.y + TITLE_H as i32,
                );
                if px >= cx0 && px < cx1 && py >= cy0 && py < cy1 {
                    let _ = self.close(wid);
                    self.drag = None;
                    return PtrAction::None;
                }
            }
            // A click on any part of a window raises + focuses it.
            self.raise(wid);
            let edges = self.resize_edges(wid, px, py);
            let win = &self.windows[wid];
            let in_title = py < win.y + TITLE_H as i32;
            if which == 1 {
                // Edges beat the title bar (the top grip is inside it).
                if edges != 0 {
                    self.drag = Some(Drag {
                        wid,
                        mode: DragMode::Resize { edges },
                        grab: (px, py),
                        start: (win.x, win.y, win.w, win.h),
                    });
                    return PtrAction::None;
                }
                if in_title {
                    self.drag = Some(Drag {
                        wid,
                        mode: DragMode::Move,
                        grab: (px - win.x, py - win.y),
                        start: (win.x, win.y, win.w, win.h),
                    });
                    return PtrAction::None;
                }
            }
            self.drag = None;
            let win = &self.windows[wid];
            let by = win.y + TITLE_H as i32;
            if win.want_ptr && py >= by && py < win.y + win.h {
                PtrAction::Deliver {
                    wid,
                    x: px - win.x,
                    y: py - by,
                }
            } else {
                PtrAction::None
            }
        } else {
            // Any release ends a drag (only left starts one).
            if which == 1 {
                self.drag = None;
            }
            // Deliver releases too, so clients see click/release pairs.
            if let Some(wid) = self.window_at(px, py) {
                let win = &self.windows[wid];
                let by = win.y + TITLE_H as i32;
                if win.want_ptr && py >= by && py < win.y + win.h {
                    return PtrAction::Deliver {
                        wid,
                        x: px - win.x,
                        y: py - by,
                    };
                }
            }
            PtrAction::None
        }
    }

    /// Consume the waiter for `wid` when the window wants pointer events,
    /// returning the endpoint to deliver the WS_PTR to.
    pub fn route_ptr(&mut self, wid: usize) -> Option<i32> {
        if self.waiter.0 >= 0 && self.waiter.1 == wid && self.windows[wid].want_ptr {
            let ep = self.waiter.0;
            self.waiter = (-1, usize::MAX);
            Some(ep)
        } else {
            None
        }
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
/// Keyboard modifier state across drains (a press and its key can land in
/// different batches).
#[cfg(target_os = "minix")]
static mut WS_SHIFT: bool = false;
#[cfg(target_os = "minix")]
static mut WS_CTRL: bool = false;
/// Held mouse-button mask for WS_PTR deliveries (bit 0 = left, …).
#[cfg(target_os = "minix")]
static mut WS_BUTTONS: u8 = 0;
/// Pointer overlay: the desktop pixels under the pointer, captured when
/// it was drawn, so a plain move can repair the vacated area without a
/// full desktop redraw. `WS_PTR_POS` is where the underlay was captured
/// (`None` before the first draw).
#[cfg(target_os = "minix")]
static mut WS_PTR_UNDERLAY: [u32; POINTER_CELLS] = [0; POINTER_CELLS];
#[cfg(target_os = "minix")]
static mut WS_PTR_POS: Option<(usize, usize)> = None;

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
fn read_pixel(fb: u64, x: usize, y: usize) -> u32 {
    if x < XRES && y < YRES {
        let off = (y * PITCH + x * 4) as u64;
        unsafe { core::ptr::read_volatile((fb + off) as *const u32) }
    } else {
        0
    }
}

/// Draw the pointer arrow bitmap at (px, py).
#[cfg(target_os = "minix")]
fn draw_pointer(fb: u64, px: usize, py: usize) {
    for r in 0..POINTER_H {
        let bits = POINTER_BITMAP[r];
        for c in 0..POINTER_W {
            if bits & (0x80 >> c) != 0 {
                put_pixel(fb, px + c, py + r, COLOR_POINTER);
            }
        }
    }
}

/// Push the framebuffer to the display (FBIOFLUSH) directly to the fb
/// server, bypassing VFS: VFS is single-worker and the shell's blocking
/// console read holds it hostage, so a /dev/fb ioctl would block the
/// redraw until console input arrives. FBIOFLUSH carries no arg struct,
/// so the CDEV_IOCTL travels with no grant; it is a no-op for VGA-style
/// backends (bochs-display) and pushes the frame on virtio-gpu.
#[cfg(target_os = "minix")]
fn fb_flush() {
    let mut msg = Message {
        m_source: 0,
        m_type: arch_common::com::CDEV_IOCTL as i32,
        m_payload: unsafe { core::mem::zeroed() },
    };
    // CDEV_IOCTL wire layout (m2 fields): minor, request, grant, user,
    // flags, id.
    msg.m_payload.m2.m2i1 = 0; // minor
    msg.m_payload.m2.m2i2 = minix_std::fs::FBIOFLUSH as i32; // request
    msg.m_payload.m2.m2i3 = 0; // grant (none — no arg struct)
    msg.m_payload.m2.m2l1 = 0; // user endpoint (unused)
    msg.m_payload.m2.m2l2 = 0; // flags
    msg.m_payload.m2.m2l3 = 0; // id
    let _ = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::FB_PROC_NR as u64,
            &mut msg as *mut Message as u64,
        )
    };
}

/// Repaint the pointer after a plain move (no drag): restore the pixels
/// the pointer vacated (the underlay captured when it was drawn), then
/// capture the new position's desktop and draw the pointer there. The
/// desktop content doesn't change during a move, so no full redraw is
/// needed — a full redraw per PS/2 packet (~100–200/s) would saturate
/// the emulated CPU.
#[cfg(target_os = "minix")]
fn pointer_overlay_move() {
    let fb = unsafe { WS_FB };
    let state = unsafe { &*core::ptr::addr_of!(WS_STATE) };
    let (px, py) = (state.pointer.0 as usize, state.pointer.1 as usize);
    unsafe {
        // Repair the area the pointer left: write back the desktop that
        // was underneath it.
        let pos = WS_PTR_POS;
        if let Some((ox, oy)) = pos {
            for i in 0..POINTER_CELLS {
                put_pixel(
                    fb,
                    ox + i % POINTER_W,
                    oy + i / POINTER_W,
                    WS_PTR_UNDERLAY[i],
                );
            }
        }
        // Capture what the pointer is about to cover, then remember where.
        for i in 0..POINTER_CELLS {
            WS_PTR_UNDERLAY[i] = read_pixel(fb, px + i % POINTER_W, py + i / POINTER_W);
        }
        WS_PTR_POS = Some((px, py));
    }
    draw_pointer(fb, px, py);
    // virtio-gpu is explicit-flush: push the change to the display.
    fb_flush();
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

/// Redraw the whole desktop: background, then windows in z-order, then
/// the mouse pointer on top.
#[cfg(target_os = "minix")]
fn redraw(fb: u64) {
    fill_rect(fb, 0, 0, XRES, YRES, COLOR_DESKTOP);
    let state = unsafe { &*core::ptr::addr_of!(WS_STATE) };
    for &i in &state.zorder[..state.nz] {
        let win = &state.windows[i];
        if !win.used {
            continue;
        }
        let (x, y) = (win.x as usize, win.y as usize);
        let (w, h) = (win.w as usize, win.h as usize);
        let body_y = y + TITLE_H;
        let body_h = h - TITLE_H;
        // Body background.
        fill_rect(fb, x, body_y, x + w, body_y + body_h, COLOR_BODY);
        // Filled rects (body-local, clamped to the body so a shrunken
        // window's stale rects don't spill onto the neighbor).
        for r in win.rects[..win.nrects].iter() {
            let x0 = (x + r.x0 as usize).min(x + w);
            let y0 = (body_y + r.y0 as usize).min(body_y + body_h);
            let x1 = (x + r.x1 as usize).min(x + w);
            let y1 = (body_y + r.y1 as usize).min(body_y + body_h);
            if x1 > x0 && y1 > y0 {
                fill_rect(fb, x0, y0, x1, y1, r.color);
            }
        }
        // Title bar chrome + title text (clamped so it never runs under
        // the close button).
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
        let max_chars = (w.saturating_sub(CLOSE_W as usize + 12)) / 8;
        draw_text(
            fb,
            x + 4,
            y,
            &win.title[..title_len.min(max_chars)],
            COLOR_TEXT,
            title_color,
        );
        // Close button: an X at the title's right end.
        let cx = x + w - CLOSE_W as usize;
        for i in 0..10usize {
            put_pixel(fb, cx + 2 + i, y + 3 + i, COLOR_TEXT);
            put_pixel(fb, cx + 2 + i, y + 13 - i, COLOR_TEXT);
        }
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
        // Block cursor: inverse-video cell (glyph dark on white; a space
        // cell renders as a solid white block).
        if let Some((r, c)) = win.cursor {
            if r < rows && c < cols {
                let cx = x + c * 8;
                let cy = body_y + r * 16;
                let ch = win.text[r][c];
                if ch != b' ' {
                    draw_char(fb, cx, cy, ch, COLOR_BODY, COLOR_TEXT);
                } else {
                    fill_rect(fb, cx, cy, cx + 8, cy + 16, COLOR_TEXT);
                }
            }
        }
    }
    // Mouse pointer on top of everything. Capture the underlay (the
    // desktop the pointer covers) so a later plain move can repair this
    // area without a full redraw.
    let (px, py) = (state.pointer.0 as usize, state.pointer.1 as usize);
    unsafe {
        for i in 0..POINTER_CELLS {
            WS_PTR_UNDERLAY[i] = read_pixel(fb, px + i % POINTER_W, py + i / POINTER_W);
        }
        WS_PTR_POS = Some((px, py));
    }
    draw_pointer(fb, px, py);
    // virtio-gpu is explicit-flush: push the new frame to the display.
    // No-op for VGA-style backends.
    fb_flush();
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

/// Map a (shifted) ASCII char to its control character for a held Ctrl.
/// US-layout convention: letters AND with 0x1F; the top-row shifted
/// specials and their unshifted digit twins map to the classic control
/// codes (xterm's default).
#[cfg_attr(not(target_os = "minix"), allow(dead_code))]
fn ctrl_map(ch: u8) -> u8 {
    match ch {
        b'@' | b' ' | b'2' => 0x00, // NUL
        b'[' | b'3' => 0x1B,        // ESC
        b'\\' | b'4' => 0x1C,       // FS
        b']' | b'5' => 0x1D,        // GS
        b'^' | b'6' => 0x1E,        // RS
        b'_' | b'7' => 0x1F,        // US
        b'?' => 0x7F,               // DEL
        c if c.is_ascii_alphabetic() => c & 0x1F,
        _ => ch,
    }
}

/// HID usages of the arrow keys (routed without an ASCII char; the client
/// turns them into escape sequences).
#[cfg_attr(not(target_os = "minix"), allow(dead_code))]
fn is_arrow(code: u16) -> bool {
    matches!(code, 0x4F..=0x52)
}
/// Route one keyboard event; returns true when a key was delivered to a
/// waiter (the caller stops draining so any further events in the batch
/// stay queued for the next WS_INPUT registration's catch-up drain).
#[cfg(target_os = "minix")]
fn handle_key(code: u16, press: i32) -> bool {
    // Modifiers: track the held state on both press and release.
    match code {
        0x00E0 | 0x00E4 => {
            unsafe { WS_CTRL = press == 1 };
            return false;
        }
        0x00E1 | 0x00E5 => {
            unsafe { WS_SHIFT = press == 1 };
            return false;
        }
        _ => {}
    }
    if press != 1 {
        return false;
    }
    let mut ch = match usage_to_ascii(code) {
        Some(c) => c,
        None if is_arrow(code) => 0, // arrows route with no char
        None => return false,
    };
    if ch.is_ascii_alphabetic() {
        if unsafe { WS_SHIFT } {
            ch = ch.to_ascii_uppercase();
        }
    } else if unsafe { WS_SHIFT } {
        ch = shifted(ch);
    }
    if unsafe { WS_CTRL } {
        ch = ctrl_map(ch);
    }
    let state = unsafe { &mut *core::ptr::addr_of_mut!(WS_STATE) };
    if let Some((ep, _)) = state.route_key(ch) {
        let mut key_msg = Message {
            m_source: 0,
            m_type: WS_KEY as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        // The key char rides in m2l1 (minix_std::wserver::ws_key_char)
        // and the HID usage in m2l2 (ws_key_usage) so clients can
        // distinguish special keys (arrows) from plain chars.
        unsafe {
            key_msg.m_payload.m2.m2l1 = ch as i64;
            key_msg.m_payload.m2.m2l2 = code as i64;
            minix_rt::syscall2(
                minix_rt::SEND_CALL,
                ep as u64,
                &mut key_msg as *mut Message as u64,
            );
        }
        return true;
    }
    false
}

/// Drain decoded input events (keys + mouse). Keyboard events route to
/// the focused window's waiter when one is blocked; mouse events always
/// move the pointer / drive chrome and may deliver WS_PTR. Returns true
/// when the desktop changed (needs a redraw).
///
/// Events are fetched directly from the input server (the consumer
/// protocol's event channel), not through a `/dev/kbd` VFS read: VFS is
/// single-worker, so the shell's blocking console read holds it hostage
/// and a /dev/kbd read would sit queued until console input arrives.
/// The input server handles CDEV_READ by popping its event ring into the
/// reply payload; the reply status is the byte count (EAGAIN when empty).
///
/// The wserver consumes the whole stream unconditionally now (it cannot
/// leave mouse events queued), so a key pressed with no waiter is dropped
/// instead of staying for another /dev/kbd reader (keytest).
#[cfg(target_os = "minix")]
fn drain_input() -> bool {
    let mut changed = false;
    loop {
        let mut msg = Message {
            m_source: 0,
            m_type: arch_common::com::CDEV_READ as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        // Standard CDEV_READ wire layout (m2 fields): minor, flags, grant,
        // position, count.
        msg.m_payload.m2.m2i1 = 0; // minor
        msg.m_payload.m2.m2i2 = 0; // flags
        msg.m_payload.m2.m2i3 = 0; // grant (unused)
        msg.m_payload.m2.m2l1 = 0; // position
        msg.m_payload.m2.m2l2 = 16; // count
        let _ = unsafe {
            minix_rt::syscall2(
                minix_rt::SENDREC_CALL,
                arch_common::com::INPUT_PROC_NR as u64,
                &mut msg as *mut Message as u64,
            )
        };
        // The SENDREC return is the sender (INPUT); the reply status (byte
        // count or EAGAIN) is in m_type. A stray notification delivered as
        // the reply (NOTIFY_MESSAGE = 0x1000) is out of range — bail; the
        // main loop recovers the collided event reply when it arrives.
        let n = msg.m_type;
        if n < 8 || n > 48 {
            break;
        }
        // SAFETY: the input server filled the reply payload with events.
        let batch = unsafe { &msg.m_payload.raw[..n as usize] };
        if process_event_batch(batch) {
            changed = true;
        }
    }
    changed
}

/// Process one batch of decoded input events (8-byte {page, code, press}
/// records). Returns true when the desktop changed (needs a redraw). A key
/// that consumed the WS_INPUT waiter stops the batch: the rest are already
/// popped from the input ring, and no second waiter can be served by one
/// batch anyway.
#[cfg(target_os = "minix")]
fn process_event_batch(data: &[u8]) -> bool {
    let mut changed = false;
    let mut off = 0;
    while off + 8 <= data.len() {
        let page = u16::from_le_bytes([data[off], data[off + 1]]);
        let code = u16::from_le_bytes([data[off + 2], data[off + 3]]);
        let press =
            i32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        off += 8;
        match page {
            KEY_PAGE => {
                if handle_key(code, press) {
                    return changed;
                }
            }
            POINTER_PAGE_GD => {
                let dx = if code == POINTER_GD_X { press } else { 0 };
                let dy = if code == POINTER_GD_Y { press } else { 0 };
                let state = unsafe { &mut *core::ptr::addr_of_mut!(WS_STATE) };
                let dragging = state.drag.is_some();
                if state.pointer_rel(dx, dy) {
                    if dragging {
                        // The drag moved/resized a window — full redraw.
                        changed = true;
                    } else {
                        // Plain move: repaint just the pointer overlay.
                        pointer_overlay_move();
                    }
                }
            }
            POINTER_PAGE_ABS => {
                // Absolute tablet position: QEMU normalizes input to
                // 0..0x7FFF; scale to the framebuffer and apply as a delta
                // from the current pointer so the drag/resize logic (which
                // anchors on the pointer position) works unchanged. The X
                // and Y arrive as separate events; each lands immediately.
                let state = unsafe { &mut *core::ptr::addr_of_mut!(WS_STATE) };
                let (dx, dy) = if code == POINTER_GD_X {
                    (press * XRES as i32 / 32768 - state.pointer.0, 0)
                } else if code == POINTER_GD_Y {
                    (0, press * YRES as i32 / 32768 - state.pointer.1)
                } else {
                    (0, 0)
                };
                let dragging = state.drag.is_some();
                if state.pointer_rel(dx, dy) {
                    if dragging {
                        changed = true;
                    } else {
                        pointer_overlay_move();
                    }
                }
            }
            POINTER_PAGE_BTN => {
                let which = (code - POINTER_BTN_1 + 1) as u8;
                if !(1..=3).contains(&which) {
                    continue;
                }
                let down = press == 1;
                let bit = 1u8 << (which - 1);
                if down {
                    unsafe { WS_BUTTONS |= bit };
                } else {
                    unsafe { WS_BUTTONS &= !bit };
                }
                let state = unsafe { &mut *core::ptr::addr_of_mut!(WS_STATE) };
                let action = state.pointer_button(which, down);
                changed = true;
                if let PtrAction::Deliver { wid, x, y } = action {
                    if let Some(ep) = state.route_ptr(wid) {
                        let mut ptr_msg = Message {
                            m_source: 0,
                            m_type: WS_PTR as i32,
                            m_payload: unsafe { core::mem::zeroed() },
                        };
                        unsafe {
                            ptr_msg.m_payload.m2.m2i1 = WS_BUTTONS as i32;
                            ptr_msg.m_payload.m2.m2i2 = x;
                            ptr_msg.m_payload.m2.m2i3 = y;
                            minix_rt::syscall2(
                                minix_rt::SEND_CALL,
                                ep as u64,
                                &mut ptr_msg as *mut Message as u64,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
    changed
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
            // Redraws are deferred to WS_FLUSH so a client can batch a
            // screenful of WS_TEXT/WS_FILL updates and repaint once.
            if r.is_ok() {
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
            // See WS_TEXT: repaint happens on WS_FLUSH.
            if r.is_ok() {
                Some(0)
            } else {
                Some(r.unwrap_err())
            }
        }
        WS_CURSOR => {
            let wid = m.m2i1 as usize;
            let row = m.m2i2;
            let col = m.m2i3;
            let r = state.set_cursor(wid, row, col);
            // Repaint happens on WS_FLUSH (the block is part of the frame).
            if r.is_ok() {
                Some(0)
            } else {
                Some(r.unwrap_err())
            }
        }
        WS_FLUSH => {
            redraw(unsafe { WS_FB });
            Some(0)
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
            if drain_input() {
                redraw(unsafe { WS_FB });
            }
            None
        }
        WS_PTRMODE => {
            let wid = m.m2i1 as usize;
            let on = m.m2i2 != 0;
            let win = match state.windows.get_mut(wid) {
                Some(w) => w,
                None => return Some(-9), // EBADF
            };
            if !win.used {
                return Some(-9); // EBADF
            }
            win.want_ptr = on;
            Some(0)
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

            // Input notification: the keyboard/mouse has queued events.
            let is_notify =
                (msg.m_type as u32).wrapping_sub(arch_common::com::NOTIFY_MESSAGE) < 0x100;
            if is_notify && src_ep == arch_common::com::INPUT_PROC_NR {
                if drain_input() {
                    redraw(unsafe { WS_FB });
                }
                continue;
            }

            // Any other message from the input server is an event fetch
            // reply that arrived outside drain_input: the SENDNB wakeup can
            // collide with the fetch SENDREC (both come from INPUT), so the
            // fetch completes with the notification and the real reply lands
            // here. Process the events instead of replying ENOSYS — a stray
            // reply to INPUT would start an ENOSYS ping-pong and drop them.
            if src_ep == arch_common::com::INPUT_PROC_NR {
                let n = msg.m_type;
                if (8..=48).contains(&n) {
                    // SAFETY: the input server filled the reply payload
                    // with events.
                    let batch = unsafe { &msg.m_payload.raw[..n as usize] };
                    if process_event_batch(batch) {
                        redraw(unsafe { WS_FB });
                    }
                }
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
    fn test_set_cursor_roundtrip() {
        let mut st = WsState::new();
        let wid = st.create(0, 0, 320, 200).unwrap();
        // No cursor until the client sets one (other clients, e.g. wdemo).
        assert_eq!(st.windows[wid].cursor, None);
        st.set_cursor(wid, 3, 5).unwrap();
        assert_eq!(st.windows[wid].cursor, Some((3, 5)));
        // Out-of-bounds cursor rejected.
        assert!(st.set_cursor(wid, 11, 0).is_err());
        assert!(st.set_cursor(wid, 0, 40).is_err());
        // A fresh window starts without a cursor.
        let b = st.create(440, 40, 320, 200).unwrap();
        assert_eq!(st.windows[b].cursor, None);
    }

    #[test]
    fn test_close_resets_cursor() {
        let mut st = WsState::new();
        let wid = st.create(0, 0, 320, 200).unwrap();
        st.set_cursor(wid, 0, 0).unwrap();
        st.close(wid).unwrap();
        // The slot is reusable; a re-create starts cursor-less.
        let b = st.create(0, 0, 320, 200).unwrap();
        assert_eq!(st.windows[b].cursor, None);
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

    #[test]
    fn test_zorder_create_raise_close() {
        let mut st = WsState::new();
        let a = st.create(0, 0, 320, 200).unwrap();
        let b = st.create(200, 120, 320, 200).unwrap();
        // Creation order is the stacking order; the newest is on top.
        assert_eq!(&st.zorder[..st.nz], &[a, b]);
        assert_eq!(st.focus, b);
        // Raising the older window puts it on top and focuses it.
        st.raise(a);
        assert_eq!(&st.zorder[..st.nz], &[b, a]);
        assert_eq!(st.focus, a);
        // Closing the top falls back to the remaining window.
        st.close(a).unwrap();
        assert_eq!(&st.zorder[..st.nz], &[b]);
        assert_eq!(st.focus, b);
    }

    #[test]
    fn test_window_at_topmost() {
        let mut st = WsState::new();
        let a = st.create(100, 100, 320, 200).unwrap(); // 100..420, 100..300
        let b = st.create(200, 120, 320, 200).unwrap(); // 200..520, 120..320
        // In both windows → the topmost (b).
        assert_eq!(st.window_at(250, 200), Some(b));
        // Only in a.
        assert_eq!(st.window_at(150, 130), Some(a));
        // On the desktop.
        assert_eq!(st.window_at(10, 10), None);
        // After raising a, it wins the overlap.
        st.raise(a);
        assert_eq!(st.window_at(250, 200), Some(a));
    }

    #[test]
    fn test_pointer_title_drag_moves_window() {
        let mut st = WsState::new();
        let wid = st.create(100, 100, 320, 200).unwrap();
        // Move the pointer to a title-bar point away from the edges
        // (y=110 is in the title but below the 4px N grip).
        st.pointer_rel(-262, -274); // (512, 384) → (250, 110)
        assert!(matches!(st.pointer_button(1, true), PtrAction::None));
        assert!(matches!(
            st.drag,
            Some(Drag {
                mode: DragMode::Move,
                ..
            })
        ));
        st.pointer_rel(30, 20); // pointer → (280, 130); grab was (150, 10)
        assert_eq!(st.windows[wid].x, 130);
        assert_eq!(st.windows[wid].y, 120);
        // Release ends the drag; further moves don't move the window.
        assert!(matches!(st.pointer_button(1, false), PtrAction::None));
        assert!(st.drag.is_none());
        let (x, y) = (st.windows[wid].x, st.windows[wid].y);
        st.pointer_rel(10, 0);
        assert_eq!((st.windows[wid].x, st.windows[wid].y), (x, y));
    }

    #[test]
    fn test_pointer_resize_snaps_to_grid() {
        let mut st = WsState::new();
        let wid = st.create(100, 100, 320, 200).unwrap();
        // Grab the SE corner (x=419 is within 4px of x+w, y=299 of y+h).
        st.pointer_rel(-93, -85); // (512, 384) → (419, 299)
        assert!(matches!(st.pointer_button(1, true), PtrAction::None));
        assert!(matches!(
            st.drag,
            Some(Drag { mode: DragMode::Resize { edges }, .. }) if edges & (EDGE_E | EDGE_S) != 0
        ));
        st.pointer_rel(21, 12); // pointer → (440, 311)
        // w = 440-100 = 340 → snap 336; h = 311-100 = 211 → 192+16 = 208.
        assert_eq!(st.windows[wid].w, 336);
        assert_eq!(st.windows[wid].h, 208);
        // Origin stays put for an E|S resize.
        assert_eq!((st.windows[wid].x, st.windows[wid].y), (100, 100));
    }

    #[test]
    fn test_pointer_close_button_closes() {
        let mut st = WsState::new();
        let wid = st.create(100, 100, 320, 200).unwrap();
        // Close button: x 404..420, y 100..116.
        st.pointer_rel(-102, -276); // (512, 384) → (410, 108)
        assert!(matches!(st.pointer_button(1, true), PtrAction::None));
        assert!(!st.windows[wid].used);
        assert_eq!(st.focus, MAX_WINDOWS);
    }

    #[test]
    fn test_pointer_body_delivers_to_ptr_client() {
        let mut st = WsState::new();
        let wid = st.create(100, 100, 320, 200).unwrap();
        st.windows[wid].want_ptr = true;
        st.waiter = (7, wid);
        // Body point (200, 200): win-local (100, 84).
        st.pointer_rel(-312, -184); // (512, 384) → (200, 200)
        match st.pointer_button(1, true) {
            PtrAction::Deliver { wid: w, x, y } => {
                assert_eq!(w, wid);
                assert_eq!(x, 100);
                assert_eq!(y, 84);
            }
            PtrAction::None => panic!("expected a pointer delivery"),
        }
        // Release delivers too (click/release pairs).
        assert!(matches!(
            st.pointer_button(1, false),
            PtrAction::Deliver { .. }
        ));
    }

    #[test]
    fn test_pointer_body_ignored_without_ptrmode() {
        let mut st = WsState::new();
        let wid = st.create(100, 100, 320, 200).unwrap();
        assert!(!st.windows[wid].want_ptr);
        st.pointer_rel(-312, -184); // body point
        assert!(matches!(st.pointer_button(1, true), PtrAction::None));
        assert!(matches!(st.pointer_button(1, false), PtrAction::None));
    }

    #[test]
    fn test_route_ptr_requires_ptrmode() {
        let mut st = WsState::new();
        let wid = st.create(0, 0, 320, 200).unwrap();
        st.waiter = (9, wid);
        // Not opted in → the waiter stays.
        assert!(st.route_ptr(wid).is_none());
        assert_eq!(st.waiter, (9, wid));
        st.windows[wid].want_ptr = true;
        assert_eq!(st.route_ptr(wid), Some(9));
        assert_eq!(st.waiter, (-1, usize::MAX));
    }

    #[test]
    fn test_pointer_move_without_drag_just_moves() {
        let mut st = WsState::new();
        assert_eq!(st.pointer, (512, 384));
        // A plain move changes the desktop (the pointer moved).
        assert!(st.pointer_rel(10, -5));
        assert_eq!(st.pointer, (522, 379));
        // A zero delta reports no change.
        assert!(!st.pointer_rel(0, 0));
        // Clamped to the framebuffer.
        st.pointer_rel(-5000, 5000);
        assert_eq!(st.pointer, (0, YRES as i32 - 1));
    }
}
