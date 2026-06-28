//! File read operations — adapted from `minix/fs/mfs/read.c`

use crate::mfs::consts::*;
use crate::mfs::glo;

// Reference: read.c fs_readwrite()
pub fn fs_readwrite() -> i32 {
    todo!("fs_readwrite: not yet wired");
}

// Reference: read.c fs_breadwrite()
pub fn fs_breadwrite() -> i32 {
    todo!("fs_breadwrite: not yet wired");
}

// Reference: read.c read_map()
pub fn read_map(rip_idx: u16, position: i64, _opportunistic: i32) -> u32 {
    unsafe {
        let rip = &*glo::get_inode_ptr(rip_idx as usize);
        let sp = match rip.i_sp.as_ref() {
            Some(s) => s,
            None => return NO_BLOCK,
        };
        let scale = sp.s_log_zone_size as u64;
        let block_pos = (position as u64) / sp.s_block_size as u64;
        let zone = block_pos >> scale;
        let boff = (block_pos - (zone << scale)) as i32;
        let dzones = rip.i_ndzones as u64;

        if zone < dzones {
            let z = rip.i_zone[zone as usize];
            if z == NO_ZONE {
                return NO_BLOCK;
            }
            return (z << scale as u32) + boff as u32;
        }
        todo!("read_map: indirect block not yet wired");
    }
}

// Reference: read.c get_block_map()
pub fn get_block_map(rip_idx: u16, position: u64) -> *mut u8 {
    let _b = read_map(rip_idx, position as i64, 0);
    todo!("get_block_map: buffer cache not yet wired");
}

// Reference: read.c rd_indir()
pub fn rd_indir(_bp: *mut u8, _index: i32) -> u32 {
    todo!("rd_indir: indirect block access not yet wired");
}

pub fn read_ahead() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe { crate::mfs::glo::mfs_init_globals(); }
    }

    #[test]
    fn test_read_ahead_is_noop() {
        // read_ahead is deliberately a no-op; verify it doesn't panic.
        read_ahead();
    }

    #[test]
    fn test_read_map_no_super_returns_no_block() {
        // After init, inode_table[0].i_sp is None → read_map returns NO_BLOCK.
        init();
        assert_eq!(read_map(0, 0, 0), NO_BLOCK);
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_readwrite_panics() {
        fs_readwrite();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_breadwrite_panics() {
        fs_breadwrite();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_get_block_map_panics() {
        get_block_map(0, 0);
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_rd_indir_panics() {
        let bp: *mut u8 = core::ptr::null_mut();
        rd_indir(bp, 0);
    }
}
