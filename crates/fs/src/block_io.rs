//! Block I/O providers for filesystem servers.
//!
//! [`bdev_ram_disk_io`] is the target (MINIX) block I/O callback: each block
//! request becomes a `BDEV_READ`/`BDEV_WRITE` IPC to the ramdisk driver
//! server, sharing the cache buffer through a direct grant and transferring
//! the bytes with `SYS_SAFECOPYTO`/`SYS_SAFECOPYFROM`. The server registers
//! its grant table with the kernel in [`bdev_init`].
//!
//! [`ram_disk_io`] is a direct-memory RAM disk used on the host, where no
//! driver server exists, and by the MFS unit tests.

use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(any(target_os = "none", test))]
use arch_common::safecopies::{
    CPF_DIRECT, CPF_READ, CPF_USED, CPF_VALID, CPF_WRITE, CpDirect, CpGrant, CpUnion, GRANT_INVALID,
};

/// Block size for the RAM disk (must match the FS block size — 4096 for Minix V3).
pub const RAM_DISK_BLOCK_SIZE: usize = 4096;

/// Static storage for the RAM disk base pointer and size.
static BASE: AtomicUsize = AtomicUsize::new(0);
static SIZE: AtomicUsize = AtomicUsize::new(0);

/// Initialize the RAM disk with a base pointer and size.
///
/// # Safety
///
/// `base` must point to a valid memory region of at least `size` bytes
/// that remains valid for the lifetime of the process.
pub unsafe fn ram_disk_init(base: *const u8, size: usize) {
    BASE.store(base as usize, Ordering::Relaxed);
    SIZE.store(size, Ordering::Relaxed);
}

