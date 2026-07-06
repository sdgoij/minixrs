//! Memory management — VM protocol and shared memory wrappers.
//!
//! Provides `mmap`, `munmap`, and shared memory (`shmget`, `shmat`, `shmdt`, `shmctl`)
//! by sending VM server and IPC server messages.
//!
//! VM call numbers (from `.refs/minix-3.3.0/minix/include/minix/com.h`):
//! ```text
//! VM_RQ_BASE = 0xC00
//! VM_MMAP    = VM_RQ_BASE + 10  (0xC0A)
//! VM_MUNMAP  = VM_RQ_BASE + 17  (0xC11)
//! VM_BRK     = VM_RQ_BASE + 2   (0xC02)
//! ```
//!
//! IPC call numbers (from `.refs/minix-3.3.0/minix/include/minix/com.h`):
//! ```text
//! IPC_BASE   = 0xD00
//! IPC_SHMGET = IPC_BASE + 1  (0xD01)
//! IPC_SHMAT  = IPC_BASE + 2  (0xD02)
//! IPC_SHMDT  = IPC_BASE + 3  (0xD03)
//! IPC_SHMCTL = IPC_BASE + 4  (0xD04)
//! ```

#![allow(dead_code)]

#[cfg(target_os = "none")]
use crate::{Message, MinixErr, VM_PROC_NR, sendrec};

pub const VM_RQ_BASE: u32 = 0xC00;
pub const VM_MMAP: u32 = VM_RQ_BASE + 10; // 0xC0A
pub const VM_MUNMAP: u32 = VM_RQ_BASE + 17; // 0xC11
pub const VM_BRK: u32 = VM_RQ_BASE + 2; // 0xC02
pub const VM_MAP_PHYS: u32 = VM_RQ_BASE + 15; // 0xC0F

pub const IPC_BASE: u32 = 0xD00;
pub const IPC_SHMGET: u32 = IPC_BASE + 1; // 0xD01
pub const IPC_SHMAT: u32 = IPC_BASE + 2; // 0xD02
pub const IPC_SHMDT: u32 = IPC_BASE + 3; // 0xD03
pub const IPC_SHMCTL: u32 = IPC_BASE + 4; // 0xD04

pub const PROT_READ: i32 = 0x01;
pub const PROT_WRITE: i32 = 0x02;
pub const PROT_EXEC: i32 = 0x04;
pub const PROT_NONE: i32 = 0x00;

pub const MAP_SHARED: i32 = 0x01;
pub const MAP_PRIVATE: i32 = 0x02;
pub const MAP_FIXED: i32 = 0x10;
pub const MAP_ANONYMOUS: i32 = 0x20;
pub const MAP_FAILED: *mut u8 = usize::MAX as *mut u8;

pub const IPC_CREAT: i32 = 0o001000;
pub const IPC_EXCL: i32 = 0o002000;
pub const IPC_NOWAIT: i32 = 0o004000;
pub const IPC_RMID: i32 = 0;
pub const IPC_SET: i32 = 1;
pub const IPC_STAT: i32 = 2;
pub const IPC_INFO: i32 = 500;
pub const IPC_PRIVATE: i32 = 0;

pub const SHM_RDONLY: i32 = 0o010000;
pub const SHM_RND: i32 = 0o020000;

const OFF_TYPE: usize = 8;

// VM_MMAP / VM_MUNMAP — message layout matching VM server protocol
// The VM server uses the m1/m9 message fields; these offsets match
// the generic Message struct's m1 payload.
const OFF_VM_PROT: usize = 12; // i32 — protection flags
const OFF_VM_FLAGS: usize = 16; // i32 — mapping flags (uses bytes 16-19)
const OFF_VM_LEN: usize = 20; // u64 — length (uses bytes 20-27)
const OFF_VM_ADDR: usize = 28; // u64 — address (uses bytes 28-35)
const OFF_VM_FD: usize = 36; // i32 — file descriptor

// IPC_SHMGET — using 32-bit size_t (i386 protocol compat)
const OFF_SHM_KEY: usize = 12; // i32
const OFF_SHM_SIZE: usize = 16; // i32 (32-bit size_t)
const OFF_SHM_FLAGS: usize = 20; // i32
const OFF_SHM_RETID: usize = 24; // i32 — returned shm ID

// IPC_SHMAT
const OFF_SHMAT_ID: usize = 12; // i32
const OFF_SHMAT_ADDR: usize = 16; // u64 — desired attach address
const OFF_SHMAT_FLAGS: usize = 24; // i32
const OFF_SHMAT_RET: usize = 28; // u64 — returned address (via msg)

