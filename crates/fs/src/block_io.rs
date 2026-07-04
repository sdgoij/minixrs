//! Block I/O provider — direct-memory RAM disk.
//!
//! Provides a [`libs::libminixfs::cache::BlockIoFn`] callback that reads/writes
//! blocks from/to a contiguous memory region.  This is used by the MFS server
//! during early boot before a full block driver process is available.
//!
//! # Usage
//!
//! ```ignore
//! unsafe {
//!     crate::block_io::ram_disk_init(base_ptr, image_size);
//!     libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::ram_disk_io);
//! }
//! ```

use core::ptr;

/// Block size for the RAM disk (must match the FS block size — 4096 for Minix V3).
pub const RAM_DISK_BLOCK_SIZE: usize = 4096;

/// Static storage for the RAM disk base pointer and size.
static mut BASE: *const u8 = core::ptr::null();
static mut SIZE: usize = 0;

/// Initialize the RAM disk with a base pointer and size.
///
/// # Safety
///
/// `base` must point to a valid memory region of at least `size` bytes
/// that remains valid for the lifetime of the process.
pub unsafe fn ram_disk_init(base: *const u8, size: usize) {
    unsafe {
        BASE = base;
        SIZE = size;
    }
}

/// Check if the RAM disk has been initialized.
pub fn ram_disk_is_initialized() -> bool {
    unsafe { !BASE.is_null() && SIZE > 0 }
}

/// Block I/O callback for a direct-memory RAM disk.
///
/// Compatible with [`libs::libminixfs::cache::BlockIoFn`].
/// Reads/writes blocks from/to the memory region set by [`ram_disk_init`].
///
/// # Safety
///
/// Must only be called after [`ram_disk_init`] with valid parameters.
pub unsafe fn ram_disk_io(
    dev: u32,
    block: u64,
    nblocks: usize,
    bufs: *const *mut u8,
    block_size: usize,
    rw_flag: i32,
) -> i32 {
    let base = unsafe { BASE };
    let size = unsafe { SIZE };
    if base.is_null() || size == 0 {
        return -5; // EIO
    }
    let _ = dev;

    let offset = (block as usize).saturating_mul(block_size);
    let total = nblocks.saturating_mul(block_size);
    if offset.saturating_add(total) > size {
        return -5; // EIO — out of range
    }

    let src = unsafe { base.add(offset) };
    match rw_flag {
        0 => {
            // READING: memcpy from disk image to buffers.
            for i in 0..nblocks {
                let buf = unsafe { *bufs.add(i) };
                if buf.is_null() {
                    return -(i as i32) - 1;
                }
                unsafe {
                    ptr::copy_nonoverlapping(src.add(i * block_size), buf, block_size);
                }
            }
        }
        1 => {
            // WRITING: memcpy from buffers to disk image.
            for i in 0..nblocks {
                let buf = unsafe { *bufs.add(i) };
                if buf.is_null() {
                    return -(i as i32) - 1;
                }
                unsafe {
                    ptr::copy_nonoverlapping(
                        buf as *const u8,
                        src.add(i * block_size) as *mut u8,
                        block_size,
                    );
                }
            }
        }
        _ => return -22, // EINVAL
    }
    nblocks as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ram_disk_read_write() {
        let mut disk = [0u8; 4096];
        let pattern: [u8; 16] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10,
        ];
        disk[0..16].copy_from_slice(&pattern);

        unsafe { ram_disk_init(disk.as_ptr(), disk.len()) };

        // Read block 0.
        let mut buf = [0u8; 1024];
        let bufs = [buf.as_mut_ptr(); 1];
        let n = unsafe { ram_disk_io(0, 0, 1, bufs.as_ptr(), 1024, 0) };
        assert_eq!(n, 1);
        assert_eq!(&buf[0..16], &pattern);

        // Write block 2.
        let write_data = [0xFFu8; 1024];
        let write_bufs = [write_data.as_ptr() as *mut u8; 1];
        let n = unsafe { ram_disk_io(0, 2, 1, write_bufs.as_ptr(), 1024, 1) };
        assert_eq!(n, 1);
        assert_eq!(&disk[2048..3072], &write_data);
    }

    #[test]
    fn test_out_of_bounds() {
        let disk = [0u8; 2048];
        unsafe { ram_disk_init(disk.as_ptr(), disk.len()) };
        let mut buf = [0u8; 1024];
        let bufs = [buf.as_mut_ptr(); 1];
        let n = unsafe { ram_disk_io(0, 2, 1, bufs.as_ptr(), 1024, 0) };
        assert!(n < 0);
    }

    #[test]
    fn test_multiblock_read() {
        let mut disk = [0u8; 4096];
        for i in 0..4 {
            disk[i * 1024] = (i + 1) as u8;
        }
        unsafe { ram_disk_init(disk.as_ptr(), disk.len()) };

        let mut b0 = [0u8; 1024];
        let mut b1 = [0u8; 1024];
        let mut b2 = [0u8; 1024];
        let bufs = [b0.as_mut_ptr(), b1.as_mut_ptr(), b2.as_mut_ptr()];
        let n = unsafe { ram_disk_io(0, 0, 3, bufs.as_ptr(), 1024, 0) };
        assert_eq!(n, 3);
        assert_eq!(b0[0], 1);
        assert_eq!(b1[0], 2);
        assert_eq!(b2[0], 3);
    }
}
