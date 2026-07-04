//! Memory device driver — null, zero, mem, kmem devices.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/memory/memory.c`
//!
//! Provides character-device access to:
//!
//! - `/dev/null` — data sink (write discards, read returns EOF)
//! - `/dev/zero` — null byte stream (read returns zeros, write discards)
//! - `/dev/mem` — physical memory (needs `vm_map_phys`, deferred)
//! - `/dev/kmem` — kernel virtual memory (needs `vm_map_phys`, deferred)
//!
//! RAM disk block devices are in the separate `ramdisk` module (see 11b.4).

use crate::DriverError;
use core::ptr::addr_of_mut;

// Minor device numbers (from `dmap.h`)

pub const MEM_DEV: usize = 1;
pub const KMEM_DEV: usize = 2;
pub const NULL_DEV: usize = 3;
pub const BOOT_DEV: usize = 4;
pub const ZERO_DEV: usize = 5;
pub const IMGRD_DEV: usize = 6;
pub const RAM_DEV_FIRST: usize = 7;

// State

/// Per-device open count.
const NR_DEVS: usize = 7 + 6; // 7 special + 6 ramdisks
static mut OPENCT: [i32; NR_DEVS] = [0; NR_DEVS];

fn openct_ptr() -> *mut [i32; NR_DEVS] {
    addr_of_mut!(OPENCT)
}

// Character device API

/// Open a memory device.
pub fn mem_open(minor: usize, _access: i32) -> Result<(), DriverError> {
    match minor {
        NULL_DEV | ZERO_DEV | MEM_DEV | KMEM_DEV => {
            unsafe {
                (*openct_ptr())[minor] += 1;
            }
            Ok(())
        }
        _ => Err(DriverError::NotFound),
    }
}

/// Close a memory device.
pub fn mem_close(minor: usize) -> Result<(), DriverError> {
    match minor {
        NULL_DEV | ZERO_DEV | MEM_DEV | KMEM_DEV => {
            unsafe {
                if (*openct_ptr())[minor] > 0 {
                    (*openct_ptr())[minor] -= 1;
                }
            }
            Ok(())
        }
        _ => Err(DriverError::NotFound),
    }
}

/// Read from a memory device.
///
/// Returns the number of bytes read, or an error.
pub fn mem_read(minor: usize, position: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
    match minor {
        NULL_DEV => {
            // /dev/null read returns EOF.
            Ok(0)
        }
        ZERO_DEV => {
            // /dev/zero read returns zeros.
            buf.fill(0);
            Ok(buf.len())
        }
        MEM_DEV => {
            // /dev/mem: read from physical memory.
            let src = position as *const u8;
            for (i, byte) in buf.iter_mut().enumerate() {
                *byte = unsafe { core::ptr::read_volatile(src.add(i)) };
            }
            Ok(buf.len())
        }
        KMEM_DEV => {
            // /dev/kmem: read from kernel virtual memory.
            let src = position as *const u8;
            for (i, byte) in buf.iter_mut().enumerate() {
                *byte = unsafe { core::ptr::read_volatile(src.add(i)) };
            }
            Ok(buf.len())
        }
        _ => Err(DriverError::NotFound),
    }
}

/// Write to a memory device.
///
/// Returns the number of bytes written, or an error.
pub fn mem_write(minor: usize, position: u64, buf: &[u8]) -> Result<usize, DriverError> {
    match minor {
        NULL_DEV | ZERO_DEV => {
            // /dev/null and /dev/zero discard all writes.
            Ok(buf.len())
        }
        MEM_DEV => {
            // /dev/mem: write to physical memory.
            let dst = position as *mut u8;
            for (i, byte) in buf.iter().enumerate() {
                unsafe {
                    core::ptr::write_volatile(dst.add(i), *byte);
                }
            }
            Ok(buf.len())
        }
        KMEM_DEV => {
            // /dev/kmem: write to kernel virtual memory.
            let dst = position as *mut u8;
            for (i, byte) in buf.iter().enumerate() {
                unsafe {
                    core::ptr::write_volatile(dst.add(i), *byte);
                }
            }
            Ok(buf.len())
        }
        _ => Err(DriverError::NotFound),
    }
}

/// Check if a minor device number is valid for this driver.
pub fn mem_is_valid(minor: usize) -> bool {
    matches!(minor, NULL_DEV | ZERO_DEV | MEM_DEV | KMEM_DEV)
}

