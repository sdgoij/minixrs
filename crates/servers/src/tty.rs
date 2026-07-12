//! MINIX TTY server — terminal I/O multiplexer.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/tty/tty/tty.c`
//!
//! The TTY server sits at the intersection of:
//! - Character driver framework (`do_*` functions)
//! - Line discipline (`in_process`, `out_process`, `handle_events`, `sigchar`)
//! - PTY driver (`crates/drivers/src/tty/pty.rs`)
//! - RS-232 driver (`crates/drivers/src/tty/rs232.rs`)
//!
//! **This is a types-and-infrastructure port.**  The chardriver framework,
//! grant-based I/O, SEF, and the server main loop are not yet available, so
//! all external calls are stubbed with "TODO: Phase 13" comments.

#![allow(dead_code, unused_unsafe)]

use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use arch_common::com::{
    CDEV_CANCEL, CDEV_CLOSE, CDEV_IOCTL, CDEV_OPEN, CDEV_READ, CDEV_SELECT, CDEV_WRITE,
    TTY_FKEY_CONTROL, TTY_INPUT_EVENT, TTY_INPUT_UP, is_cdev_rq,
};
use arch_common::endpoint::ANY;

// Imports from kernel

use kernel::r#priv::MinixTimer;

// Constants — device configuration

/// Number of console lines.
pub const NR_CONS: usize = 4;

/// Number of RS-232 serial lines.
pub const NR_RS_LINES: usize = 4;

/// First minor for console devices.
pub const CONS_MINOR: u32 = 0;

/// Minor for the log device.
pub const LOG_MINOR: u32 = 15;

/// First minor for RS-232 serial devices.
pub const RS232_MINOR: u32 = 16;

/// First minor for PTY master devices (mirrors pty.rs).
pub const PTYPX_MINOR: u32 = 192;

/// First minor for PTY slave devices (mirrors pty.rs).
pub const TTYPX_MINOR: u32 = 128;

/// Video minor (handled separately).
pub const VIDEO_MINOR: u32 = 125;

/// Console= boot param length.
const CONS_ARG: usize = 30;

/// TTY input queue size (16-bit entries).
pub const TTY_IN_BYTES: usize = 256;

/// Tab stop distance.
const TAB_SIZE: usize = 8;

/// Mask to compute tab stop position.
const TAB_MASK: usize = 7;

/// Escape character.
const ESC: u8 = b'\x1b';

// Termios constants (CC_* indexes)

/// Number of control characters.
pub const NCCS: usize = 20;

pub const VEOF: usize = 0; // ICANON
pub const VEOL: usize = 1; // ICANON
pub const VEOL2: usize = 2; // ICANON
pub const VERASE: usize = 3; // ICANON
pub const VWERASE: usize = 4; // ICANON
pub const VKILL: usize = 5; // ICANON
pub const VREPRINT: usize = 6; // ICANON
const _SPARE1: usize = 7;
pub const VINTR: usize = 8; // ISIG
pub const VQUIT: usize = 9; // ISIG
pub const VSUSP: usize = 10; // ISIG
pub const VDSUSP: usize = 11; // ISIG
pub const VSTART: usize = 12; // IXON, IXOFF
pub const VSTOP: usize = 13; // IXON, IXOFF
pub const VLNEXT: usize = 14; // IEXTEN
pub const VDISCARD: usize = 15; // IEXTEN
pub const VMIN: usize = 16; // !ICANON
pub const VTIME: usize = 17; // !ICANON
pub const VSTATUS: usize = 18; // ICANON
const _SPARE2: usize = 19;

/// _POSIX_VDISABLE value (0xFF).
pub const POSIX_VDISABLE: u8 = 0xFF;

// Input flags (c_iflag)

pub const IGNBRK: u32 = 0x00000001;
pub const BRKINT: u32 = 0x00000002;
pub const IGNPAR: u32 = 0x00000004;
pub const PARMRK: u32 = 0x00000008;
pub const INPCK: u32 = 0x00000010;
pub const ISTRIP: u32 = 0x00000020;
pub const INLCR: u32 = 0x00000040;
pub const IGNCR: u32 = 0x00000080;
pub const ICRNL: u32 = 0x00000100;
pub const IXON: u32 = 0x00000200;
pub const IXOFF: u32 = 0x00000400;
pub const IXANY: u32 = 0x00000800;
pub const IMAXBEL: u32 = 0x00002000;

// Output flags (c_oflag)

pub const OPOST: u32 = 0x00000001;
pub const ONLCR: u32 = 0x00000002;
pub const OXTABS: u32 = 0x00000004;
pub const ONOEOT: u32 = 0x00000008;
pub const OCRNL: u32 = 0x00000010;
pub const ONOCR: u32 = 0x00000020;
pub const ONLRET: u32 = 0x00000040;

// Control flags (c_cflag)

pub const CIGNORE: u32 = 0x00000001;
pub const CSIZE: u32 = 0x00000300;
pub const CS5: u32 = 0x00000000;
pub const CS6: u32 = 0x00000100;
pub const CS7: u32 = 0x00000200;
pub const CS8: u32 = 0x00000300;
pub const CSTOPB: u32 = 0x00000400;
pub const CREAD: u32 = 0x00000800;
pub const PARENB: u32 = 0x00001000;
pub const PARODD: u32 = 0x00002000;
pub const HUPCL: u32 = 0x00004000;
pub const CLOCAL: u32 = 0x00008000;
pub const CRTSCTS: u32 = 0x00010000;
pub const CDTRCTS: u32 = 0x00020000;
pub const MDMBUF: u32 = 0x00100000;

// Local flags (c_lflag)

pub const ECHOKE: u32 = 0x00000001;
pub const ECHOE: u32 = 0x00000002;
pub const ECHOK: u32 = 0x00000004;
pub const ECHO: u32 = 0x00000008;
pub const ECHONL: u32 = 0x00000010;
pub const ECHOPRT: u32 = 0x00000020;
pub const ECHOCTL: u32 = 0x00000040;
pub const ISIG: u32 = 0x00000080;
pub const ICANON: u32 = 0x00000100;
pub const ALTWERASE: u32 = 0x00000200;
pub const IEXTEN: u32 = 0x00000400;
pub const EXTPROC: u32 = 0x00000800;
pub const TOSTOP: u32 = 0x00400000;
pub const FLUSHO: u32 = 0x00800000;
pub const NOKERNINFO: u32 = 0x02000000;
pub const PENDIN: u32 = 0x20000000;
pub const NOFLSH: u32 = 0x80000000;

// Speed constants

pub const B0: u32 = 0;
pub const B50: u32 = 50;
pub const B75: u32 = 75;
pub const B110: u32 = 110;
pub const B134: u32 = 134;
pub const B150: u32 = 150;
pub const B200: u32 = 200;
pub const B300: u32 = 300;
pub const B600: u32 = 600;
pub const B1200: u32 = 1200;
pub const B1800: u32 = 1800;
pub const B2400: u32 = 2400;
pub const B4800: u32 = 4800;
pub const B9600: u32 = 9600;
pub const B19200: u32 = 19200;
pub const B38400: u32 = 38400;
pub const B7200: u32 = 7200;
pub const B115200: u32 = 115200;

// Default character values

/// CTRL(x) macro: (x & 0x1F)
const fn ctrl(x: u8) -> u8 {
    x & 0x1F
}

const CEOF: u8 = ctrl(b'd');
const CEOL: u8 = 0xFF; // same as _POSIX_VDISABLE
const CERASE: u8 = ctrl(b'h');
const CINTR: u8 = ctrl(b'c');
const CSTATUS: u8 = ctrl(b't');
const CKILL: u8 = ctrl(b'u');
const CMIN: u8 = 1;
const CQUIT: u8 = 0x1C; // FS, ^\
const CSUSP: u8 = ctrl(b'z');
const CTIME: u8 = 0;
const CDSUSP: u8 = ctrl(b'y');
const CSTART: u8 = ctrl(b'q');
const CSTOP: u8 = ctrl(b's');
const CLNEXT: u8 = ctrl(b'v');
const CDISCARD: u8 = ctrl(b'o');
const CWERASE: u8 = ctrl(b'w');
const CREPRINT: u8 = ctrl(b'r');

const TTYDEF_IFLAG: u32 = BRKINT | ICRNL | IMAXBEL | IXON | IXANY;
const TTYDEF_OFLAG: u32 = OPOST | ONLCR | OXTABS;
const TTYDEF_LFLAG: u32 = ECHO | ICANON | ISIG | IEXTEN | ECHOE | ECHOKE | ECHOCTL;
const TTYDEF_CFLAG: u32 = CREAD | CS8 | HUPCL;
const TTYDEF_SPEED: u32 = B115200;

// Signal numbers (local definitions, minimal set)

pub const SIGINT: u8 = 2;
pub const SIGQUIT: u8 = 3;
pub const SIGKILL: u8 = 9;
pub const SIGHUP: u8 = 1;
pub const SIGWINCH: u8 = 28;
pub const SIGINFO: u8 = 29;

// IOCTL codes (for stubs)

pub const TIOCGETA: u32 = 0;
pub const TIOCSETA: u32 = 1;
pub const TIOCSETAW: u32 = 2;
pub const TIOCSETAF: u32 = 3;
pub const TIOCDRAIN: u32 = 4;
pub const TIOCFLUSH: u32 = 5;
pub const TIOCSTART: u32 = 6;
pub const TIOCSTOP: u32 = 7;
pub const TIOCSBRK: u32 = 8;
pub const TIOCCBRK: u32 = 9;
pub const TIOCGWINSZ: u32 = 10;
pub const TIOCSWINSZ: u32 = 11;
pub const TIOCGETD: u32 = 12;
pub const TIOCSETD: u32 = 13;
pub const TIOCGLINED: u32 = 14;
pub const TIOCGQSIZE: u32 = 15;
pub const TIOCSCTTY: u32 = 16;
pub const TIOCGPGRP: u32 = 17;
pub const TIOCSPGRP: u32 = 18;
pub const KIOCBELL: u32 = 19;
pub const KIOCSMAP: u32 = 20;
pub const TIOCSFON: u32 = 21;

// Character driver constants

/// No endpoint / no caller.
pub const NONE: u32 = u32::MAX;

/// Do not reply yet (suspend caller).
pub const EDONTREPLY: i32 = -201;

/// Non-blocking flag.
pub const CDEV_NONBLOCK: i32 = 0x01;
pub const CDEV_NOCTTY: i32 = 0x02;
pub const CDEV_R_BIT: i32 = 0x04;

/// Select operations.
pub const CDEV_OP_RD: u32 = 0x01;
pub const CDEV_OP_WR: u32 = 0x02;
pub const CDEV_OP_ERR: u32 = 0x04;
pub const CDEV_NOTIFY: u32 = 0x08;

/// Returned when a tty is made the controlling tty.
pub const CDEV_CTTY: i32 = 2;

/// Error codes.
pub const OK: i32 = 0;
pub const ENXIO: i32 = -6;
pub const EIO: i32 = -5;
pub const EINVAL: i32 = -22;
pub const EAGAIN: i32 = -11;
pub const EACCES: i32 = -13;
pub const EINTR: i32 = -4;
pub const ENOTTY: i32 = -25;
pub const EBADF: i32 = -9;
pub const BUSY: i32 = -16;
pub const ENOSYS: i32 = -71;

/// FREAD / FWRITE for TIOCFLUSH.
pub const FREAD: i32 = 0x01;
pub const FWRITE: i32 = 0x02;

/// Line discipline name.
const TTYDISC: i32 = 0;
const TTLINEDNAMELEN: usize = 8;
const LINED_NAME: [u8; TTLINEDNAMELEN] = *b"termios\0";

// Input queue flags (stored in upper bits of u16 entries)

