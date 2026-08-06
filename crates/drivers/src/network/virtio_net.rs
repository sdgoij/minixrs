//! virtio-net driver — modern virtio 1.x transport.
//!
//! Implements one RX queue (queue 0) and one TX queue (queue 1) for a
//! virtio-net device. The driver negotiates no offload features, so every
//! packet carries a zeroed 12-byte `virtio_net_hdr` and the device performs
//! software checksumming.
//!
//! RX uses a static pool of [`RX_BUF_COUNT`] buffers. Every buffer is
//! chained into the RX queue as `[hdr|buf]` (both writable); completed
//! slots are reaped into a small FIFO and only refilled once their packet
//! has been consumed, so the device cannot overwrite an unconsumed packet.
//! TX is serialized: one `[hdr|buf]` chain at a time, waiting for the host
//! to complete it before the next transmit.
//!
//! All DMA buffers are statics in the loaded image, so their guest-physical
//! addresses are derived from the process's phys delta (`SYS_GETINFO`
//! GET_PHYS_DELTA) exactly like the virtio-blk driver.

use core::cell::UnsafeCell;

use crate::DriverError;
use crate::bus::virtio;

/// virtio device ID for network.
pub const VIRTIO_ID_NET: u16 = 1;

/// Feature bit: the device provides a MAC in its config space.
pub const VIRTIO_NET_F_MAC: u8 = 5;
/// Feature bit: the device provides a status field in its config space.
pub const VIRTIO_NET_F_STATUS: u8 = 16;

/// Features this driver negotiates.
static VIRTIO_NET_FEATURES: &[virtio::VirtioFeature] = &[virtio::VirtioFeature {
    name: "VIRTIO_NET_F_MAC",
    bit: VIRTIO_NET_F_MAC,
    host_support: 0,
    guest_support: 1,
}];

/// Size of the `virtio_net_hdr` prepended to every packet. The modern
/// (virtio 1.x) header is 12 bytes (`flags`, `gso_type`, four u16 fields,
/// and `num_buffers`); QEMU's virtio-net uses this size for the 1.x
/// guest header regardless of VIRTIO_NET_F_MRG_RXBUF, so the driver must
/// match it or the device shifts every frame by 2 bytes.
pub const VIRTIO_NET_HDR_SIZE: usize = 12;

/// `virtio_net_hdr.gso_type` for a packet with no offload.
pub const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;

/// Queue indices.
pub const RXQ: usize = 0;
pub const TXQ: usize = 1;

/// Number of RX buffers chained into the RX queue.
pub const RX_BUF_COUNT: usize = 64;
/// RX buffer size — fits the largest ethernet frame (1514 bytes).
pub const RX_BUF_SIZE: usize = 2048;
/// TX staging buffer size.
pub const TX_BUF_SIZE: usize = 2048;

/// Modern `virtio_net_hdr` (12 bytes, virtio 1.x).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    pub num_buffers: u16,
}

impl VirtioNetHdr {
    /// A zeroed header: no checksum offload, GSO, or mergeable buffers.
    pub const fn new() -> Self {
        Self {
            flags: 0,
            gso_type: 0,
            hdr_len: 0,
            gso_size: 0,
            csum_start: 0,
            csum_offset: 0,
            num_buffers: 0,
        }
    }
}

/// Device config region (offset 0 in the device config space).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VirtioNetConfig {
    pub mac: [u8; 6],
    pub status: u16,
    pub max_virtqueue_pairs: u16,
    pub mtu: u16,
}

//
// Static DMA storage. Every cell is explicitly aligned to 16 bytes: the
// transport uses bit 0 of a buffer address as the writable flag, so an
// odd byte address would be misread as writable (RX) or not (TX).
//

#[repr(align(16))]
struct HdrCell(UnsafeCell<[VirtioNetHdr; RX_BUF_COUNT]>);
unsafe impl Sync for HdrCell {}
impl HdrCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([VirtioNetHdr::new(); RX_BUF_COUNT]))
    }
    fn get(&self) -> *mut [VirtioNetHdr; RX_BUF_COUNT] {
        self.0.get()
    }
}
static RX_HDRS: HdrCell = HdrCell::new();

#[repr(align(16))]
struct RxBufCell(UnsafeCell<[[u8; RX_BUF_SIZE]; RX_BUF_COUNT]>);
unsafe impl Sync for RxBufCell {}
impl RxBufCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([[0u8; RX_BUF_SIZE]; RX_BUF_COUNT]))
    }
    fn get(&self) -> *mut [[u8; RX_BUF_SIZE]; RX_BUF_COUNT] {
        self.0.get()
    }
}
static RX_BUFS: RxBufCell = RxBufCell::new();

