//! virtio-net block driver — serves DL_* (data-link) messages via IPC.
//!
//! Mirrors the MINIX network driver protocol (`libnetdriver`): a network
//! stack client sends `DL_CONF` to learn the MAC address, `DL_READV_S`
//! with a grant for an array of `iovec_s_t` buffers to receive packets,
//! and `DL_WRITEV_S` to transmit. The driver replies `DL_CONF_REPLY`,
//! `DL_TASK_REPLY`, or `DL_STAT_REPLY`.
//!
//! Message payload layout (payload = `Message::m_payload`, message byte
//! 8+, matching the C `mess_net_netdrv_dl_*` unions):
//!
//! - `DL_CONF`      request: `mode` i32 at payload 0
//! - `DL_GETSTAT_S` request: `grant` i32 at payload 0
//! - `DL_READV_S` / `DL_WRITEV_S`: `grant` i32 at 0, `count` i32 at 4
//! - `DL_CONF_REPLY`: `stat` i32 at 0, `hw_addr[6]` at 4
//! - `DL_TASK_REPLY`: `count` i32 at 0, `flags` u32 at 4
//!
//! The iovec array is a grant in the client's grant table; each element
//! is `iovec_s_t { iov_grant: i32, iov_size: u32 }` (8 bytes). The
//! granter is always the kernel-stamped `m_source` (confused-deputy
//! rule).

//! # Dead-code allowance
//!
//! All functions and constants in this module are used only by the
//! `virtio_net` binary target (`src/bin/virtio_net.rs`), not by the
//! `servers` library target.  Clippy's `dead_code` lint fires for library
//! builds.  The `dead_code` allowance is intentional — the binary target
//! does use everything.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use arch_common::ipc::Message;
use drivers::bus::virtio;
use drivers::network::virtio_net;

/// DL message types (from arch_common::com).
const DL_CONF: u32 = arch_common::com::DL_CONF;
const DL_GETSTAT_S: u32 = arch_common::com::DL_GETSTAT_S;
const DL_WRITEV_S: u32 = arch_common::com::DL_WRITEV_S;
const DL_READV_S: u32 = arch_common::com::DL_READV_S;

const DL_CONF_REPLY: u32 = arch_common::com::DL_CONF_REPLY;
const DL_STAT_REPLY: u32 = arch_common::com::DL_STAT_REPLY;
const DL_TASK_REPLY: u32 = arch_common::com::DL_TASK_REPLY;

const DL_PACK_SEND: u32 = arch_common::com::DL_PACK_SEND;
const DL_PACK_RECV: u32 = arch_common::com::DL_PACK_RECV;

/// Payload offsets (within `m_payload.raw`).
const OFF_GRANT: usize = 0; // i32
const OFF_COUNT: usize = 4; // i32 (request side)
const OFF_REPLY_COUNT: usize = 0; // i32 (DL_TASK_REPLY count)
const OFF_FLAGS: usize = 4; // u32
const OFF_STAT: usize = 0; // i32
const OFF_MAC: usize = 4; // 6 bytes

// SYS_SAFECOPY kernel call numbers (arch_common::com::sys).
const SYS_SAFECOPYFROM: i32 = 31;
const SYS_SAFECOPYTO: i32 = 32;

// SYS_SAFECOPY message offsets (payload starts at byte 8).
const SAFE_GRANTER_OFF: usize = 8;
const SAFE_GRANT_ID_OFF: usize = 12;
const SAFE_OFFSET_OFF: usize = 16;
const SAFE_ADDR_OFF: usize = 24;
const SAFE_BYTES_OFF: usize = 32;

/// Maximum iovecs per DL read/write request.
const MAX_IOVECS: usize = 16;
/// Bounded RX poll window in the DL_READV_S handler. The comment intent
/// was ~50ms of spinning; 50M iterations is seconds in QEMU TCG, which
/// makes no-packet reads (e.g. the net server's datagram recv poll) look
/// like hangs. 2M iterations is roughly the native ~50ms target while
/// keeping TCG responsive.
const RX_POLL_SPINS: u32 = 2_000_000;

/// One iovec element (`iovec_s_t`): a grant for a packet buffer plus its
/// size. 8 bytes, matching the C layout's fields.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IovecGrant {
    iov_grant: i32,
    iov_size: u32,
}