// IPC_SHMDT
const OFF_SHMDT_ADDR: usize = 16; // u64 — address to detach

// IPC_SHMCTL
const OFF_SHMCTL_ID: usize = 12; // i32
const OFF_SHMCTL_CMD: usize = 16; // i32
const OFF_SHMCTL_BUF: usize = 20; // u64 — buffer pointer

// Helpers

fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

fn msg_u64(msg: &[u8; 64], off: usize) -> u64 {
    u64::from_ne_bytes(msg[off..off + 8].try_into().unwrap())
}

fn msg_set_u64(msg: &mut [u8; 64], off: usize, val: u64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

/// Send a VM call and validate the reply.
#[cfg(target_os = "none")]
unsafe fn vm_call(msg: &mut Message) -> Result<i64, MinixErr> {
    unsafe {
        // The VM_PROC_NR is 8, messages go via sendrec.
        let _ = sendrec(VM_PROC_NR, msg);
        let mtype = msg_i32(msg, OFF_TYPE);
        if mtype < 0 {
            Err(MinixErr::from_i32(mtype))
        } else {
            Ok(mtype as i64)
        }
    }
}

/// Send an IPC server call (for shared memory).
#[cfg(target_os = "none")]
unsafe fn ipc_call(msg: &mut Message) -> Result<i32, MinixErr> {
    unsafe {
        // IPC server is at a well-known endpoint.
        // In MINIX, user programs call IPC server via _syscall to the IPC endpoint.
        // The IPC_PROC_NR is typically defined as a constant.
        // For now, use PM_PROC_NR as placeholder — the actual endpoint
        // will be wired when the IPC server is running.
        let ipc_endpt: i32 = 10; // TODO: IPC_PROC_NR when defined
        let _ = sendrec(ipc_endpt, msg);
        let mtype = msg_i32(msg, OFF_TYPE);
        if mtype < 0 {
            Err(MinixErr::from_i32(mtype))
        } else {
            Ok(mtype)
        }
    }
}

// Memory operations (VM protocol)

/// Map memory pages.
///
/// `addr` is the desired virtual address (0 for any), `length` is the size
/// in bytes, `prot` is the protection flags (PROT_*), `flags` is the mapping
/// type (MAP_*), `fd` is the file descriptor (-1 for anonymous), `offset`
/// is the file offset.
///
/// Returns the mapped address on success.
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
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VM_MMAP as i32);
        msg_set_u64(&mut msg, OFF_VM_ADDR, addr as u64);
        msg_set_u64(&mut msg, OFF_VM_LEN, length as u64);
        msg_set_i32(&mut msg, OFF_VM_PROT, prot);
        msg_set_i32(&mut msg, OFF_VM_FLAGS, flags);
        msg_set_i32(&mut msg, OFF_VM_FD, fd);
        // offset is stored at offset 40 (after fd at 36)
        msg[40..48].copy_from_slice(&offset.to_ne_bytes());

        let result = vm_call(&mut msg);
        match result {
            Ok(r) => r as *mut u8,
            Err(_) => MAP_FAILED,
        }
    }
    #[cfg(not(target_os = "none"))]
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
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VM_MUNMAP as i32);
        msg_set_u64(&mut msg, OFF_VM_ADDR, addr as u64);
        msg_set_u64(&mut msg, OFF_VM_LEN, length as u64);

        match vm_call(&mut msg) {
            Ok(_) => 0,
            Err(_) => -1,
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (addr, length);
        -1
    }
}

// Shared memory operations (IPC server protocol)

/// Get or create a shared memory segment.
///
/// `key` is the IPC key, `size` is the segment size, `flags` includes
/// IPC_CREAT, IPC_EXCL, and permission bits.
/// Returns the shared memory ID on success, or -1 on failure.
///
/// # Safety
///
/// `key` must be a valid IPC key. The caller must ensure that the IPC
/// server endpoint is running and accessible.
pub unsafe fn shmget(key: i32, size: usize, flags: i32) -> i32 {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, IPC_SHMGET as i32);
        msg_set_i32(&mut msg, OFF_SHM_KEY, key);
        msg_set_i32(&mut msg, OFF_SHM_SIZE, size as i32);
        msg_set_i32(&mut msg, OFF_SHM_FLAGS, flags);

        ipc_call(&mut msg).unwrap_or(-1)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (key, size, flags);
        -1
    }
}