/// Low 8 bits are the character itself.
const IN_CHAR: u16 = 0x00FF;
/// Length of char if it has been echoed.
const IN_LEN: u16 = 0x0F00;
/// Shift count for length.
const IN_LSHIFT: usize = 8;
/// Char is a line break (^D, LF).
const IN_EOT: u16 = 0x1000;
/// Char is EOF (^D), do not return to user.
const IN_EOF: u16 = 0x2000;
/// Escaped by LNEXT (^V), no interpretation.
const IN_ESC: u16 = 0x4000;

/// Previous character is not LNEXT (^V).
const NOT_ESCAPED: u8 = 0;
/// Previous character was LNEXT (^V).
const ESCAPED: u8 = 1;

/// No STOP (^S) has been typed to stop output.
const RUNNING: u8 = 0;
/// STOP (^S) has been typed to stop output.
const STOPPED: u8 = 1;

// Device function types

/// Device function returning int (e.g., devread, devwrite).  `try_only` indicates
/// a non-blocking probe.
type DevFun = fn(tp: &mut Tty, try_only: i32) -> i32;

/// Device function taking a character (e.g., echo).
type DevFunArg = fn(tp: &mut Tty, c: i32);

// winsize struct

/// Window size (lines and columns).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct WinSize {
    /// Number of rows.
    pub ws_row: u16,
    /// Number of columns.
    pub ws_col: u16,
    /// Horizontal pixel size (unused).
    pub ws_xpixel: u16,
    /// Vertical pixel size (unused).
    pub ws_ypixel: u16,
}

impl WinSize {
    const fn zeroed() -> Self {
        Self {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}

// termios struct

/// Terminal I/O attributes (POSIX termios).
#[derive(Clone, Copy)]
#[repr(C)]
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
    const fn defaults() -> Self {
        Self {
            c_iflag: TTYDEF_IFLAG,
            c_oflag: TTYDEF_OFLAG,
            c_cflag: TTYDEF_CFLAG,
            c_lflag: TTYDEF_LFLAG,
            c_ispeed: TTYDEF_SPEED as i32,
            c_ospeed: TTYDEF_SPEED as i32,
            c_cc: [
                CEOF,           // VEOF = 0
                CEOL,           // VEOL = 1
                CEOL,           // VEOL2 = 2
                CERASE,         // VERASE = 3
                CWERASE,        // VWERASE = 4
                CKILL,          // VKILL = 5
                CREPRINT,       // VREPRINT = 6
                POSIX_VDISABLE, // spare = 7
                CINTR,          // VINTR = 8
                CQUIT,          // VQUIT = 9
                CSUSP,          // VSUSP = 10
                CDSUSP,         // VDSUSP = 11
                CSTART,         // VSTART = 12
                CSTOP,          // VSTOP = 13
                CLNEXT,         // VLNEXT = 14
                CDISCARD,       // VDISCARD = 15
                CMIN,           // VMIN = 16
                CTIME,          // VTIME = 17
                CSTATUS,        // VSTATUS = 18
                POSIX_VDISABLE, // spare = 19
            ],
        }
    }
}

// TTY per-line structure

/// Per-terminal state structure.
///
/// Mirrors the C `tty_t` struct from `tty.h`.
#[repr(C)]
pub struct Tty {
    /// Set when TTY should inspect this line (events pending).
    pub tty_events: i32,

    /// Index into TTY table.
    pub tty_index: i32,

    /// Device minor number.
    pub tty_minor: u32,

    /// Pointer to place where next char goes (offset into tty_inbuf).
    pub tty_inhead: usize,
    /// Pointer to next char to be given to program (offset into tty_inbuf).
    pub tty_intail: usize,
    /// Number of chars in the input queue.
    pub tty_incount: i32,
    /// Number of "line breaks" (EOT markers) in input queue.
    pub tty_eotct: i32,
    /// Routine to read from low level buffers.
    pub tty_devread: DevFun,
    /// Cancel any device input.
    pub tty_icancel_fn: DevFun,
    /// Minimum requested number of chars in input queue.
    pub tty_min: i32,
    /// Timer for this TTY.
    pub tty_tmr: MinixTimer,

    /// Routine to start actual device output.
    pub tty_devwrite: DevFun,
    /// Routine to echo characters input.
    pub tty_echo_fn: DevFunArg,
    /// Cancel any ongoing device output.
    pub tty_ocancel: DevFun,
    /// Let the device assert a break.
    pub tty_break_on: DevFun,
    /// Let the device de-assert a break.
    pub tty_break_off: DevFun,

    /// Current position on the screen for echoing.
    pub tty_position: i32,
    /// 1 when echoed input messed up, else 0.
    pub tty_reprint: u8,
    /// 1 when LNEXT (^V) just seen, else 0.
    pub tty_escaped: u8,
    /// 1 when STOP (^S) just seen (stops output).
    pub tty_inhibited: u8,
    /// Endpoint of controlling process (pgrp leader).
    pub tty_pgrp: u32,
    /// Count of number of opens of this TTY.
    pub tty_openct: u8,

    /// Process that made the read call, or NONE.
    pub tty_incaller: u32,
    /// ID of suspended read request.
    pub tty_inid: u32,
    /// Grant where read data is to go.
    pub tty_ingrant: u32,
    /// How many chars are still needed.
    pub tty_inleft: usize,
    /// Number of chars input so far.
    pub tty_incum: usize,

    /// Process that made the write call, or NONE.
    pub tty_outcaller: u32,
    /// ID of suspended write request.
    pub tty_outid: u32,
    /// Grant where write data comes from.
    pub tty_outgrant: u32,
    /// Number of chars yet to be output.
    pub tty_outleft: usize,
    /// Number of chars output so far.
    pub tty_outcum: usize,

    /// Process that made the ioctl call, or NONE.
    pub tty_iocaller: u32,
    /// ID of suspended ioctl request.
    pub tty_ioid: u32,
    /// Ioctl request code.
    pub tty_ioreq: u32,
    /// Grant for ioctl buffer.
    pub tty_iogrant: u32,

    /// Which operations are interesting.
    pub tty_select_ops: u32,
    /// Which process wants notification.
    pub tty_select_proc: u32,
    /// Minor used to start select query (for translated minors).
    pub tty_select_minor: u32,

    /// Set line speed, etc. at the device level.
    pub tty_ioctl: DevFun,
    /// Tell the device that the tty is opened.
    pub tty_open: DevFun,
    /// Tell the device that the tty is closed.
    pub tty_close: DevFun,
    /// Pointer to per-device private data.
    pub tty_priv: *mut u8,

    /// Terminal attributes.
    pub tty_termios: Termios,
    /// Window size.
    pub tty_winsize: WinSize,

    /// Input buffer (16-bit entries for flags + char data).
    pub tty_inbuf: [u16; TTY_IN_BYTES],
}

impl Tty {
    /// Create a zeroed Tty with defaults.
    const fn zeroed() -> Self {
        Self {
            tty_events: 0,
            tty_index: 0,
            tty_minor: 0,
            tty_inhead: 0,
            tty_intail: 0,
            tty_incount: 0,
            tty_eotct: 0,
            tty_devread: tty_devnop,
            tty_icancel_fn: tty_devnop,
            tty_min: 1,
            tty_tmr: MinixTimer {
                tmr_next: core::ptr::null_mut(),
                tmr_exp_time: 0,
                tmr_func: 0,
                tmr_arg: 0,
            },
            tty_devwrite: tty_devnop,
            tty_echo_fn: tty_echo_dummy,
            tty_ocancel: tty_devnop,
            tty_break_on: tty_devnop,
            tty_break_off: tty_devnop,
            tty_position: 0,
            tty_reprint: 0,
            tty_escaped: 0,
            tty_inhibited: 0,
            tty_pgrp: 0,
            tty_openct: 0,
            tty_incaller: NONE,
            tty_inid: 0,
            tty_ingrant: 0,
            tty_inleft: 0,
            tty_incum: 0,
            tty_outcaller: NONE,
            tty_outid: 0,
            tty_outgrant: 0,
            tty_outleft: 0,
            tty_outcum: 0,
            tty_iocaller: NONE,
            tty_ioid: 0,
            tty_ioreq: 0,
            tty_iogrant: 0,
            tty_select_ops: 0,
            tty_select_proc: 0,
            tty_select_minor: 0,
            tty_ioctl: tty_devnop,
            tty_open: tty_devnop,
            tty_close: tty_devnop,
            tty_priv: ptr::null_mut(),
            tty_termios: Termios::defaults(),
            tty_winsize: WinSize::zeroed(),
            tty_inbuf: [0u16; TTY_IN_BYTES],
        }
    }
}

// Static TTY table

/// The global TTY table — one entry per terminal line.
///
/// Safety: All access to the TTY table must be externally synchronized.
/// The TTY server is single-threaded in MINIX; `UnsafeCell` provides
/// interior mutability for the static.
struct TtyTableRaw(UnsafeCell<[Tty; NR_CONS + NR_RS_LINES]>);
unsafe impl Sync for TtyTableRaw {}

impl TtyTableRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const { Tty::zeroed() }; NR_CONS + NR_RS_LINES],
        ))
    }

    /// Get a raw pointer to the table.
    fn as_ptr(&self) -> *mut Tty {
        self.0.get() as *mut Tty
    }
}

static TTY_TABLE: TtyTableRaw = TtyTableRaw::new();

/// The currently active console line (minor number).
static CONSOLE_LINE: AtomicU32 = AtomicU32::new(CONS_MINOR);

/// System clock frequency (Hz).
static SYSTEM_HZ: AtomicU32 = AtomicU32::new(0);

/// Currently visible console index.
static CCURRENT: AtomicU32 = AtomicU32::new(0);

// Helper functions

/// Return a pointer to a TTY entry by its index in the table.
#[inline]
fn tty_addr(idx: usize) -> *mut Tty {
    unsafe { TTY_TABLE.as_ptr().add(idx) }
}

/// Returns `true` if the TTY line is active (has a devread function).
#[inline]
fn tty_active(tp: &Tty) -> bool {
    tp.tty_devread as usize != tty_devnop as *const () as usize
}

/// Returns `true` if the TTY is a console line.
fn isconsole(tp: &Tty) -> bool {
    (tp.tty_minor as usize) < NR_CONS
}

// Buffer helper functions

/// Compute the length (number of elements) of a slice-like buffer.
fn buflen<T>(_buf: &[T]) -> usize {
    _buf.len()
}

/// Return a pointer past the end of a fixed-size array.
fn bufend<T>(buf: &[T]) -> usize {
    buf.len()
}

// Default device functions (no-ops)

/// No-op device function.
fn tty_devnop(_tp: &mut Tty, _try_only: i32) -> i32 {
    0
}

/// Dummy echo function (no-op).
fn tty_echo_dummy(_tp: &mut Tty, _c: i32) {}

// line2tty — minor number to TTY line mapping

/// Convert a minor device number to a `&mut Tty`.
///
/// Console minors 0..NR_CONS-1 map directly.  The log minor (15) redirects to
/// the console line.  RS-232 minors start at RS232_MINOR (16).  Returns `None`
/// for inactive lines (no devread function) or the video minor.
pub fn line2tty(minor: u32) -> Option<&'static mut Tty> {
    let mut line = minor;

    // /dev/log goes to /dev/console, and both may be redirected.
    if line == CONS_MINOR || line == LOG_MINOR {
        line = CONSOLE_LINE.load(Ordering::Relaxed);
    }

    if line == VIDEO_MINOR {
        return None;
    }

    let tp: *mut Tty;
    if (line.wrapping_sub(CONS_MINOR)) < NR_CONS as u32 {
        tp = tty_addr(line.wrapping_sub(CONS_MINOR) as usize);
    } else if (line.wrapping_sub(RS232_MINOR)) < NR_RS_LINES as u32 {
        tp = tty_addr(line.wrapping_sub(RS232_MINOR) as usize + NR_CONS);
    } else {
        tp = ptr::null_mut();
    }

    if tp.is_null() {
        return None;
    }

    // Safety: We have exclusive access in the single-threaded TTY server.
    let tp_ref = unsafe { &mut *tp };
    if !tty_active(tp_ref) {
        return None;
    }

    Some(tp_ref)
}