/// Check if the RAM disk has been initialized.
pub fn ram_disk_is_initialized() -> bool {
    !(BASE.load(Ordering::Relaxed) as *const u8).is_null() && SIZE.load(Ordering::Relaxed) > 0
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
    let base = BASE.load(Ordering::Relaxed) as *const u8;
    let size = SIZE.load(Ordering::Relaxed);
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

// ---- Grant-based BDEV path (MINIX target) ----

/// Number of grant entries in the block I/O grant table.
#[cfg(any(target_os = "none", test))]
const NR_BDEV_GRANTS: usize = 16;

/// Direct-grant table registered with the kernel via `SYS_SETGRANT`.
///
/// # Safety
///
/// Only accessed from the single-threaded filesystem server. The address is
/// registered with the kernel so that the block driver can resolve grants
/// during `SYS_SAFECOPYTO`/`SYS_SAFECOPYFROM`.
#[cfg(any(target_os = "none", test))]
struct BdevGrantTable {
    entries: core::cell::UnsafeCell<[CpGrant; NR_BDEV_GRANTS]>,
}

#[cfg(any(target_os = "none", test))]
unsafe impl Sync for BdevGrantTable {}

#[cfg(any(target_os = "none", test))]
impl BdevGrantTable {
    const fn new() -> Self {
        const ENTRY: CpGrant = CpGrant {
            cp_flags: 0,
            cp_u: CpUnion {
                cp_direct: CpDirect {
                    cp_who_to: 0,
                    cp_start: 0,
                    cp_len: 0,
                    cp_reserved: [0u8; 8],
                },
            },
            cp_reserved: [0u8; 8],
        };
        Self {
            entries: core::cell::UnsafeCell::new([ENTRY; NR_BDEV_GRANTS]),
        }
    }

    /// Address of the table, registered with the kernel via `SYS_SETGRANT`.
    #[cfg(target_os = "none")]
    fn as_ptr(&self) -> u64 {
        self.entries.get() as u64
    }

    /// Allocate a direct grant giving `callee` access to `len` bytes at
    /// `addr`. `write` selects `CPF_WRITE` (callee writes the buffer, used
    /// for reads) vs `CPF_READ` (callee reads the buffer, used for writes).
    fn grant_direct(&self, callee: i32, addr: u64, len: usize, write: bool) -> i32 {
        unsafe {
            let entries = &mut *self.entries.get();
            for (i, entry) in entries.iter_mut().enumerate() {
                if entry.cp_flags == 0 {
                    let access = if write { CPF_WRITE } else { CPF_READ };
                    entry.cp_flags = CPF_USED | CPF_VALID | CPF_DIRECT | access;
                    entry.cp_u.cp_direct = CpDirect {
                        cp_who_to: callee,
                        cp_start: addr,
                        cp_len: len,
                        cp_reserved: [0u8; 8],
                    };
                    return i as i32;
                }
            }
        }
        GRANT_INVALID
    }

    #[cfg(any(target_os = "none", test))]
    fn revoke(&self, grant_id: i32) {
        if grant_id < 0 || grant_id >= NR_BDEV_GRANTS as i32 {
            return;
        }
        unsafe {
            let entries = &mut *self.entries.get();
            entries[grant_id as usize].cp_flags = 0;
        }
    }
}

#[cfg(target_os = "none")]
static BDEV_GRANT_TABLE: BdevGrantTable = BdevGrantTable::new();

/// Register the block I/O grant table with the kernel (`SYS_SETGRANT`).
///
/// Must be called once during filesystem server init, before any block I/O.
#[cfg(target_os = "none")]
pub fn bdev_init() {
    let mut msg = [0u8; 64];
    msg[8..16].copy_from_slice(&BDEV_GRANT_TABLE.as_ptr().to_le_bytes());
    msg[16..20].copy_from_slice(&(NR_BDEV_GRANTS as i32).to_le_bytes());
    let _r = minix_rt::kernel_call(34, &mut msg); // SYS_SETGRANT
}

/// Send one block-sized `BDEV_READ`/`BDEV_WRITE` request to the ramdisk
/// driver server and return the byte count transferred, or a negative error.
///
/// # Safety
///
/// `buf` must be valid for `block_size` bytes.
#[cfg(target_os = "none")]
unsafe fn bdev_request(rw_flag: i32, dev: u32, block: u64, buf: *mut u8, block_size: usize) -> i32 {
    unsafe {
        // READING: the driver writes into our buffer (CPF_WRITE).
        // WRITING: the driver reads our buffer (CPF_READ).
        let grant_write = rw_flag == 0;
        let grant = BDEV_GRANT_TABLE.grant_direct(
            arch_common::com::RAMDISK_PROC_NR,
            buf as u64,
            block_size,
            grant_write,
        );
        if grant == GRANT_INVALID {
            return -1;
        }

        let mut msg = arch_common::ipc::Message {
            m_source: 0,
            m_type: if rw_flag == 1 {
                arch_common::com::BDEV_WRITE as i32
            } else {
                arch_common::com::BDEV_READ as i32
            },
            m_payload: core::mem::zeroed(),
        };
        // Payload layout matches the ramdisk driver server:
        //   raw[0..4]   minor device (i32)
        //   raw[4..8]   flags (i32)
        //   raw[8..16]  grant id (i64)
        //   raw[16..24] byte count (i64)
        //   raw[24..32] byte position (i64)
        {
            let raw = &mut msg.m_payload.raw;
            raw[0..4].copy_from_slice(&(dev as i32).to_ne_bytes());
            raw[4..8].copy_from_slice(&0i32.to_ne_bytes());
            raw[8..16].copy_from_slice(&(grant as i64).to_ne_bytes());
            raw[16..24].copy_from_slice(&(block_size as i64).to_ne_bytes());
            raw[24..32].copy_from_slice(&((block as u64) * (block_size as u64)).to_ne_bytes());
        }

        let r = minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::RAMDISK_PROC_NR as u64,
            &mut msg as *mut arch_common::ipc::Message as u64,
        );
        BDEV_GRANT_TABLE.revoke(grant);
        if r < 0 {
            return r as i32;
        }
        if msg.m_type < 0 {
            return msg.m_type;
        }
        // Reply status lives at payload raw[16..24] (BDEV reply byte count).
        let status = i64::from_ne_bytes(msg.m_payload.raw[16..24].try_into().unwrap_or([0u8; 8]));
        status as i32
    }
}

