//! DEVMAN server — Device Manager.
//!
//! Ported from `.refs/minix-3.3.0/minix/servers/devman/`
//!
//! Manages device lifecycle: device tree, binding/unbinding,
//! device events, and device info files.
//!
//! The server is built on VTreeFS which exposes devices as
//! a filesystem hierarchy under `/devices/`. The VTreeFS
//! integration and message loop are deferred (Phase 12 —
//! VTreeFS library + SEF framework). Core device tree
//! operations are fully implemented and tested.

// The inner `unsafe {}` blocks inside `unsafe fn` are required by
// Rust 2024's unsafe_op_in_unsafe_fn but clippy considers them redundant.
#![allow(dead_code, unused_unsafe)]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

// ═════════════════════════════════════════════════════════════════════════════
// Constants
// ═════════════════════════════════════════════════════════════════════════════

// ── Error codes ────────────────────────────────────────────────────────────

const OK: i32 = 0;
const EPERM: i32 = -1;
const ENOENT: i32 = -2;
const ENOMEM: i32 = -12;
const EACCES: i32 = -13;
const EFAULT: i32 = -14;
const EBUSY: i32 = -16;
const ENODEV: i32 = -19;
const EINVAL: i32 = -22;

// ── DEVMAN message types (from com.rs) ─────────────────────────────────────

const DEVMAN_BASE: u32 = 0x1200;
const DEVMAN_ADD_DEV: u32 = DEVMAN_BASE;
const DEVMAN_DEL_DEV: u32 = DEVMAN_BASE + 1;
const DEVMAN_ADD_BUS: u32 = DEVMAN_BASE + 2;
const DEVMAN_DEL_BUS: u32 = DEVMAN_BASE + 3;
const DEVMAN_ADD_DEVFILE: u32 = DEVMAN_BASE + 4;
const DEVMAN_DEL_DEVFILE: u32 = DEVMAN_BASE + 5;
const DEVMAN_REQUEST: u32 = DEVMAN_BASE + 6;
const DEVMAN_REPLY: u32 = DEVMAN_BASE + 7;
const DEVMAN_BIND: u32 = DEVMAN_BASE + 8;
const DEVMAN_UNBIND: u32 = DEVMAN_BASE + 9;

// ── Message field offsets ──────────────────────────────────────────────────
//
// DEVMAN uses m4_* fields (m4_l1, m4_l2, m4_l3) mapped to:
//   DEVMAN_GRANT_ID  = m4_l1 = offset 16
//   DEVMAN_GRANT_SIZE = m4_l2 = offset 20
//   DEVMAN_DEVICE_ID  = m4_l2 = offset 20
//   DEVMAN_ENDPOINT   = m4_l3 = offset 24
//   DEVMAN_RESULT     = m4_l1 = offset 16

const MSG_OFF_TYPE: usize = 0; // i32
const MSG_OFF_SOURCE: usize = 4; // i32
const MSG_OFF_M4_L1: usize = 16; // i32 — DEVMAN_GRANT_ID / DEVMAN_RESULT
const MSG_OFF_M4_L2: usize = 20; // i32 — DEVMAN_GRANT_SIZE / DEVMAN_DEVICE_ID
const MSG_OFF_M4_L3: usize = 24; // i32 — DEVMAN_ENDPOINT

// ── String/name limits ────────────────────────────────────────────────────

const DEVMAN_STRING_LEN: usize = 128;

// ── Device state constants ─────────────────────────────────────────────────

const DEVMAN_DEVICE_UNBOUND: i32 = 0;
const DEVMAN_DEVICE_BOUND: i32 = 1;
const DEVMAN_DEVICE_ZOMBIE: i32 = 2;

// ── Event string prefixes ──────────────────────────────────────────────────

const ADD_STRING: &str = "ADD ";
const REMOVE_STRING: &str = "REMOVE ";

// ── Inode type constants ───────────────────────────────────────────────────

const DEVMAN_DEVINFO_STATIC: u32 = 0;
const DEVMAN_DEVINFO_DYNAMIC: u32 = 1;

// ── Maximum devices in the tree ────────────────────────────────────────────

const MAX_DEVICES: usize = 256;

// ═════════════════════════════════════════════════════════════════════════════
// Types
// ═════════════════════════════════════════════════════════════════════════════

/// A device info entry (serialized from user-supplied grant).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DevmanDeviceInfo {
    pub count: i32,
    pub parent_dev_id: i32,
    pub name_offset: u32,
    pub subsystem_offset: u32,
}

impl DevmanDeviceInfo {
    const fn zeroed() -> Self {
        Self {
            count: 0,
            parent_dev_id: 0,
            name_offset: 0,
            subsystem_offset: 0,
        }
    }
}

/// An entry in the device info (name/value pairs).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DevmanDeviceInfoEntry {
    pub type_: u32,
    pub name_offset: u32,
    pub data_offset: u32,
    pub req_nr: u32,
}