// in_process — input processing pipeline

/// Process input characters through the line discipline.
///
/// Characters are processed according to the termios settings:
/// - ISTRIP: strip to 7 bits
/// - IEXTEN: LNEXT (^V) escape, REPRINT (^R)
/// - IGNCR/ICRNL/INLCR: CR/LF mapping
/// - ICANON: VERASE, VKILL, VEOF, VEOL
/// - IXON: VSTOP/VSTART flow control
/// - ISIG: VINTR/VQUIT signal generation
/// - Echo (via tty_echo_fn)
///
/// Returns the number of characters processed.
pub fn in_process(tp: &mut Tty, buf: &[u8]) -> usize {
    let mut timeset = false;
    let mut processed = 0usize;

    for &raw_ch in buf {
        let mut ch = raw_ch as i32;

        // Strip to seven bits?
        if tp.tty_termios.c_iflag & ISTRIP != 0 {
            ch &= 0x7F;
        }

        // Input extensions?
        if tp.tty_termios.c_lflag & IEXTEN != 0 {
            // Previous character was a character escape?
            if tp.tty_escaped != 0 {
                tp.tty_escaped = NOT_ESCAPED;
                ch |= IN_ESC as i32;
            }

            // LNEXT (^V) to escape the next character?
            if ch as u8 == tp.tty_termios.c_cc[VLNEXT] {
                tp.tty_escaped = ESCAPED;
                rawecho(tp, b'^' as i32);
                rawecho(tp, b'\x08' as i32);
                processed += 1;
                continue; // do not store the escape
            }

            // REPRINT (^R) to reprint echoed characters?
            if ch as u8 == tp.tty_termios.c_cc[VREPRINT] {
                reprint(tp);
                processed += 1;
                continue;
            }
        }

        // _POSIX_VDISABLE is a normal character value, so escape it.
        if ch as u8 == POSIX_VDISABLE {
            ch |= IN_ESC as i32;
        }

        // Map CR to LF, ignore CR, or map LF to CR.
        if ch == b'\r' as i32 {
            if tp.tty_termios.c_iflag & IGNCR != 0 {
                processed += 1;
                continue;
            }
            if tp.tty_termios.c_iflag & ICRNL != 0 {
                ch = b'\n' as i32;
            }
        } else if ch == b'\n' as i32 && tp.tty_termios.c_iflag & INLCR != 0 {
            ch = b'\r' as i32;
        }

        // Canonical mode?
        if tp.tty_termios.c_lflag & ICANON != 0 {
            // Erase processing (rub out of last character).
            if ch as u8 == tp.tty_termios.c_cc[VERASE] {
                back_over(tp);
                if tp.tty_termios.c_lflag & ECHOE == 0 {
                    tty_echo(tp, ch);
                }
                processed += 1;
                continue;
            }

            // Kill processing (remove current line).
            if ch as u8 == tp.tty_termios.c_cc[VKILL] {
                while back_over(tp) > 0 {}
                if tp.tty_termios.c_lflag & ECHOE == 0 {
                    tty_echo(tp, ch);
                    if tp.tty_termios.c_lflag & ECHOK != 0 {
                        rawecho(tp, b'\n' as i32);
                    }
                }
                processed += 1;
                continue;
            }

            // EOF (^D) means end-of-file, an invisible "line break".
            if ch as u8 == tp.tty_termios.c_cc[VEOF] {
                ch |= (IN_EOT | IN_EOF) as i32;
            }

            // The line may be returned to the user after an LF.
            if ch == b'\n' as i32 {
                ch |= IN_EOT as i32;
            }

            // Same thing with EOL, whatever it may be.
            if ch as u8 == tp.tty_termios.c_cc[VEOL] {
                ch |= IN_EOT as i32;
            }
        }

        // Start/stop input control?
        if tp.tty_termios.c_iflag & IXON != 0 {
            // Output stops on STOP (^S).
            if ch as u8 == tp.tty_termios.c_cc[VSTOP] {
                tp.tty_inhibited = STOPPED;
                tp.tty_events = 1;
                processed += 1;
                continue;
            }

            // Output restarts on START (^Q) or any character if IXANY.
            if tp.tty_inhibited != 0
                && (ch as u8 == tp.tty_termios.c_cc[VSTART] || tp.tty_termios.c_iflag & IXANY != 0)
            {
                tp.tty_inhibited = RUNNING;
                tp.tty_events = 1;
                if ch as u8 == tp.tty_termios.c_cc[VSTART] {
                    processed += 1;
                    continue;
                }
            }
        }

        if tp.tty_termios.c_lflag & ISIG != 0 {
            // Check for INTR, QUIT and STATUS characters.
            let mut sig: i32 = -1;
            if ch as u8 == tp.tty_termios.c_cc[VINTR] {
                sig = SIGINT as i32;
            } else if ch as u8 == tp.tty_termios.c_cc[VQUIT] {
                sig = SIGQUIT as i32;
            } else if ch as u8 == tp.tty_termios.c_cc[VSTATUS] {
                sig = SIGINFO as i32;
            }

            if sig >= 0 {
                sigchar(tp, sig as u8, true);
                tty_echo(tp, ch);
                processed += 1;
                continue;
            }
        }

        // Is there space in the input buffer?
        if tp.tty_incount as usize == tp.tty_inbuf.len() {
            // No space; discard in canonical mode, keep in raw mode.
            if tp.tty_termios.c_lflag & ICANON != 0 {
                processed += 1;
                continue;
            }
            // Raw mode: stop storing but still treat partial processing as
            // a consumer of this byte.
            break;
        }

        if tp.tty_termios.c_lflag & ICANON == 0 {
            // In raw mode all characters are "line breaks".
            ch |= IN_EOT as i32;

            // Start an inter-byte timer?
            if !timeset && tp.tty_termios.c_cc[VMIN] > 0 && tp.tty_termios.c_cc[VTIME] > 0 {
                settimer(tp, true);
                timeset = true;
            }
        }

        // Perform the intricate function of echoing.
        if tp.tty_termios.c_lflag & (ECHO | ECHONL) != 0 {
            ch = tty_echo(tp, ch);
        }

        // Save the character in the input queue.
        let ch_u16 = ch as u16;
        tp.tty_inbuf[tp.tty_inhead] = ch_u16;
        let qlen = tp.tty_inbuf.len();
        tp.tty_inhead = if tp.tty_inhead + 1 >= qlen {
            0
        } else {
            tp.tty_inhead + 1
        };
        tp.tty_incount += 1;
        if ch_u16 & IN_EOT != 0 {
            tp.tty_eotct += 1;
        }

        // Try to finish input if the queue threatens to overflow.
        if tp.tty_incount as usize == tp.tty_inbuf.len() {
            in_transfer(tp);
        }

        processed += 1;
    }

    processed
}

// tty_echo — echo a character with processing

/// Echo a character if echoing is on.
///
/// Returns the character with the echoed length added to its attributes.
fn tty_echo(tp: &mut Tty, mut ch: i32) -> i32 {
    ch &= !(IN_LEN as i32);

    if tp.tty_termios.c_lflag & ECHO == 0 {
        // Only echo NL in canonical mode with ECHONL.
        if ch == (b'\n' as i32 | IN_EOT as i32)
            && tp.tty_termios.c_lflag & (ICANON | ECHONL) == (ICANON | ECHONL)
        {
            (tp.tty_echo_fn)(tp, b'\n' as i32);
        }
        return ch;
    }

    // "Reprint" tells if the echo output has been messed up by other output.
    let rp = if tp.tty_incount == 0 {
        false
    } else {
        tp.tty_reprint != 0
    };

    let mut len: i32;
    let char_part = (ch as u16 & IN_CHAR) as u8;

    if char_part < b' ' {
        match ch as u16 & (IN_ESC | IN_EOF | IN_EOT | IN_CHAR) {
            v if v == b'\t' as u16 => {
                len = 0;
                loop {
                    (tp.tty_echo_fn)(tp, b' ' as i32);
                    len += 1;
                    if len >= TAB_SIZE as i32 || (tp.tty_position as usize & TAB_MASK) == 0 {
                        break;
                    }
                }
            }
            v if v == (b'\r' as u16 | IN_EOT) || v == (b'\n' as u16 | IN_EOT) => {
                (tp.tty_echo_fn)(tp, ch & IN_CHAR as i32);
                len = 0;
            }
            _ => {
                (tp.tty_echo_fn)(tp, b'^' as i32);
                (tp.tty_echo_fn)(tp, (b'@' + char_part) as i32);
                len = 2;
            }
        }
    } else if char_part == 0x7F {
        // DEL prints as "^?".
        (tp.tty_echo_fn)(tp, b'^' as i32);
        (tp.tty_echo_fn)(tp, b'?' as i32);
        len = 2;
    } else {
        (tp.tty_echo_fn)(tp, char_part as i32);
        len = 1;
    }

    if ch as u16 & IN_EOF != 0 {
        let mut remaining = len;
        while remaining > 0 {
            (tp.tty_echo_fn)(tp, b'\x08' as i32);
            remaining -= 1;
        }
    }

    tp.tty_reprint = rp as u8;
    ch | (len << IN_LSHIFT)
}

// rawecho — echo without interpretation

/// Echo without interpretation if ECHO is set.
fn rawecho(tp: &mut Tty, ch: i32) {
    let rp = tp.tty_reprint;
    if tp.tty_termios.c_lflag & ECHO != 0 {
        (tp.tty_echo_fn)(tp, ch);
    }
    tp.tty_reprint = rp;
}

// back_over — backspace and erase previous character

/// Backspace to previous character on screen and erase it.
///
/// Returns 1 if a character was erased, 0 if the queue was empty.
fn back_over(tp: &mut Tty) -> i32 {
    if tp.tty_incount == 0 {
        return 0; // queue empty
    }

    // Find the previous head position.
    let qlen = tp.tty_inbuf.len();
    let head = if tp.tty_inhead == 0 {
        qlen - 1
    } else {
        tp.tty_inhead - 1
    };

    if tp.tty_inbuf[head] & IN_EOT != 0 {
        return 0; // can't erase "line breaks"
    }

    if tp.tty_reprint != 0 {
        reprint(tp); // reprint if messed up
    }

    tp.tty_inhead = head;
    tp.tty_incount -= 1;

    if tp.tty_termios.c_lflag & ECHOE != 0 {
        let len = (tp.tty_inbuf[head] & IN_LEN) >> IN_LSHIFT;
        let mut remaining = len as i32;
        while remaining > 0 {
            rawecho(tp, b'\x08' as i32);
            rawecho(tp, b' ' as i32);
            rawecho(tp, b'\x08' as i32);
            remaining -= 1;
        }
    }

    1 // one character erased
}

// reprint — restore echoed characters on screen