/// Staging buffer for one packet transfer. A static (not stack) so its VA
/// lives in the loaded image and the `phys_delta` translation applies.
struct StagingCell(UnsafeCell<[u8; 2048]>);
unsafe impl Sync for StagingCell {}
impl StagingCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; 2048]))
    }
    fn get(&self) -> *mut [u8; 2048] {
        self.0.get()
    }
}
static STAGING: StagingCell = StagingCell::new();

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

fn pld_i32(msg: &Message, off: usize) -> i32 {
    unsafe {
        let bytes = &msg.m_payload.raw[off..][..4];
        i32::from_ne_bytes(bytes.try_into().unwrap())
    }
}

fn set_pld_i32(msg: &mut Message, off: usize, val: i32) {
    unsafe {
        msg.m_payload.raw[off..off + 4].copy_from_slice(&val.to_ne_bytes());
    }
}

fn set_pld_u32(msg: &mut Message, off: usize, val: u32) {
    unsafe {
        msg.m_payload.raw[off..off + 4].copy_from_slice(&val.to_ne_bytes());
    }
}

/// Copy the client's iovec array (via `grant`) into `iovs`.
fn copy_iovecs(src_ep: i32, grant: i32, count: usize, iovs: &mut [IovecGrant]) -> i32 {
    if count == 0 || count > iovs.len() {
        return -22; // EINVAL
    }
    let bytes = count * core::mem::size_of::<IovecGrant>();
    let slice = unsafe { core::slice::from_raw_parts_mut(iovs.as_mut_ptr() as *mut u8, bytes) };
    safecopy_from_client(src_ep, grant, slice)
}

/// Reply to the current request.
///
/// `DL_TASK_REPLY` layout (matches C `mess_netdrv_net_dl_task`):
/// `count` i32 at payload 0, `flags` u32 at payload 4.
fn reply(msg: &mut Message, mtype: u32, count: i32, flags: u32) {
    msg.m_type = mtype as i32;
    set_pld_i32(msg, OFF_REPLY_COUNT, count);
    set_pld_u32(msg, OFF_FLAGS, flags);
}

/// Handle a single DL message and write the reply.
fn handle_dl(msg: &mut Message, src_ep: i32) {
    match msg.m_type as u32 {
        DL_CONF => {
            let mac = virtio_net::virtio_net_mac();
            msg.m_type = DL_CONF_REPLY as i32;
            set_pld_i32(msg, OFF_STAT, 0);
            unsafe {
                msg.m_payload.raw[OFF_MAC..OFF_MAC + 6].copy_from_slice(&mac);
            }
        }
        DL_GETSTAT_S => {
            let grant = pld_i32(msg, OFF_GRANT);
            // Minimal stats: all zero. The client (net server) does not
            // depend on them yet.
            let stats = [0u8; 32];
            let _ = safecopy_to_client(src_ep, grant, &stats);
            msg.m_type = DL_STAT_REPLY as i32;
        }
        DL_WRITEV_S => {
            let grant = pld_i32(msg, OFF_GRANT);
            let count = pld_i32(msg, OFF_COUNT);
            if count <= 0 || count as usize > MAX_IOVECS {
                reply(msg, DL_TASK_REPLY, -22, 0);
                return;
            }
            let mut iovs = [IovecGrant::default(); MAX_IOVECS];
            if copy_iovecs(src_ep, grant, count as usize, &mut iovs) != 0 {
                reply(msg, DL_TASK_REPLY, -22, 0);
                return;
            }
            // Gather the packet into the TX staging buffer.
            let staging = unsafe { &mut *STAGING.get() };
            let mut total = 0usize;
            for iov in iovs.iter().take(count as usize) {
                let size = (iov.iov_size as usize).min(staging.len() - total);
                if size == 0 {
                    break;
                }
                if safecopy_from_client(src_ep, iov.iov_grant, &mut staging[total..total + size])
                    != 0
                {
                    reply(msg, DL_TASK_REPLY, -22, 0);
                    return;
                }
                total += size;
            }
            if total == 0 {
                reply(msg, DL_TASK_REPLY, -22, 0);
                return;
            }
            match virtio_net::virtio_net_transmit(&staging[..total]) {
                Ok(()) => reply(msg, DL_TASK_REPLY, 0, DL_PACK_SEND),
                Err(_) => reply(msg, DL_TASK_REPLY, -5, 0), // EIO
            }
        }
        DL_READV_S => {
            let grant = pld_i32(msg, OFF_GRANT);
            let count = pld_i32(msg, OFF_COUNT);
            if count <= 0 || count as usize > MAX_IOVECS {
                reply(msg, DL_TASK_REPLY, -22, 0);
                return;
            }
            let mut iovs = [IovecGrant::default(); MAX_IOVECS];
            if copy_iovecs(src_ep, grant, count as usize, &mut iovs) != 0 {
                reply(msg, DL_TASK_REPLY, -22, 0);
                return;
            }
            // Serve one packet per request, split across the iovecs
            // (MINIX semantics: the reply `count` is the number of bytes
            // copied). Wait briefly for a packet before giving up so the
            // caller can retry.
            let mut spins = 0;
            while virtio_net::virtio_net_rx_pending() == 0 && spins < RX_POLL_SPINS {
                spins += 1;
            }
            let staging = unsafe { &mut *STAGING.get() };
            let n = virtio_net::virtio_net_receive(staging);
            let mut left = n;
            let mut bytes = 0usize;
            for iov in iovs.iter().take(count as usize) {
                if left == 0 {
                    break;
                }
                let size = left.min(iov.iov_size as usize);
                if size == 0 {
                    break;
                }
                if safecopy_to_client(src_ep, iov.iov_grant, &staging[bytes..bytes + size]) != 0 {
                    break;
                }
                left -= size;
                bytes += size;
            }
            let flags = if bytes > 0 { DL_PACK_RECV } else { 0 };
            reply(msg, DL_TASK_REPLY, bytes as i32, flags);
        }
        _ => {
            reply(msg, DL_TASK_REPLY, -22, 0);
        }
    }
}