#[repr(align(16))]
struct TxHdrCell(UnsafeCell<VirtioNetHdr>);
unsafe impl Sync for TxHdrCell {}
impl TxHdrCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(VirtioNetHdr::new()))
    }
    fn get(&self) -> *mut VirtioNetHdr {
        self.0.get()
    }
}
static TX_HDR: TxHdrCell = TxHdrCell::new();

#[repr(align(16))]
struct TxBufCell(UnsafeCell<[u8; TX_BUF_SIZE]>);
unsafe impl Sync for TxBufCell {}
impl TxBufCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; TX_BUF_SIZE]))
    }
    fn get(&self) -> *mut [u8; TX_BUF_SIZE] {
        self.0.get()
    }
}
static TX_BUF: TxBufCell = TxBufCell::new();

// ---- Driver state ----

/// A received packet waiting in the FIFO: which RX slot holds it and how
/// long it is (packet bytes, excluding the virtio header).
#[derive(Clone, Copy, Debug)]
struct RxEntry {
    slot: u16,
    len: u16,
}

struct VirtioNetState {
    dev: Option<virtio::VirtioDevice>,
    mac: [u8; 6],
    open_count: u32,
    rx_fifo_count: usize,
    rx_fifo_head: usize,
    rx_fifo_tail: usize,
    rx_fifo: [RxEntry; RX_BUF_COUNT],
}

impl VirtioNetState {
    const fn new() -> Self {
        Self {
            dev: None,
            mac: [0; 6],
            open_count: 0,
            rx_fifo_count: 0,
            rx_fifo_head: 0,
            rx_fifo_tail: 0,
            rx_fifo: [RxEntry { slot: 0, len: 0 }; RX_BUF_COUNT],
        }
    }
}

struct StateCell(UnsafeCell<VirtioNetState>);
unsafe impl Sync for StateCell {}
impl StateCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(VirtioNetState::new()))
    }
    fn get(&self) -> *mut VirtioNetState {
        self.0.get()
    }
}
static STATE: StateCell = StateCell::new();

fn state_ptr() -> *mut VirtioNetState {
    STATE.get()
}

// ---- RX FIFO ----

fn fifo_len(st: &VirtioNetState) -> usize {
    st.rx_fifo_count
}

fn fifo_push(st: &mut VirtioNetState, e: RxEntry) -> bool {
    if st.rx_fifo_count == RX_BUF_COUNT {
        return false;
    }
    st.rx_fifo[st.rx_fifo_tail] = e;
    st.rx_fifo_tail = (st.rx_fifo_tail + 1) % RX_BUF_COUNT;
    st.rx_fifo_count += 1;
    true
}

fn fifo_pop(st: &mut VirtioNetState) -> Option<RxEntry> {
    if st.rx_fifo_count == 0 {
        return None;
    }
    let e = st.rx_fifo[st.rx_fifo_head];
    st.rx_fifo_head = (st.rx_fifo_head + 1) % RX_BUF_COUNT;
    st.rx_fifo_count -= 1;
    Some(e)
}

// ---- Queue helpers ----

/// Chain one RX slot (`[hdr|buf]`, both writable) into the RX queue.
fn refill_slot(st: &mut VirtioNetState, slot: usize) -> bool {
    let dev = match st.dev.as_mut() {
        Some(d) => d,
        None => return false,
    };
    unsafe {
        let hdr = &mut (*RX_HDRS.get())[slot];
        *hdr = VirtioNetHdr::default();
        let buf = &mut (*RX_BUFS.get())[slot];
        let bufs = [
            virtio::VirtioPhysBuf {
                addr: hdr as *mut VirtioNetHdr as u64 | 1,
                size: VIRTIO_NET_HDR_SIZE as u32,
            },
            virtio::VirtioPhysBuf {
                addr: buf.as_mut_ptr() as u64 | 1,
                size: RX_BUF_SIZE as u32,
            },
        ];
        virtio::virtio_to_queue(dev, RXQ, &bufs, slot).is_ok()
    }
}

/// Refill every RX slot; returns the number successfully submitted.
fn refill_all(st: &mut VirtioNetState) -> usize {
    let mut n = 0;
    for slot in 0..RX_BUF_COUNT {
        if refill_slot(st, slot) {
            n += 1;
        }
    }
    n
}

/// Reap completed RX chains into the pending FIFO. A slot whose packet
/// cannot be queued (FIFO full) is refilled immediately so the device
/// never loses a buffer.
fn reap_rx(st: &mut VirtioNetState) {
    loop {
        let (slot, used_len) = match st
            .dev
            .as_mut()
            .and_then(|d| virtio::virtio_from_queue(d, RXQ))
        {
            Some(x) => x,
            None => return,
        };
        let len = used_len.saturating_sub(VIRTIO_NET_HDR_SIZE as u32) as u16;
        let len = len.min(RX_BUF_SIZE as u16);
        if !fifo_push(
            st,
            RxEntry {
                slot: slot as u16,
                len,
            },
        ) {
            refill_slot(st, slot);
        }
    }
}