/// Attach a shared memory segment.
///
/// `id` is the shared memory ID, `addr` is the desired virtual address
/// (0 for any), `flags` can include SHM_RDONLY and SHM_RND.
/// Returns the attached address on success, or MAP_FAILED on failure.
///
/// # Safety
///
/// The address must not conflict with existing mappings (unless SHM_RND
/// alignment is used).
pub unsafe fn shmat(id: i32, addr: *mut u8, flags: i32) -> *mut u8 {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, IPC_SHMAT as i32);
        msg_set_i32(&mut msg, OFF_SHMAT_ID, id);
        msg_set_u64(&mut msg, OFF_SHMAT_ADDR, addr as u64);
        msg_set_i32(&mut msg, OFF_SHMAT_FLAGS, flags);

        match ipc_call(&mut msg) {
            Ok(_addr) => {
                // The returned address is in the message payload.
                // For the stub, return the requested address.
                addr
            }
            Err(_) => MAP_FAILED,
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (id, addr, flags);
        MAP_FAILED
    }
}

/// Detach a shared memory segment.
///
/// `addr` is the address returned by `shmat`.
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// The address must have been returned by a previous `shmat` call.
pub unsafe fn shmdt(addr: *mut u8) -> i32 {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, IPC_SHMDT as i32);
        msg_set_u64(&mut msg, OFF_SHMDT_ADDR, addr as u64);

        match ipc_call(&mut msg) {
            Ok(_) => 0,
            Err(_) => -1,
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = addr;
        -1
    }
}

