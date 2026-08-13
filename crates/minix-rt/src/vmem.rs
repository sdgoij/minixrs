//! VM memory operations: `mmap`/`munmap` message protocol.
//!
//! Lives in `minix-rt` (not `minix-std`) so the runtime's own allocator can
//! map chunks with it; `minix-std::vmem` re-exports these.

/// VM call-number base for memory requests.
pub const VM_RQ_BASE: u32 = 0xC00;
/// Map memory (VM_MMAP = VM_RQ_BASE + 10 = 0xC0A).
pub const VM_MMAP: u32 = VM_RQ_BASE + 10;
/// Unmap memory (VM_MUNMAP = VM_RQ_BASE + 17 = 0xC11).
pub const VM_MUNMAP: u32 = VM_RQ_BASE + 17;

pub const PROT_READ: i32 = 0x01;
pub const PROT_WRITE: i32 = 0x02;
pub const PROT_EXEC: i32 = 0x04;
pub const PROT_NONE: i32 = 0x00;

pub const MAP_SHARED: i32 = 0x01;
pub const MAP_PRIVATE: i32 = 0x02;
pub const MAP_FIXED: i32 = 0x10;
pub const MAP_ANONYMOUS: i32 = 0x20;
pub const MAP_FAILED: *mut u8 = usize::MAX as *mut u8;

// The kernel `Message` layout (see `crates/arch-common/src/ipc.rs`):
//   [0..4] = m_source (filled by the kernel), [4..8] = m_type,
//   [8..56] = m_payload (m1 fields at payload offset 0..).
const OFF_TYPE: usize = 4;

// VM_MMAP / VM_MUNMAP — message layout matching VM server protocol
// (offsets are absolute message-byte offsets; the VM server reads them
// relative to `m_payload.raw`, which starts at byte 8).
const OFF_VM_RET: usize = 8; // u64 — reply: mapped address in m1i1|m1i2
const OFF_VM_PROT: usize = 12; // i32 — protection flags
const OFF_VM_FLAGS: usize = 16; // i32 — mapping flags (uses bytes 16-19)
const OFF_VM_LEN: usize = 20; // u64 — length (uses bytes 20-27)
const OFF_VM_ADDR: usize = 28; // u64 — address (uses bytes 28-35)
const OFF_VM_FD: usize = 36; // i32 — file descriptor

#[inline]
fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

#[inline]
fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

#[inline]
fn msg_u64(msg: &[u8; 64], off: usize) -> u64 {
    u64::from_ne_bytes(msg[off..off + 8].try_into().unwrap())
}

