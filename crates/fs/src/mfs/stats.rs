//! Filesystem statistics — adapted from `minix/fs/mfs/stats.c`

use crate::mfs::consts::{FULL_DATA_BLOCK, START_BLOCK};
use crate::mfs::types::*;
use libs::libminixfs::cache::{lmfs_get_block, lmfs_put_block};

// Reference: stats.c count_free_bits()
pub fn count_free_bits(sp: &SuperBlock, map: i32) -> u32 {
    let (start_block_base, bit_blocks) = if map == IMAP {
        (START_BLOCK as u64, sp.s_imap_blocks as u32)
    } else {
        (
            START_BLOCK as u64 + sp.s_imap_blocks as u64,
            sp.s_zmap_blocks as u32,
        )
    };
    if bit_blocks == 0 || sp.s_block_size == 0 {
        return 0;
    }
    let (map_bits, mut origin) = if map == IMAP {
        (sp.s_ninodes + 1, sp.s_isearch)
    } else {
        (
            sp.s_zones
                .saturating_sub(sp.s_firstdatazone.saturating_sub(1)),
            sp.s_zsearch,
        )
    };
    if origin >= map_bits {
        origin = 0;
    }
    let block_size = sp.s_block_size as usize;
    let bits_per_block = fs_bits_per_block(block_size) as u64;
    if bits_per_block == 0 {
        return 0;
    }
    let chunks = fs_bitmap_chunks(block_size);
    let mut block = origin as u64 / bits_per_block;
    let mut word = ((origin as u64 % bits_per_block) / FS_BITCHUNK_BITS as u64) as usize;
    let mut free_bits = 0u32;
    let mut bcount = bit_blocks;
    while bcount > 0 {
        let bp = unsafe { lmfs_get_block(sp.s_dev, start_block_base + block) };
        assert!(!bp.is_null());
        let data = unsafe { (*bp).data_ptr as *const BitchunkT };
        for wptr_idx in word..chunks {
            let w = unsafe { *data.add(wptr_idx) };
            // All bits set — skip entirely (word is ~0 regardless of endianness).
            if w == !0u32 {
                continue;
            }
            for i in 0..FS_BITCHUNK_BITS {
                let b =
                    block * bits_per_block + wptr_idx as u64 * FS_BITCHUNK_BITS as u64 + i as u64;
                if b >= map_bits as u64 {
                    break;
                }
                if (w & (1 << i)) == 0 {
                    free_bits += 1;
                }
            }
        }
        unsafe { lmfs_put_block(bp, FULL_DATA_BLOCK) };
        block += 1;
        word = 0;
        bcount -= 1;
    }
    free_bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_free_bits_zero_block_size() {
        let sp = SuperBlock::default();
        assert_eq!(count_free_bits(&sp, IMAP), 0);
        assert_eq!(count_free_bits(&sp, ZMAP), 0);
    }

    #[test]
    fn test_count_free_bits_zero_bit_blocks() {
        let mut sp = SuperBlock::default();
        sp.s_block_size = 1024;
        assert_eq!(count_free_bits(&sp, IMAP), 0);
        assert_eq!(count_free_bits(&sp, ZMAP), 0);
    }
}