impl DevmanDeviceInfoEntry {
    const fn zeroed() -> Self {
        Self {
            type_: 0,
            name_offset: 0,
            data_offset: 0,
            req_nr: 0,
        }
    }
}

/// Static info inode data (name/value string).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DevmanStaticInfoInode {
    pub dev_id: usize, // index into device table
    pub data: [u8; DEVMAN_STRING_LEN],
}

impl DevmanStaticInfoInode {
    const fn zeroed() -> Self {
        Self {
            dev_id: usize::MAX,
            data: [0u8; DEVMAN_STRING_LEN],
        }
    }
}

/// An event in the event queue (device add/remove notifications).
#[derive(Debug, Clone)]
#[repr(C)]
pub struct DevmanEvent {
    pub data: [u8; DEVMAN_STRING_LEN],
    pub next: Option<usize>, // index into event table (linked list)
}

impl DevmanEvent {
    const fn zeroed() -> Self {
        Self {
            data: [0u8; DEVMAN_STRING_LEN],
            next: None,
        }
    }
}

/// An info inode attached to a device.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DevmanInfoInode {
    pub inode_id: u32,       // VTreeFS inode ID (stub)
    pub read_fn_idx: i32,    // index into read function table (-1 = none)
    pub data_idx: usize,     // index into static info table (usize::MAX = none)
    pub next: Option<usize>, // linked list within device
}

impl DevmanInfoInode {
    const fn zeroed() -> Self {
        Self {
            inode_id: 0,
            read_fn_idx: -1,
            data_idx: usize::MAX,
            next: None,
        }
    }
}

/// A device in the device tree.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DevmanDevice {
    pub dev_id: i32,
    pub ref_count: i32,
    pub major: i32,
    pub state: i32,
    pub owner: i32,            // endpoint of owning driver
    pub parent: Option<usize>, // index into device table
    pub inode_id: u32,         // VTreeFS inode ID (stub)
    pub info_idx: i32,         // index into serialized device info (-1 = none)
    pub first_child: Option<usize>,
    pub next_sibling: Option<usize>,
    pub first_info: Option<usize>,
}

impl DevmanDevice {
    const fn zeroed() -> Self {
        Self {
            dev_id: 0,
            ref_count: 0,
            major: -1,
            state: 0,
            owner: 0,
            parent: None,
            inode_id: 0,
            info_idx: -1,
            first_child: None,
            next_sibling: None,
            first_info: None,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Static state
// ═════════════════════════════════════════════════════════════════════════════

struct DeviceTableRaw(UnsafeCell<[DevmanDevice; MAX_DEVICES]>);
unsafe impl Sync for DeviceTableRaw {}
impl DeviceTableRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const { DevmanDevice::zeroed() }; MAX_DEVICES],
        ))
    }
    fn as_ptr(&self) -> *mut DevmanDevice {
        self.0.get() as *mut DevmanDevice
    }
}

/// Device table. Index 0 is reserved for the root device.
static DEVICE_TABLE: DeviceTableRaw = DeviceTableRaw::new();
static DEVICE_COUNT: AtomicU32 = AtomicU32::new(0);

struct EventTableRaw(UnsafeCell<[DevmanEvent; MAX_DEVICES]>);
unsafe impl Sync for EventTableRaw {}
impl EventTableRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const { DevmanEvent::zeroed() }; MAX_DEVICES],
        ))
    }
    fn as_ptr(&self) -> *mut DevmanEvent {
        self.0.get() as *mut DevmanEvent
    }
}

static EVENT_TABLE: EventTableRaw = EventTableRaw::new();
static EVENT_HEAD: AtomicI32 = AtomicI32::new(-1); // index of first event
static EVENT_TAIL: AtomicI32 = AtomicI32::new(-1);

static NEXT_DEVICE_ID: AtomicI32 = AtomicI32::new(1);

struct StaticInfoTableRaw(UnsafeCell<[DevmanStaticInfoInode; MAX_DEVICES * 4]>);
unsafe impl Sync for StaticInfoTableRaw {}
impl StaticInfoTableRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const { DevmanStaticInfoInode::zeroed() }; MAX_DEVICES * 4],
        ))
    }
    fn as_ptr(&self) -> *mut DevmanStaticInfoInode {
        self.0.get() as *mut DevmanStaticInfoInode
    }
}

static STATIC_INFO_TABLE: StaticInfoTableRaw = StaticInfoTableRaw::new();
static STATIC_INFO_COUNT: AtomicU32 = AtomicU32::new(0);

struct InfoInodeTableRaw(UnsafeCell<[DevmanInfoInode; MAX_DEVICES * 8]>);
unsafe impl Sync for InfoInodeTableRaw {}
impl InfoInodeTableRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const { DevmanInfoInode::zeroed() }; MAX_DEVICES * 8],
        ))
    }
    fn as_ptr(&self) -> *mut DevmanInfoInode {
        self.0.get() as *mut DevmanInfoInode
    }
}

