//! A stand-in for the `terminal_size` crate, for minix.
//!
//! Upstream asks `rustix::termios` for `TIOCGWINSZ`. The port's rustix fork has
//! no termios surface, and the request encoding is the port's own (NetBSD's
//! layout, not Linux's), so this asks the tty server directly. `clap`'s
//! `wrap_help` feature is the consumer: without a size it falls back to
//! `COLUMNS`/`LINES` and then to its default width.

use std::ffi::{c_int, c_ulong, c_void};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Width(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Height(pub u16);

/// `_IOR('t', 104, struct winsize)` in the port's encoding, the number
/// `minix_std::termios::TIOCGWINSZ` and the tty server share.
const TIOCGWINSZ: c_ulong = 0x4008_7468;

#[repr(C)]
#[derive(Clone, Copy)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, arg: *mut c_void) -> c_int;
}

/// Returns the size of the terminal, checking stdout, stderr and stdin in that
/// order and returning the first that is a terminal.
pub fn terminal_size() -> Option<(Width, Height)> {
    if let Some(size) = terminal_size_of(std::io::stdout()) {
        Some(size)
    } else if let Some(size) = terminal_size_of(std::io::stderr()) {
        Some(size)
    } else {
        terminal_size_of(std::io::stdin())
    }
}

/// Returns the terminal size of `fd`, or `None` when it is not a terminal or
/// the tty server reports no size yet.
pub fn terminal_size_of<Fd: AsFd>(fd: Fd) -> Option<(Width, Height)> {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the request is the tty server's window-size query and `ws` is the
    // `struct winsize` it writes.
    let r = unsafe {
        ioctl(
            fd.as_fd().as_raw_fd(),
            TIOCGWINSZ,
            &mut ws as *mut Winsize as *mut c_void,
        )
    };
    if r < 0 {
        return None;
    }
    if ws.ws_row > 0 && ws.ws_col > 0 {
        Some((Width(ws.ws_col), Height(ws.ws_row)))
    } else {
        None
    }
}

/// Returns the size of the terminal for a raw descriptor.
///
/// # Safety
///
/// `fd` must be an open descriptor.
#[deprecated(note = "Use `terminal_size_of` instead, with a `BorrowedFd`.")]
pub unsafe fn terminal_size_using_fd(fd: RawFd) -> Option<(Width, Height)> {
    // SAFETY: the caller guarantees `fd` is open.
    terminal_size_of(unsafe { BorrowedFd::borrow_raw(fd) })
}
