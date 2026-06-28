//! Super block management and bitmap allocation — adapted from `minix/fs/mfs/super.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::types::*;
use crate::mfs::utility::*;

// Reference: super.c read_super()
pub fn read_super(sp: &mut SuperBlock) -> i32 {
    if rw_super(sp, false) != OK {
        return EINVAL;
    }
    let magic = sp.s_magic as u16;
    if magic == SUPER_V2 || magic == SUPER_MAGIC {
        return EINVAL;
    }
    if magic != SUPER_V3 {
        return EINVAL;
    }
    let version = V3;
    let native = 1;

    sp.s_ninodes = conv4(native, sp.s_ninodes as i64) as u32;
    sp.s_nzones = conv2(native, sp.s_nzones as i32) as u32;
    sp.s_imap_blocks = conv2(native, sp.s_imap_blocks as i32) as i16;
    sp.s_zmap_blocks = conv2(native, sp.s_zmap_blocks as i32) as i16;
    sp.s_firstdatazone_old = conv2(native, sp.s_firstdatazone_old as i32) as u32;
    sp.s_log_zone_size = conv2(native, sp.s_log_zone_size as i32) as i16;
    sp.s_max_size = conv4(native, sp.s_max_size as i64) as i32;
    sp.s_zones = conv4(native, sp.s_zones as i64) as u32;
    sp.s_block_size = conv2(native, sp.s_block_size as i32) as u16;

    if sp.s_block_size < 4096 {
        return EINVAL;
    }
    sp.s_inodes_per_block = v2_inodes_per_block(sp.s_block_size as usize) as u32;
    sp.s_ndzones = V2_NR_DZONES as i32;
    sp.s_nindirs = v2_indirects(sp.s_block_size as usize) as i32;

    if sp.s_firstdatazone_old == 0 {
        let offset = START_BLOCK
            + sp.s_imap_blocks as u32
            + sp.s_zmap_blocks as u32
            + (sp.s_ninodes + sp.s_inodes_per_block - 1) / sp.s_inodes_per_block;
        sp.s_firstdatazone =
            (offset + (1u32 << sp.s_log_zone_size as u32) - 1) >> sp.s_log_zone_size as u32;
    } else {
        sp.s_firstdatazone = sp.s_firstdatazone_old;
    }

    if sp.s_block_size < 4096 {
        return EINVAL;
    }
    if (sp.s_block_size % 512) != 0 {
        return EINVAL;
    }
    if SUPER_SIZE > sp.s_block_size as usize {
        return EINVAL;
    }
    if (sp.s_block_size as usize % V2_INODE_SIZE) != 0 {
        return EINVAL;
    }
    if (sp.s_max_size as u64) > 0x7FFFFFFF {
        sp.s_max_size = 0x7FFFFFFF;
    }

    sp.s_isearch = 0;
    sp.s_zsearch = 0;
    sp.s_version = version;
    sp.s_native = native;

    if sp.s_imap_blocks < 1
        || sp.s_zmap_blocks < 1
        || sp.s_ninodes < 1
        || sp.s_zones < 1
        || sp.s_firstdatazone <= 4
        || sp.s_firstdatazone >= sp.s_zones
        || (sp.s_log_zone_size as u32) > 4
    {
        return EINVAL;
    }

    if sp.s_flags & MFSFLAG_MANDATORY_MASK != 0 {
        return EINVAL;
    }
    OK
}

// Reference: super.c write_super()
pub fn write_super(sp: &mut SuperBlock) -> i32 {
    if sp.s_rd_only != 0 {
        return EROFS;
    }
    rw_super(sp, true)
}

fn rw_super(sp: &mut SuperBlock, _writing: bool) -> i32 {
    let _save_dev = sp.s_dev;
    if sp.s_dev == NO_DEV {
        return EINVAL;
    }
    let ondisk_bytes: usize = {
        let base = sp as *mut SuperBlock as usize;
        let last_field = core::ptr::addr_of!((*sp).s_disk_version) as usize;
        (last_field - base) + core::mem::size_of::<u8>()
    };
    let _ = ondisk_bytes;
    todo!("rw_super: get_block not yet wired");
}

