//! Minimal POSIX termios over the VFS ioctl path.
//!
//! The tty server (`crates/servers/src/tty.rs`) implements the full
//! NetBSD-style termios via `TIOCGETA`/`TIOCSETA` ioctls (grant-based
//! arg data). This module exposes the small surface userland needs — the
//! `Termios` layout must stay byte-identical to the server's, so the
//! ioctl request codes are derived from `size_of::<Termios>()` with the
//! same `ioc_encode` formula the server uses.

use crate::MinixErr;

/// Number of control characters (matches the tty's `NCCS`).
pub const NCCS: usize = 20;

/// Terminal attributes (byte layout matches the tty server's `Termios`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    /// Input flags.
    pub c_iflag: u32,
    /// Output flags.
    pub c_oflag: u32,
    /// Control flags.
    pub c_cflag: u32,
    /// Local flags.
    pub c_lflag: u32,
    /// Control characters.
    pub c_cc: [u8; NCCS],
    /// Input speed.
    pub c_ispeed: i32,
    /// Output speed.
    pub c_ospeed: i32,
}

impl Termios {
    /// All-zero termios.
    pub const fn zeroed() -> Self {
        Self {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: 0,
            c_cc: [0; NCCS],
            c_ispeed: 0,
            c_ospeed: 0,
        }
    }
}

/// Encode an ioctl request number (same formula as `net::ioc_encode`,
/// used by the tty server's `TIOCGETA`/`TIOCSETA` constants).
const fn ioc_encode(dir: u32, group: u8, num: u8, size: usize) -> u32 {
    dir | (((size as u32) & 0x1FFF) << 16) | ((group as u32) << 8) | (num as u32)
}

/// Get the terminal attributes (NetBSD `TIOCGETA`).
pub const TIOCGETA: u32 = ioc_encode(0x4000_0000, b't', 19, core::mem::size_of::<Termios>());
/// Set the terminal attributes (NetBSD `TIOCSETA`).
pub const TIOCSETA: u32 = ioc_encode(0x8000_0000, b't', 20, core::mem::size_of::<Termios>());
/// Set the attributes once output has drained (POSIX `TCSADRAIN`).
pub const TIOCSETAW: u32 = ioc_encode(0x8000_0000, b't', 21, core::mem::size_of::<Termios>());
/// Set the attributes after discarding input (POSIX `TCSAFLUSH`).
pub const TIOCSETAF: u32 = ioc_encode(0x8000_0000, b't', 22, core::mem::size_of::<Termios>());
/// Wait for output to drain; no argument.
pub const TIOCDRAIN: u32 = ioc_encode(0, b't', 94, 0);
/// Discard queued input and/or output; the argument is the `FREAD`/`FWRITE`
/// mask (POSIX `TCIFLUSH`/`TCOFLUSH`/`TCIOFLUSH`).
pub const TIOCFLUSH: u32 = ioc_encode(0x8000_0000, b't', 16, core::mem::size_of::<i32>());
/// Suspend output (POSIX `TCOOFF`); no argument.
pub const TIOCSTOP: u32 = ioc_encode(0, b't', 111, 0);
/// Resume output (POSIX `TCOON`); no argument.
pub const TIOCSTART: u32 = ioc_encode(0, b't', 110, 0);
/// Read the foreground process group of the terminal.
pub const TIOCGPGRP: u32 = ioc_encode(0x4000_0000, b't', 119, core::mem::size_of::<i32>());
/// Set the foreground process group of the terminal.
pub const TIOCSPGRP: u32 = ioc_encode(0x8000_0000, b't', 118, core::mem::size_of::<i32>());

/// Window size (rows/cols; the pty master's ioctl sets the slave's).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WinSize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Get the terminal window size (NetBSD `TIOCGWINSZ`).
pub const TIOCGWINSZ: u32 = ioc_encode(0x4000_0000, b't', 104, core::mem::size_of::<WinSize>());
/// Set the terminal window size (NetBSD `TIOCSWINSZ`).
pub const TIOCSWINSZ: u32 = ioc_encode(0x8000_0000, b't', 103, core::mem::size_of::<WinSize>());

// Input flags (c_iflag) — matches the tty server.
/// Signal on break.
pub const IGNBRK: u32 = 0x0000_0001;
/// Interrupt on break.
pub const BRKINT: u32 = 0x0000_0002;
/// Mark parity errors.
pub const PARMRK: u32 = 0x0000_0008;
/// Strip the eighth bit.
pub const ISTRIP: u32 = 0x0000_0020;
/// Translate NL to CR.
pub const INLCR: u32 = 0x0000_0040;
/// Discard CR.
pub const IGNCR: u32 = 0x0000_0080;
/// Translate CR to NL.
pub const ICRNL: u32 = 0x0000_0100;
/// Enable output flow control (^S/^Q).
pub const IXON: u32 = 0x0000_0200;

// Output flags (c_oflag) — matches the tty server.
/// Post-process output.
pub const OPOST: u32 = 0x0000_0001;

