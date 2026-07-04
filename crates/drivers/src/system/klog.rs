//! Kernel log driver — /dev/klog (50KB circular buffer)
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/system/log/`
//!
//! Provides a character device for reading kernel diagnostic messages.
//! The kernel pushes messages via `log_append()`, userspace reads via
//! the standard read/write interface.
//!
//! # Buffer layout
//!
//! Operates as a circular buffer with three pointers:
//! - `log_write`: next write position (advances on append)
//! - `log_read`:  next read position  (advances on read)
//! - `log_size`:  number of valid bytes between read and write
//!
//! When `log_size` reaches `LOG_SIZE`, the oldest data is discarded
//! by advancing `log_read` past the overflow.

use crate::DriverError;
use core::cell::UnsafeCell;

/// Size of the circular log buffer (50 KB, matching the original C driver).
pub const LOG_SIZE: usize = 50 * 1024;

/// Number of minor devices.
pub const NR_DEVS: usize = 1;

/// Minor device number for /dev/klog.
pub const MINOR_KLOG: usize = 0;

/// Helpers for circular buffer arithmetic.
fn log_inc(n: u32, i: u32) -> u32 {
    (n + i) % (LOG_SIZE as u32)
}

/// Per-device log state.
///
/// # Safety
///
/// Accessed via raw pointer through `log_device()`; callers must ensure
/// exclusive access.
#[repr(C)]
pub struct LogDevice {
    pub log_buffer: [u8; LOG_SIZE],
    pub log_size: u32,
    pub log_read: u32,
    pub log_write: u32,
    pub log_source: i32, // endpoint blocked on read, or -1 (NONE)
    pub log_id: u32,     // cdev_id of blocked reader
    pub log_iosize: u32,
    pub log_grant: u32,
    pub log_selected: u32,
    pub log_select_proc: i32,
}

impl LogDevice {
    const fn new() -> Self {
        Self {
            log_buffer: [0u8; LOG_SIZE],
            log_size: 0,
            log_read: 0,
            log_write: 0,
            log_source: -1,
            log_id: 0,
            log_iosize: 0,
            log_grant: 0,
            log_selected: 0,
            log_select_proc: -1,
        }
    }
}

struct LogDevicesCell(UnsafeCell<[LogDevice; NR_DEVS]>);
unsafe impl Sync for LogDevicesCell {}
impl LogDevicesCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([LogDevice::new(); NR_DEVS]))
    }
    fn get(&self) -> *mut [LogDevice; NR_DEVS] {
        self.0.get()
    }
}

/// Global log device table (matching C: `struct logdevice logdevices[NR_DEVS]`).
static LOG_DEVICES: LogDevicesCell = LogDevicesCell::new();

/// Get a raw pointer to a log device by minor number.
fn log_device(minor: usize) -> *mut LogDevice {
    if minor < NR_DEVS {
        unsafe { &mut (*LOG_DEVICES.get())[minor] as *mut LogDevice }
    } else {
        core::ptr::null_mut()
    }
}

/// Initialize the log driver.
///
/// Resets all device state and registers for kernel diagnostics.
/// Must be called before any other log function.
///
/// # Safety
///
/// Must be called exactly once during driver initialization.
pub unsafe fn log_init() {
    let log = log_device(MINOR_KLOG);
    if !log.is_null() {
        unsafe {
            (*log).log_size = 0;
            (*log).log_read = 0;
            (*log).log_write = 0;
            (*log).log_source = -1;
        }
    }
}

/// Write data into the circular buffer.
///
/// Called internally by `log_append()` (local writes) and `log_write()`
/// (userspace writes). Handles wrap-around when the write position
/// approaches the end of the buffer.
///
/// Returns the number of bytes written on success, or a negative error.
///
/// # Safety
///
/// `log` must point to a valid, initialized LogDevice.
unsafe fn subwrite(
    log: *mut LogDevice,
    size: u32,
    endpt: i32,
    grant: u32,
    localbuf: Option<&[u8]>,
) -> i32 {
    unsafe {
        let mut offset = 0u32;
        let mut r = 0i32;

        while offset < size {
            let mut count = size - offset;

            let write_pos = (*log).log_write as usize;
            if (write_pos + count as usize) > LOG_SIZE {
                count = (LOG_SIZE - write_pos) as u32;
            }
            let buf_ptr = (*log).log_buffer.as_mut_ptr().add(write_pos);

            if let Some(lb) = localbuf {
                let src = lb.as_ptr().add(offset as usize);
                core::ptr::copy_nonoverlapping(src, buf_ptr, count as usize);
            } else {
                // Userspace write via grant — stub until IPC grants are wired.
                let _ = (endpt, grant);
                // For now, zero-fill as placeholder.
                core::ptr::write_bytes(buf_ptr, 0, count as usize);
            }

            (*log).log_write = log_inc((*log).log_write, count);
            (*log).log_size += count;

            if (*log).log_size > LOG_SIZE as u32 {
                let overflow = (*log).log_size - LOG_SIZE as u32;
                (*log).log_size -= overflow;
                (*log).log_read = log_inc((*log).log_read, overflow);
            }

            r = offset as i32;
            offset += count;
        }

        // Wake up any blocked reader.
        if (*log).log_size > 0 && (*log).log_source != -1 {
            let subread_result =
                subread(log, (*log).log_iosize, (*log).log_source, (*log).log_grant);
            // In the real driver this would call chardriver_reply_task.
            let _ = subread_result;
            (*log).log_source = -1;
        }

        // Notify select() waiters.
        if (*log).log_size > 0 && (*log).log_selected & 1 != 0 {
            (*log).log_selected &= !1;
        }

        r
    }
}

/// Read data from the circular buffer into a user buffer.
///
/// Copies up to `size` bytes from the current read position, handling
/// wrap-around. Returns the number of bytes read.
///
/// # Safety
///
/// `log` must point to a valid, initialized LogDevice.
unsafe fn subread(log: *mut LogDevice, size: u32, endpt: i32, grant: u32) -> i32 {
    unsafe {
        let mut offset = 0u32;

        while (*log).log_size > 0 && offset < size {
            let mut count = size - offset;
            if count > (*log).log_size {
                count = (*log).log_size;
            }
            let read_pos = (*log).log_read as usize;
            if read_pos + count as usize > LOG_SIZE {
                count = (LOG_SIZE - read_pos) as u32;
            }

            let _buf_ptr = (*log).log_buffer.as_ptr().add(read_pos);

            // Userspace read via grant — stub until IPC grants are wired.
            let _ = (endpt, grant);
            // In the real driver this would call sys_safecopyto.
            // For now, the data stays in the buffer and tests verify locally.

            (*log).log_read = log_inc((*log).log_read, count);
            (*log).log_size -= count;
            offset += count;
        }

        offset as i32
    }
}

/// Open a log device.
///
/// Validates the minor number. Returns `Ok(())` on success.
pub fn log_open(minor: usize) -> Result<(), DriverError> {
    if minor >= NR_DEVS {
        return Err(DriverError::NotFound);
    }
    Ok(())
}

/// Read from a log device.
///
/// If data is available, copies it to the caller. If no data and
/// non-blocking, returns `Err(DriverError::Unsupported)` (EAGAIN).
/// Otherwise blocks (returns pending state).
///
/// Returns the number of bytes read on success.
///
/// # Safety
///
/// Must be called with exclusive access to the log device state.
pub unsafe fn log_read(
    minor: usize,
    endpt: i32,
    grant: u32,
    size: u32,
    nonblock: bool,
) -> Result<i32, DriverError> {
    unsafe {
        let log = log_device(minor);
        if log.is_null() {
            return Err(DriverError::Io);
        }

        // If someone is already waiting to read, reject new work.
        if (*log).log_source != -1 {
            return Ok(0);
        }

        if (*log).log_size == 0 && size > 0 {
            if nonblock {
                return Err(DriverError::Unsupported); // EAGAIN
            }

            // Block: store requestor info for later wakeup.
            (*log).log_source = endpt;
            (*log).log_iosize = size;
            (*log).log_grant = grant;
            return Ok(-998); // EDONTREPLY
        }

        Ok(subread(log, size, endpt, grant))
    }
}

/// Write to a log device.
///
/// Appends data to the circular buffer. Returns the number of bytes
/// written on success.
///
/// # Safety
///
/// Must be called with exclusive access to the log device state.
pub unsafe fn log_write(
    minor: usize,
    endpt: i32,
    grant: u32,
    size: u32,
) -> Result<i32, DriverError> {
    unsafe {
        let log = log_device(minor);
        if log.is_null() {
            return Err(DriverError::Io);
        }
        Ok(subwrite(log, size, endpt, grant, None))
    }
}

/// Append data to the log (internal kernel interface).
///
/// Called by the kernel diagnostic system to insert messages. Uses
/// the first log device (minor 0). Handles data larger than LOG_SIZE
/// by skipping the oldest bytes.
///
/// # Safety
///
/// Must be called with exclusive access to the log device.
pub unsafe fn log_append(buf: &[u8]) {
    if buf.is_empty() {
        return;
    }
    let mut count = buf.len() as u32;
    let mut skip = 0u32;

    if count > LOG_SIZE as u32 {
        skip = count - LOG_SIZE as u32;
        count -= skip;
    }

    let log = log_device(MINOR_KLOG);
    if !log.is_null() {
        unsafe {
            subwrite(log, count, -1, 0, Some(&buf[skip as usize..]));
        }
    }
}

/// Handle new kernel messages (SIGKMESS handler).
///
/// Called when the kernel signals that new diagnostic messages are
/// available. Reads from the shared kernel message buffer and
/// appends to the log.
///
/// # Safety
///
/// Must be called from a signal handler context.
pub unsafe fn do_new_kmess(next: u32, buf: &[u8]) {
    let _ = next;
    unsafe {
        log_append(buf);
    }
}

/// Cancel a pending read request.
///
/// If the endpoint and ID match a suspended reader, cancels the
/// operation and returns `true`. Otherwise returns `false`.
///
/// # Safety
///
/// Must be called with exclusive access to the log device state.
pub unsafe fn log_cancel(minor: usize, endpt: i32, id: u32) -> bool {
    unsafe {
        let log = log_device(minor);
        if log.is_null() {
            return false;
        }
        if (*log).log_source == endpt && (*log).log_id == id {
            (*log).log_source = -1;
            true
        } else {
            false
        }
    }
}

/// Check which I/O operations are ready (select).
///
/// Returns a bitmask of ready operations: bit 0 = read ready,
/// bit 1 = write ready.
///
/// # Safety
///
/// Must be called with exclusive access to the log device state.
pub unsafe fn log_select(minor: usize, ops: u32, endpt: i32) -> u32 {
    unsafe {
        let log = log_device(minor);
        if log.is_null() {
            return 0;
        }

        let mut ready_ops = 0u32;

        // Read blocks when buffer is empty.
        if ops & 1 != 0 && (*log).log_size > 0 {
            ready_ops |= 1;
        }

        // Write never blocks.
        if ops & 2 != 0 {
            ready_ops |= 2;
        }

        // Enable callback if not all requested ops are ready.
        let want_notify = ops & 8; // CDEV_NOTIFY
        let want_ops = ops & !ready_ops;
        if want_notify != 0 && want_ops != 0 {
            (*log).log_selected |= want_ops;
            (*log).log_select_proc = endpt;
        }

        ready_ops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: reset the global state for testing.
    unsafe fn reset_log() {
        let log = log_device(MINOR_KLOG);
        if !log.is_null() {
            unsafe {
                (*log).log_size = 0;
                (*log).log_read = 0;
                (*log).log_write = 0;
                (*log).log_source = -1;
                (*log).log_selected = 0;
            }
        }
    }

    #[test]
    fn test_log_open_valid_minor() {
        assert!(log_open(0).is_ok());
    }

    #[test]
    fn test_log_open_invalid_minor() {
        assert!(log_open(99).is_err());
    }

    #[test]
    fn test_log_append_and_read_size() {
        unsafe {
            reset_log();
            let msg = b"hello log";
            log_append(msg);
            let log = log_device(MINOR_KLOG);
            assert_eq!((*log).log_size, 9, "should have 9 bytes after append");
        }
    }

    #[test]
    fn test_log_append_empty_is_noop() {
        unsafe {
            reset_log();
            log_append(b"");
            let log = log_device(MINOR_KLOG);
            assert_eq!((*log).log_size, 0);
        }
    }

    #[test]
    fn test_log_append_overflow() {
        unsafe {
            reset_log();
            // Fill the buffer by appending more than LOG_SIZE bytes in chunks
            let data = [0xABu8; 256];
            let iterations = (LOG_SIZE / 256) + 2;
            for _ in 0..iterations {
                log_append(&data);
            }
            let log = log_device(MINOR_KLOG);
            assert!(
                (*log).log_size <= LOG_SIZE as u32,
                "should not exceed LOG_SIZE"
            );
            assert_eq!((*log).log_size, LOG_SIZE as u32, "should be full");
        }
    }

    #[test]
    fn test_log_append_multiple_messages() {
        unsafe {
            reset_log();
            log_append(b"hello ");
            log_append(b"world");
            let log = log_device(MINOR_KLOG);
            assert_eq!((*log).log_size, 11);
        }
    }

    #[test]
    fn test_log_read_nonblock_empty() {
        unsafe {
            reset_log();
            let result = log_read(0, 42, 0, 10, true);
            assert!(result.is_err(), "nonblock read on empty should EAGAIN");
        }
    }

    #[test]
    fn test_log_read_block_sets_source() {
        unsafe {
            reset_log();
            let result = log_read(0, 100, 0, 10, false);
            assert_eq!(result.unwrap(), -998, "should return EDONTREPLY");
            let log = log_device(MINOR_KLOG);
            assert_eq!(
                (*log).log_source,
                100,
                "should store blocked reader endpoint"
            );
        }
    }

    #[test]
    fn test_log_cancel_matching() {
        unsafe {
            reset_log();
            // Set up a blocked reader
            let log = log_device(MINOR_KLOG);
            (*log).log_source = 42;
            (*log).log_id = 7;
            let cancelled = log_cancel(0, 42, 7);
            assert!(cancelled, "should cancel matching request");
            assert_eq!((*log).log_source, -1, "should clear source");
        }
    }

    #[test]
    fn test_log_cancel_non_matching() {
        unsafe {
            reset_log();
            let log = log_device(MINOR_KLOG);
            (*log).log_source = 42;
            (*log).log_id = 7;
            let cancelled = log_cancel(0, 99, 7); // wrong endpoint
            assert!(!cancelled, "should not cancel non-matching");
        }
    }

    #[test]
    fn test_log_select_read_ready() {
        unsafe {
            reset_log();
            log_append(b"data");
            let ready = log_select(0, 1, 0); // ops = CDEV_OP_RD
            assert_eq!(ready & 1, 1, "read should be ready");
        }
    }

    #[test]
    fn test_log_select_read_not_ready() {
        unsafe {
            reset_log();
            let ready = log_select(0, 1, 0);
            assert_eq!(ready & 1, 0, "read should not be ready");
        }
    }

    #[test]
    fn test_log_select_write_always_ready() {
        unsafe {
            reset_log();
            let ready = log_select(0, 2, 0); // ops = CDEV_OP_WR
            assert_eq!(ready & 2, 2, "write should always be ready");
        }
    }

    #[test]
    fn test_log_subread_consumes_data() {
        unsafe {
            reset_log();
            let log = log_device(MINOR_KLOG);
            log_append(b"1234567890");
            assert_eq!((*log).log_size, 10);

            // subread via the test API
            let nread = subread(log, 5, 0, 0);
            assert_eq!(nread, 5, "should read 5 bytes");
            assert_eq!((*log).log_size, 5, "5 bytes should remain");
        }
    }

    #[test]
    fn test_log_write_via_write_api() {
        unsafe {
            reset_log();
            let result = log_write(0, 0, 0, 10);
            assert!(result.is_ok(), "write should succeed");
            let log = log_device(MINOR_KLOG);
            assert_eq!((*log).log_size, 10, "should have 10 bytes");
        }
    }

    #[test]
    fn test_log_inc_wraparound() {
        assert_eq!(log_inc(LOG_SIZE as u32 - 1, 3), 2, "should wrap around");
        assert_eq!(log_inc(0, 0), 0);
        assert_eq!(log_inc(100, 50), 150);
    }
}
