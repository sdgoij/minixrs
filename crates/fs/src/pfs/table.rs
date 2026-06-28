//! VFS dispatch table — adapted from `minix/fs/pfs/table.c`
//!
//! Maps FS request numbers (FS_BASE + n) to handler functions.
//! Index 0..32 corresponds to request codes FS_BASE through FS_BASE+32.
//!
//! Note: The original Minix `const.h` declares `FS_CALL_VEC_SIZE = 31`,
//! but the actual dispatch table in `table.c` has 33 entries (indices 0..32).
//! We use 33 entries here to match the full table layout.

use crate::pfs::consts::*;
use crate::pfs::inode::*;
use crate::pfs::link::*;
use crate::pfs::misc::*;
use crate::pfs::mount::*;
use crate::pfs::open::*;
use crate::pfs::read::*;
use crate::pfs::stadir::*;
use crate::pfs::time::*;
use crate::pfs::utility::*;

/// Dispatch table: 33 entries indexed by `req_nr - FS_BASE`.
// Reference: table.c fs_call_vec[]
pub static FS_CALL_VEC: [fn() -> i32; FS_CALL_VEC_SIZE] = [
    no_sys,        //  0 (FS_BASE + 0)  not used
    no_sys,        //  1 (FS_BASE + 1)
    fs_putnode,    //  2 (FS_BASE + 2)
    no_sys,        //  3 (FS_BASE + 3)
    fs_ftrunc,     //  4 (FS_BASE + 4)
    no_sys,        //  5 (FS_BASE + 5)
    fs_chmod,      //  6 (FS_BASE + 6)
    no_sys,        //  7 (FS_BASE + 7)
    fs_stat,       //  8 (FS_BASE + 8)
    fs_utime,      //  9 (FS_BASE + 9)
    no_sys,        // 10 (FS_BASE + 10)
    no_sys,        // 11 (FS_BASE + 11)
    no_sys,        // 12 (FS_BASE + 12)
    no_sys,        // 13 (FS_BASE + 13)
    no_sys,        // 14 (FS_BASE + 14)
    fs_unmount,    // 15 (FS_BASE + 15)
    fs_sync,       // 16 (FS_BASE + 16)
    no_sys,        // 17 (FS_BASE + 17)
    fs_flush,      // 18 (FS_BASE + 18)
    fs_readwrite,  // 19 (FS_BASE + 19)
    fs_readwrite,  // 20 (FS_BASE + 20)
    no_sys,        // 21 (FS_BASE + 21)
    no_sys,        // 22 (FS_BASE + 22)
    no_sys,        // 23 (FS_BASE + 23)
    no_sys,        // 24 (FS_BASE + 24)
    no_sys,        // 25 (FS_BASE + 25)
    no_sys,        // 26 (FS_BASE + 26)
    fs_mountpoint, // 27 (FS_BASE + 27)
    no_sys,        // 28 (FS_BASE + 28)
    fs_newnode,    // 29 (FS_BASE + 29)
    no_sys,        // 30 (FS_BASE + 30)
    no_sys,        // 31 (FS_BASE + 31)
    no_sys,        // 32 (FS_BASE + 32)
];

/// Dispatch a request by index into the call vector.
pub fn dispatch(ind: usize) -> i32 {
    if ind >= FS_CALL_VEC_SIZE {
        return EINVAL;
    }
    FS_CALL_VEC[ind]()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_size() {
        assert_eq!(FS_CALL_VEC.len(), FS_CALL_VEC_SIZE);
    }

    #[test]
    fn test_dispatch_oob() {
        assert_eq!(dispatch(FS_CALL_VEC_SIZE), EINVAL);
    }

    #[test]
    fn test_dispatch_no_sys() {
        assert_eq!(dispatch(0), EINVAL);
    }

    #[test]
    fn test_dispatch_sync() {
        assert_eq!(dispatch(16), OK);
    }

    #[test]
    fn test_dispatch_unmount() {
        // fs_unmount returns OK when no filesystem is mounted
        unsafe {
            crate::pfs::glo::pfs_init_globals();
        }
        // dispatch returns i32 from function call
        assert_eq!(dispatch(15), OK);
    }
}