/// Block I/O callback routing each block through the BDEV protocol to the
/// ramdisk driver server. On the host (no driver server) it reports failure.
///
/// # Safety
///
/// `bufs` must point to an array of `nblocks` valid buffer pointers.
pub unsafe fn bdev_ram_disk_io(
    dev: u32,
    block: u64,
    nblocks: usize,
    bufs: *const *mut u8,
    block_size: usize,
    rw_flag: i32,
) -> i32 {
    #[cfg(target_os = "none")]
    {
        for i in 0..nblocks {
            let buf = unsafe { *bufs.add(i) };
            if buf.is_null() {
                return -(i as i32) - 1;
            }
            let r = unsafe { bdev_request(rw_flag, dev, block + i as u64, buf, block_size) };
            if r != block_size as i32 {
                return r;
            }
        }
        nblocks as i32
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (dev, block, nblocks, bufs, block_size, rw_flag);
        -5 // EIO — no driver server on the host
    }
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

    #[test]
    fn test_grant_direct_alloc_and_revoke() {
        let table = BdevGrantTable::new();
        let id = table.grant_direct(11, 0x1000, 4096, true);
        assert!(id >= 0 && id < NR_BDEV_GRANTS as i32);
        unsafe {
            let entries = &*table.entries.get();
            let e = &entries[id as usize];
            assert!(e.cp_flags & CPF_USED != 0);
            assert!(e.cp_flags & CPF_VALID != 0);
            assert!(e.cp_flags & CPF_DIRECT != 0);
            assert!(e.cp_flags & CPF_WRITE != 0);
            assert_eq!(e.cp_u.cp_direct.cp_who_to, 11);
            assert_eq!(e.cp_u.cp_direct.cp_start, 0x1000);
            assert_eq!(e.cp_u.cp_direct.cp_len, 4096);
        }
        table.revoke(id);
        unsafe {
            let entries = &*table.entries.get();
            assert_eq!(entries[id as usize].cp_flags, 0);
        }
    }

    #[test]
    fn test_grant_direct_read_access() {
        let table = BdevGrantTable::new();
        // A write request grants the driver READ access to the buffer.
        let id = table.grant_direct(11, 0x2000, 512, false);
        assert!(id >= 0);
        unsafe {
            let entries = &*table.entries.get();
            let e = &entries[id as usize];
            assert!(e.cp_flags & CPF_READ != 0);
            assert!(e.cp_flags & CPF_WRITE == 0);
        }
    }

    #[test]
    fn test_grant_table_full_returns_invalid() {
        let table = BdevGrantTable::new();
        for i in 0..NR_BDEV_GRANTS {
            let id = table.grant_direct(11, 0x3000 + i as u64, 4096, true);
            assert_eq!(id, i as i32);
        }
        assert_eq!(table.grant_direct(11, 0x4000, 4096, true), GRANT_INVALID);
    }

    #[test]
    fn test_revoke_reuses_slot() {
        let table = BdevGrantTable::new();
        let id1 = table.grant_direct(11, 0x1000, 4096, true);
        table.revoke(id1);
        let id2 = table.grant_direct(11, 0x2000, 4096, true);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_bdev_ram_disk_io_host_reports_eio() {
        // On the host there is no driver server; the BDEV callback must
        // report an error rather than pretending to transfer blocks.
        let mut buf = [0u8; 4096];
        let bufs = [buf.as_mut_ptr(); 1];
        let n = unsafe { bdev_ram_disk_io(0, 0, 1, bufs.as_ptr(), 4096, 0) };
        assert!(n < 0);
    }
}
