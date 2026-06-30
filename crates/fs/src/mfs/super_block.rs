//! Super block management and bitmap allocation — adapted from `minix/fs/mfs/super.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::types::*;
use crate::mfs::utility::*;
use libs::libminixfs::cache::{lmfs_get_block, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::FULL_DATA_BLOCK;

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

fn rw_super(sp: &mut SuperBlock, writing: bool) -> i32 {
    let save_dev = sp.s_dev;
    if sp.s_dev == NO_DEV {
        return EINVAL;
    }
    let ondisk_bytes: usize = {
        let base = sp as *mut SuperBlock as usize;
        let last_field = core::ptr::addr_of!((*sp).s_disk_version) as usize;
        (last_field - base) + core::mem::size_of::<u8>()
    };

    // The superblock is stored at offset SUPER_BLOCK_BYTES within block 0
    let bp = unsafe { lmfs_get_block(sp.s_dev, 0) };
    if bp.is_null() {
        return EIO;
    }

    // sbbuf points into the cached block at the superblock offset
    let sbbuf = unsafe { (*bp).data_ptr.add(SUPER_BLOCK_BYTES as usize) };

    if writing {
        // Zero the entire block, then copy on-disk fields into position
        unsafe {
            core::ptr::write_bytes((*bp).data_ptr, 0, (*bp).lmfs_bytes as usize);
            core::ptr::copy_nonoverlapping(sp as *mut SuperBlock as *const u8, sbbuf, ondisk_bytes);
            lmfs_markdirty(bp);
        }
    } else {
        // Zero the in-memory superblock, then copy on-disk fields from disk
        unsafe {
            core::ptr::write_bytes(
                sp as *mut SuperBlock as *mut u8,
                0,
                core::mem::size_of::<SuperBlock>(),
            );
            core::ptr::copy_nonoverlapping(sbbuf, sp as *mut SuperBlock as *mut u8, ondisk_bytes);
        }
        sp.s_dev = save_dev;
    }

    unsafe { lmfs_put_block(bp, libs::libminixfs::constants::FULL_DATA_BLOCK) };
    OK
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
pub fn alloc_bit(sp: &mut SuperBlock, map: i32, origin: u32) -> u32 {
    if sp.s_rd_only != 0 {
        return NO_BIT;
    }

    // Determine start_block, map_bits, bit_blocks based on map type
    let (start_block, map_bits, bit_blocks) = if map == IMAP {
        (START_BLOCK, sp.s_ninodes + 1, sp.s_imap_blocks as u32)
    } else {
        (
            START_BLOCK + sp.s_imap_blocks as u32,
            sp.s_zones - (sp.s_firstdatazone - 1),
            sp.s_zmap_blocks as u32,
        )
    };

    // Figure out where to start the bit search (depends on 'origin')
    let mut origin = origin;
    if origin >= map_bits {
        origin = 0;
    }

    let bits_per_block = fs_bits_per_block(sp.s_block_size as usize) as u32;
    let mut block = origin / bits_per_block;
    let mut word = ((origin % bits_per_block) / FS_BITCHUNK_BITS as u32) as usize;

    // Iterate over all blocks plus one, because we start in the middle
    let mut bcount = bit_blocks + 1;
    loop {
        let bp = unsafe { lmfs_get_block(sp.s_dev, (start_block + block) as u64) };
        if bp.is_null() {
            return NO_BIT;
        }

        let data_ptr = unsafe { (*bp).data_ptr };
        let wlim = fs_bitmap_chunks(sp.s_block_size as usize);

        // Iterate over the words in this block starting from 'word'
        for wptr_idx in word..wlim {
            let wptr = unsafe {
                &mut *(data_ptr.add(wptr_idx * core::mem::size_of::<BitchunkT>()) as *mut BitchunkT)
            };

            // Does this word contain a free bit?
            if *wptr == !0 {
                continue;
            }

            // Find and allocate the free bit
            let mut k = conv4(sp.s_native, *wptr as i64) as BitchunkT;
            let mut i: u32 = 0;
            while (k & (1 << i)) != 0 {
                i += 1;
            }

            // Bit number from the start of the bit map
            let b = block * bits_per_block
                + (wptr_idx as u32) * FS_BITCHUNK_BITS as u32
                + i;

            // Don't allocate bits beyond the end of the map
            if b >= map_bits {
                break;
            }

            // Allocate and return bit number
            k |= 1 << i;
            *wptr = conv4(sp.s_native, k as i64) as BitchunkT;
            unsafe {
                lmfs_markdirty(bp);
                lmfs_put_block(bp, FULL_DATA_BLOCK);
            }
            return b;
        }

        unsafe { lmfs_put_block(bp, FULL_DATA_BLOCK) };

        block += 1;
        if block >= bit_blocks {
            block = 0;
        }
        word = 0;

        bcount -= 1;
        if bcount == 0 {
            break;
        }
    }

    NO_BIT
}

// Reference: super.c free_bit()
pub fn free_bit(sp: &mut SuperBlock, map: i32, bit_returned: u32) {
    if sp.s_rd_only != 0 {
        return;
    }

    // Determine start_block based on map type
    let start_block = if map == IMAP {
        START_BLOCK
    } else {
        START_BLOCK + sp.s_imap_blocks as u32
    };

    let bits_per_block = fs_bits_per_block(sp.s_block_size as usize) as u32;
    let block = bit_returned / bits_per_block;
    let word = ((bit_returned % bits_per_block) / FS_BITCHUNK_BITS as u32) as usize;
    let bit = (bit_returned % FS_BITCHUNK_BITS as u32) as usize;
    let mask: BitchunkT = 1 << bit;

    let bp = unsafe { lmfs_get_block(sp.s_dev, (start_block + block) as u64) };
    if bp.is_null() {
        return;
    }

    let data_ptr = unsafe { (*bp).data_ptr };
    let wptr = unsafe {
        &mut *(data_ptr.add(word * core::mem::size_of::<BitchunkT>()) as *mut BitchunkT)
    };

    let mut k = conv4(sp.s_native, *wptr as i64) as BitchunkT;

    // The C code panics if the bit was already free; match that behavior.
    if (k & mask) == 0 {
        if map == IMAP {
            panic!("tried to free unused inode");
        } else {
            panic!("tried to free unused block: {}", bit_returned);
        }
    }

    k &= !mask;
    *wptr = conv4(sp.s_native, k as i64) as BitchunkT;
    unsafe {
        lmfs_markdirty(bp);
        lmfs_put_block(bp, FULL_DATA_BLOCK);
    }
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
