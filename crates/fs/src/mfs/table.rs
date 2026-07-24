//! VFS dispatch table — adapted from `minix/fs/mfs/table.c`
//!
//! Maps FS request numbers (FS_BASE + n) to handler functions.
//! Index 0..33 corresponds to request codes FS_BASE through FS_BASE+33.

use crate::mfs::consts::*;
use crate::mfs::inode::*;
use crate::mfs::link::*;
use crate::mfs::misc::*;
use crate::mfs::mount::*;
use crate::mfs::open::*;
use crate::mfs::path::*;
use crate::mfs::protect::*;
use crate::mfs::read::*;
use crate::mfs::time::*;
use crate::mfs::utility::*;
use crate::mfs::write::*;

/// Dispatch table: 34 entries indexed by `req_nr - FS_BASE`.
// Reference: table.c fs_call_vec[]
pub static FS_CALL_VEC: [fn() -> i32; NREQS] = [
    no_sys,        //  0 (FS_BASE + 0)  not used
    no_sys,        //  1 (FS_BASE + 1)  was fs_getnode
    fs_putnode,    //  2 (FS_BASE + 2)
    fs_slink,      //  3 (FS_BASE + 3)
    fs_ftrunc,     //  4 (FS_BASE + 4)
    fs_chown,      //  5 (FS_BASE + 5)
    fs_chmod,      //  6 (FS_BASE + 6)
    fs_inhibread,  //  7 (FS_BASE + 7)
    no_sys,        //  8 (FS_BASE + 8)  fs_stat
    fs_utime,      //  9 (FS_BASE + 9)
    no_sys,        // 10 (FS_BASE + 10) fs_statvfs
    fs_breadwrite, // 11 (FS_BASE + 11)
    fs_breadwrite, // 12 (FS_BASE + 12)
    fs_unlink,     // 13 (FS_BASE + 13)
    fs_unlink,     // 14 (FS_BASE + 14)
    fs_unmount,    // 15 (FS_BASE + 15)
    fs_sync,       // 16 (FS_BASE + 16)
    fs_new_driver, // 17 (FS_BASE + 17)
    fs_flush,      // 18 (FS_BASE + 18)
    fs_readwrite,  // 19 (FS_BASE + 19)
    fs_readwrite,  // 20 (FS_BASE + 20)
    fs_mknod,      // 21 (FS_BASE + 21)
    fs_mkdir,      // 22 (FS_BASE + 22)
    fs_create,     // 23 (FS_BASE + 23)
    fs_link,       // 24 (FS_BASE + 24)
    fs_rename,     // 25 (FS_BASE + 25)
    fs_lookup,     // 26 (FS_BASE + 26)
    fs_mountpoint, // 27 (FS_BASE + 27)
    fs_readsuper,  // 28 (FS_BASE + 28)
    no_sys,        // 29 (FS_BASE + 29) was fs_newnode
    fs_rdlink,     // 30 (FS_BASE + 30)
    fs_getdents,   // 31 (FS_BASE + 31)
    fs_readwrite,  // 32 (FS_BASE + 32)
    fs_bpeek,      // 33 (FS_BASE + 33)
];

/// Dispatch a request by index into the call vector.
pub fn dispatch(ind: usize) -> i32 {
    if ind >= NREQS {
        return EINVAL;
    }
    FS_CALL_VEC[ind]()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_size() {
        assert_eq!(FS_CALL_VEC.len(), NREQS);
    }

    #[test]
    fn test_dispatch_oob() {
        assert_eq!(dispatch(NREQS), EINVAL);
    }

    #[test]
    fn test_dispatch_no_sys() {
        assert_eq!(dispatch(0), EINVAL); // no_sys returns EINVAL
    }
}
