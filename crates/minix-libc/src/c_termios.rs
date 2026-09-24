//! C `termios.h`: the `tc*` calls, the speed accessors and `cfmakeraw`.
//!
//! None of this reaches the kernel itself. The tty server implements the
//! NetBSD `TIOCGETA`/`TIOCSETA` family and `minix-std` owns the request codes,
//! which are derived from `size_of::<Termios>()` — so the byte layout below is
//! load-bearing, since the kernel reads and writes 44 bytes through the
//! caller's pointer. A test pins it against `minix_std::termios::Termios`.

use core::ffi::c_int;

/// `NCCS`: the length of `c_cc`, fixed by the kernels's `Termios` and the
/// `NCCS` in `minix-std`.
pub const NCCS: usize = 20;

/// C `struct termios`. The field order and widths are the tty server's, which
/// `minix_std::termios` mirrors and whose `termios_layout_matches_tty_server`
/// test holds to 44 bytes with no padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_cc: [u8; NCCS],
    pub c_ispeed: i32,
    pub c_ospeed: i32,
}

/// `tcsetattr` actions. The values are POSIX's and must match `termios.h`,
/// which is where a C caller gets them from.
pub const TCSANOW: c_int = 0;
pub const TCSADRAIN: c_int = 1;
pub const TCSAFLUSH: c_int = 2;

/// `tcflush` queues: the kernel's `TIOCFLUSH` argument is the mask of the
/// first two.
pub const TCIFLUSH: c_int = 1;
pub const TCOFLUSH: c_int = 2;
pub const TCIOFLUSH: c_int = 3;

/// `tcflow` actions. POSIX numbers them in this order.
pub const TCOOFF: c_int = 0;
pub const TCOON: c_int = 1;
pub const TCIOFF: c_int = 2;
pub const TCION: c_int = 3;

const EINVAL: i32 = 22;
const ENOSYS: i32 = 78;

/// The ioctl every call here makes, with the lockstep `minix_std::termios`
/// keeps with the tty server.
#[cfg(target_os = "minix")]
unsafe fn ioctl(fd: c_int, request: u32, arg: *mut u8) -> c_int {
    match unsafe { minix_std::fs::ioctl(fd, request, arg) } {
        Ok(_) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// POSIX `tcgetattr()`: read `fd`'s terminal attributes into `t`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcgetattr(fd: c_int, t: *mut Termios) -> c_int {
    if t.is_null() {
        return crate::fail(EINVAL);
    }
    unsafe { ioctl(fd, minix_std::termios::TIOCGETA, t as *mut u8) }
}

/// POSIX `tcsetattr()`: write `t` to `fd`, applying it as `actions` says —
/// now, after draining output, or after discarding input as well.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcsetattr(fd: c_int, actions: c_int, t: *const Termios) -> c_int {
    if t.is_null() {
        return crate::fail(EINVAL);
    }
    let request = match actions {
        TCSADRAIN => minix_std::termios::TIOCSETAW,
        TCSAFLUSH => minix_std::termios::TIOCSETAF,
        // `TCSANOW`, and anything else: apply immediately. An out-of-range
        // action is undefined in POSIX, and applying the change is what the
        // caller asked for either way.
        _ => minix_std::termios::TIOCSETA,
    };
    unsafe { ioctl(fd, request, t as *mut u8) }
}

/// POSIX `tcdrain()`: wait until `fd`'s queued output has been written.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcdrain(fd: c_int) -> c_int {
    unsafe { ioctl(fd, minix_std::termios::TIOCDRAIN, core::ptr::null_mut()) }
}

/// POSIX `tcflush()`: discard queued input (`TCIFLUSH`), output (`TCOFLUSH`)
/// or both.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcflush(fd: c_int, queue: c_int) -> c_int {
    let mut mask = queue;
    unsafe { ioctl(fd, minix_std::termios::TIOCFLUSH, &raw mut mask as *mut u8) }
}

