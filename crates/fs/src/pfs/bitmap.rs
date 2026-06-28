//! Bitmap management for inode numbers — adapted from `minix/fs/pfs/super.c`
//!
//! PFS maintains a bitmap to track which inode numbers are in use.
//! Since PFS has no disk, this bitmap lives entirely in memory.

use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::types::*;

/// Allocate a bit from the inode bitmap and return its bit number.
///
/// Returns `NO_BIT` if no free inode is available.
// Reference: super.c alloc_bit()
pub fn alloc_bit() -> BitT {
    unsafe {
        let pfs = glo::pfs_ptr();
        let wlim = &raw mut (*pfs).inodemap[0];
        let wlim = wlim.add(INODEMAP_CHUNKS);

        let mut wptr = &raw mut (*pfs).inodemap[0];
        while wptr < wlim {
            let val = *wptr;

            // Does this word contain a free bit?
            if val != !0u32 {
                // Find the first free bit
                let mut i = 0u32;
                while (val & (1 << i)) != 0 {
                    i += 1;
                }

                // Calculate bit number (inode number)
                let chunk_index = (wptr as usize - &raw const (*pfs).inodemap[0] as usize)
                    / core::mem::size_of::<BitchunkT>();
                let b = (chunk_index * FS_BITCHUNK_BITS) + i as usize;

                // Don't allocate bits beyond the end of the map
                if b >= PFS_NR_INODES {
                    break;
                }

                // Allocate the bit
                *wptr = val | (1 << i);
                return b as BitT;
            }

            wptr = wptr.add(1);
        }

        NO_BIT
    }
}

/// Free a previously allocated bit in the inode bitmap.
///
/// # Safety
///
/// `bit_returned` must be a bit that was previously returned by `alloc_bit()`
/// and not already freed.
// Reference: super.c free_bit()
pub fn free_bit(bit_returned: BitT) {
    unsafe {
        let pfs = glo::pfs_ptr();
        let word = (bit_returned as usize) / FS_BITCHUNK_BITS;
        let bit = (bit_returned as usize) % FS_BITCHUNK_BITS;

        if word < INODEMAP_CHUNKS {
            let k = &raw mut (*pfs).inodemap[word];
            let mask = 1u32 << bit;
            *k &= !mask;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            // Ensure inodemap is all zeros (all bits free)
            let pfs = glo::pfs_ptr();
            for i in 0..INODEMAP_CHUNKS {
                (*pfs).inodemap[i] = 0;
            }
            // Reserve bit 0 (like init_inode_cache does) so the next
            // alloc_bit returns a value other than bit 0.
            // alloc_bit returns bit 0 both on success (bit 0 is free)
            // and on failure (NO_BIT = 0), so we cannot assert the result.
            let _ = alloc_bit();
        }
    }

    #[test]
    fn test_alloc_bit_returns_valid_range() {
        init();
        let b = alloc_bit();
        assert_ne!(b, NO_BIT);
        assert!((b as usize) < PFS_NR_INODES);
    }

    #[test]
    fn test_alloc_bit_marks_bit_in_use() {
        init();
        let b = alloc_bit();
        assert_ne!(b, NO_BIT);
        unsafe {
            let pfs = glo::pfs_ptr();
            let word = (b as usize) / FS_BITCHUNK_BITS;
            let bit = (b as usize) % FS_BITCHUNK_BITS;
            assert_ne!((*pfs).inodemap[word] & (1 << bit), 0);
        }
    }

    #[test]
    fn test_free_bit_clears_bit() {
        init();
        let b = alloc_bit();
        assert_ne!(b, NO_BIT);
        free_bit(b);
        unsafe {
            let pfs = glo::pfs_ptr();
            let word = (b as usize) / FS_BITCHUNK_BITS;
            let bit = (b as usize) % FS_BITCHUNK_BITS;
            assert_eq!((*pfs).inodemap[word] & (1 << bit), 0);
        }
    }

    #[test]
    fn test_alloc_all_bits_returns_no_bit() {
        init();
        unsafe {
            let pfs = glo::pfs_ptr();
            // Fill all chunks with all-ones
            for i in 0..INODEMAP_CHUNKS {
                (*pfs).inodemap[i] = !0u32;
            }
        }
        assert_eq!(alloc_bit(), NO_BIT);
    }

    #[test]
    fn test_alloc_and_free_roundtrip() {
        init();
        let b1 = alloc_bit();
        assert_ne!(b1, NO_BIT);
        free_bit(b1);
        let b2 = alloc_bit();
        assert_eq!(b1, b2);
    }
}
