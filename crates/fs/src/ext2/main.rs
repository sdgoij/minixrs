//! Main server loop — adapted from `minix/fs/ext2/main.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::misc::*;
use crate::ext2::read::*;
use crate::ext2::table::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// Initialize the ext2 file server.
pub unsafe fn init_server() -> i32 {
    // Initialize globals
    glo::ext2_init_globals();

    // Set default options
    let opt_ptr = core::ptr::addr_of_mut!(glo::OPT);
    (*opt_ptr).use_orlov = TRUE;
    (*opt_ptr).mfsalloc = FALSE;
    (*opt_ptr).use_reserved_blocks = FALSE;
    (*opt_ptr).block_with_super = 0;
    (*opt_ptr).use_prealloc = FALSE;

    // Init inode table
    for i in 0..NR_INODES {
        let rip = glo::get_inode_ptr(i);
        (*rip).i_count = 0;
    }

    init_inode_cache();

    OK
}

/// Main message processing loop.
pub unsafe fn main_loop() -> i32 {
    loop {
        let ext2 = glo::ext2_ptr();

        // Check termination condition
        if (*ext2).unmountdone != 0 && (*ext2).exitsignaled != 0 {
            break;
        }

        // TODO: receive message from VFS
        // For now, break after first iteration as a stub
        break;
    }
    OK
}

/// Signal handler for cleanup.
pub unsafe fn signal_handler(_signo: i32) {
    let ext2 = glo::ext2_ptr();
    (*ext2).exitsignaled = 1;
    fs_sync();

    if (*ext2).unmountdone != 0 {
        // exit(0) would be called here in C
    }
}