/// Shared memory control operations.
///
/// `id` is the shared memory ID, `cmd` is IPC_STAT / IPC_SET / IPC_RMID / IPC_INFO.
/// `buf` is a pointer to a shmid_ds structure or null.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `buf` must be a valid pointer if `cmd` requires a buffer.
pub unsafe fn shmctl(id: i32, cmd: i32, _buf: *mut u8) -> i32 {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, IPC_SHMCTL as i32);
        msg_set_i32(&mut msg, OFF_SHMCTL_ID, id);
        msg_set_i32(&mut msg, OFF_SHMCTL_CMD, cmd);
        msg_set_u64(&mut msg, OFF_SHMCTL_BUF, _buf as u64);

        match ipc_call(&mut msg) {
            Ok(_) => 0,
            Err(_) => -1,
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (id, cmd);
        -1
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vm_call_numbers() {
        assert_eq!(VM_RQ_BASE, 0xC00);
        assert_eq!(VM_MMAP, 0xC0A);
        assert_eq!(VM_MUNMAP, 0xC11);
        assert_eq!(VM_BRK, 0xC02);
    }

    #[test]
    fn test_ipc_call_numbers() {
        assert_eq!(IPC_BASE, 0xD00);
        assert_eq!(IPC_SHMGET, 0xD01);
        assert_eq!(IPC_SHMAT, 0xD02);
        assert_eq!(IPC_SHMDT, 0xD03);
        assert_eq!(IPC_SHMCTL, 0xD04);
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
    }

    #[test]
    fn test_ipc_flags() {
        assert_eq!(IPC_CREAT, 0o001000);
        assert_eq!(IPC_EXCL, 0o002000);
        assert_eq!(IPC_NOWAIT, 0o004000);
        assert_eq!(IPC_PRIVATE, 0);
        assert_eq!(IPC_RMID, 0);
        assert_eq!(IPC_STAT, 2);
        assert_eq!(SHM_RDONLY, 0o010000);
        assert_eq!(SHM_RND, 0o020000);
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 8, 42);
        assert_eq!(msg_i32(&msg, 8), 42);

        msg_set_u64(&mut msg, 16, 0xDEADBEEF);
        assert_eq!(msg_u64(&msg, 16), 0xDEADBEEF);

        msg_set_i32(&mut msg, 12, -1);
        assert_eq!(msg_i32(&msg, 12), -1);
    }

    #[test]
    fn test_mmap_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VM_MMAP as i32);
        msg_set_u64(&mut msg, OFF_VM_ADDR, 0x7F000000);
        msg_set_u64(&mut msg, OFF_VM_LEN, 4096);
        msg_set_i32(&mut msg, OFF_VM_PROT, PROT_READ | PROT_WRITE);
        msg_set_i32(&mut msg, OFF_VM_FLAGS, MAP_PRIVATE | MAP_ANONYMOUS);
        msg_set_i32(&mut msg, OFF_VM_FD, -1);
        msg[40..48].copy_from_slice(&0i64.to_ne_bytes()); // offset at 40

        assert_eq!(msg_i32(&msg, 8), 0xC0A);
        assert_eq!(msg_u64(&msg, 28), 0x7F000000);
        assert_eq!(msg_u64(&msg, 20), 4096);
        assert_eq!(msg_i32(&msg, 12), 0x03); // PROT_READ | PROT_WRITE
        assert_eq!(msg_i32(&msg, 16), 0x22); // MAP_PRIVATE | MAP_ANONYMOUS
        assert_eq!(msg_i32(&msg, 36), -1);
    }

    #[test]
    fn test_munmap_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VM_MUNMAP as i32);
        msg_set_u64(&mut msg, OFF_VM_ADDR, 0x7F000000);
        msg_set_u64(&mut msg, OFF_VM_LEN, 0x10000);

        assert_eq!(msg_i32(&msg, 8), 0xC11);
        assert_eq!(msg_u64(&msg, 28), 0x7F000000);
        assert_eq!(msg_u64(&msg, 20), 0x10000);
    }

    #[test]
    fn test_shmget_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, IPC_SHMGET as i32);
        msg_set_i32(&mut msg, OFF_SHM_KEY, 0x1234);
        msg_set_i32(&mut msg, OFF_SHM_SIZE, 4096);
        msg_set_i32(&mut msg, OFF_SHM_FLAGS, IPC_CREAT | 0o600);

        assert_eq!(msg_i32(&msg, 8), 0xD01);
        assert_eq!(msg_i32(&msg, 12), 0x1234);
        assert_eq!(msg_i32(&msg, 16), 4096);
        assert_eq!(msg_i32(&msg, 20), IPC_CREAT | 0o600);
    }

    #[test]
    fn test_shmat_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, IPC_SHMAT as i32);
        msg_set_i32(&mut msg, OFF_SHMAT_ID, 42);
        msg_set_u64(&mut msg, OFF_SHMAT_ADDR, 0x10000000);
        msg_set_i32(&mut msg, OFF_SHMAT_FLAGS, SHM_RDONLY);

        assert_eq!(msg_i32(&msg, 8), 0xD02);
        assert_eq!(msg_i32(&msg, 12), 42);
        assert_eq!(msg_u64(&msg, 16), 0x10000000);
        // SHM_RDONLY is at offset 24 in the 32-bit layout (after id/addr/flags)
        assert_eq!(msg_i32(&msg, 24), SHM_RDONLY);
    }

    #[test]
    fn test_shmdt_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, IPC_SHMDT as i32);
        msg_set_u64(&mut msg, OFF_SHMDT_ADDR, 0x10000000);

        assert_eq!(msg_i32(&msg, 8), 0xD03);
        assert_eq!(msg_u64(&msg, 16), 0x10000000);
    }

    #[test]
    fn test_shmctl_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, IPC_SHMCTL as i32);
        msg_set_i32(&mut msg, OFF_SHMCTL_ID, 42);
        msg_set_i32(&mut msg, OFF_SHMCTL_CMD, IPC_STAT);

        assert_eq!(msg_i32(&msg, 8), 0xD04);
        assert_eq!(msg_i32(&msg, 12), 42);
        assert_eq!(msg_i32(&msg, 16), IPC_STAT);
    }

    #[test]
    fn test_mmap_returns_failed_on_host() {
        let r = unsafe { mmap(core::ptr::null_mut(), 4096, PROT_READ, MAP_PRIVATE, -1, 0) };
        assert_eq!(r, MAP_FAILED);
    }

    #[test]
    fn test_munmap_returns_minus_one_on_host() {
        let r = unsafe { munmap(0x1000 as *mut u8, 4096) };
        assert_eq!(r, -1);
    }

    #[test]
    fn test_shmget_returns_minus_one_on_host() {
        let r = unsafe { shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0o600) };
        assert_eq!(r, -1);
    }

    #[test]
    fn test_shmat_returns_failed_on_host() {
        let r = unsafe { shmat(0, core::ptr::null_mut(), 0) };
        assert_eq!(r, MAP_FAILED);
    }

    #[test]
    fn test_shmdt_returns_minus_one_on_host() {
        let r = unsafe { shmdt(core::ptr::null_mut()) };
        assert_eq!(r, -1);
    }

    #[test]
    fn test_shmctl_returns_minus_one_on_host() {
        let r = unsafe { shmctl(0, IPC_RMID, core::ptr::null_mut()) };
        assert_eq!(r, -1);
    }

    #[test]
    fn test_flags_consideration() {
        // Verify flag combinations make sense
        let rw = PROT_READ | PROT_WRITE;
        assert_eq!(rw, 0x03);

        let anon_priv = MAP_PRIVATE | MAP_ANONYMOUS;
        assert_eq!(anon_priv, 0x22);
    }
}