/// Restore what has been echoed to screen if the user input has been
/// messed up by output, or if REPRINT (^R) is typed.
fn reprint(tp: &mut Tty) {
    tp.tty_reprint = 0;

    // Find the last line break in the input.
    let mut head = tp.tty_inhead;
    let mut count = tp.tty_incount;

    while count > 0 {
        let idx = if head == 0 {
            tp.tty_inbuf.len() - 1
        } else {
            head - 1
        };
        if tp.tty_inbuf[idx] & IN_EOT != 0 {
            break;
        }
        head = idx;
        count -= 1;
    }

    if count == tp.tty_incount {
        return; // no reason to reprint
    }

    // Show REPRINT (^R) and move to a new line.
    tty_echo(tp, tp.tty_termios.c_cc[VREPRINT] as i32 | IN_ESC as i32);
    rawecho(tp, b'\r' as i32);
    rawecho(tp, b'\n' as i32);

    // Reprint from the last break onwards.
    let qlen = tp.tty_inbuf.len();
    while count < tp.tty_incount {
        let val = tty_echo(tp, tp.tty_inbuf[head] as i32);
        tp.tty_inbuf[head] = val as u16;
        head += 1;
        if head >= qlen {
            head = 0;
        }
        count += 1;
    }
}

// out_process — output processing

/// Perform output processing on a circular buffer.
///
/// `icount` is the number of bytes to process on input, updated to the number
/// of bytes actually consumed on output.  `ocount` is the space available on
/// input and the space used on output.
///
/// Buffer parameters: `bstart` is the start of the circular buffer, `bpos` is
/// the current position, `bend` is one past the end.  The buffer is modified
/// in-place (tab expansion, CR+LF insertion).
pub fn out_process(
    tp: &mut Tty,
    buf: &mut [u8],
    offset: &mut usize,
    icount: &mut usize,
    ocount: &mut usize,
) {
    let mut ict = *icount;
    let mut oct = *ocount;
    let mut pos = tp.tty_position as usize;
    let buf_len = buf.len();
    let mut off = *offset;

    while ict > 0 {
        let ch = buf[off % buf_len];

        match ch {
            0x07 => {
                // BEL — no position change
            }
            0x08 => {
                // Backspace
                pos = pos.saturating_sub(1);
            }
            b'\r' => {
                pos = 0;
            }
            b'\n' => {
                if tp.tty_termios.c_oflag & (OPOST | ONLCR) == (OPOST | ONLCR) {
                    // Map LF to CR+LF if there is space.
                    if oct >= 2 {
                        let cr_idx = off % buf_len;
                        buf[cr_idx] = b'\r';
                        off += 1;
                        let lf_idx = off % buf_len;
                        buf[lf_idx] = b'\n';
                        pos = 0;
                        ict -= 1;
                        oct -= 2;
                    }
                    tp.tty_position = (pos & TAB_MASK) as i32;
                    *offset = off;
                    *icount -= ict;
                    *ocount -= oct;
                    return;
                }
            }
            b'\t' => {
                // Best guess for the tab length.
                let tablen = TAB_SIZE - (pos & TAB_MASK);

                if tp.tty_termios.c_oflag & (OPOST | OXTABS) == (OPOST | OXTABS) {
                    // Tabs must be expanded.
                    if oct >= tablen {
                        pos += tablen;
                        ict -= 1;
                        oct -= tablen;
                        let mut remaining_tab = tablen;
                        while remaining_tab > 0 {
                            let idx = off % buf_len;
                            buf[idx] = b' ';
                            off += 1;
                            remaining_tab -= 1;
                        }
                    }
                    tp.tty_position = (pos & TAB_MASK) as i32;
                    *offset = off;
                    *icount -= ict;
                    *ocount -= oct;
                    return;
                }
                // Tabs are output directly.
                pos += tablen;
            }
            _ => {
                // Assume any other character prints as one character.
                pos += 1;
            }
        }

        off += 1;
        ict -= 1;
        oct -= 1;
    }

    tp.tty_position = (pos & TAB_MASK) as i32;
    *offset = off;
    *icount -= ict;
    *ocount -= oct;
}

// sigchar — signal delivery to process group

/// Send a signal to the foreground process group of this TTY.
///
/// `sig` is the signal number (SIGINT, SIGQUIT, SIGHUP, etc.).
/// `may_flush` controls whether input/output is flushed.
pub fn sigchar(tp: &mut Tty, _sig: u8, may_flush: bool) {
    if tp.tty_pgrp != 0 {
        // TODO: Phase 13 — call sys_kill(tp->tty_pgrp, sig)
        // For now, this is a stub.
        // sys_kill(tp.tty_pgrp, sig);
    }

    if may_flush && tp.tty_termios.c_lflag & NOFLSH == 0 {
        // Kill earlier input.
        tp.tty_incount = 0;
        tp.tty_eotct = 0;
        tp.tty_intail = tp.tty_inhead;

        // Kill all output.
        (tp.tty_ocancel)(tp, 0);

        tp.tty_inhibited = RUNNING;
        tp.tty_events = 1;
    }
}

// in_transfer — transfer from input queue to reader

/// Transfer bytes from the input queue to a process reading from a terminal.
///
/// Copies characters from the circular input queue into a temporary buffer and
/// (in the real implementation) uses `sys_safecopyto` to deliver them to the
/// reader's grant.  Here we only advance the queue state.
pub fn in_transfer(tp: &mut Tty) {
    // Force read to succeed if the line is hung up.
    if tp.tty_termios.c_ospeed as u32 == B0 {
        tp.tty_min = 0;
    }

    // Anything to do?
    if tp.tty_inleft == 0 || tp.tty_eotct < tp.tty_min {
        return;
    }

    // Temp buffer for batching transfers.
    let mut buf = [0u8; 64];
    let mut bp = 0usize;

    while tp.tty_inleft > 0 && tp.tty_eotct > 0 {
        let ch = tp.tty_inbuf[tp.tty_intail];

        if ch & IN_EOF == 0 {
            // One character to be delivered to the user.
            buf[bp] = (ch & IN_CHAR) as u8;
            tp.tty_inleft -= 1;
            bp += 1;
            if bp >= buf.len() {
                // Temp buffer full, copy to user space.
                // TODO: Phase 13 — sys_safecopyto(tp.tty_incaller, tp.tty_ingrant,
                //       tp.tty_incum, buf.as_ptr(), buf.len())
                tp.tty_incum += buf.len();
                bp = 0;
            }
        }

        // Remove the character from the input queue.
        let qlen = tp.tty_inbuf.len();
        tp.tty_intail = if tp.tty_intail + 1 >= qlen {
            0
        } else {
            tp.tty_intail + 1
        };
        tp.tty_incount -= 1;

        if ch & IN_EOT != 0 {
            tp.tty_eotct -= 1;
            // Don't read past a line break in canonical mode.
            if tp.tty_termios.c_lflag & ICANON != 0 {
                tp.tty_inleft = 0;
            }
        }
    }

    if bp > 0 {
        // Leftover characters in the buffer.
        // TODO: Phase 13 — sys_safecopyto(tp.tty_incaller, tp.tty_ingrant,
        //       tp.tty_incum, buf.as_ptr(), bp)
        tp.tty_incum += bp;
    }

    // Usually reply to the reader, possibly even if incum == 0 (EOF).
    if tp.tty_inleft == 0 {
        // TODO: Phase 13 — chardriver_reply_task(tp.tty_incaller, tp.tty_inid,
        //       tp.tty_incum as i32)
        tp.tty_inleft = 0;
        tp.tty_incum = 0;
        tp.tty_incaller = NONE;
    }
}

// handle_events — event loop for a TTY line

/// Handle any events pending on a TTY line.
///
/// Calls device read/write handlers, processes ioctls, and transfers data
/// to waiting readers.
pub fn handle_events(tp: &mut Tty) {
    loop {
        tp.tty_events = 0;

        // Read input and perform input processing.
        (tp.tty_devread)(tp, 0);

        // Perform output processing and write output.
        (tp.tty_devwrite)(tp, 0);

        // Ioctl waiting for some event?
        if tp.tty_ioreq != 0 {
            dev_ioctl(tp);
        }

        if tp.tty_events == 0 {
            break;
        }
    }

    // Transfer characters from the input queue to a waiting process.
    in_transfer(tp);

    // Reply if enough bytes are available.
    if tp.tty_incum >= tp.tty_min as usize && tp.tty_inleft > 0 {
        // TODO: Phase 13 — chardriver_reply_task(tp.tty_incaller, tp.tty_inid,
        //       tp.tty_incum as i32)
        tp.tty_inleft = 0;
        tp.tty_incum = 0;
        tp.tty_incaller = NONE;
    }

    if tp.tty_select_ops != 0 {
        select_retry(tp);
    }
}

// dev_ioctl — execute deferred ioctl

/// Execute an ioctl that was deferred pending output drain.
fn dev_ioctl(tp: &mut Tty) {
    if tp.tty_outleft > 0 {
        return; // output not finished
    }

    if tp.tty_ioreq != TIOCDRAIN {
        if tp.tty_ioreq == TIOCSETAF {
            tty_icancel(tp);
        }
        // TODO: Phase 13 — sys_safecopyfrom(tp.tty_iocaller, tp.tty_iogrant, 0,
        //       &tp.tty_termios, size_of::<Termios>())
        setattr(tp);
    }

    tp.tty_ioreq = 0;
    // TODO: Phase 13 — chardriver_reply_task(tp.tty_iocaller, tp.tty_ioid, OK)
    tp.tty_iocaller = NONE;
}

// setattr — apply new terminal attributes

/// Apply new line attributes (raw/canonical, line speed, etc.).
fn setattr(tp: &mut Tty) {
    if tp.tty_termios.c_lflag & ICANON == 0 {
        // Raw mode: put a "line break" on all characters in the input queue.
        tp.tty_eotct = tp.tty_incount;
        let qlen = tp.tty_inbuf.len();
        let mut idx = tp.tty_intail;
        let mut count = tp.tty_incount;
        while count > 0 {
            tp.tty_inbuf[idx] |= IN_EOT;
            idx = if idx + 1 >= qlen { 0 } else { idx + 1 };
            count -= 1;
        }
    }

    // Inspect MIN and TIME.
    settimer(tp, false);
    if tp.tty_termios.c_lflag & ICANON != 0 {
        // No MIN & TIME in canonical mode.
        tp.tty_min = 1;
    } else {
        // In raw mode MIN is the number of chars wanted, and TIME how long
        // to wait for them.
        tp.tty_min = tp.tty_termios.c_cc[VMIN] as i32;
        if tp.tty_min == 0 && tp.tty_termios.c_cc[VTIME] > 0 {
            tp.tty_min = 1;
        }
    }

    if tp.tty_termios.c_iflag & IXON == 0 {
        // No start/stop output control, so don't leave output inhibited.
        tp.tty_inhibited = RUNNING;
        tp.tty_events = 1;
    }

    // Setting the output speed to zero hangs up the phone.
    if tp.tty_termios.c_ospeed as u32 == B0 {
        sigchar(tp, SIGHUP, true);
    }

    // Set new line speed, character size, etc. at the device level.
    (tp.tty_ioctl)(tp, 0);
}

// tty_icancel — discard all pending input

/// Discard all pending input, both in the TTY buffer and in the device.
fn tty_icancel(tp: &mut Tty) {
    tp.tty_incount = 0;
    tp.tty_eotct = 0;
    tp.tty_intail = tp.tty_inhead;
    (tp.tty_icancel_fn)(tp, 0);
}

// select_try / select_retry

/// Test which select operations would not block.
fn select_try(tp: &mut Tty, ops: u32) -> u32 {
    let mut ready_ops = 0u32;

    // Special case: if line is hung up, no operations will block.
    if tp.tty_termios.c_ospeed as u32 == B0 {
        ready_ops |= ops;
    }

    if ops & CDEV_OP_RD != 0 {
        // Will I/O not block on read?
        if tp.tty_inleft > 0 {
            ready_ops |= CDEV_OP_RD; // EIO - no blocking
        } else if tp.tty_incount > 0 {
            // Is a regular read possible?
            if tp.tty_termios.c_lflag & ICANON == 0 || tp.tty_eotct > 0 {
                ready_ops |= CDEV_OP_RD;
            }
        }
    }

    if ops & CDEV_OP_WR != 0 && (tp.tty_outleft > 0 || (tp.tty_devwrite)(tp, 1) != 0) {
        ready_ops |= CDEV_OP_WR;
    }

    ready_ops
}

