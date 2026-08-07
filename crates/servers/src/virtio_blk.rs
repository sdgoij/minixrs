//! virtio-blk block driver — serves BDEV messages via IPC.
//!
//! Mirrors `ramdisk.rs`: a BDEV server loop that dispatches to
//! `drivers::storage::virtio_blk`. Block data moves between the client
//! and this server through the client's grant table: the client sends a
//! direct grant naming this server, and the server pulls/pushes the bytes
//! with `SYS_SAFECOPYFROM`/`SYS_SAFECOPYTO`. The granter endpoint is
//! always the kernel-stamped `m_source`.
//!
//! Registered as endpoint `VIRTIO_BLK_PROC_NR` in the boot process table
//! (x86_64). Filesystem servers send BDEV messages to this endpoint when
//! the root filesystem lives on the attached virtio disk.

//! # Dead-code allowance
//!
//! All functions and constants in this module are used only by the
//! `virtio_blk` binary target (`src/bin/virtio_blk.rs`), not by the
//! `servers` library target.  Clippy's `dead_code` lint fires for library
//! builds.  The `dead_code` allowance is intentional — the binary target
//! does use everything.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use arch_common::ipc::Message;
use drivers::bus::virtio;
use drivers::storage::virtio_blk;

/// BDEV message types (from arch_common::com).
const BDEV_RQ_BASE: u32 = 0x500;
const BDEV_OPEN: u32 = BDEV_RQ_BASE;
const BDEV_CLOSE: u32 = BDEV_RQ_BASE + 1;
const BDEV_READ: u32 = BDEV_RQ_BASE + 2;
const BDEV_WRITE: u32 = BDEV_RQ_BASE + 3;
const BDEV_GATHER: u32 = BDEV_RQ_BASE + 4;
const BDEV_SCATTER: u32 = BDEV_RQ_BASE + 5;
const BDEV_IOCTL: u32 = BDEV_RQ_BASE + 6;

const BDEV_REPLY: u32 = 0x580;

/// Byte offsets in the message (match `minix-util/src/bdev.rs`).
const OFF_MINOR: usize = 8; // i32
const OFF_FLAGS: usize = 12; // i32
const OFF_GRANT: usize = 16; // i64 (grant ID in low 32 bits)
const OFF_COUNT: usize = 24; // i64
const OFF_ADDR: usize = 32; // i64 (position)

// SYS_SAFECOPY kernel call numbers (arch_common::com::sys).
const SYS_SAFECOPYFROM: i32 = 31;
const SYS_SAFECOPYTO: i32 = 32;

// SYS_SAFECOPY message offsets (payload starts at byte 8).
const SAFE_GRANTER_OFF: usize = 8;
const SAFE_GRANT_ID_OFF: usize = 12;
const SAFE_OFFSET_OFF: usize = 16;
const SAFE_ADDR_OFF: usize = 24;
const SAFE_BYTES_OFF: usize = 32;

/// Maximum single-transfer size (one MFS block).
const MAX_IO: usize = 4096;

/// virtio-blk sector size (device block).
const SECTOR_SIZE: usize = 512;

/// Scratch buffer for one block transfer. A static (not stack) so its VA
/// lives in the loaded image and the `phys_delta` translation applies.
struct ScratchCell(UnsafeCell<[u8; MAX_IO]>);
unsafe impl Sync for ScratchCell {}
impl ScratchCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; MAX_IO]))
    }
    fn get(&self) -> *mut [u8; MAX_IO] {
        self.0.get()
    }
}
static SCRATCH: ScratchCell = ScratchCell::new();

/// Copy `data` into the client's granted buffer via `SYS_SAFECOPYTO`.
///
/// The granter is the client endpoint from the kernel-stamped `m_source`,
/// never a payload field (confused-deputy rule). Returns the kernel call
/// result: 0 on success, negative error code otherwise.
fn safecopy_to_client(src_ep: i32, grant_id: i32, data: &[u8]) -> i32 {
    let mut kmsg = [0u8; 64];
    kmsg[SAFE_GRANTER_OFF..SAFE_GRANTER_OFF + 4].copy_from_slice(&src_ep.to_ne_bytes());
    kmsg[SAFE_GRANT_ID_OFF..SAFE_GRANT_ID_OFF + 4].copy_from_slice(&grant_id.to_ne_bytes());
    kmsg[SAFE_OFFSET_OFF..SAFE_OFFSET_OFF + 8].copy_from_slice(&0u64.to_ne_bytes());
    kmsg[SAFE_ADDR_OFF..SAFE_ADDR_OFF + 8].copy_from_slice(&(data.as_ptr() as u64).to_ne_bytes());
    kmsg[SAFE_BYTES_OFF..SAFE_BYTES_OFF + 8].copy_from_slice(&(data.len() as u64).to_ne_bytes());
    minix_rt::kernel_call(SYS_SAFECOPYTO, &mut kmsg)
}