// Control flags (c_cflag) — matches the tty server.
/// The character-size mask.
pub const CSIZE: u32 = 0x0000_0300;
/// Eight bits per character.
pub const CS8: u32 = 0x0000_0300;
/// Enable parity generation and detection.
pub const PARENB: u32 = 0x0000_1000;

// Local flags (c_lflag) — matches the tty server.
/// Echo input characters.
pub const ECHO: u32 = 0x0000_0008;
/// Erase echoes as backspace-space-backspace.
pub const ECHOE: u32 = 0x0000_0002;
/// Kill echoes the erased characters.
pub const ECHOK: u32 = 0x0000_0004;
/// Echo the newline even when ECHO is off.
pub const ECHONL: u32 = 0x0000_0010;
/// Echo control characters as `^X`.
pub const ECHOCTL: u32 = 0x0000_0040;
/// Generate signals on INTR/QUIT/SUSP characters.
pub const ISIG: u32 = 0x0000_0080;
/// Canonical (line-by-line) input mode.
pub const ICANON: u32 = 0x0000_0100;
/// Input extensions (LNEXT/REPRINT).
pub const IEXTEN: u32 = 0x0000_0400;

// Control-character indices (c_cc) — matches the tty server.
/// VMIN index (raw-mode minimum bytes).
pub const VMIN: usize = 16;
/// VTIME index (raw-mode inter-byte timeout, tenths).
pub const VTIME: usize = 17;

/// Fetch the terminal attributes of `fd` into `t`.
///
/// # Safety
///
/// `fd` must be a valid terminal descriptor and `t` a writable `Termios`.
#[cfg(target_os = "minix")]
pub unsafe fn tcgetattr(fd: i32, t: &mut Termios) -> Result<(), MinixErr> {
    unsafe { crate::fs::ioctl(fd, TIOCGETA, t as *mut Termios as *mut u8) }.map(|_| ())
}

/// Set the terminal attributes of `fd` from `t` (immediate, no drain).
///
/// # Safety
///
/// `fd` must be a valid terminal descriptor and `t` a readable `Termios`.
#[cfg(target_os = "minix")]
pub unsafe fn tcsetattr(fd: i32, request: u32, t: &Termios) -> Result<(), MinixErr> {
    unsafe { crate::fs::ioctl(fd, request, t as *const Termios as *mut u8) }.map(|_| ())
}

#[cfg(not(target_os = "minix"))]
/// Host stub (always ENOSYS).
///
/// # Safety
///
/// No-op on host; arguments are ignored.
pub unsafe fn tcgetattr(_fd: i32, _t: &mut Termios) -> Result<(), MinixErr> {
    Err(MinixErr::ENOSYS)
}

#[cfg(not(target_os = "minix"))]
/// Host stub (always ENOSYS).
///
/// # Safety
///
/// No-op on host; arguments are ignored.
pub unsafe fn tcsetattr(_fd: i32, _request: u32, _t: &Termios) -> Result<(), MinixErr> {
    Err(MinixErr::ENOSYS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termios_layout_matches_tty_server() {
        // The tty server's Termios is 4 u32 + c_cc[20] + 2 i32, repr(C),
        // with no padding. The ioctl size must stay 44 or the request
        // codes drift from the server's.
        assert_eq!(core::mem::size_of::<Termios>(), 44);
        assert_eq!(core::mem::offset_of!(Termios, c_lflag), 12);
        assert_eq!(core::mem::offset_of!(Termios, c_cc), 16);
        assert_eq!(core::mem::offset_of!(Termios, c_ispeed), 36);
        assert_eq!(core::mem::offset_of!(Termios, c_ospeed), 40);
    }

    #[test]
    fn ioctl_codes_match_tty_server() {
        assert_eq!(TIOCGETA, 0x402C_7413);
        assert_eq!(TIOCSETA, 0x802C_7414);
        assert_eq!(TIOCSETAW, 0x802C_7415);
        assert_eq!(TIOCSETAF, 0x802C_7416);
        assert_eq!(TIOCDRAIN, 0x0000_745E);
        assert_eq!(TIOCFLUSH, 0x8004_7410);
        assert_eq!(TIOCSTART, 0x0000_746E);
        assert_eq!(TIOCSTOP, 0x0000_746F);
        assert_eq!(TIOCGPGRP, 0x4004_7477);
        assert_eq!(TIOCSPGRP, 0x8004_7476);
        assert_eq!(TIOCGWINSZ, 0x4008_7468);
        assert_eq!(TIOCSWINSZ, 0x8008_7467);
        assert_eq!(ECHO, 0x8);
        // The flags `cfmakeraw` clears and sets.
        assert_eq!(ICRNL | IXON, 0x300);
        assert_eq!(OPOST, 0x1);
        assert_eq!(CSIZE | PARENB, 0x1300);
        assert_eq!(CS8, 0x300);
    }
}