/// Get the open count for a device.
pub fn mem_open_count(minor: usize) -> i32 {
    unsafe {
        if minor < NR_DEVS {
            (*openct_ptr())[minor]
        } else {
            0
        }
    }
}

/// Initialize the memory driver.
///
/// # Safety
///
/// Must be called once with exclusive access.
pub unsafe fn mem_init() {
    // SAFETY: caller guarantees exclusive access.
    let oc = unsafe { &mut *openct_ptr() };
    for item in oc.iter_mut() {
        *item = 0;
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn reset() {
        // SAFETY: single-threaded test context.
        let oc = unsafe { &mut *openct_ptr() };
        for item in oc.iter_mut() {
            *item = 0;
        }
    }


    #[test]
    fn test_minor_constants() {
        assert_eq!(MEM_DEV, 1);
        assert_eq!(KMEM_DEV, 2);
        assert_eq!(NULL_DEV, 3);
        assert_eq!(BOOT_DEV, 4);
        assert_eq!(ZERO_DEV, 5);
        assert_eq!(IMGRD_DEV, 6);
        assert_eq!(RAM_DEV_FIRST, 7);
    }


    #[test]
    fn test_null_open_close() {
        unsafe {
            reset();
        }
        assert!(mem_open(NULL_DEV, 0).is_ok());
        assert_eq!(mem_open_count(NULL_DEV), 1);
        assert!(mem_close(NULL_DEV).is_ok());
        assert_eq!(mem_open_count(NULL_DEV), 0);
    }

    #[test]
    fn test_zero_open_close() {
        unsafe {
            reset();
        }
        assert!(mem_open(ZERO_DEV, 0).is_ok());
        assert_eq!(mem_open_count(ZERO_DEV), 1);
        assert!(mem_close(ZERO_DEV).is_ok());
        assert_eq!(mem_open_count(ZERO_DEV), 0);
    }

    #[test]
    fn test_invalid_minor() {
        assert!(mem_open(0, 0).is_err());
        assert!(mem_open(99, 0).is_err());
    }

    #[test]
    fn test_close_unopened() {
        unsafe {
            reset();
        }
        assert!(mem_close(NULL_DEV).is_ok()); // no-op
        assert_eq!(mem_open_count(NULL_DEV), 0);
    }


    #[test]
    fn test_null_read() {
        let mut buf = [0xABu8; 64];
        let n = mem_read(NULL_DEV, 0, &mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_zero_read() {
        let mut buf = [0xABu8; 64];
        let n = mem_read(ZERO_DEV, 0, &mut buf).unwrap();
        assert_eq!(n, 64);
        assert_eq!(buf, [0u8; 64]);
    }

    #[test]
    fn test_zero_read_large() {
        let mut buf = [0xFFu8; 1024];
        let n = mem_read(ZERO_DEV, 0, &mut buf).unwrap();
        assert_eq!(n, 1024);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_invalid_read() {
        let mut buf = [0u8; 16];
        assert!(mem_read(0, 0, &mut buf).is_err());
    }


    #[test]
    fn test_null_write() {
        let buf = [0xABu8; 64];
        let n = mem_write(NULL_DEV, 0, &buf).unwrap();
        assert_eq!(n, 64);
    }

    #[test]
    fn test_zero_write() {
        let buf = [0xABu8; 64];
        let n = mem_write(ZERO_DEV, 0, &buf).unwrap();
        assert_eq!(n, 64);
    }

    #[test]
    fn test_invalid_write() {
        let buf = [0u8; 16];
        assert!(mem_write(0, 0, &buf).is_err());
    }


    #[test]
    fn test_mem_is_valid() {
        assert!(mem_is_valid(NULL_DEV));
        assert!(mem_is_valid(ZERO_DEV));
        assert!(mem_is_valid(MEM_DEV));
        assert!(mem_is_valid(KMEM_DEV));
        assert!(!mem_is_valid(0));
        assert!(!mem_is_valid(99));
    }


    #[test]
    fn test_mem_init() {
        unsafe {
            reset();
            mem_open(NULL_DEV, 0).unwrap();
            mem_init();
            assert_eq!(mem_open_count(NULL_DEV), 0);
        }
    }
}