// ---- Public API ----

/// Reset driver state (must be called before anything else).
pub fn virtio_net_init() {
    let st = unsafe { &mut *state_ptr() };
    st.dev = None;
    st.mac = [0; 6];
    st.open_count = 0;
    st.rx_fifo_count = 0;
    st.rx_fifo_head = 0;
    st.rx_fifo_tail = 0;
}

/// Probe for a virtio-net device and set up RX/TX queues.
///
/// `instance` selects which matching device to use (0 = first).
///
/// # Safety
///
/// Must be called once, after PCI/MMIO init and before any I/O. Must not
/// be called concurrently.
pub unsafe fn virtio_net_probe(instance: u16) -> Result<(), DriverError> {
    let st = unsafe { &mut *state_ptr() };

    let mut dev = virtio::virtio_probe(VIRTIO_ID_NET, "virtio-net", VIRTIO_NET_FEATURES, instance)
        .map_err(|_| DriverError::NotFound)?;
    virtio::virtio_alloc_queue(&mut dev, RXQ).map_err(|_| DriverError::Io)?;
    virtio::virtio_alloc_queue(&mut dev, TXQ).map_err(|_| DriverError::Io)?;
    dev.num_queues = 2;

    // Read the MAC from the device config (offset 0..6).
    for i in 0..6u16 {
        st.mac[i as usize] = virtio::virtio_sread8(&dev, i);
    }

    virtio::virtio_device_ready(&mut dev);
    virtio::virtio_irq_enable(&mut dev);

    st.dev = Some(dev);
    Ok(())
}

/// Open the device: (re)fill the RX queue with free buffers.
pub fn virtio_net_open() -> Result<(), DriverError> {
    let st = unsafe { &mut *state_ptr() };
    if st.dev.is_none() {
        return Err(DriverError::NotFound);
    }
    st.open_count += 1;
    if refill_all(st) == 0 {
        return Err(DriverError::Io);
    }
    Ok(())
}

/// Close the device.
pub fn virtio_net_close() -> Result<(), DriverError> {
    let st = unsafe { &mut *state_ptr() };
    if st.dev.is_none() {
        return Err(DriverError::NotFound);
    }
    if st.open_count > 0 {
        st.open_count -= 1;
    }
    Ok(())
}

/// The device MAC address (from the device config).
pub fn virtio_net_mac() -> [u8; 6] {
    unsafe { (*state_ptr()).mac }
}

/// Reap completed RX chains and report how many packets are pending.
pub fn virtio_net_rx_pending() -> usize {
    let st = unsafe { &mut *state_ptr() };
    if st.dev.is_none() {
        return 0;
    }
    reap_rx(st);
    fifo_len(st)
}

/// Copy the next pending RX packet into `buf`; returns the number of
/// bytes copied (0 if none pending). Refills the consumed slot.
pub fn virtio_net_receive(buf: &mut [u8]) -> usize {
    let st = unsafe { &mut *state_ptr() };
    let entry = match fifo_pop(st) {
        Some(e) => e,
        None => return 0,
    };
    let len = (entry.len as usize).min(buf.len());
    unsafe {
        core::ptr::copy_nonoverlapping(
            (*RX_BUFS.get())[entry.slot as usize].as_ptr(),
            buf.as_mut_ptr(),
            len,
        );
    }
    refill_slot(st, entry.slot as usize);
    len
}