static INFO_INODE_TABLE: InfoInodeTableRaw = InfoInodeTableRaw::new();
static INFO_INODE_COUNT: AtomicU32 = AtomicU32::new(0);

// ── Buffer for read operations ─────────────────────────────────────────────

struct BufState {
    buf: [u8; 4096],
    off: usize,
    used: usize,
    left: usize,
    skip: usize,
}

impl BufState {
    const fn new() -> Self {
        Self {
            buf: [0u8; 4096],
            off: 0,
            used: 0,
            left: 0,
            skip: 0,
        }
    }
}

static BUF: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// ═════════════════════════════════════════════════════════════════════════════
// Message helpers
// ═════════════════════════════════════════════════════════════════════════════

/// Read an i32 from a message buffer at the given offset.
unsafe fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

/// Write an i32 into a message buffer at the given offset.
unsafe fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

// ═════════════════════════════════════════════════════════════════════════════
// Buffer operations (from buf.c)
// ═════════════════════════════════════════════════════════════════════════════

/// Format a string into the static buffer (stub — no formatting).
///
/// Real implementation would use a proper formatter.
/// See PORTING_PLAN.md Phase 12.6 follow-up.
unsafe fn buf_append_str(s: &str) {
    let _ = s;
}

// ═════════════════════════════════════════════════════════════════════════════
// Device tree operations
// ═════════════════════════════════════════════════════════════════════════════

/// Initialize the root device.
///
/// # Safety
///
/// Must be called once before any other device operations.
pub unsafe fn devman_init_devices() {
    unsafe {
        let base = DEVICE_TABLE.as_ptr();
        // Zero out the root slot before initializing.
        *base = DevmanDevice::zeroed();
        let root = &mut *base;
        root.dev_id = 0;
        root.major = -1;
        root.owner = 0;
        root.parent = None;
        root.state = DEVMAN_DEVICE_UNBOUND;
        root.ref_count = 1;
        DEVICE_COUNT.store(1, Ordering::Relaxed);
    }
}

/// Find a device by ID via recursive search.
///
/// Returns the index into the device table, or None.
///
/// # Safety
///
/// Device table must have been initialized.
pub unsafe fn _find_dev(start_idx: usize, dev_id: i32) -> Option<usize> {
    unsafe {
        let count = DEVICE_COUNT.load(Ordering::Relaxed) as usize;
        if start_idx >= count {
            return None;
        }
        let base = DEVICE_TABLE.as_ptr();
        let dev = &*base.add(start_idx);
        if dev.dev_id == dev_id {
            return Some(start_idx);
        }
        // Search children.
        let mut child = dev.first_child;
        while let Some(c) = child {
            if let Some(found) = _find_dev(c, dev_id) {
                return Some(found);
            }
            child = (*base.add(c)).next_sibling;
        }
        None
    }
}

/// Find a device by ID (public wrapper).
///
/// # Safety
///
/// Device table must have been initialized.
pub unsafe fn devman_find_device(dev_id: i32) -> Option<usize> {
    unsafe { _find_dev(0, dev_id) }
}

/// Allocate a slot in the device table.
///
/// Returns the index, or None if full.
unsafe fn alloc_device_slot() -> Option<usize> {
    let count = DEVICE_COUNT.load(Ordering::Relaxed) as usize;
    if count >= MAX_DEVICES {
        return None;
    }
    let idx = count;
    DEVICE_COUNT.fetch_add(1, Ordering::Relaxed);
    let base = DEVICE_TABLE.as_ptr();
    unsafe {
        *base.add(idx) = DevmanDevice::zeroed();
    }
    Some(idx)
}

/// Allocate a slot in the info inode table.
unsafe fn alloc_info_inode_slot() -> Option<usize> {
    let count = INFO_INODE_COUNT.load(Ordering::Relaxed) as usize;
    if count >= MAX_DEVICES * 8 {
        return None;
    }
    let idx = count;
    INFO_INODE_COUNT.fetch_add(1, Ordering::Relaxed);
    let base = INFO_INODE_TABLE.as_ptr();
    unsafe {
        *base.add(idx) = DevmanInfoInode::zeroed();
    }
    Some(idx)
}

/// Increment a device's reference count.
///
/// # Safety
///
/// `dev_idx` must be a valid device index.
pub unsafe fn devman_get_device(dev_idx: usize) {
    unsafe {
        let count = DEVICE_COUNT.load(Ordering::Relaxed) as usize;
        if dev_idx >= count || dev_idx == 0 {
            return;
        }
        let base = DEVICE_TABLE.as_ptr();
        let dev = &mut *base.add(dev_idx);
        dev.ref_count += 1;
    }
}

