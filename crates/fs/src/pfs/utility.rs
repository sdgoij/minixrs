//! PFS utility functions — adapted from `minix/fs/pfs/utility.c`

use crate::pfs::consts::*;

/// Default handler for unimplemented / invalid system calls.
// Reference: utility.c no_sys()
pub fn no_sys() -> i32 {
    EINVAL
}

/// Return current time in seconds since epoch.
///
/// Stubbed — depends on Minix kernel `getuptime` / `sys_hz`.
/// For now, returns 0.
// Reference: utility.c clock_time()
pub fn clock_time() -> i64 {
    // TODO: Wire up to system clock when kernel interface is available.
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_sys_returns_einval() {
        assert_eq!(no_sys(), EINVAL);
    }

    #[test]
    fn test_clock_time_returns_zero() {
        // Currently stubbed to 0
        assert_eq!(clock_time(), 0);
    }
}