/// Copy `count` bytes from the client's granted buffer via `SYS_SAFECOPYFROM`.
///
/// Returns the kernel call result: 0 on success, negative error code otherwise.
fn safecopy_from_client(src_ep: i32, grant_id: i32, data: &mut [u8]) -> i32 {
    let mut kmsg = [0u8; 64];
    kmsg[SAFE_GRANTER_OFF..SAFE_GRANTER_OFF + 4].copy_from_slice(&src_ep.to_ne_bytes());
    kmsg[SAFE_GRANT_ID_OFF..SAFE_GRANT_ID_OFF + 4].copy_from_slice(&grant_id.to_ne_bytes());
    kmsg[SAFE_OFFSET_OFF..SAFE_OFFSET_OFF + 8].copy_from_slice(&0u64.to_ne_bytes());
    kmsg[SAFE_ADDR_OFF..SAFE_ADDR_OFF + 8]
        .copy_from_slice(&(data.as_mut_ptr() as u64).to_ne_bytes());
    kmsg[SAFE_BYTES_OFF..SAFE_BYTES_OFF + 8].copy_from_slice(&(data.len() as u64).to_ne_bytes());
    minix_rt::kernel_call(SYS_SAFECOPYFROM, &mut kmsg)
}

fn msg_get_i32(msg: &Message, off: usize) -> i32 {
    unsafe {
        let bytes = &msg.m_payload.raw[off - 8..][..4];
        i32::from_ne_bytes(bytes.try_into().unwrap())
    }
}

fn msg_get_i64(msg: &Message, off: usize) -> i64 {
    unsafe {
        let bytes = &msg.m_payload.raw[off - 8..][..8];
        i64::from_ne_bytes(bytes.try_into().unwrap())
    }
}

fn msg_set_i32(msg: &mut Message, off: usize, val: i32) {
    unsafe {
        let dst = &mut msg.m_payload.raw[off - 8..][..4];
        dst.copy_from_slice(&val.to_ne_bytes());
    }
}

/// Build a BDEV reply message with a status code.
fn build_reply(msg: &mut Message, status: i32) {
    msg.m_type = BDEV_REPLY as i32;
    msg_set_i32(msg, OFF_COUNT, status);
}

/// Query this process's VA→PA image translation offset (SYS_GETINFO
/// GET_PHYS_DELTA) and hand it to the virtio transport so queue and
/// descriptor addresses are programmed as guest-physical addresses.
///
/// Kernel-call convention: the request lives in the payload (byte 8+);
/// the first 8 bytes are overwritten with the call number/source by
/// sys_kernel_call_handler. The reply (written back at message offset 0)
/// is copied back with the whole buffer.
fn init_phys_delta() {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&arch_common::com::GET_PHYS_DELTA.to_ne_bytes());
    minix_rt::kernel_call(26, &mut msg); // SYS_GETINFO
    let delta = i64::from_ne_bytes(msg[0..8].try_into().unwrap_or([0u8; 8]));
    virtio::virtio_set_phys_delta(delta);
}

/// Port I/O hook for the virtio transport: executes the access via
/// SYS_DEVIO (user-mode drivers have no direct I/O port access; the
/// kernel performs it on their behalf). The request/port/value live in
/// the kernel-call payload (byte 8+); the reply value for input
/// operations is written back at the request offset.
fn devio_hook(request: u32, port: u16, value: u32) -> u32 {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&request.to_ne_bytes());
    msg[12..16].copy_from_slice(&(port as u32).to_ne_bytes());
    msg[16..20].copy_from_slice(&value.to_ne_bytes());
    // SYS_DEVIO is call index 21 (kernel_call adds KERNEL_CALL itself;
    // passing the full 0x615 number would dispatch to call 1557).
    minix_rt::kernel_call(21, &mut msg);
    u32::from_ne_bytes(msg[8..12].try_into().unwrap())
}