/// Query this process's VA→PA image translation offset (SYS_GETINFO
/// GET_PHYS_DELTA) and hand it to the virtio transport so queue and
/// descriptor addresses are programmed as guest-physical addresses.
fn init_phys_delta() {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&arch_common::com::GET_PHYS_DELTA.to_ne_bytes());
    minix_rt::kernel_call(26, &mut msg); // SYS_GETINFO
    let delta = i64::from_ne_bytes(msg[0..8].try_into().unwrap_or([0u8; 8]));
    virtio::virtio_set_phys_delta(delta);
}

/// Port I/O hook for the virtio transport (PCI config access via
/// SYS_DEVIO, since user-mode drivers have no direct I/O port access).
fn devio_hook(request: u32, port: u16, value: u32) -> u32 {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&request.to_ne_bytes());
    msg[12..16].copy_from_slice(&(port as u32).to_ne_bytes());
    msg[16..20].copy_from_slice(&value.to_ne_bytes());
    minix_rt::kernel_call(21, &mut msg); // SYS_DEVIO
    u32::from_ne_bytes(msg[8..12].try_into().unwrap())
}

/// Main entry point for the virtio-net driver process.
///
/// Initializes the virtio transport (phys delta, PCI probe), then enters
/// the message loop: receive a DL message → dispatch → reply.
pub fn virtio_net_server_main() {
    #[cfg(target_os = "minix")]
    {
        const RECEIVE_CALL: u64 = 47;
        const SEND_CALL: u64 = 46;
        const ANY: i32 = 0x0000ffff;

        virtio_net::virtio_net_init();
        virtio::virtio_set_devio(devio_hook);
        init_phys_delta();

        // Probe PCI for the virtio-net device. Without an attached NIC
        // the server still runs and answers DL_* with errors.
        let _ = unsafe { virtio_net::virtio_net_probe(0) };

        // Chain the RX buffers into the device so inbound packets (ARP
        // replies, echo replies) can be delivered — MINIX's driver runs
        // virtio_net_refill_rx_queue in its main loop for the same reason.
        // Consumed slots are refilled by virtio_net_receive, so one
        // initial refill keeps the queue fed.
        let _ = virtio_net::virtio_net_open();

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            let src = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64)
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            handle_dl(&mut msg, src_ep);

            let _ = unsafe {
                minix_rt::syscall2(SEND_CALL, src_ep as u64, &mut msg as *mut Message as u64)
            };
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // No-op on host builds — dispatch is tested directly.
    }
}
