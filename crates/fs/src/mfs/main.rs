//! MFS main server loop — adapted from `minix/fs/mfs/main.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::misc::*;

// Reference: main.c sef_cb_init_fresh()
pub fn mfs_init() -> i32 {
    unsafe {
        glo::mfs_init_globals();
        for i in 0..NR_INODES {
            let inode_ptr = glo::get_inode_ptr(i);
            (*inode_ptr).i_count = 0;
            (*glo::mfs_ptr()).cch[i] = 0;
        }
        init_inode_cache();
    }
    OK
}

// Reference: main.c main()
pub fn mfs_main() -> i32 {
    mfs_init();
    OK
}

// Reference: main.c sef_cb_signal_handler()
pub fn signal_handler(signo: i32) {
    if signo != 15 {
        return;
    }
    unsafe {
        (*glo::mfs_ptr()).exitsignaled = TRUE;
    }
    fs_sync();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_mfs_init() {
        assert_eq!(mfs_init(), OK);
    }
}