/// Handle a single BDEV message and write the reply.
fn handle_bdev(msg: &mut Message, src_ep: i32) {
    let mtype = msg.m_type as u32;
    let _minor = msg_get_i32(msg, OFF_MINOR) as usize;
    let _flags = msg_get_i32(msg, OFF_FLAGS) as u32;
    let grant_id = msg_get_i32(msg, OFF_GRANT);

    match mtype {
        BDEV_OPEN => match virtio_blk::virtio_blk_open() {
            Ok(()) => build_reply(msg, 0),
            Err(_) => build_reply(msg, -5), // EIO
        },
        BDEV_CLOSE => match virtio_blk::virtio_blk_close() {
            Ok(()) => build_reply(msg, 0),
            Err(_) => build_reply(msg, -5),
        },
        BDEV_READ => {
            let position = msg_get_i64(msg, OFF_ADDR) as u64;
            let count = msg_get_i64(msg, OFF_COUNT) as usize;
            if count == 0 {
                build_reply(msg, 0);
                return;
            }
            // virtio transfers whole 512-byte sectors.
            if !position.is_multiple_of(SECTOR_SIZE as u64) || !count.is_multiple_of(SECTOR_SIZE) {
                build_reply(msg, -22); // EINVAL
                return;
            }
            let n = count.min(MAX_IO);
            let sector = position / SECTOR_SIZE as u64;
            let scratch = unsafe { &mut *SCRATCH.get() };
            match unsafe { virtio_blk::virtio_blk_transfer(false, sector, &mut scratch[..n]) } {
                Ok(bytes) => {
                    let r = safecopy_to_client(src_ep, grant_id, &scratch[..bytes]);
                    if r != 0 {
                        build_reply(msg, r);
                    } else {
                        build_reply(msg, bytes as i32);
                    }
                }
                Err(_) => build_reply(msg, -5),
            }
        }
        BDEV_WRITE => {
            let position = msg_get_i64(msg, OFF_ADDR) as u64;
            let count = msg_get_i64(msg, OFF_COUNT) as usize;
            if count == 0 {
                build_reply(msg, 0);
                return;
            }
            if !position.is_multiple_of(SECTOR_SIZE as u64) || !count.is_multiple_of(SECTOR_SIZE) {
                build_reply(msg, -22); // EINVAL
                return;
            }
            let n = count.min(MAX_IO);
            let sector = position / SECTOR_SIZE as u64;
            let scratch = unsafe { &mut *SCRATCH.get() };
            let r = safecopy_from_client(src_ep, grant_id, &mut scratch[..n]);
            if r != 0 {
                build_reply(msg, r);
            } else {
                match unsafe { virtio_blk::virtio_blk_transfer(true, sector, &mut scratch[..n]) } {
                    Ok(bytes) => build_reply(msg, bytes as i32),
                    Err(_) => build_reply(msg, -5),
                }
            }
        }
        BDEV_GATHER | BDEV_SCATTER | BDEV_IOCTL => {
            build_reply(msg, -95); // ENOTSUP
        }
        _ => {
            build_reply(msg, -22); // EINVAL
        }
    }
}

/// Main entry point for the virtio-blk driver process.
///
/// Initializes the virtio transport (phys delta, PCI probe), then enters
/// the message loop: receive a BDEV message → dispatch → reply.
pub fn virtio_blk_server_main() {
    #[cfg(target_os = "minix")]
    {
        const RECEIVE_CALL: u64 = 47;
        const SEND_CALL: u64 = 46;
        const ANY: i32 = 0x0000ffff;

        virtio_blk::virtio_blk_init();
        virtio::virtio_set_devio(devio_hook);
        init_phys_delta();

        // Probe PCI for the virtio-blk device. Without an attached disk
        // the server still runs and answers BDEV_* with errors.
        let _ = unsafe { virtio_blk::virtio_blk_probe(0) };

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            // Receive a message from any sender.
            let src = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64)
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            // Handle the BDEV message, then reply with a plain SEND (46),
            // matching the ramdisk server's proven reply pattern (a SENDREC
            // reply would immediately re-block this loop in RECEIVE).
            handle_bdev(&mut msg, src_ep);

            let _ = unsafe {
                minix_rt::syscall2(SEND_CALL, src_ep as u64, &mut msg as *mut Message as u64)
            };
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // No-op on host builds — dispatch is tested directly
    }
}
