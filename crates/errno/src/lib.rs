//! A stand-in for the `errno` crate, for minix.
//!
//! Upstream selects its backend with `#[cfg_attr(unix | windows | wasi |
//! hermit, path = ...)] mod sys`, and minix matches none of them, so its
//! package cannot be built for this target. The port has one platform, so this
//! crate has one implementation and no dispatch.
//!
//! `__errno_location` is the port's TLS errno slot: `minix-libc` defines it and
//! `rustix`'s libc backend declares the same symbol.

use std::ffi::{c_char, c_int};
use std::fmt;
#[cfg(feature = "std")]
use std::io;

/// Wraps a platform error code.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct Errno(pub i32);

impl Errno {
    /// Calls `callback` with the message `strerror` has for this code. The
    /// message lives in a stack buffer, so it is handed to the callback rather
    /// than returned.
    fn with_description<F, T>(self, callback: F) -> T
    where
        F: FnOnce(&str) -> T,
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer and length handed to `strerror_r` are `buf` and
        // its length; `minix-libc` copies at most `buf.len() - 1` bytes into
        // it and terminates the copy, returning `buf` (or null if it cannot).
        let described =
            unsafe { !strerror_r(self.0, buf.as_mut_ptr().cast(), buf.len()).is_null() };
        let msg = if described {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            std::str::from_utf8(&buf[..len]).unwrap_or("Unknown error")
        } else {
            "Unknown error"
        };
        callback(msg)
    }
}

impl fmt::Debug for Errno {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with_description(|desc| {
            fmt.debug_struct("Errno")
                .field("code", &self.0)
                .field("description", &desc)
                .finish()
        })
    }
}

impl fmt::Display for Errno {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with_description(|desc| fmt.write_str(desc))
    }
}

impl From<Errno> for i32 {
    fn from(err: Errno) -> Self {
        err.0
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Errno {}

#[cfg(feature = "std")]
impl From<Errno> for io::Error {
    fn from(err: Errno) -> Self {
        io::Error::from_raw_os_error(err.0)
    }
}

/// Returns this thread's `errno`.
pub fn errno() -> Errno {
    // SAFETY: `__errno_location` returns a valid pointer to this thread's slot.
    Errno(unsafe { *__errno_location() })
}

/// Sets this thread's `errno`.
pub fn set_errno(err: Errno) {
    // SAFETY: as in `errno`.
    unsafe { *__errno_location() = err.0 }
}

unsafe extern "C" {
    fn __errno_location() -> *mut c_int;
    fn strerror_r(errnum: c_int, buf: *mut c_char, buflen: usize) -> *mut c_char;
}