#[inline]
fn msg_set_u64(msg: &mut [u8; 64], off: usize, val: u64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

/// Send a VM call and validate the reply status (`m_type` at byte 4:
/// 0 = OK, negative = errno). The reply payload (e.g. a mapped address
/// for `mmap`) stays in the message buffer for the caller to read.
#[cfg(target_os = "minix")]
fn vm_call(msg: &mut [u8; 64]) -> Result<(), i32> {
    // VM_PROC_NR is 8; messages go via sendrec.
    let _ = crate::sendrec(crate::VM_PROC_NR, msg);
    let status = msg_i32(msg, OFF_TYPE);
    if status < 0 { Err(status) } else { Ok(()) }
}

/// Map memory pages.
///
/// `addr` is the desired virtual address (0 for any), `length` is the size
/// in bytes, `prot` is the protection flags (PROT_*), `flags` is the mapping
/// type (MAP_*), `fd` is the file descriptor (-1 for anonymous), `offset`
/// is the file offset.
///
/// Returns the mapped address on success, [`MAP_FAILED`] on error.
///
/// # Safety
///
/// The caller must ensure that the address range is valid and not
/// already mapped (unless MAP_FIXED is used).
pub unsafe fn mmap(
    addr: *mut u8,
    length: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> *mut u8 {
    #[cfg(target_os = "minix")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VM_MMAP as i32);
        msg_set_u64(&mut msg, OFF_VM_ADDR, addr as u64);
        msg_set_u64(&mut msg, OFF_VM_LEN, length as u64);
        msg_set_i32(&mut msg, OFF_VM_PROT, prot);
        msg_set_i32(&mut msg, OFF_VM_FLAGS, flags);
        msg_set_i32(&mut msg, OFF_VM_FD, fd);
        // offset is stored at bytes 40..48 (after fd at 36)
        msg[40..48].copy_from_slice(&offset.to_ne_bytes());

        if vm_call(&mut msg).is_ok() {
            // The reply carries the mapped address as a u64 in m1i1|m1i2
            // (message bytes 8..16).
            msg_u64(&msg, OFF_VM_RET) as *mut u8
        } else {
            MAP_FAILED
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (addr, length, prot, flags, fd, offset);
        MAP_FAILED
    }
}

/// Unmap memory pages.
///
/// `addr` is the starting address, `length` is the size in bytes.
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// The address range must have been previously mapped by `mmap`.
pub unsafe fn munmap(addr: *mut u8, length: usize) -> i32 {
    #[cfg(target_os = "minix")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VM_MUNMAP as i32);
        msg_set_u64(&mut msg, OFF_VM_ADDR, addr as u64);
        msg_set_u64(&mut msg, OFF_VM_LEN, length as u64);

        match vm_call(&mut msg) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (addr, length);
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vm_call_numbers() {
        assert_eq!(VM_RQ_BASE, 0xC00);
        assert_eq!(VM_MMAP, 0xC0A);
        assert_eq!(VM_MUNMAP, 0xC11);
    }

    #[test]
    fn test_protection_flags() {
        assert_eq!(PROT_READ, 0x01);
        assert_eq!(PROT_WRITE, 0x02);
        assert_eq!(PROT_EXEC, 0x04);
        assert_eq!(PROT_NONE, 0x00);
    }

    #[test]
    fn test_map_flags() {
        assert_eq!(MAP_SHARED, 0x01);
        assert_eq!(MAP_PRIVATE, 0x02);
        assert_eq!(MAP_FIXED, 0x10);
        assert_eq!(MAP_ANONYMOUS, 0x20);
        assert_eq!(MAP_FAILED as usize, usize::MAX);
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 8, 0xDEADBEEFu32 as i32);
        assert_eq!(msg_i32(&msg, 8), 0xDEADBEEFu32 as i32);
        msg_set_u64(&mut msg, 16, 0x0102030405060708);
        assert_eq!(msg_u64(&msg, 16), 0x0102030405060708);
    }

    #[test]
    fn test_mmap_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VM_MMAP as i32);
        msg_set_u64(&mut msg, OFF_VM_ADDR, 0x1234_5678 as u64);
        msg_set_u64(&mut msg, OFF_VM_LEN, 0x1000);
        msg_set_i32(&mut msg, OFF_VM_PROT, PROT_READ | PROT_WRITE);
        msg_set_i32(&mut msg, OFF_VM_FLAGS, MAP_PRIVATE | MAP_ANONYMOUS);
        msg_set_i32(&mut msg, OFF_VM_FD, -1);
        msg[40..48].copy_from_slice(&0x1000i64.to_ne_bytes());

        // m_type at bytes 4-7
        assert_eq!(msg_i32(&msg, OFF_TYPE), 0xC0A);
        // addr at bytes 28-35
        assert_eq!(msg_u64(&msg, OFF_VM_ADDR), 0x1234_5678);
        // len at bytes 20-27
        assert_eq!(msg_u64(&msg, OFF_VM_LEN), 0x1000);
        // prot at bytes 12-15
        assert_eq!(msg_i32(&msg, OFF_VM_PROT), PROT_READ | PROT_WRITE);
        // flags at bytes 16-19
        assert_eq!(msg_i32(&msg, OFF_VM_FLAGS), MAP_PRIVATE | MAP_ANONYMOUS);
        // fd at bytes 36-39
        assert_eq!(msg_i32(&msg, OFF_VM_FD), -1);
        // offset at bytes 40-47
        assert_eq!(msg_u64(&msg, 40), 0x1000);
    }

    #[test]
    fn test_munmap_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VM_MUNMAP as i32);
        msg_set_u64(&mut msg, OFF_VM_ADDR, 0x8000_0000 as u64);
        msg_set_u64(&mut msg, OFF_VM_LEN, 0x2000);
        assert_eq!(msg_i32(&msg, OFF_TYPE), 0xC11);
        assert_eq!(msg_u64(&msg, OFF_VM_ADDR), 0x8000_0000);
        assert_eq!(msg_u64(&msg, OFF_VM_LEN), 0x2000);
    }

    #[test]
    fn test_mmap_returns_failed_on_host() {
        // No VM server on the host: mmap must return MAP_FAILED cleanly.
        let p = unsafe { mmap(core::ptr::null_mut(), 4096, PROT_READ, MAP_PRIVATE, -1, 0) };
        assert_eq!(p, MAP_FAILED);
    }

    #[test]
    fn test_munmap_returns_minus_one_on_host() {
        assert_eq!(unsafe { munmap(core::ptr::null_mut(), 4096) }, -1);
    }
}