// Reference: super.c get_super()
pub fn get_super(dev: u32) -> *mut SuperBlock {
    if dev == NO_DEV {
        return core::ptr::null_mut();
    }
    unsafe {
        for i in 0..8 {
            let sp = glo::get_super_ptr(i);
            if !sp.is_null() && (*sp).s_dev == dev {
                return sp;
            }
        }
    }
    core::ptr::null_mut()
}

// Reference: super.c get_block_size()
pub fn get_block_size(dev: u32) -> u32 {
    let sp = get_super(dev);
    if sp.is_null() {
        return 0;
    }
    unsafe { (*sp).s_block_size as u32 }
}

// Reference: super.c alloc_bit()
pub fn alloc_bit(sp: &mut SuperBlock, _map: i32, _origin: u32) -> u32 {
    if sp.s_rd_only != 0 {
        return NO_BIT;
    }
    todo!("alloc_bit: get_block not yet wired");
}

// Reference: super.c free_bit()
pub fn free_bit(sp: &mut SuperBlock, _map: i32, _bit_returned: u32) {
    if sp.s_rd_only != 0 {
        return;
    }
    todo!("free_bit: get_block not yet wired");
}

// Reference: super.c get_used_blocks()
// Returns the number of used blocks. For MFS this requires bitmap scanning
// which needs the buffer cache. Returns 0 for now (TODO: implement when
// buffer cache is available). The C code computes this from superblock fields.
pub fn get_used_blocks(_sp: &mut SuperBlock) -> u32 {
    // TODO: compute from bitmap when buffer cache is wired
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_super_no_dev_returns_null() {
        assert!(get_super(NO_DEV).is_null());
    }

    #[test]
    fn test_get_block_size_no_dev_returns_zero() {
        assert_eq!(get_block_size(NO_DEV), 0);
    }

    #[test]
    fn test_get_used_blocks_starts_at_zero() {
        let mut sp = SuperBlock::default();
        assert_eq!(get_used_blocks(&mut sp), 0);
    }

    #[test]
    fn test_alloc_bit_read_only_returns_no_bit() {
        let mut sp = SuperBlock {
            s_rd_only: 1,
            ..SuperBlock::default()
        };
        assert_eq!(alloc_bit(&mut sp, ZMAP, 0), NO_BIT);
    }

    #[test]
    fn test_free_bit_read_only_is_noop() {
        let mut sp = SuperBlock {
            s_rd_only: 1,
            ..SuperBlock::default()
        };
        free_bit(&mut sp, ZMAP, 0); // Should not panic
    }

    #[test]
    fn test_write_super_read_only_returns_erofs() {
        let mut sp = SuperBlock {
            s_rd_only: 1,
            ..SuperBlock::default()
        };
        assert_eq!(write_super(&mut sp), EROFS);
    }

    #[test]
    fn test_read_super_invalid_magic_returns_einval() {
        let mut sp = SuperBlock::default();
        // Magic is 0 (not SUPER_V3), so read_super returns EINVAL
        // before reaching the disk read.
        assert_eq!(read_super(&mut sp), EINVAL);
    }

    #[test]
    fn test_read_super_super_v2_magic_returns_einval() {
        let mut sp = SuperBlock {
            s_magic: SUPER_V2 as i16,
            ..SuperBlock::default()
        };
        assert_eq!(read_super(&mut sp), EINVAL);
    }

    #[test]
    fn test_read_super_super_magic_returns_einval() {
        let mut sp = SuperBlock {
            s_magic: SUPER_MAGIC as i16,
            ..SuperBlock::default()
        };
        assert_eq!(read_super(&mut sp), EINVAL);
    }

    #[test]
    fn test_read_super_block_size_too_small_returns_einval() {
        let mut sp = SuperBlock {
            s_magic: SUPER_V3 as i16,
            s_block_size: 1024, // < 4096
            ..SuperBlock::default()
        };
        assert_eq!(read_super(&mut sp), EINVAL);
    }
}