/// POSIX `tcflow()`: suspend (`TCOOFF`) or resume (`TCOON`) output.
///
/// `TCIOFF`/`TCION` — transmitting a STOP/START character — have no kernel
/// support: a virtual console has no line to send them down, and the tty
/// server has no request for it, so they report `ENOSYS` rather than pretend.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcflow(fd: c_int, action: c_int) -> c_int {
    let request = match action {
        TCOOFF => minix_std::termios::TIOCSTOP,
        TCOON => minix_std::termios::TIOCSTART,
        _ => return crate::fail(ENOSYS),
    };
    unsafe { ioctl(fd, request, core::ptr::null_mut()) }
}

/// POSIX `tcgetpgrp()`: the foreground process group of `fd`'s terminal.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcgetpgrp(fd: c_int) -> c_int {
    let mut pgrp: c_int = 0;
    let r = unsafe { ioctl(fd, minix_std::termios::TIOCGPGRP, &raw mut pgrp as *mut u8) };
    if r < 0 { r } else { pgrp }
}

/// POSIX `tcsetpgrp()`: make `pgrp` the foreground process group of `fd`'s
/// terminal.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcsetpgrp(fd: c_int, pgrp: c_int) -> c_int {
    let mut pgrp = pgrp;
    unsafe { ioctl(fd, minix_std::termios::TIOCSPGRP, &raw mut pgrp as *mut u8) }
}

/// `cfgetospeed()`: the output baud rate recorded in `t`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn cfgetospeed(t: *const Termios) -> c_int {
    if t.is_null() {
        -1
    } else {
        unsafe { (*t).c_ospeed }
    }
}

/// `cfgetispeed()`: the input baud rate recorded in `t`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn cfgetispeed(t: *const Termios) -> c_int {
    if t.is_null() {
        -1
    } else {
        unsafe { (*t).c_ispeed }
    }
}

/// `cfsetospeed()`: record an output baud rate in `t`.
///
/// The port's tty server takes the speed as a plain number in `c_ospeed` (its
/// `B9600` is 9600, not a bit field in `c_cflag`), so this is an assignment.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfsetospeed(t: *mut Termios, speed: c_int) -> c_int {
    if t.is_null() {
        return crate::fail(EINVAL);
    }
    unsafe { (*t).c_ospeed = speed };
    0
}

/// `cfsetispeed()`: record an input baud rate in `t`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfsetispeed(t: *mut Termios, speed: c_int) -> c_int {
    if t.is_null() {
        return crate::fail(EINVAL);
    }
    unsafe { (*t).c_ispeed = speed };
    0
}

/// `cfmakeraw()`: turn `t` into raw mode — no canonical processing, no echo,
/// no signal characters, no input translation, and byte-at-a-time reads.
///
/// The flag values are the tty server's, since that is what a `tcsetattr` with
/// this struct will be judged by.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfmakeraw(t: *mut Termios) {
    use minix_std::termios as k;
    if t.is_null() {
        return;
    }
    let t = unsafe { &mut *t };
    t.c_iflag &=
        !(k::IGNBRK | k::BRKINT | k::PARMRK | k::ISTRIP | k::INLCR | k::IGNCR | k::ICRNL | k::IXON);
    t.c_oflag &= !k::OPOST;
    t.c_lflag &= !(k::ECHO | k::ECHONL | k::ICANON | k::ISIG | k::IEXTEN);
    t.c_cflag &= !(k::CSIZE | k::PARENB);
    t.c_cflag |= k::CS8;
    t.c_cc[k::VMIN] = 1;
    t.c_cc[k::VTIME] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel writes 44 bytes through the caller's pointer, so this struct
    /// and `minix_std::termios::Termios` have to be the same bytes. If either
    /// moves, the ioctl request codes move with it — which is why this fails
    /// rather than the C header silently drifting.
    #[test]
    fn layout_matches_minix_std() {
        use core::mem::{offset_of, size_of};
        assert_eq!(
            size_of::<Termios>(),
            size_of::<minix_std::termios::Termios>()
        );
        assert_eq!(NCCS, minix_std::termios::NCCS);
        assert_eq!(offset_of!(Termios, c_lflag), 12);
        assert_eq!(offset_of!(Termios, c_cc), 16);
        assert_eq!(offset_of!(Termios, c_ispeed), 36);
        assert_eq!(offset_of!(Termios, c_ospeed), 40);
        assert_eq!(size_of::<Termios>(), 44);
    }
}