/// Retry select notification.
fn select_retry(tp: &mut Tty) -> i32 {
    if tp.tty_select_ops != 0 {
        let ops = select_try(tp, tp.tty_select_ops);
        if ops != 0 {
            // TODO: Phase 13 — chardriver_reply_select(tp.tty_select_proc,
            //       tp.tty_select_minor, ops)
            tp.tty_select_ops &= !ops;
        }
    }
    OK
}

// Timer functions

/// Timer callback — called when a TTY timer expires.
///
/// Sets `tty_min = 0` to force the read to succeed and sets `tty_events` to
/// trigger event processing.
fn tty_timed_out(tp: *mut MinixTimer) {
    // Safety: We trust the timer subsystem passes a valid pointer.
    // The timer arg stores the tty_index.
    unsafe {
        let tmr_arg_val = (*tp).tmr_arg;
        let tty_idx = tmr_arg_val as i32;
        let tty_ptr = tty_addr(tty_idx as usize);
        (*tty_ptr).tty_min = 0;
        (*tty_ptr).tty_events = 1;
    }
}

/// Set or cancel the inter-byte / read timer for a TTY line.
fn settimer(tp: &mut Tty, enable: bool) {
    if enable {
        let system_hz = SYSTEM_HZ.load(Ordering::Relaxed);
        let ticks = tp.tty_termios.c_cc[VTIME] as u64 * (system_hz as u64 / 10);

        if ticks > 0 {
            // TODO: Phase 13 — use timer infrastructure to set a timer
            // set_timer(&tp.tty_tmr, ticks, tty_timed_out, tp.tty_index);
        }
    } else {
        // TODO: Phase 13 — cancel_timer(&tp.tty_tmr);
    }
}

// tty_init — TTY table initialization

/// Initialize the TTY structure and call device initialization routines.
///
/// Must be called once at startup before any TTY operations are performed.
pub fn tty_init(system_hz_val: u32) {
    SYSTEM_HZ.store(system_hz_val, Ordering::Relaxed);

    // Safety: We have exclusive access during initialization.
    let table: &mut [Tty; NR_CONS + NR_RS_LINES] =
        unsafe { &mut *TTY_TABLE.as_ptr().cast::<[Tty; NR_CONS + NR_RS_LINES]>() };

    for (s, tp) in table.iter_mut().enumerate() {
        tp.tty_index = s as i32;
        tp.tty_intail = 0;
        tp.tty_inhead = 0;
        tp.tty_min = 1;
        tp.tty_incaller = NONE;
        tp.tty_outcaller = NONE;
        tp.tty_iocaller = NONE;
        tp.tty_termios = Termios::defaults();

        // Set up default device function pointers.
        tp.tty_icancel_fn = tty_devnop;
        tp.tty_ocancel = tty_devnop;
        tp.tty_ioctl = tty_devnop;
        tp.tty_close = tty_devnop;
        tp.tty_open = tty_devnop;

        if s < NR_CONS {
            // Console lines: scr_init and kb_init will be called by
            // the console/keyboard drivers.
            tp.tty_minor = CONS_MINOR + s as u32;
        } else {
            // RS-232 lines: rs_init will be called by the serial driver.
            tp.tty_minor = RS232_MINOR + s as u32 - NR_CONS as u32;
        }
    }
}

// Character driver interface stubs
//
// These depend on the chardriver framework (Phase 13).
// They are provided as stubs matching the original C signatures.

// Placeholder types for chardriver framework (not yet defined).
type DevMinor = u32;
type Endpoint = i32;
type CpGrantId = u32;
type CDevId = u32;
type Position = u64;

/// do_open — open a TTY line.
///
/// Makes the TTY the caller's controlling TTY unless NOCTTY is set or the
/// device is the log device.
pub fn do_open(minor: DevMinor, access: i32, user_endpt: Endpoint) -> i32 {
    let tp = match line2tty(minor) {
        Some(tp) => tp,
        None => return ENXIO,
    };

    let is_log = minor == LOG_MINOR;
    if is_log && {
        // Check if console line via line2tty logic
        let line = CONSOLE_LINE.load(Ordering::Relaxed);
        (line as usize) < NR_CONS
    } {
        // The log device is a write-only diagnostics device.
        if access & CDEV_R_BIT != 0 {
            return EACCES;
        }
    } else {
        if access & CDEV_NOCTTY == 0 {
            tp.tty_pgrp = user_endpt as u32;
            tp.tty_openct += 1;
            if tp.tty_openct == 1 {
                // Tell the device that the tty is opened.
                (tp.tty_open)(tp, 0);
            }
            return CDEV_CTTY;
        }
        tp.tty_openct += 1;
        if tp.tty_openct == 1 {
            (tp.tty_open)(tp, 0);
        }
    }

    OK
}

/// do_close — close a TTY line.
pub fn do_close(minor: DevMinor) -> i32 {
    let tp = match line2tty(minor) {
        Some(tp) => tp,
        None => return ENXIO,
    };

    let is_log = minor == LOG_MINOR;
    if (is_log && (CONSOLE_LINE.load(Ordering::Relaxed) as usize) < NR_CONS) || tp.tty_openct == 0 {
        return OK;
    }

    tp.tty_openct -= 1;
    if tp.tty_openct == 0 {
        tp.tty_pgrp = 0;
        tty_icancel(tp);
        (tp.tty_ocancel)(tp, 0);
        (tp.tty_close)(tp, 0);
        tp.tty_termios = Termios::defaults();
        tp.tty_winsize = WinSize::zeroed();
        setattr(tp);
    }

    OK
}

/// do_read — read from a TTY line.
pub fn do_read(
    minor: DevMinor,
    _position: Position,
    endpt: Endpoint,
    grant: CpGrantId,
    size: usize,
    flags: i32,
    id: CDevId,
) -> i32 {
    let tp = match line2tty(minor) {
        Some(tp) => tp,
        None => return ENXIO,
    };

    // Check if there is already a process hanging in a read.
    if tp.tty_incaller != NONE || tp.tty_inleft > 0 {
        return EIO;
    }
    if size == 0 {
        return EINVAL;
    }

    // Copy information from the message to the tty struct.
    tp.tty_incaller = endpt as u32;
    tp.tty_inid = id;
    tp.tty_ingrant = grant;
    tp.tty_inleft = size;

    if tp.tty_termios.c_lflag & ICANON == 0 && tp.tty_termios.c_cc[VTIME] > 0 {
        if tp.tty_termios.c_cc[VMIN] == 0 {
            // MIN & TIME specify a read timer.
            settimer(tp, true);
            tp.tty_min = 1;
        } else {
            // MIN & TIME specify an inter-byte timer.
            if tp.tty_eotct == 0 {
                settimer(tp, false);
                tp.tty_min = tp.tty_termios.c_cc[VMIN] as i32;
            }
        }
    }

    // Anything waiting in the input buffer? Clear it out...
    in_transfer(tp);
    // ...then go back for more.
    handle_events(tp);
    if tp.tty_inleft == 0 {
        return EDONTREPLY; // already done
    }

    // There were no bytes in the input queue available.
    if flags & CDEV_NONBLOCK != 0 {
        tty_icancel(tp);
        let r = if tp.tty_incum > 0 {
            tp.tty_incum as i32
        } else {
            EAGAIN
        };
        tp.tty_inleft = 0;
        tp.tty_incum = 0;
        tp.tty_incaller = NONE;
        return r;
    }

    if tp.tty_select_ops != 0 {
        select_retry(tp);
    }

    EDONTREPLY // suspend the caller
}

/// do_write — write to a TTY line.
pub fn do_write(
    minor: DevMinor,
    _position: Position,
    endpt: Endpoint,
    grant: CpGrantId,
    size: usize,
    flags: i32,
    id: CDevId,
) -> i32 {
    let tp = match line2tty(minor) {
        Some(tp) => tp,
        None => return ENXIO,
    };

    // Check if there is already a process hanging in a write.
    if tp.tty_outcaller != NONE || tp.tty_outleft > 0 {
        return EIO;
    }
    if size == 0 {
        return EINVAL;
    }

    // Copy message parameters to the tty structure.
    tp.tty_outcaller = endpt as u32;
    tp.tty_outid = id;
    tp.tty_outgrant = grant;
    tp.tty_outleft = size;

    // Try to write.
    handle_events(tp);
    if tp.tty_outleft == 0 {
        return EDONTREPLY; // already done
    }

    // None or not all the bytes could be written.
    if flags & CDEV_NONBLOCK != 0 {
        let r = if tp.tty_outcum > 0 {
            tp.tty_outcum as i32
        } else {
            EAGAIN
        };
        tp.tty_outleft = 0;
        tp.tty_outcum = 0;
        tp.tty_outcaller = NONE;
        return r;
    }

    if tp.tty_select_ops != 0 {
        select_retry(tp);
    }

    EDONTREPLY // suspend the caller
}

/// do_ioctl — perform an IOCTL on a TTY line.
pub fn do_ioctl(
    minor: DevMinor,
    request: u32,
    endpt: Endpoint,
    grant: CpGrantId,
    flags: i32,
    _user_endpt: Endpoint,
    id: CDevId,
) -> i32 {
    let tp = match line2tty(minor) {
        Some(tp) => tp,
        None => return ENXIO,
    };

    let mut r = OK;

    match request {
        TIOCGETA => {
            // Get the termios attributes.
            // TODO: Phase 13 — sys_safecopyto(endpt, grant, 0,
            //       &tp.tty_termios, size_of::<Termios>())
            r = OK;
        }
        TIOCSETAW | TIOCSETAF | TIOCDRAIN => {
            if tp.tty_outleft > 0 {
                if flags & CDEV_NONBLOCK != 0 {
                    return EAGAIN;
                }
                // Wait for all ongoing output processing to finish.
                tp.tty_iocaller = endpt as u32;
                tp.tty_ioid = id;
                tp.tty_ioreq = request;
                tp.tty_iogrant = grant;
                return EDONTREPLY;
            }
            if request != TIOCDRAIN {
                if request == TIOCSETAF {
                    tty_icancel(tp);
                }
                // TODO: Phase 13 — sys_safecopyfrom(endpt, grant, 0,
                //       &tp.tty_termios, size_of::<Termios>())
                if r == OK {
                    setattr(tp);
                }
            }
        }
        TIOCSETA => {
            // Set the termios attributes.
            // TODO: Phase 13 — sys_safecopyfrom(endpt, grant, 0,
            //       &tp.tty_termios, size_of::<Termios>())
            if r == OK {
                setattr(tp);
            }
        }
        TIOCFLUSH => {
            let _i: i32 = 0;
            // TODO: Phase 13 — sys_safecopyfrom(endpt, grant, 0, &mut _i, size_of::<i32>())
            if r == OK {
                if _i & FREAD != 0 {
                    tty_icancel(tp);
                }
                if _i & FWRITE != 0 {
                    (tp.tty_ocancel)(tp, 0);
                }
            }
        }
        TIOCSTART => {
            tp.tty_inhibited = 0;
            tp.tty_events = 1;
        }
        TIOCSTOP => {
            tp.tty_inhibited = 1;
            tp.tty_events = 1;
        }
        TIOCSBRK => {
            if tp.tty_break_on as usize != tty_devnop as *const () as usize {
                (tp.tty_break_on)(tp, 0);
            }
        }
        TIOCCBRK => {
            if tp.tty_break_off as usize != tty_devnop as *const () as usize {
                (tp.tty_break_off)(tp, 0);
            }
        }
        TIOCGWINSZ => {
            // TODO: Phase 13 — sys_safecopyto(endpt, grant, 0,
            //       &tp.tty_winsize, size_of::<WinSize>())
        }
        TIOCSWINSZ => {
            // TODO: Phase 13 — sys_safecopyfrom(endpt, grant, 0,
            //       &tp.tty_winsize, size_of::<WinSize>())
            sigchar(tp, SIGWINCH, false);
        }
        TIOCGETD => {
            let _i: i32 = TTYDISC;
            // TODO: Phase 13 — sys_safecopyto(endpt, grant, 0, &_i, size_of::<i32>())
        }
        TIOCSETD => {
            // TODO: Phase 13 — print warning
            r = ENOTTY;
        }
        TIOCGLINED => {
            // TODO: Phase 13 — sys_safecopyto(endpt, grant, 0,
            //       LINED_NAME.as_ptr(), LINED_NAME.len())
        }
        TIOCGQSIZE => {
            let _i: i32 = TTY_IN_BYTES as i32;
            // TODO: Phase 13 — sys_safecopyto(endpt, grant, 0, &_i, size_of::<i32>())
        }
        TIOCSCTTY => {
            // Process sets this tty as its controlling tty.
            tp.tty_pgrp = _user_endpt as u32;
        }
        TIOCGPGRP | TIOCSPGRP => {}
        _ => {
            r = ENOTTY;
        }
    }

    r
}