/// Transmit one packet (serialized; waits for the host to complete it).
pub fn virtio_net_transmit(packet: &[u8]) -> Result<(), DriverError> {
    if packet.is_empty() || packet.len() > TX_BUF_SIZE {
        return Err(DriverError::InvalidArgument);
    }
    let st = unsafe { &mut *state_ptr() };
    let dev = st.dev.as_mut().ok_or(DriverError::NotFound)?;

    unsafe {
        core::ptr::copy_nonoverlapping(packet.as_ptr(), TX_BUF.get() as *mut u8, packet.len());
        let hdr = &mut *TX_HDR.get();
        *hdr = VirtioNetHdr::default();
        let bufs = [
            virtio::VirtioPhysBuf {
                addr: hdr as *mut VirtioNetHdr as u64,
                size: VIRTIO_NET_HDR_SIZE as u32,
            },
            virtio::VirtioPhysBuf {
                addr: TX_BUF.get() as *mut u8 as u64,
                size: packet.len() as u32,
            },
        ];
        virtio::virtio_to_queue(dev, TXQ, &bufs, 0).map_err(|_| DriverError::Io)?;
    }

    // Wait for the host to complete the transmit.
    for _ in 0..100_000_000u32 {
        if virtio::virtio_from_queue(dev, TXQ).is_some() {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(DriverError::Busy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_state() {
        unsafe {
            let st = &mut *state_ptr();
            st.dev = None;
            st.mac = [0; 6];
            st.open_count = 0;
            st.rx_fifo_count = 0;
            st.rx_fifo_head = 0;
            st.rx_fifo_tail = 0;
        }
    }

    #[test]
    fn test_virtio_constants() {
        assert_eq!(VIRTIO_ID_NET, 1);
        assert_eq!(VIRTIO_NET_F_MAC, 5);
        assert_eq!(VIRTIO_NET_F_STATUS, 16);
        assert_eq!(VIRTIO_NET_HDR_SIZE, 12);
        assert_eq!(VIRTIO_NET_HDR_GSO_NONE, 0);
        assert_eq!(RXQ, 0);
        assert_eq!(TXQ, 1);
        assert_eq!(RX_BUF_COUNT, 64);
        assert_eq!(RX_BUF_SIZE, 2048);
        assert!(RX_BUF_SIZE >= 1514); // fits a maximum ethernet frame
    }

    #[test]
    fn test_net_hdr_default_zeroed() {
        let h = VirtioNetHdr::default();
        assert_eq!(h.flags, 0);
        assert_eq!(h.gso_type, 0);
        assert_eq!(h.hdr_len, 0);
        assert_eq!(h.gso_size, 0);
        assert_eq!(h.csum_start, 0);
        assert_eq!(h.csum_offset, 0);
        // 12 bytes, matching the virtio 1.x wire format.
        assert_eq!(core::mem::size_of::<VirtioNetHdr>(), VIRTIO_NET_HDR_SIZE);
    }

    #[test]
    fn test_config_default() {
        let c = VirtioNetConfig::default();
        assert_eq!(c.mac, [0; 6]);
        assert_eq!(c.status, 0);
        assert_eq!(c.max_virtqueue_pairs, 0);
        assert_eq!(c.mtu, 0);
    }

    #[test]
    fn test_state_new() {
        let st = VirtioNetState::new();
        assert!(st.dev.is_none());
        assert_eq!(st.mac, [0; 6]);
        assert_eq!(st.open_count, 0);
        assert_eq!(fifo_len(&st), 0);
    }

    #[test]
    fn test_fifo_roundtrip() {
        let mut st = VirtioNetState::new();
        assert!(fifo_push(&mut st, RxEntry { slot: 3, len: 100 }));
        assert!(fifo_push(&mut st, RxEntry { slot: 7, len: 200 }));
        assert_eq!(fifo_len(&st), 2);
        let e = fifo_pop(&mut st).unwrap();
        assert_eq!(e.slot, 3);
        assert_eq!(e.len, 100);
        let e = fifo_pop(&mut st).unwrap();
        assert_eq!(e.slot, 7);
        assert_eq!(fifo_len(&st), 0);
        assert!(fifo_pop(&mut st).is_none());
    }

    #[test]
    fn test_fifo_wraps() {
        let mut st = VirtioNetState::new();
        // Fill to capacity.
        for i in 0..RX_BUF_COUNT {
            assert!(fifo_push(
                &mut st,
                RxEntry {
                    slot: i as u16,
                    len: 1
                }
            ));
        }
        // Full: further pushes fail.
        assert!(!fifo_push(&mut st, RxEntry { slot: 0, len: 1 }));
        // Pop one, push again (wrap-around reuse of the slot).
        fifo_pop(&mut st);
        assert!(fifo_push(&mut st, RxEntry { slot: 99, len: 1 }));
        assert_eq!(fifo_len(&mut st), RX_BUF_COUNT);
    }

    #[test]
    fn test_ops_without_device() {
        reset_state();
        assert!(virtio_net_open().is_err());
        assert!(virtio_net_close().is_err());
        assert_eq!(virtio_net_rx_pending(), 0);
        assert_eq!(virtio_net_mac(), [0; 6]);
        let mut buf = [0u8; 64];
        assert_eq!(virtio_net_receive(&mut buf), 0);
        assert!(virtio_net_transmit(&[1, 2, 3]).is_err());
    }

    #[test]
    fn test_transmit_rejects_bad_args() {
        reset_state();
        assert!(virtio_net_transmit(&[]).is_err());
        let big = [0u8; TX_BUF_SIZE + 1];
        assert!(virtio_net_transmit(&big).is_err());
    }

    #[test]
    #[ignore = "covered in kernel-tests (QEMU)"]
    fn test_virtio_net_probe_fails_no_hardware() {
        unsafe {
            reset_state();
            // Without real hardware, probe must fail gracefully, not panic.
            assert!(virtio_net_probe(0).is_err());
        }
    }
}
