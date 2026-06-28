//! ProcFS miscellaneous utilities — adapted from `minix/fs/procfs/util.c`

use crate::procfs::types::Load;

/// Retrieve system load average information.
///
/// Fills up to `nelem` entries in `loads` with load data.
///
/// # Returns
///
/// Number of elements filled, or 0 on failure.
///
/// TODO: call `sys_getloadinfo(&loadinfo)` and compute scaled load values
///       using `sys_hz()` and `_LOAD_UNIT_SECS`.
pub fn procfs_getloadavg(loads: &mut [Load; 3]) -> i32 {
    // Stub: leave loads at their default (zero) values.
    let _ = loads;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getloadavg_returns_zero() {
        let mut loads = [Load::default(); 3];
        assert_eq!(procfs_getloadavg(&mut loads), 0);
    }
}