/// do_cancel — cancel a suspended I/O request.
pub fn do_cancel(minor: DevMinor, endpt: Endpoint, id: CDevId) -> i32 {
    let tp = match line2tty(minor) {
        Some(tp) => tp,
        None => return ENXIO,
    };

    let mut r = EDONTREPLY;

    if tp.tty_inleft != 0 && endpt as u32 == tp.tty_incaller && id == tp.tty_inid {
        // Process was reading when killed. Clean up input.
        tty_icancel(tp);
        r = if tp.tty_incum > 0 {
            tp.tty_incum as i32
        } else {
            EAGAIN
        };
        tp.tty_inleft = 0;
        tp.tty_incum = 0;
        tp.tty_incaller = NONE;
    } else if tp.tty_outleft != 0 && endpt as u32 == tp.tty_outcaller && id == tp.tty_outid {
        // Process was writing when killed. Clean up output.
        r = if tp.tty_outcum > 0 {
            tp.tty_outcum as i32
        } else {
            EAGAIN
        };
        tp.tty_outleft = 0;
        tp.tty_outcum = 0;
        tp.tty_outcaller = NONE;
    } else if tp.tty_ioreq != 0 && endpt as u32 == tp.tty_iocaller && id == tp.tty_ioid {
        // Process was waiting for output to drain.
        r = EINTR;
        tp.tty_ioreq = 0;
        tp.tty_iocaller = NONE;
    }

    if r != EDONTREPLY {
        tp.tty_events = 1;
    }

    r
}

/// do_select — select/poll on a TTY line.
pub fn do_select(minor: DevMinor, mut ops: u32, endpt: Endpoint) -> i32 {
    let tp = match line2tty(minor) {
        Some(tp) => tp,
        None => return ENXIO,
    };

    let watch = ops & CDEV_NOTIFY;
    ops &= CDEV_OP_RD | CDEV_OP_WR | CDEV_OP_ERR;

    let ready_ops = select_try(tp, ops);

    ops &= !ready_ops;
    if ops != 0 && watch != 0 {
        // Translated minor numbers are a problem with late select replies.
        if tp.tty_select_ops != 0 && tp.tty_select_minor != minor {
            // TODO: Phase 13 — print warning
            return EBADF;
        }
        tp.tty_select_ops |= ops;
        tp.tty_select_proc = endpt as u32;
        tp.tty_select_minor = minor;
    }

    ready_ops as i32
}

// Server main loop stub

/// Main loop for the TTY server.
///
/// Receives messages, dispatches character driver requests, handles
/// notifications (clock, hardware interrupts), and processes TTY events.
pub fn tty_server_main() {
    tty_init(100);

    loop {
        let mut msg = arch_common::ipc::Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };

        // Receive a message from any sender.
        // syscall2(RECEIVE_CALL=47, src=ANY, msg_ptr) → sender endpoint
        let src = unsafe {
            minix_rt::syscall2(
                minix_rt::RECEIVE_CALL,
                ANY as u64,
                &mut msg as *mut arch_common::ipc::Message as u64,
            )
        };
        if src < 0 {
            continue;
        }
        let src_ep = src as i32;
        let call_type = msg.m_type as u32;

        // Check for kernel notifications (NOTIFY_MESSAGE = 0x1000).
        let is_notify = (msg.m_type as u32).wrapping_sub(arch_common::com::NOTIFY_MESSAGE) < 0x100;
        if is_notify {
            // Handle events on all TTY lines.
            for i in 0..NR_CONS + NR_RS_LINES {
                let tp = unsafe { &mut *TTY_TABLE.as_ptr().add(i) };
                handle_events(tp);
            }
            continue;
        }

        // Handle CDEV requests.
        if is_cdev_rq(call_type) {
            let result = unsafe { handle_cdev_request(&mut msg, src_ep, call_type) };
            if result != EDONTREPLY {
                msg.m_type = result;
                unsafe {
                    minix_rt::syscall2(
                        minix_rt::SENDREC_CALL,
                        src_ep as u64,
                        &mut msg as *mut arch_common::ipc::Message as u64,
                    );
                }
            }
            continue;
        }

        // Handle TTY-specific messages (reply OK).
        if call_type == TTY_FKEY_CONTROL
            || call_type == TTY_INPUT_UP
            || call_type == TTY_INPUT_EVENT
        {
            msg.m_type = OK;
            unsafe {
                minix_rt::syscall2(
                    minix_rt::SENDREC_CALL,
                    src_ep as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                );
            }
            continue;
        }

        // Unknown message type: reply ENOSYS.
        msg.m_type = ENOSYS;
        unsafe {
            minix_rt::syscall2(
                minix_rt::SENDREC_CALL,
                src_ep as u64,
                &mut msg as *mut arch_common::ipc::Message as u64,
            );
        }
    }
}

/// Dispatch a CDEV request to the appropriate handler.
///
/// # Safety
///
/// `msg` must point to a valid received message.
unsafe fn handle_cdev_request(
    msg: &mut arch_common::ipc::Message,
    who_e: i32,
    call_type: u32,
) -> i32 {
    // Standard CDEV message layout (MINIX `message` m2 fields):
    //   m2_i1 = minor   (payload offset 0)
    //   m2_i2 = flags   (payload offset 4)
    //   m2_i3 = grant   (payload offset 8)
    //   m2_l1 = position (payload offset 16)
    //   m2_l2 = count    (payload offset 24)
    let minor = unsafe { msg.m_payload.m2.m2i1 as u32 };

    match call_type {
        CDEV_OPEN => {
            let access = unsafe { msg.m_payload.m2.m2i2 };
            do_open(minor, access, who_e)
        }
        CDEV_CLOSE => do_close(minor),
        CDEV_READ => {
            let flags = unsafe { msg.m_payload.m2.m2i2 };
            let grant = unsafe { msg.m_payload.m2.m2i3 as u32 };
            let position = unsafe { msg.m_payload.m2.m2l1 as u64 };
            let count = unsafe { msg.m_payload.m2.m2l2 as usize };
            do_read(minor, position, who_e, grant, count, flags, 0)
        }
        CDEV_WRITE => {
            let flags = unsafe { msg.m_payload.m2.m2i2 };
            let grant = unsafe { msg.m_payload.m2.m2i3 as u32 };
            let position = unsafe { msg.m_payload.m2.m2l1 as u64 };
            let count = unsafe { msg.m_payload.m2.m2l2 as usize };
            do_write(minor, position, who_e, grant, count, flags, 0)
        }
        CDEV_IOCTL => {
            let request = unsafe { msg.m_payload.m2.m2i2 as u32 };
            let grant = unsafe { msg.m_payload.m2.m2i3 as u32 };
            let flags = unsafe { msg.m_payload.m2.m2i2 };
            do_ioctl(minor, request, who_e, grant, flags, who_e, 0)
        }
        CDEV_CANCEL => {
            let endpt = unsafe { msg.m_payload.m2.m2i2 };
            let id = unsafe { msg.m_payload.m2.m2i3 as u32 };
            do_cancel(minor, endpt, id)
        }
        CDEV_SELECT => {
            let ops = unsafe { msg.m_payload.m2.m2i2 as u32 };
            do_select(minor, ops, who_e)
        }
        _ => ENOSYS,
    }
}

// Public accessors (for tests and device drivers)

/// Get the TTY table pointer (for testing).
pub fn get_tty_table() -> *mut Tty {
    TTY_TABLE.as_ptr()
}

/// Set the console line (minor number).
pub fn set_console_line(minor: u32) {
    CONSOLE_LINE.store(minor, Ordering::Relaxed);
}