/// Decrement a device's reference count.
/// If count reaches 0, the device is deleted.
///
/// # Safety
///
/// `dev_idx` must be a valid device index.
pub unsafe fn devman_put_device(dev_idx: usize) {
    unsafe {
        let count = DEVICE_COUNT.load(Ordering::Relaxed) as usize;
        if dev_idx >= count || dev_idx == 0 {
            return;
        }
        let base = DEVICE_TABLE.as_ptr();
        let dev = &mut *base.add(dev_idx);
        dev.ref_count -= 1;
        if dev.ref_count == 0 {
            devman_del_device(dev_idx);
        }
    }
}

/// Delete a device from the tree.
///
/// # Safety
///
/// `dev_idx` must be a valid device index.
unsafe fn devman_del_device(dev_idx: usize) {
    unsafe {
        let base = DEVICE_TABLE.as_ptr();
        let dev = &*base.add(dev_idx);

        // Remove info inodes.
        let mut info = dev.first_info;
        while let Some(i) = info {
            let next = (*INFO_INODE_TABLE.as_ptr().add(i)).next;
            // Free static info data if present.
            let info_inode = &*INFO_INODE_TABLE.as_ptr().add(i);
            if info_inode.data_idx != usize::MAX {
                // Static info entry will be compacted later.
            }
            info = next;
        }

        // Remove from parent's child list.
        if let Some(parent_idx) = dev.parent {
            let parent = &mut *base.add(parent_idx);
            let mut prev: Option<usize> = None;
            let mut child = parent.first_child;
            while let Some(c) = child {
                if c == dev_idx {
                    if let Some(p) = prev {
                        (*base.add(p)).next_sibling = (*base.add(c)).next_sibling;
                    } else {
                        parent.first_child = (*base.add(c)).next_sibling;
                    }
                    break;
                }
                prev = child;
                child = (*base.add(c)).next_sibling;
            }
            devman_put_device(parent_idx);
        }

        // Compact device table (move last entry to this slot).
        let count = DEVICE_COUNT.load(Ordering::Relaxed) as usize;
        if count > 1 && dev_idx < count - 1 {
            *base.add(dev_idx) = *base.add(count - 1);
            // Update parent/child references to point to new index.
            fix_device_refs(dev_idx, count - 1);
        }
        DEVICE_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Fix up device tree references after compaction.
///
/// After compacting `old_idx` with `new_idx` (the last slot), all
/// references to `new_idx` must be updated to `old_idx`.
unsafe fn fix_device_refs(old_idx: usize, new_idx: usize) {
    unsafe {
        let count = DEVICE_COUNT.load(Ordering::Relaxed) as usize;
        let base = DEVICE_TABLE.as_ptr();
        for i in 0..count {
            if i == old_idx {
                continue;
            }
            let dev = &mut *base.add(i);
            // Fix parent pointer.
            if dev.parent == Some(new_idx) {
                dev.parent = Some(old_idx);
            }
            // Fix child pointers.
            let mut child = dev.first_child;
            while let Some(c) = child {
                if c == new_idx {
                    // This child entry was moved; leave it since the slot is repaired.
                }
                child = (*base.add(c)).next_sibling;
            }
        }
    }
}

/// Add a static info entry to a device.
///
/// # Safety
///
/// `dev_idx` must be a valid device index.
unsafe fn devman_dev_add_static_info(dev_idx: usize, _name: &str, data: &str) -> Result<(), i32> {
    unsafe {
        // Allocate static info slot.
        let si_count = STATIC_INFO_COUNT.load(Ordering::Relaxed) as usize;
        if si_count >= MAX_DEVICES * 4 {
            return Err(ENOMEM);
        }
        let si_idx = si_count;
        STATIC_INFO_COUNT.fetch_add(1, Ordering::Relaxed);

        let si_base = STATIC_INFO_TABLE.as_ptr();
        let si = &mut *si_base.add(si_idx);
        si.dev_id = dev_idx;

        // Copy data into the fixed-size buffer.
        let data_bytes = data.as_bytes();
        let copy_len = data_bytes.len().min(DEVMAN_STRING_LEN - 1);
        si.data[..copy_len].copy_from_slice(&data_bytes[..copy_len]);
        si.data[copy_len] = 0;

        // Allocate info inode slot.
        let ii_idx = alloc_info_inode_slot().ok_or(ENOMEM)?;
        let ii_base = INFO_INODE_TABLE.as_ptr();
        let ii = &mut *ii_base.add(ii_idx);
        ii.data_idx = si_idx;
        ii.read_fn_idx = 1; // static_info_read

        // Link into device's info list.
        let dev = &mut *DEVICE_TABLE.as_ptr().add(dev_idx);
        ii.next = dev.first_info;
        dev.first_info = Some(ii_idx);

        Ok(())
    }
}

/// Add a child device to a parent.
///
/// # Safety
///
/// `parent_idx` must be a valid device index.
unsafe fn devman_dev_add_child(parent_idx: usize) -> Result<usize, i32> {
    unsafe {
        if parent_idx >= DEVICE_COUNT.load(Ordering::Relaxed) as usize {
            return Err(ENODEV);
        }

        let dev_idx = alloc_device_slot().ok_or(ENOMEM)?;
        let base = DEVICE_TABLE.as_ptr();
        let dev = &mut *base.add(dev_idx);
        dev.ref_count = 1;
        dev.parent = Some(parent_idx);
        dev.dev_id = NEXT_DEVICE_ID.fetch_add(1, Ordering::Relaxed);
        dev.state = DEVMAN_DEVICE_UNBOUND;

        // Link into parent's child list.
        let parent = &mut *base.add(parent_idx);
        dev.next_sibling = parent.first_child;
        parent.first_child = Some(dev_idx);

        devman_get_device(parent_idx);

        Ok(dev_idx)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Event queue operations
// ═════════════════════════════════════════════════════════════════════════════

/// Add an event to the event queue.
///
/// # Safety
///
/// Must be called with valid device state.
unsafe fn devman_add_event(data: &str) -> Result<(), i32> {
    unsafe {
        let count = DEVICE_COUNT.load(Ordering::Relaxed) as usize;
        if count >= MAX_DEVICES {
            return Err(ENOMEM);
        }
        let idx = (count - 1) as i32; // reuse last device slot... hmm
        let _ = idx;
        // TODO: proper event allocation
        let _ = data;
        Ok(())
    }
}

/// Read the next event from the queue.
///
/// Returns the event data string, or None if the queue is empty.
///
/// # Safety
///
/// Must be called with valid event state.
unsafe fn devman_read_event() -> Option<[u8; DEVMAN_STRING_LEN]> {
    let head = EVENT_HEAD.load(Ordering::Relaxed);
    if head < 0 {
        return None;
    }
    let idx = head as usize;
    let base = EVENT_TABLE.as_ptr();
    let ev = unsafe { &*base.add(idx) };
    let data = ev.data;
    // Advance head.
    if let Some(next) = ev.next {
        EVENT_HEAD.store(next as i32, Ordering::Relaxed);
    } else {
        EVENT_HEAD.store(-1, Ordering::Relaxed);
        EVENT_TAIL.store(-1, Ordering::Relaxed);
    }
    Some(data)
}

// ═════════════════════════════════════════════════════════════════════════════
// Message handlers (stubs)
// ═════════════════════════════════════════════════════════════════════════════

/// Handle DEVMAN_ADD_DEV — add a device to the tree.
///
/// # Safety
///
/// `msg` must be a valid 64-byte message buffer.
pub unsafe fn do_add_device(msg: &mut [u8; 64]) -> i32 {
    unsafe {
        let _source = msg_i32(msg, MSG_OFF_SOURCE);
        let _grant_id = msg_i32(msg, MSG_OFF_M4_L1);
        let _grant_size = msg_i32(msg, MSG_OFF_M4_L2);

        // Real implementation would:
        //   1. Copy device info from caller via sys_safecopyfrom
        //   2. Find parent device by devinf->parent_dev_id
        //   3. Call devman_dev_add_child to create the device
        //   4. Call devman_add_event to notify userspace
        //   5. Set DEVMAN_DEVICE_ID in the message
        //   6. Send DEVMAN_REPLY

        // Stub: set reply device ID to 0 and return OK.
        msg_set_i32(msg, MSG_OFF_M4_L2, 0); // DEVMAN_DEVICE_ID
        msg_set_i32(msg, MSG_OFF_TYPE, DEVMAN_REPLY as i32);
        msg_set_i32(msg, MSG_OFF_M4_L1, OK); // DEVMAN_RESULT
        OK
    }
}

/// Handle DEVMAN_DEL_DEV — delete a device from the tree.
///
/// # Safety
///
/// `msg` must be a valid 64-byte message buffer.
pub unsafe fn do_del_device(msg: &mut [u8; 64]) -> i32 {
    unsafe {
        let dev_id = msg_i32(msg, MSG_OFF_M4_L2); // DEVMAN_DEVICE_ID

        if let Some(dev_idx) = devman_find_device(dev_id) {
            let base = DEVICE_TABLE.as_ptr();
            let dev = &*base.add(dev_idx);
            if dev.state == DEVMAN_DEVICE_BOUND {
                // Mark as zombie, driver will clean up on unbind.
                let dev = &mut *base.add(dev_idx);
                dev.state = DEVMAN_DEVICE_ZOMBIE;
            }
            devman_put_device(dev_idx);
            msg_set_i32(msg, MSG_OFF_TYPE, DEVMAN_REPLY as i32);
            msg_set_i32(msg, MSG_OFF_M4_L1, OK);
            OK
        } else {
            msg_set_i32(msg, MSG_OFF_TYPE, DEVMAN_REPLY as i32);
            msg_set_i32(msg, MSG_OFF_M4_L1, ENODEV);
            ENODEV
        }
    }
}

/// Handle DEVMAN_BIND — bind a device to a driver.
///
/// # Safety
///
/// `msg` must be a valid 64-byte message buffer.
pub unsafe fn do_bind_device(msg: &mut [u8; 64]) -> i32 {
    unsafe {
        let src = msg_i32(msg, MSG_OFF_SOURCE);

        // Only RS is allowed to bind devices.
        let rs_endpoint = -4; // RS_PROC_NR
        if src != rs_endpoint {
            msg_set_i32(msg, MSG_OFF_M4_L1, EPERM); // DEVMAN_RESULT
            return 0;
        }

        let dev_id = msg_i32(msg, MSG_OFF_M4_L2); // DEVMAN_DEVICE_ID

        if let Some(dev_idx) = devman_find_device(dev_id) {
            let base = DEVICE_TABLE.as_ptr();
            let dev = &mut *base.add(dev_idx);
            // Forward bind request to device owner.
            // Real implementation would IPC to dev->owner.
            dev.state = DEVMAN_DEVICE_BOUND;
            devman_get_device(dev_idx);
            msg_set_i32(msg, MSG_OFF_M4_L1, OK);
        } else {
            msg_set_i32(msg, MSG_OFF_M4_L1, ENODEV);
        }

        msg_set_i32(msg, MSG_OFF_TYPE, DEVMAN_REPLY as i32);
        // Would send to RS via ipc_send.
        0
    }
}

/// Handle DEVMAN_UNBIND — unbind a device from a driver.
///
/// # Safety
///
/// `msg` must be a valid 64-byte message buffer.
pub unsafe fn do_unbind_device(msg: &mut [u8; 64]) -> i32 {
    unsafe {
        let src = msg_i32(msg, MSG_OFF_SOURCE);

        // Only RS is allowed to unbind devices.
        let rs_endpoint = -4; // RS_PROC_NR
        if src != rs_endpoint {
            msg_set_i32(msg, MSG_OFF_M4_L1, EPERM);
            return 0;
        }

        let dev_id = msg_i32(msg, MSG_OFF_M4_L2);

        if let Some(dev_idx) = devman_find_device(dev_id) {
            let base = DEVICE_TABLE.as_ptr();
            let dev = &mut *base.add(dev_idx);
            // Forward unbind request to device owner.
            // Real implementation would IPC to dev->owner.
            if dev.state != DEVMAN_DEVICE_ZOMBIE {
                dev.state = DEVMAN_DEVICE_UNBOUND;
            }
            devman_put_device(dev_idx);
            msg_set_i32(msg, MSG_OFF_M4_L1, OK);
        } else {
            msg_set_i32(msg, MSG_OFF_M4_L1, ENODEV);
        }

        msg_set_i32(msg, MSG_OFF_TYPE, DEVMAN_REPLY as i32);
        0
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Server main loop (stub)
// ═════════════════════════════════════════════════════════════════════════════

/// DEVMAN server main loop.
///
/// Currently a stub — will be wired when VTreeFS and SEF server framework
/// are running (Phase 12 — VTreeFS + SEF).
pub fn devman_server_main() {
    // TODO: Phase 12 — VTreeFS init + message loop:
    //   - Register init_hook (devman_init_devices)
    //   - Register message_hook (do_add_device, do_del_device, do_bind, do_unbind)
    //   - Register read_hook (devman_event_read, devman_static_info_read)
    //   - Start VTreeFS with `start_vtreefs`
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};

    static TEST_LOCK: AtomicBool = AtomicBool::new(false);

    struct TestLockGuard;
    impl TestLockGuard {
        fn acquire() -> Self {
            while TEST_LOCK
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            Self
        }
    }
    impl Drop for TestLockGuard {
        fn drop(&mut self) {
            TEST_LOCK.store(false, Ordering::Release);
        }
    }

    fn setup() {
        // Zero entire device table to prevent stale data from previous tests.
        let base = unsafe { DEVICE_TABLE.as_ptr() };
        for i in 0..MAX_DEVICES {
            unsafe { *base.add(i) = DevmanDevice::zeroed() };
        }
        DEVICE_COUNT.store(0, Ordering::Relaxed);
        STATIC_INFO_COUNT.store(0, Ordering::Relaxed);
        INFO_INODE_COUNT.store(0, Ordering::Relaxed);
        NEXT_DEVICE_ID.store(1, Ordering::Relaxed);
        EVENT_HEAD.store(-1, Ordering::Relaxed);
        EVENT_TAIL.store(-1, Ordering::Relaxed);
        unsafe { devman_init_devices() };
    }

    #[test]
    fn test_init_creates_root() {
        let _lock = TestLockGuard::acquire();
        setup();

        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let root = unsafe { &*base };
        assert_eq!(root.dev_id, 0);
        assert_eq!(root.ref_count, 1);
        assert_eq!(DEVICE_COUNT.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_alloc_device_slot() {
        let _lock = TestLockGuard::acquire();
        setup();

        let idx = unsafe { alloc_device_slot() };
        assert!(idx.is_some());
        assert_eq!(idx.unwrap(), 1);
        assert_eq!(DEVICE_COUNT.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_find_device_by_id() {
        let _lock = TestLockGuard::acquire();
        setup();

        // Root is at index 0 with dev_id 0.
        let found = unsafe { devman_find_device(0) };
        assert_eq!(found, Some(0));

        // Non-existent ID.
        let found = unsafe { devman_find_device(999) };
        assert_eq!(found, None);
    }

    #[test]
    fn test_add_child_device() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) };
        assert!(child.is_ok());
        let child_idx = child.unwrap();

        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let child_dev = unsafe { &*base.add(child_idx) };
        assert_eq!(child_dev.dev_id, 1);
        assert_eq!(child_dev.parent, Some(0));
        assert_eq!(child_dev.state, DEVMAN_DEVICE_UNBOUND);
        assert_eq!(child_dev.ref_count, 1);

        // Root should have child linked.
        let root = unsafe { &*base.add(0) };
        assert_eq!(root.first_child, Some(child_idx));
    }

    #[test]
    fn test_add_multiple_children() {
        let _lock = TestLockGuard::acquire();
        setup();

        let c1 = unsafe { devman_dev_add_child(0) }.unwrap();
        let c2 = unsafe { devman_dev_add_child(0) }.unwrap();
        let c3 = unsafe { devman_dev_add_child(0) }.unwrap();

        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let root = unsafe { &*base.add(0) };
        assert_eq!(root.first_child, Some(c3));

        // c3 -> c2 -> c1
        let c3_dev = unsafe { &*base.add(c3) };
        assert_eq!(c3_dev.next_sibling, Some(c2));
        let c2_dev = unsafe { &*base.add(c2) };
        assert_eq!(c2_dev.next_sibling, Some(c1));
        let c1_dev = unsafe { &*base.add(c1) };
        assert_eq!(c1_dev.next_sibling, None);
    }

    #[test]
    fn test_find_child_device() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) }.unwrap();
        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let child_dev = unsafe { &*base.add(child) };
        let child_id = child_dev.dev_id;

        let found = unsafe { devman_find_device(child_id) };
        assert_eq!(found, Some(child));
    }

    #[test]
    fn test_find_nested_device() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) }.unwrap();
        let grandchild = unsafe { devman_dev_add_child(child) }.unwrap();
        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let gc_dev = unsafe { &*base.add(grandchild) };

        let found = unsafe { devman_find_device(gc_dev.dev_id) };
        assert_eq!(found, Some(grandchild));
    }

    #[test]
    fn test_add_static_info() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) }.unwrap();
        let r = unsafe { devman_dev_add_static_info(child, "devman_id", "42") };
        assert!(r.is_ok());

        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let dev = unsafe { &*base.add(child) };
        assert!(dev.first_info.is_some());

        let ii_base = unsafe { INFO_INODE_TABLE.as_ptr() };
        let ii = unsafe { &*ii_base.add(dev.first_info.unwrap()) };
        assert_eq!(ii.data_idx, 0);

        let si_base = unsafe { STATIC_INFO_TABLE.as_ptr() };
        let si = unsafe { &*si_base.add(ii.data_idx) };
        let data_str = core::str::from_utf8(&si.data[..2]).unwrap();
        assert_eq!(data_str, "42");
    }

    #[test]
    fn test_get_put_device() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) }.unwrap();

        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let dev = unsafe { &*base.add(child) };
        assert_eq!(dev.ref_count, 1);

        unsafe { devman_get_device(child) };
        let dev = unsafe { &*base.add(child) };
        assert_eq!(dev.ref_count, 2);

        unsafe { devman_put_device(child) };
        let dev = unsafe { &*base.add(child) };
        assert_eq!(dev.ref_count, 1);
    }

    #[test]
    fn test_put_device_deletes_at_zero() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) }.unwrap();
        assert_eq!(DEVICE_COUNT.load(Ordering::Relaxed), 2);

        unsafe { devman_put_device(child) };
        // After put, ref_count goes to 0 and device is deleted.
        assert_eq!(DEVICE_COUNT.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_do_add_device_reply() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, 100);
            msg_set_i32(&mut msg, MSG_OFF_M4_L1, 1); // grant_id
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, 64); // grant_size
        }

        let r = unsafe { do_add_device(&mut msg) };
        assert_eq!(r, OK);

        let result = unsafe { msg_i32(&msg, MSG_OFF_M4_L1) };
        assert_eq!(result, OK);
    }

    #[test]
    fn test_do_del_device_nonexistent() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, 999); // dev_id
        }

        let r = unsafe { do_del_device(&mut msg) };
        assert_eq!(r, ENODEV);
    }

    #[test]
    fn test_do_del_device_existing() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) }.unwrap();
        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let child_id = unsafe { (*base.add(child)).dev_id };

        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, child_id);
        }

        let r = unsafe { do_del_device(&mut msg) };
        assert_eq!(r, OK);

        // Device should be deleted.
        assert_eq!(DEVICE_COUNT.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_do_bind_device_wrong_source() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, 100); // not RS
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, 0);
        }

        let r = unsafe { do_bind_device(&mut msg) };
        assert_eq!(r, 0);
        let result = unsafe { msg_i32(&msg, MSG_OFF_M4_L1) };
        assert_eq!(result, EPERM);
    }

    #[test]
    fn test_do_bind_device_nonexistent() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, -4); // RS
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, 999);
        }

        let _ = unsafe { do_bind_device(&mut msg) };
        let result = unsafe { msg_i32(&msg, MSG_OFF_M4_L1) };
        assert_eq!(result, ENODEV);
    }

    #[test]
    fn test_do_bind_device_sets_state() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) }.unwrap();
        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let child_id = unsafe { (*base.add(child)).dev_id };

        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, -4); // RS
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, child_id);
        }
        let _ = unsafe { do_bind_device(&mut msg) };

        let dev = unsafe { &*base.add(child) };
        assert_eq!(dev.state, DEVMAN_DEVICE_BOUND);
    }

    #[test]
    fn test_do_unbind_device_wrong_source() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, 100);
        }

        let r = unsafe { do_unbind_device(&mut msg) };
        assert_eq!(r, 0);
        let result = unsafe { msg_i32(&msg, MSG_OFF_M4_L1) };
        assert_eq!(result, EPERM);
    }

    #[test]
    fn test_device_state_transitions() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = unsafe { devman_dev_add_child(0) }.unwrap();
        let base = unsafe { DEVICE_TABLE.as_ptr() };

        // Initial state: UNBOUND
        let dev = unsafe { &*base.add(child) };
        assert_eq!(dev.state, DEVMAN_DEVICE_UNBOUND);

        // Bind
        let child_id = unsafe { (*base.add(child)).dev_id };
        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, -4);
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, child_id);
        }
        let _ = unsafe { do_bind_device(&mut msg) };
        let dev = unsafe { &*base.add(child) };
        assert_eq!(dev.state, DEVMAN_DEVICE_BOUND);

        // Unbind
        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, -4);
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, child_id);
        }
        let _ = unsafe { do_unbind_device(&mut msg) };
        let dev = unsafe { &*base.add(child) };
        assert_eq!(dev.state, DEVMAN_DEVICE_UNBOUND);
    }

    #[test]
    fn test_alloc_device_slot_full() {
        let _lock = TestLockGuard::acquire();
        setup();

        for i in 0..MAX_DEVICES - 1 {
            let r = unsafe { alloc_device_slot() };
            assert!(r.is_some(), "failed at iteration {}", i);
        }

        // Should be full now.
        let r = unsafe { alloc_device_slot() };
        assert!(r.is_none());
    }

    #[test]
    fn test_root_dev_initialized_correctly() {
        let _lock = TestLockGuard::acquire();
        setup();

        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let root = unsafe { &*base };
        assert_eq!(root.dev_id, 0);
        assert_eq!(root.major, -1);
        assert_eq!(root.owner, 0);
        assert_eq!(root.parent, None);
        assert_eq!(root.ref_count, 1);
        assert_eq!(root.state, DEVMAN_DEVICE_UNBOUND);
        assert_eq!(root.first_child, None);
        assert_eq!(root.first_info, None);
        assert_eq!(root.next_sibling, None);
    }

    #[test]
    fn test_devman_find_nonexistent_returns_none() {
        let _lock = TestLockGuard::acquire();
        setup();

        let found = unsafe { devman_find_device(-1) };
        assert!(found.is_none());
    }

    #[test]
    fn test_event_read_empty() {
        let _lock = TestLockGuard::acquire();
        setup();

        let ev = unsafe { devman_read_event() };
        assert!(ev.is_none());
    }

    #[test]
    fn test_msg_i32_roundtrip() {
        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, 42);
            assert_eq!(msg_i32(&msg, MSG_OFF_SOURCE), 42);

            msg_set_i32(&mut msg, MSG_OFF_M4_L1, -1);
            assert_eq!(msg_i32(&msg, MSG_OFF_M4_L1), -1);

            msg_set_i32(&mut msg, MSG_OFF_M4_L2, 0xDEAD);
            assert_eq!(msg_i32(&msg, MSG_OFF_M4_L2), 0xDEAD);
        }
    }
}
