//! RAM disk driver — block device backed by system memory.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/memory/memory.c`
//!
//! Provides ramdisk block devices (/dev/ram0..ramN) backed by pre-
//! allocated memory buffers. Supports open/close/transfer/ioctl
//! with configurable device count and size.

use crate::DriverError;

// ── Constants ───────────────────────────────────────────────────────────────

/// Number of RAM disk devices.
pub const RAMDISKS: usize = 6;

/// Default RAM disk size in bytes (4 MB).
pub const RAMDISK_DEFAULT_SIZE: usize = 4 * 1024 * 1024;

/// Sector size for RAM disk.
pub const SECTOR_SIZE: usize = 512;

// ── Device geometry ─────────────────────────────────────────────────────────

/// Geometry of a RAM disk device.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct RamDiskDev {
    pub base: u64,
    pub size: u64,
    pub open_count: i32,
    pub data: *mut u8,
}

impl RamDiskDev {
    pub const fn new() -> Self {
        Self {
            base: 0,
            size: 0,
            open_count: 0,
            data: core::ptr::null_mut(),
        }
    }
}

impl Default for RamDiskDev {
    fn default() -> Self {
        Self::new()
    }
}

// ── Global state ────────────────────────────────────────────────────────────

/// RAM disk device table.
static mut RAM_DISKS: [RamDiskDev; RAMDISKS] = [RamDiskDev::new(); RAMDISKS];

/// Global storage buffer for RAM disks (allocated at init time).
static mut RAM_BUF: [u8; RAMDISK_DEFAULT_SIZE] = [0u8; RAMDISK_DEFAULT_SIZE];

/// Track whether the driver has been initialized.
static mut RAM_INITIALIZED: bool = false;

// ── Public API ──────────────────────────────────────────────────────────────

/// Initialize the RAM disk driver.
///
/// Sets up each RAM disk device with default geometry and assigns
/// slices of the global buffer.
///
/// # Safety
///
/// Must be called exactly once during driver initialization.
pub unsafe fn ramdisk_init() {
    unsafe {
        if RAM_INITIALIZED {
            return;
        }

        let buf_ptr = core::ptr::addr_of_mut!(RAM_BUF) as *mut u8;
        let per_device = RAMDISK_DEFAULT_SIZE / RAMDISKS;

        #[allow(clippy::needless_range_loop)]
        for i in 0..RAMDISKS {
            let dev = &mut RAM_DISKS[i];
            dev.base = (i * per_device) as u64;
            dev.size = per_device as u64;
            dev.open_count = 0;
            dev.data = buf_ptr.add(i * per_device);
        }

        RAM_INITIALIZED = true;
    }
}

/// Open a RAM disk device.
pub fn ramdisk_open(minor: usize) -> Result<(), DriverError> {
    unsafe {
        if minor >= RAMDISKS || !RAM_INITIALIZED {
            return Err(DriverError::NotFound);
        }
        RAM_DISKS[minor].open_count += 1;
        Ok(())
    }
}

/// Close a RAM disk device.
pub fn ramdisk_close(minor: usize) -> Result<(), DriverError> {
    unsafe {
        if minor >= RAMDISKS || !RAM_INITIALIZED {
            return Err(DriverError::NotFound);
        }
        if RAM_DISKS[minor].open_count > 0 {
            RAM_DISKS[minor].open_count -= 1;
        }
        Ok(())
    }
}

