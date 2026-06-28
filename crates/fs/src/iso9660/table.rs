//! ISO 9660 VFS dispatch table — adapted from `minix/fs/iso9660fs/table.c`
//!
//! Maps system call numbers (FS_BASE + n) to handler functions.
//!
//! | idx | Call          | Handler        |
//! |-----|---------------|----------------|
//! |  0  | (unused)      | no_sys         |
//! |  1  | (unused)      | no_sys         |
//! |  2  | PUT_INODE     | fs_putnode     |
//! |  3  | (unused)      | no_sys         |
//! |  4  | (unused)      | no_sys         |
//! |  5  | (unused)      | no_sys         |
//! |  6  | (unused)      | no_sys         |
//! |  7  | (noop)        | do_noop        |
//! |  8  | STAT          | fs_stat        |
//! |  9  | (unused)      | no_sys         |
//! | 10  | STATVFS       | fs_statvfs     |
//! | 11  | BREAD         | fs_bread       |
//! | 12  | (unused)      | no_sys         |
//! | 13  | (unused)      | no_sys         |
//! | 14  | (unused)      | no_sys         |
//! | 15  | UNMOUNT       | fs_unmount     |
//! | 16  | SYNC          | fs_sync        |
//! | 17  | NEW_DRIVER    | fs_new_driver  |
//! | 18  | (unused)      | no_sys         |
//! | 19  | READ          | fs_readwrite   |
//! | 20  | (unused)      | no_sys         |
//! | 21  | (unused)      | no_sys         |
//! | 22  | (unused)      | no_sys         |
//! | 23  | (unused)      | no_sys         |
//! | 24  | (unused)      | no_sys         |
//! | 25  | (unused)      | no_sys         |
//! | 26  | LOOKUP        | fs_lookup      |
//! | 27  | MOUNTPOINT    | fs_mountpoint  |
//! | 28  | READSUPER     | fs_readsuper   |
//! | 29  | (unused)      | no_sys         |
//! | 30  | (unused)      | no_sys         |
//! | 31  | GETDENTS      | fs_getdents    |
//! | 32  | (unused)      | no_sys         |
//! | 33  | (unused)      | no_sys         |

use crate::iso9660::consts::*;
use crate::iso9660::inode;
use crate::iso9660::misc;
use crate::iso9660::mount;
use crate::iso9660::path;
use crate::iso9660::read;
use crate::iso9660::stadir;
use crate::iso9660::utility;

/// The dispatch table as an array of unsafe fn pointers.
/// 34 entries matching the MFS convention (FS_BASE + 0..33).
const TABLE: [unsafe fn() -> i32; 34] = [
    utility::no_sys,      //  0: not used
    utility::no_sys,      //  1: not used
    inode::fs_putnode,    //  2
    utility::no_sys,      //  3: not used
    utility::no_sys,      //  4: not used
    utility::no_sys,      //  5: not used
    utility::no_sys,      //  6: not used
    utility::do_noop,     //  7
    stadir::fs_stat,      //  8
    utility::no_sys,      //  9: not used
    stadir::fs_statvfs,   // 10
    read::fs_bread,       // 11
    utility::no_sys,      // 12: not used
    utility::no_sys,      // 13: not used
    utility::no_sys,      // 14: not used
    mount::fs_unmount,    // 15
    misc::fs_sync,        // 16
    misc::fs_new_driver,  // 17
    utility::no_sys,      // 18: not used
    read::fs_readwrite,   // 19
    utility::no_sys,      // 20: not used
    utility::no_sys,      // 21: not used
    utility::no_sys,      // 22: not used
    utility::no_sys,      // 23: not used
    utility::no_sys,      // 24: not used
    utility::no_sys,      // 25: not used
    path::fs_lookup,      // 26
    mount::fs_mountpoint, // 27
    mount::fs_readsuper,  // 28
    utility::no_sys,      // 29: not used
    utility::no_sys,      // 30: not used
    read::fs_getdents,    // 31
    utility::no_sys,      // 32: not used
    utility::no_sys,      // 33: not used
];

/// Dispatch a VFS call by index.
///
/// # Safety
///
/// The caller must ensure exclusive access to globals.
pub unsafe fn dispatch_call(ind: usize) -> i32 {
    if ind >= TABLE.len() {
        return EINVAL;
    }
    TABLE[ind]()
}

#[cfg(test)]
mod tests {
    use super::TABLE;

    #[test]
    fn table_has_34_entries() {
        assert_eq!(TABLE.len(), 34);
    }

    #[test]
    fn table_entries_are_not_null() {
        for (i, &handler) in TABLE.iter().enumerate() {
            assert!(handler as usize != 0, "entry {} is null", i);
        }
    }
}