/// Get the current console line.
pub fn get_console_line() -> u32 {
    CONSOLE_LINE.load(Ordering::Relaxed)
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};

    static TEST_LOCK: AtomicBool = AtomicBool::new(false);

    struct TestLockGuard;
    impl TestLockGuard {
        fn acquire() -> Self {
            while TEST_LOCK
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            Self
        }
    }
    impl Drop for TestLockGuard {
        fn drop(&mut self) {
            TEST_LOCK.store(false, Ordering::Release);
        }
    }

    /// Reset a TTY line to its default state for testing.
    fn setup_tty(tp: &mut Tty) {
        tp.tty_events = 0;
        tp.tty_incount = 0;
        tp.tty_eotct = 0;
        tp.tty_inhead = 0;
        tp.tty_intail = 0;
        tp.tty_inbuf = [0u16; TTY_IN_BYTES];
        tp.tty_min = 1;
        tp.tty_reprint = 0;
        tp.tty_escaped = 0;
        tp.tty_inhibited = 0;
        tp.tty_pgrp = 42;
        tp.tty_termios = Termios::defaults();
        tp.tty_position = 0;
        tp.tty_incaller = NONE;
        tp.tty_inid = 0;
        tp.tty_ingrant = 0;
        tp.tty_inleft = 0;
        tp.tty_incum = 0;
        tp.tty_echo_fn = tty_echo_dummy;
    }

    /// Reset a TTY line for raw mode tests.
    fn setup_raw(tp: &mut Tty) {
        setup_tty(tp);
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag &= !(IXON | ISTRIP | IGNCR | ICRNL | INLCR);
    }

    /// Reset a TTY line with canonical mode, no echo, no signals.
    fn setup_canon(tp: &mut Tty) {
        setup_tty(tp);
        tp.tty_termios.c_lflag |= ICANON;
        tp.tty_termios.c_lflag &= !(ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag &= !(IXON | ISTRIP | IGNCR | ICRNL | INLCR);
    }

    /// Helper to get a mutable reference to a table entry for testing.
    fn table_mut(idx: usize) -> &'static mut Tty {
        unsafe { &mut *TTY_TABLE.as_ptr().add(idx) }
    }

    /// Helper to get an immutable reference to a table entry for testing.
    fn table_ref(idx: usize) -> &'static Tty {
        unsafe { &*TTY_TABLE.as_ptr().add(idx) }
    }

    /// Check that the input queue contains specific characters (ignoring flags).
    fn assert_queue_chars(tp: &Tty, expected: &[u8]) {
        assert_eq!(tp.tty_incount as usize, expected.len());
        let qlen = tp.tty_inbuf.len();
        let mut idx = tp.tty_intail;
        for &exp in expected {
            let ch = (tp.tty_inbuf[idx] & IN_CHAR) as u8;
            assert_eq!(ch, exp, "queue char mismatch at position {idx}");
            idx = if idx + 1 >= qlen { 0 } else { idx + 1 };
        }
    }

    #[test]
    fn test_line2tty_console_minor() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        // Activate console line 0.
        table_mut(0).tty_devread = |_, _| 0;

        let tp = line2tty(CONS_MINOR);
        assert!(tp.is_some(), "console minor 0 should map to a TTY");
        assert_eq!(tp.unwrap().tty_minor, CONS_MINOR);
    }

    #[test]
    fn test_line2tty_console_minor_1() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        table_mut(1).tty_devread = |_, _| 0;

        let tp = line2tty(CONS_MINOR + 1);
        assert!(tp.is_some());
        assert_eq!(tp.unwrap().tty_minor, CONS_MINOR + 1);
    }

    #[test]
    fn test_line2tty_log_minor_redirects() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        // Activate console line 0.
        table_mut(0).tty_devread = |_, _| 0;

        let tp = line2tty(LOG_MINOR);
        assert!(tp.is_some(), "log minor should map to console");
    }

    #[test]
    fn test_line2tty_rs232_minor() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        table_mut(NR_CONS).tty_devread = |_, _| 0;

        let tp = line2tty(RS232_MINOR);
        assert!(tp.is_some(), "RS-232 minor should map");
        assert_eq!(tp.unwrap().tty_minor, RS232_MINOR);
    }

    #[test]
    fn test_line2tty_rs232_second_line() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        table_mut(NR_CONS + 1).tty_devread = |_, _| 0;

        let tp = line2tty(RS232_MINOR + 1);
        assert!(tp.is_some());
        assert_eq!(tp.unwrap().tty_minor, RS232_MINOR + 1);
    }

    #[test]
    fn test_line2tty_video_minor() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = line2tty(VIDEO_MINOR);
        assert!(tp.is_none(), "video minor should return None");
    }

    #[test]
    fn test_line2tty_inactive_line() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = line2tty(RS232_MINOR);
        assert!(tp.is_none(), "inactive RS-232 line should return None");
    }

    #[test]
    fn test_line2tty_out_of_range() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = line2tty(999);
        assert!(tp.is_none());
    }

    #[test]
    fn test_tty_init_initializes_table() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        assert_eq!(SYSTEM_HZ.load(Ordering::Relaxed), 100);

        // Console lines should have consecutive minor numbers.
        for s in 0..NR_CONS {
            let tp = table_ref(s);
            assert_eq!(
                tp.tty_minor,
                CONS_MINOR + s as u32,
                "console line {s} minor"
            );
            assert_eq!(tp.tty_index, s as i32);
            assert_eq!(tp.tty_min, 1);
            assert_eq!(tp.tty_incaller, NONE);
            assert_eq!(tp.tty_incount, 0);
            assert_eq!(tp.tty_eotct, 0);
        }

        // RS-232 lines should have consecutive minor numbers starting at RS232_MINOR.
        for s in NR_CONS..NR_CONS + NR_RS_LINES {
            let rs_idx = s - NR_CONS;
            let tp = table_ref(s);
            assert_eq!(tp.tty_minor, RS232_MINOR + rs_idx as u32);
            assert_eq!(tp.tty_index, s as i32);
        }
    }

    #[test]
    fn test_in_process_raw_mode_basic() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_raw(&mut tp);

        let input = b"hello";
        let count = in_process(&mut tp, input);
        assert_eq!(count, input.len(), "all bytes processed");

        assert_eq!(tp.tty_incount, input.len() as i32);
        assert_eq!(tp.tty_eotct, input.len() as i32);
        assert_queue_chars(&tp, input);
    }

    #[test]
    fn test_in_process_canonical_mode() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_canon(&mut tp);

        let input = b"hello\n";
        let count = in_process(&mut tp, input);
        assert_eq!(count, input.len());

        assert_eq!(tp.tty_incount, 6);
        assert_eq!(tp.tty_eotct, 1);

        let last_idx = (tp.tty_intail + 5) % tp.tty_inbuf.len();
        assert!(
            tp.tty_inbuf[last_idx] & IN_EOT != 0,
            "newline should have IN_EOT"
        );
    }

    #[test]
    fn test_in_process_istrip() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag |= ISTRIP;
        tp.tty_termios.c_iflag &= !(IXON | IGNCR | ICRNL | INLCR);

        let input = [0xC1u8];
        let count = in_process(&mut tp, &input);
        assert_eq!(count, 1);

        let ch = (tp.tty_inbuf[tp.tty_intail] & IN_CHAR) as u8;
        assert_eq!(ch, b'A', "ISTRIP should strip high bit");
    }

    #[test]
    fn test_in_process_icrnl() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag |= ICRNL;
        tp.tty_termios.c_iflag &= !(IXON | ISTRIP | IGNCR | INLCR);

        in_process(&mut tp, b"\r");
        let ch = (tp.tty_inbuf[tp.tty_intail] & IN_CHAR) as u8;
        assert_eq!(ch, b'\n', "ICRNL should map CR to LF");
    }

    #[test]
    fn test_in_process_igncr() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag |= IGNCR;
        tp.tty_termios.c_iflag &= !(IXON | ISTRIP | ICRNL | INLCR);

        let count = in_process(&mut tp, b"\r");
        assert_eq!(count, 1);
        assert_eq!(tp.tty_incount, 0, "IGNCR should discard CR");
    }

    #[test]
    fn test_in_process_inlcr() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag |= INLCR;
        tp.tty_termios.c_iflag &= !(IXON | ISTRIP | IGNCR | ICRNL);

        in_process(&mut tp, b"\n");
        let ch = (tp.tty_inbuf[tp.tty_intail] & IN_CHAR) as u8;
        assert_eq!(ch, b'\r', "INLCR should map LF to CR");
    }

    #[test]
    fn test_in_process_icanon_erase() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag |= ICANON | ECHOE;
        tp.tty_termios.c_lflag &= !(ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag &= !(IXON | ISTRIP | IGNCR | ICRNL | INLCR);
        tp.tty_termios.c_cc[VERASE] = 0x7F;

        let count = in_process(&mut tp, b"ab\x7fc\n");
        assert_eq!(count, 5);
        assert_queue_chars(&tp, b"ac\n");
        assert_eq!(tp.tty_eotct, 1);
    }

    #[test]
    fn test_in_process_icanon_kill() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag |= ICANON | ECHOE;
        tp.tty_termios.c_lflag &= !(ISIG | IEXTEN | ECHO | ECHOK);
        tp.tty_termios.c_iflag &= !(IXON);
        tp.tty_termios.c_cc[VKILL] = 0x15;

        let count = in_process(&mut tp, b"hello\x15world\n");
        assert_eq!(count, 12);
        assert_queue_chars(&tp, b"world\n");
        assert_eq!(tp.tty_eotct, 1);
    }

    #[test]
    fn test_in_process_icanon_eof() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_canon(&mut tp);

        in_process(&mut tp, &[0x04u8]);

        assert_eq!(tp.tty_incount, 1);
        assert_eq!(tp.tty_eotct, 1);

        let entry = tp.tty_inbuf[tp.tty_intail];
        assert!(entry & IN_EOF != 0, "EOF should have IN_EOF");
        assert!(entry & IN_EOT != 0, "EOF should have IN_EOT");
    }

    #[test]
    fn test_in_process_icanon_eol() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_canon(&mut tp);
        tp.tty_termios.c_cc[VEOL] = b'$';

        let count = in_process(&mut tp, b"hello$world\n");
        assert_eq!(count, 12);
        assert_eq!(tp.tty_incount, 12);
        assert_eq!(tp.tty_eotct, 2); // $ and \n
    }

    #[test]
    fn test_in_process_ixon_stop() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_iflag |= IXON;
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | IEXTEN | ECHO);

        let count = in_process(&mut tp, &[0x13u8]);
        assert_eq!(count, 1);

        assert_eq!(tp.tty_inhibited, STOPPED);
        assert_eq!(tp.tty_events, 1);
        assert_eq!(tp.tty_incount, 0);
    }

    #[test]
    fn test_in_process_ixon_start() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_iflag |= IXON | IXANY;
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | IEXTEN | ECHO);

        tp.tty_inhibited = STOPPED;

        let count = in_process(&mut tp, &[0x11u8]);
        assert_eq!(count, 1);

        assert_eq!(tp.tty_inhibited, RUNNING);
        assert_eq!(tp.tty_events, 1);
        assert_eq!(tp.tty_incount, 0);
    }

    #[test]
    fn test_in_process_ixany_restart() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_iflag |= IXON | IXANY;
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | IEXTEN | ECHO);

        tp.tty_inhibited = STOPPED;

        let count = in_process(&mut tp, b"x");
        assert_eq!(count, 1);

        assert_eq!(tp.tty_inhibited, RUNNING);
        assert_eq!(tp.tty_events, 1);
        assert_eq!(tp.tty_incount, 1);
    }

    #[test]
    fn test_in_process_isig_intr() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag |= ISIG;
        tp.tty_termios.c_lflag &= !(ICANON | IEXTEN | ECHO);
        tp.tty_termios.c_iflag &= !(IXON);
        tp.tty_pgrp = 100;

        let count = in_process(&mut tp, &[0x03u8]);
        assert_eq!(count, 1);
        assert_eq!(tp.tty_incount, 0);
        assert_eq!(tp.tty_events, 1);
    }

    #[test]
    fn test_in_process_isig_quit() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag |= ISIG;
        tp.tty_termios.c_lflag &= !(ICANON | IEXTEN | ECHO);
        tp.tty_termios.c_iflag &= !(IXON);
        tp.tty_pgrp = 100;

        let count = in_process(&mut tp, &[0x1Cu8]);
        assert_eq!(count, 1);
        assert_eq!(tp.tty_incount, 0);
    }

    #[test]
    fn test_in_process_iexten_lnext() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag |= IEXTEN;
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | ECHO);
        tp.tty_termios.c_iflag &= !(IXON);
        tp.tty_termios.c_cc[VLNEXT] = 0x16;

        in_process(&mut tp, &[0x16, 0x03]);

        assert_eq!(tp.tty_incount, 1);
        let entry = tp.tty_inbuf[tp.tty_intail];
        assert!(entry & IN_ESC != 0);
        assert_eq!((entry & IN_CHAR) as u8, 0x03);
    }

    #[test]
    fn test_in_process_iexten_reprint() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag |= IEXTEN;
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | ECHO);
        tp.tty_termios.c_iflag &= !(IXON);
        tp.tty_termios.c_cc[VREPRINT] = 0x12;

        tp.tty_incount = 5;
        tp.tty_inbuf = [0u16; TTY_IN_BYTES];
        for i in 0..5 {
            tp.tty_inbuf[i] = b'a' as u16 + i as u16;
        }
        tp.tty_inhead = 5;

        let count = in_process(&mut tp, &[0x12u8]);
        assert_eq!(count, 1);
        assert_eq!(tp.tty_incount, 5);
    }

    #[test]
    fn test_in_process_vdisable_escaped() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag &= !(IXON | ISTRIP | IGNCR | ICRNL | INLCR);

        in_process(&mut tp, &[0xFFu8]);

        assert_eq!(tp.tty_incount, 1);
        assert!(tp.tty_inbuf[tp.tty_intail] & IN_ESC != 0);
    }

    #[test]
    fn test_in_process_buffer_full_canonical() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag |= ICANON;
        tp.tty_termios.c_lflag &= !(ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag &= !(IXON);

        tp.tty_incount = TTY_IN_BYTES as i32;
        tp.tty_inhead = 0;

        let count = in_process(&mut tp, b"x");
        assert_eq!(count, 1);
        assert_eq!(tp.tty_incount, TTY_IN_BYTES as i32);
    }

    #[test]
    fn test_in_process_buffer_full_raw() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ICANON | ISIG | IEXTEN | ECHO);
        tp.tty_termios.c_iflag &= !(IXON);

        tp.tty_incount = TTY_IN_BYTES as i32;
        tp.tty_inhead = 0;

        let count = in_process(&mut tp, b"x");
        assert_eq!(count, 0);
    }
    #[test]
    fn test_out_process_tab_expansion() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_oflag |= OPOST | OXTABS;
        tp.tty_position = 0;

        let mut buf = [b'\t'; 64];
        let mut offset = 0usize;
        let mut icount = 1usize;
        let mut ocount = 64usize;

        out_process(&mut tp, &mut buf, &mut offset, &mut icount, &mut ocount);

        assert_eq!(icount, 1);
        assert_eq!(ocount, 8);
        assert_eq!(&buf[..8], b"        ");
    }

    #[test]
    fn test_out_process_tab_at_position_4() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_oflag |= OPOST | OXTABS;
        tp.tty_position = 4;

        let mut buf = [b'\t'; 64];
        let mut offset = 0usize;
        let mut icount = 1usize;
        let mut ocount = 64usize;

        out_process(&mut tp, &mut buf, &mut offset, &mut icount, &mut ocount);

        assert_eq!(icount, 1);
        assert_eq!(ocount, 4);
        assert_eq!(&buf[..4], b"    ");
    }

    #[test]
    fn test_out_process_newline_crlf() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_oflag |= OPOST | ONLCR;
        tp.tty_position = 0;

        let mut buf = [b'\n'; 64];
        let mut offset = 0usize;
        let mut icount = 1usize;
        let mut ocount = 64usize;

        out_process(&mut tp, &mut buf, &mut offset, &mut icount, &mut ocount);

        assert_eq!(icount, 1);
        assert_eq!(ocount, 2);
        assert_eq!(buf[0], b'\r');
        assert_eq!(buf[1], b'\n');
    }

    #[test]
    fn test_out_process_no_opost() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_oflag &= !(OPOST | ONLCR | OXTABS);
        tp.tty_position = 0;

        let mut buf = [b'\t'; 64];
        let mut offset = 0usize;
        let mut icount = 1usize;
        let mut ocount = 64usize;

        out_process(&mut tp, &mut buf, &mut offset, &mut icount, &mut ocount);

        assert_eq!(icount, 1);
        assert_eq!(ocount, 1);
        assert_eq!(buf[0], b'\t');
    }

    #[test]
    fn test_out_process_backspace() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_oflag |= OPOST;
        tp.tty_position = 5;

        let mut buf = [b'\x08'; 64];
        let mut offset = 0usize;
        let mut icount = 1usize;
        let mut ocount = 64usize;

        out_process(&mut tp, &mut buf, &mut offset, &mut icount, &mut ocount);

        assert_eq!(icount, 1);
        assert_eq!(tp.tty_position, 4);
    }

    #[test]
    fn test_out_process_carriage_return() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_oflag |= OPOST;
        tp.tty_position = 10;

        let mut buf = [b'\r'; 64];
        let mut offset = 0usize;
        let mut icount = 1usize;
        let mut ocount = 64usize;

        out_process(&mut tp, &mut buf, &mut offset, &mut icount, &mut ocount);

        assert_eq!(icount, 1);
        assert_eq!(tp.tty_position, 0);
    }

    #[test]
    fn test_sigchar_flushes_input() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);

        tp.tty_incount = 10;
        tp.tty_eotct = 3;
        tp.tty_inhead = 5;
        tp.tty_intail = 0;

        sigchar(&mut tp, SIGINT, true);

        assert_eq!(tp.tty_incount, 0);
        assert_eq!(tp.tty_eotct, 0);
        assert_eq!(tp.tty_intail, tp.tty_inhead);
        assert_eq!(tp.tty_events, 1);
        assert_eq!(tp.tty_inhibited, RUNNING);
    }

    #[test]
    fn test_sigchar_no_flush_with_noflsh() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);

        tp.tty_incount = 10;
        tp.tty_eotct = 3;
        tp.tty_termios.c_lflag |= NOFLSH;

        sigchar(&mut tp, SIGINT, true);

        assert_eq!(tp.tty_incount, 10);
        assert_eq!(tp.tty_eotct, 3);
    }

    #[test]
    fn test_sigchar_no_flush_when_not_requested() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);

        tp.tty_incount = 10;
        sigchar(&mut tp, SIGINT, false);
        assert_eq!(tp.tty_incount, 10);
    }

    fn test_devread(tp: &mut Tty, _try_only: i32) -> i32 {
        if tp.tty_incount < 5 {
            let ch = b'A' as u16 + tp.tty_incount as u16;
            tp.tty_inbuf[tp.tty_inhead] = ch;
            let qlen = tp.tty_inbuf.len();
            tp.tty_inhead = (tp.tty_inhead + 1) % qlen;
            tp.tty_incount += 1;
            tp.tty_eotct += 1;
            tp.tty_events = 1;
        }
        0
    }

    #[test]
    fn test_handle_events_processes_input() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ICANON | ECHO | ISIG | IEXTEN);
        tp.tty_termios.c_iflag &= !(IXON | ISTRIP | IGNCR | ICRNL | INLCR);
        tp.tty_devread = test_devread;
        tp.tty_events = 1;

        handle_events(&mut tp);

        assert!(tp.tty_incount > 0);
    }

    #[test]
    fn test_handle_events_transfers_to_reader() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ICANON | ECHO | ISIG | IEXTEN);
        tp.tty_termios.c_iflag &= !(IXON);
        tp.tty_devread = test_devread;

        tp.tty_incaller = 100;
        tp.tty_inid = 1;
        tp.tty_ingrant = 0;
        tp.tty_inleft = 10;
        tp.tty_min = 1;
        tp.tty_events = 1;

        handle_events(&mut tp);

        assert!(tp.tty_inleft == 0 || tp.tty_incum > 0);
    }

    #[test]
    fn test_handle_events_loop_terminates() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !(ICANON | ECHO | ISIG | IEXTEN);
        tp.tty_termios.c_iflag &= !(IXON);

        fn one_shot_devread(tp: &mut Tty, _try_only: i32) -> i32 {
            if tp.tty_incount == 0 {
                tp.tty_inbuf[0] = b'x' as u16 | IN_EOT;
                tp.tty_inhead = 1;
                tp.tty_incount = 1;
                tp.tty_eotct = 1;
            }
            0
        }
        tp.tty_devread = one_shot_devread;
        tp.tty_events = 1;

        handle_events(&mut tp);

        assert_eq!(tp.tty_incount, 1);
        assert_eq!(tp.tty_events, 0);
    }

    /// Echo collector for tests.
    fn echo_collector(tp: &mut Tty, c: i32) {
        // Store in a fake "echo buffer" at position 0 in tty_inbuf
        // We'll just use the private data pointer as a counter.
        let _ = (tp, c);
    }

    #[test]
    fn test_rawecho_no_echo_when_echo_clear() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag &= !ECHO;

        // Just verify rawecho doesn't crash when echo is clear.
        rawecho(&mut tp, b'X' as i32);
    }

    #[test]
    fn test_back_over_empty_queue() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);

        let result = back_over(&mut tp);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_back_over_on_eot() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);

        tp.tty_inbuf[0] = b'\n' as u16 | IN_EOT;
        tp.tty_inhead = 1;
        tp.tty_incount = 1;

        let result = back_over(&mut tp);
        assert_eq!(result, 0);
        assert_eq!(tp.tty_incount, 1);
    }

    #[test]
    fn test_back_over_erases_character() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_tty(&mut tp);
        tp.tty_termios.c_lflag |= ECHOE;

        tp.tty_inbuf[0] = b'a' as u16 | (1 << IN_LSHIFT);
        tp.tty_inhead = 1;
        tp.tty_incount = 1;

        let result = back_over(&mut tp);
        assert_eq!(result, 1);
        assert_eq!(tp.tty_incount, 0);
        assert_eq!(tp.tty_inhead, 0);
    }

    #[test]
    fn test_do_cancel_reading_process() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = table_mut(0);
        tp.tty_devread = |_, _| 0;
        tp.tty_incaller = 100;
        tp.tty_inid = 5;
        tp.tty_inleft = 10;
        tp.tty_incum = 3;

        let r = do_cancel(CONS_MINOR, 100, 5);
        assert_eq!(r, 3);
        assert_eq!(tp.tty_incaller, NONE);
        assert_eq!(tp.tty_inleft, 0);
    }

    #[test]
    fn test_do_cancel_writing_process() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = table_mut(0);
        tp.tty_devread = |_, _| 0;
        tp.tty_outcaller = 200;
        tp.tty_outid = 10;
        tp.tty_outleft = 20;
        tp.tty_outcum = 5;

        let r = do_cancel(CONS_MINOR, 200, 10);
        assert_eq!(r, 5);
        assert_eq!(tp.tty_outcaller, NONE);
    }

    #[test]
    fn test_do_cancel_no_match() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = table_mut(0);
        tp.tty_devread = |_, _| 0;
        tp.tty_incaller = 100;
        tp.tty_inid = 5;
        tp.tty_inleft = 10;

        let r = do_cancel(CONS_MINOR, 200, 5);
        assert_eq!(r, EDONTREPLY);
    }

    #[test]
    fn test_do_open_console_line() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = table_mut(0);
        tp.tty_devread = |_, _| 0;

        let r = do_open(CONS_MINOR, 0, 123);
        assert_eq!(r, CDEV_CTTY);
        assert_eq!(tp.tty_pgrp, 123);
        assert_eq!(tp.tty_openct, 1);
    }

    #[test]
    fn test_do_open_log_device() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = table_mut(0);
        tp.tty_devread = |_, _| 0;

        let r = do_open(LOG_MINOR, CDEV_R_BIT, 123);
        assert_eq!(r, EACCES);
    }

    #[test]
    fn test_do_close_resets_state() {
        let _lock = TestLockGuard::acquire();
        tty_init(100);

        let tp = table_mut(0);
        tp.tty_devread = |_, _| 0;
        tp.tty_openct = 1;
        tp.tty_pgrp = 123;

        let r = do_close(CONS_MINOR);
        assert_eq!(r, OK);
        assert_eq!(tp.tty_pgrp, 0);
        assert_eq!(tp.tty_openct, 0);
    }

    fn setup_try(tp: &mut Tty) {
        tp.tty_events = 0;
        tp.tty_incount = 0;
        tp.tty_eotct = 0;
        tp.tty_inhead = 0;
        tp.tty_intail = 0;
        tp.tty_termios = Termios::defaults();
        tp.tty_termios.c_lflag &= !(ECHO | ISIG | IEXTEN);
        tp.tty_termios.c_iflag &= !(IXON);
    }

    #[test]
    fn test_select_try_readable() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_try(&mut tp);
        tp.tty_termios.c_lflag &= !ICANON;
        tp.tty_incount = 5;
        tp.tty_eotct = 5;

        let ready = select_try(&mut tp, CDEV_OP_RD);
        assert_eq!(ready, CDEV_OP_RD);
    }

    #[test]
    fn test_select_try_not_readable() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_try(&mut tp);

        let ready = select_try(&mut tp, CDEV_OP_RD);
        assert_eq!(ready, 0);
    }

    #[test]
    fn test_select_try_hungup() {
        let _lock = TestLockGuard::acquire();
        let mut tp = Tty::zeroed();
        setup_try(&mut tp);
        tp.tty_termios.c_ospeed = 0;

        let ready = select_try(&mut tp, CDEV_OP_RD | CDEV_OP_WR);
        assert_eq!(ready, CDEV_OP_RD | CDEV_OP_WR);
    }
}