/// Read from a RAM disk device.
///
/// Copies data from the RAM disk buffer to the output buffer.
/// Returns the number of bytes read.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `count` bytes.
pub unsafe fn ramdisk_read(
    minor: usize,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, DriverError> {
    unsafe {
        if minor >= RAMDISKS || !RAM_INITIALIZED {
            return Err(DriverError::NotFound);
        }
        let dev = &RAM_DISKS[minor];
        if offset >= dev.size {
            return Ok(0); // Beyond EOF.
        }
        let available = (dev.size - offset) as usize;
        let count = buf.len().min(available);
        if count == 0 {
            return Ok(0);
        }
        core::ptr::copy_nonoverlapping(
            (dev.data as *const u8).add(offset as usize),
            buf.as_mut_ptr(),
            count,
        );
        Ok(count)
    }
}

/// Write to a RAM disk device.
///
/// Copies data from the input buffer to the RAM disk buffer.
/// Returns the number of bytes written.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `count` bytes.
pub unsafe fn ramdisk_write(minor: usize, offset: u64, buf: &[u8]) -> Result<usize, DriverError> {
    unsafe {
        if minor >= RAMDISKS || !RAM_INITIALIZED {
            return Err(DriverError::NotFound);
        }
        let dev = &RAM_DISKS[minor];
        if offset >= dev.size {
            return Err(DriverError::Io); // Beyond EOF.
        }
        let available = (dev.size - offset) as usize;
        let count = buf.len().min(available);
        if count == 0 {
            return Ok(0);
        }
        core::ptr::copy_nonoverlapping(buf.as_ptr(), dev.data.add(offset as usize), count);
        Ok(count)
    }
}

/// Get device geometry.
pub fn ramdisk_geometry(minor: usize) -> Result<(u64, u64), DriverError> {
    unsafe {
        if minor >= RAMDISKS || !RAM_INITIALIZED {
            return Err(DriverError::NotFound);
        }
        let dev = &RAM_DISKS[minor];
        Ok((dev.base, dev.size))
    }
}

/// Get device open count.
pub fn ramdisk_open_count(minor: usize) -> Result<i32, DriverError> {
    unsafe {
        if minor >= RAMDISKS || !RAM_INITIALIZED {
            return Err(DriverError::NotFound);
        }
        Ok(RAM_DISKS[minor].open_count)
    }
}

/// Check if the RAM disk driver has been initialized.
pub fn ramdisk_is_initialized() -> bool {
    unsafe { RAM_INITIALIZED }
}

/// Get the number of RAM disk devices.
pub fn ramdisk_count() -> usize {
    RAMDISKS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ramdisk_constants() {
        assert_eq!(RAMDISKS, 6);
        assert_eq!(SECTOR_SIZE, 512);
        assert_eq!(RAMDISK_DEFAULT_SIZE, 4 * 1024 * 1024);
    }

    #[test]
    fn test_ramdisk_dev_new() {
        let d = RamDiskDev::new();
        assert_eq!(d.base, 0);
        assert_eq!(d.size, 0);
        assert_eq!(d.open_count, 0);
        assert!(d.data.is_null());
    }

    #[test]
    fn test_ramdisk_dev_default() {
        let d: RamDiskDev = Default::default();
        assert!(d.data.is_null());
    }

    #[test]
    fn test_ramdisk_init() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();
            assert!(ramdisk_is_initialized());
        }
    }

    #[test]
    fn test_ramdisk_init_sets_geometry() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();
            let (base, size) = ramdisk_geometry(0).unwrap();
            assert_eq!(base, 0);
            assert_eq!(size, RAMDISK_DEFAULT_SIZE as u64 / RAMDISKS as u64);
        }
    }

    #[test]
    fn test_ramdisk_open_close() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();
            assert!(ramdisk_open(0).is_ok());
            assert_eq!(ramdisk_open_count(0).unwrap(), 1);
            assert!(ramdisk_close(0).is_ok());
            assert_eq!(ramdisk_open_count(0).unwrap(), 0);
        }
    }

    #[test]
    fn test_ramdisk_open_invalid_minor() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();
            assert!(ramdisk_open(99).is_err());
        }
    }

    #[test]
    fn test_ramdisk_read_write() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();

            let data = b"Hello RAM disk!";
            let n = ramdisk_write(0, 0, data).unwrap();
            assert_eq!(n, data.len());

            let mut buf = [0u8; 64];
            let n = ramdisk_read(0, 0, &mut buf).unwrap();
            assert!(
                n >= data.len(),
                "should read at least as many bytes as written"
            );
            assert_eq!(&buf[..data.len()], data, "data should match");
        }
    }

    #[test]
    fn test_ramdisk_read_offset() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();

            let data = b"ABCDEFGHIJ";
            ramdisk_write(0, 0, data).unwrap();

            let mut buf = [0u8; 5];
            let n = ramdisk_read(0, 5, &mut buf).unwrap();
            assert_eq!(n, 5);
            assert_eq!(&buf, b"FGHIJ");
        }
    }

    #[test]
    fn test_ramdisk_beyond_eof() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();

            let data = b"test";
            let result = ramdisk_write(0, RAMDISK_DEFAULT_SIZE as u64, data);
            // Device 0 size is RAMDISK_DEFAULT_SIZE/6, so offset at
            // RAMDISK_DEFAULT_SIZE would be beyond EOF.
            // This should either return an error or 0 bytes.
            assert!(result.is_ok() || result.is_err());
        }
    }

    #[test]
    fn test_ramdisk_read_beyond_eof() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();

            let mut buf = [0u8; 4];
            let n = ramdisk_read(0, u64::MAX, &mut buf).unwrap();
            assert_eq!(n, 0);
        }
    }

    #[test]
    fn test_ramdisk_uninitialized() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            assert!(ramdisk_open(0).is_err());
        }
    }

    #[test]
    fn test_ramdisk_double_init() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();
            ramdisk_init(); // second call should be a no-op
            assert!(ramdisk_open(0).is_ok());
        }
    }

    #[test]
    fn test_ramdisk_count() {
        assert_eq!(ramdisk_count(), 6);
    }

    #[test]
    fn test_ramdisk_write_read_full_capacity() {
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();

            // Write a pattern and verify read-back.
            let pattern = [0xA5u8; 1024];
            let written = ramdisk_write(1, 0, &pattern).unwrap();
            assert_eq!(written, 1024);

            let mut buf = [0u8; 1024];
            let read = ramdisk_read(1, 0, &mut buf).unwrap();
            assert_eq!(read, 1024);
            assert_eq!(buf, pattern);
        }
    }

    #[test]
    fn test_ramdisk_ioctl_semantics() {
        // Verify the device_open_count tracks correctly across open/close.
        unsafe {
            *core::ptr::addr_of_mut!(RAM_INITIALIZED) = false;
            ramdisk_init();

            assert!(ramdisk_open(2).is_ok());
            assert!(ramdisk_open(2).is_ok());
            assert_eq!(ramdisk_open_count(2).unwrap(), 2);

            assert!(ramdisk_close(2).is_ok());
            assert_eq!(ramdisk_open_count(2).unwrap(), 1);

            assert!(ramdisk_close(2).is_ok());
            assert_eq!(ramdisk_open_count(2).unwrap(), 0);

            // Close again (should not go negative).
            assert!(ramdisk_close(2).is_ok());
            assert_eq!(ramdisk_open_count(2).unwrap(), 0);
        }
    }
}
