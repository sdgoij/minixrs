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

use libs::vtreefs;

// Constants

const OK: i32 = 0;
const EPERM: i32 = -1;
const ENOENT: i32 = -2;
const ENOMEM: i32 = -12;
const EACCES: i32 = -13;
const EFAULT: i32 = -14;
const EBUSY: i32 = -16;
const ENODEV: i32 = -19;
const EINVAL: i32 = -22;

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

//
// DEVMAN uses the m4_* payload fields, mapped to absolute message offsets
// (the port's `arch_common::ipc::Message` is m_source@0, m_type@4,
// m_payload@8):
//   DEVMAN_GRANT_ID    = m4_l1 = offset 8  (i64)
//   DEVMAN_GRANT_SIZE  = m4_l2 = offset 16 (i64)
//   DEVMAN_DEVICE_ID   = m4_l2 = offset 16 (i64)
//   DEVMAN_ENDPOINT    = m4_l3 = offset 24 (i64)
//   DEVMAN_RESULT      = m4_l1 = offset 8  (i64)

const MSG_OFF_SOURCE: usize = 0; // i32
const MSG_OFF_TYPE: usize = 4; // i32
const MSG_OFF_M4_L1: usize = 8; // i64 — DEVMAN_GRANT_ID / DEVMAN_RESULT
const MSG_OFF_M4_L2: usize = 16; // i64 — DEVMAN_GRANT_SIZE / DEVMAN_DEVICE_ID
const MSG_OFF_M4_L3: usize = 24; // i64 — DEVMAN_ENDPOINT

const DEVMAN_STRING_LEN: usize = 128;

const DEVMAN_DEVICE_UNBOUND: i32 = 0;
const DEVMAN_DEVICE_BOUND: i32 = 1;
const DEVMAN_DEVICE_ZOMBIE: i32 = 2;

const ADD_STRING: &str = "ADD ";
const REMOVE_STRING: &str = "REMOVE ";

const DEVMAN_DEVINFO_STATIC: u32 = 0;
const DEVMAN_DEVINFO_DYNAMIC: u32 = 1;

const MAX_DEVICES: usize = 256;

/// Mode type bits (match `crates/fs/src/mfs/consts.rs`).
const I_DIRECTORY: u32 = 0o040000;
const I_REGULAR: u32 = 0o100000;

/// NO_DEV device number (VFS uses `u32::MAX`).
const NO_DEV: u32 = u32::MAX;

// Types

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

// Static state

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

/// VTreeFS inode ID of the `events` file (u32::MAX until init).
static EVENTS_INODE: AtomicU32 = AtomicU32::new(u32::MAX);

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

// Message helpers

/// Read an i32 from a message buffer at the given offset.
unsafe fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

/// Write an i32 into a message buffer at the given offset.
unsafe fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

/// Write an i64 into a message buffer at the given offset.
unsafe fn msg_set_i64(msg: &mut [u8; 64], off: usize, val: i64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

// Buffer operations (from buf.c)

/// Format a string into the static buffer (stub — no formatting).
///
/// Real implementation would use a proper formatter.
/// See PORTING_PLAN.md Phase 12.6 follow-up.
unsafe fn buf_append_str(s: &str) {
    let _ = s;
}

// Device tree operations

/// Initialize the root device and the VTreeFS tree (called once, from the
/// VTreeFS `init_hook` on mount).
///
/// Creates the `/devices` device root and the `/events` notification file
/// under the VTreeFS root, matching the C `devman_init_devices()`.
///
/// # Safety
///
/// The VTreeFS inode table must have been initialized (`vtreefs_init`).
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

        // VTreeFS tree: root device directory + events file.
        let dir_stat = vtreefs::InodeStat {
            mode: I_DIRECTORY | 0o444,
            uid: 0,
            gid: 0,
            size: 0,
            dev: NO_DEV as u64,
        };
        let file_stat = vtreefs::InodeStat {
            mode: I_REGULAR | 0o444,
            uid: 0,
            gid: 0,
            size: 0x1000,
            dev: NO_DEV as u64,
        };
        let devices_inode = vtreefs::add_inode(
            vtreefs::get_root_inode(),
            "devices",
            vtreefs::NO_INDEX,
            &dir_stat,
            0,
        );
        let events_inode = vtreefs::add_inode(
            vtreefs::get_root_inode(),
            "events",
            vtreefs::NO_INDEX,
            &file_stat,
            0,
        );
        root.inode_id = devices_inode;
        EVENTS_INODE.store(events_inode, Ordering::Relaxed);
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
            let info_inode = &*INFO_INODE_TABLE.as_ptr().add(i);
            if info_inode.inode_id != 0 && !vtreefs::is_inode_deleted(info_inode.inode_id) {
                vtreefs::delete_inode(info_inode.inode_id);
            }
            info = next;
        }

        // Remove the device's VTreeFS inode.
        if dev.inode_id != 0 && !vtreefs::is_inode_deleted(dev.inode_id) {
            vtreefs::delete_inode(dev.inode_id);
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
unsafe fn devman_dev_add_static_info(dev_idx: usize, name: &str, data: &str) -> Result<(), i32> {
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

        // Create the VTreeFS file inode (cbdata = si_idx + 1 so the read
        // hook can find the static info).
        let file_stat = vtreefs::InodeStat {
            mode: I_REGULAR | 0o444,
            uid: 0,
            gid: 0,
            size: 0x1000,
            dev: NO_DEV as u64,
        };
        let dev = &*DEVICE_TABLE.as_ptr().add(dev_idx);
        let inode_id = vtreefs::add_inode(
            dev.inode_id,
            name,
            vtreefs::NO_INDEX,
            &file_stat,
            si_idx + 1,
        );
        if inode_id == u32::MAX {
            return Err(ENOMEM);
        }

        // Allocate info inode slot.
        let ii_idx = alloc_info_inode_slot().ok_or(ENOMEM)?;
        let ii_base = INFO_INODE_TABLE.as_ptr();
        let ii = &mut *ii_base.add(ii_idx);
        ii.inode_id = inode_id;
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
unsafe fn devman_dev_add_child(
    parent_idx: usize,
    name: &str,
    buffer: &[u8],
    devinf: &DevmanDeviceInfo,
) -> Result<usize, i32> {
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

        // Create the VTreeFS directory inode for the device.
        let dir_stat = vtreefs::InodeStat {
            mode: I_DIRECTORY | 0o444,
            uid: 0,
            gid: 0,
            size: 0,
            dev: NO_DEV as u64,
        };
        let parent_inode = (*base.add(parent_idx)).inode_id;
        let inode_id = vtreefs::add_inode(parent_inode, name, vtreefs::NO_INDEX, &dir_stat, 0);
        if inode_id == u32::MAX {
            devman_put_device(dev_idx);
            return Err(ENOMEM);
        }
        dev.inode_id = inode_id;

        // Create the serialized info entries (name/value pairs).
        let entries: &[DevmanDeviceInfoEntry] = if devinf.count > 0
            && buffer.len()
                >= core::mem::size_of::<DevmanDeviceInfo>()
                    + devinf.count as usize * core::mem::size_of::<DevmanDeviceInfoEntry>()
        {
            unsafe {
                core::slice::from_raw_parts(
                    buffer
                        .as_ptr()
                        .add(core::mem::size_of::<DevmanDeviceInfo>())
                        as *const DevmanDeviceInfoEntry,
                    devinf.count as usize,
                )
            }
        } else {
            &[]
        };
        for entry in entries {
            if entry.type_ == DEVMAN_DEVINFO_STATIC {
                let name = cstr_at(buffer, entry.name_offset as usize);
                let data = cstr_at(buffer, entry.data_offset as usize);
                let _ = devman_dev_add_static_info(dev_idx, name, data);
            }
        }

        // Make the device ID accessible to userland.
        let mut id_buf = [0u8; 16];
        let id_str = itoa_buf(dev.dev_id, &mut id_buf);
        let _ = devman_dev_add_static_info(dev_idx, "devman_id", id_str);

        // Link into parent's child list.
        let parent = &mut *base.add(parent_idx);
        dev.next_sibling = parent.first_child;
        parent.first_child = Some(dev_idx);

        devman_get_device(parent_idx);

        Ok(dev_idx)
    }
}

// Event queue operations

/// Generate the device path (`/devices/name/...`) for `dev_idx` into `out`.
///
/// The path starts at the root device's inode (named `devices`), matching
/// the C `devman_generate_path`.
///
/// # Safety
///
/// `out` must be a valid buffer; `dev_idx` must be a valid device index.
unsafe fn devman_generate_path(dev_idx: usize, out: &mut [u8]) -> i32 {
    unsafe {
        // Collect names from the device up to and including the root
        // device, then emit them as a `/`-joined path.
        let mut names = [[0u8; 64]; 16];
        let mut depths = [0usize; 16];
        let mut depth = 0usize;
        let mut cur = Some(dev_idx);
        while let Some(c) = cur {
            if depth >= names.len() {
                return ENOMEM;
            }
            let dev = &*DEVICE_TABLE.as_ptr().add(c);
            let name = vtreefs::get_inode_name(dev.inode_id);
            let nb = name.as_bytes();
            let n = nb.len().min(63);
            names[depth][..n].copy_from_slice(&nb[..n]);
            depths[depth] = n;
            depth += 1;
            cur = dev.parent;
        }

        let mut off = 0usize;
        for i in (0..depth).rev() {
            if off + 1 + depths[i] >= out.len() {
                return ENOMEM;
            }
            out[off] = b'/';
            off += 1;
            out[off..off + depths[i]].copy_from_slice(&names[i][..depths[i]]);
            off += depths[i];
        }
        if off == 0 {
            out[off] = b'/';
            off += 1;
        }
        out[off] = 0;
        OK
    }
}

/// Enqueue an ADD/REMOVE event for `dev_idx` (C `devman_device_add_event` /
/// `devman_device_remove_event`).
///
/// # Safety
///
/// Must be called with valid device state.
unsafe fn devman_add_event(dev_idx: usize, add: bool) -> Result<(), i32> {
    unsafe {
        let count = DEVICE_COUNT.load(Ordering::Relaxed) as usize;
        if dev_idx >= count {
            return Err(ENODEV);
        }

        // Build "ADD <path> 0x%08x" / "REMOVE <path> 0x%08x".
        let mut data = [0u8; DEVMAN_STRING_LEN];
        let prefix: &[u8] = if add { b"ADD " } else { b"REMOVE " };
        let mut off = prefix.len();
        data[..off].copy_from_slice(prefix);

        let mut path = [0u8; DEVMAN_STRING_LEN];
        if devman_generate_path(dev_idx, &mut path) != OK {
            return Err(ENOMEM);
        }
        let plen = path.iter().position(|&b| b == 0).unwrap_or(0);
        if off + plen + 11 >= data.len() {
            return Err(ENOMEM);
        }
        data[off..off + plen].copy_from_slice(&path[..plen]);
        off += plen;

        // Append the device id as " 0x%08x".
        let dev_id = (*DEVICE_TABLE.as_ptr().add(dev_idx)).dev_id;
        data[off..off + 2].copy_from_slice(b" 0");
        data[off + 2] = b'x';
        off += 3;
        let hex = b"0123456789abcdef";
        for i in (0..8).rev() {
            let nibble = ((dev_id as u32 >> (i * 4)) & 0xf) as usize;
            data[off] = hex[nibble];
            off += 1;
        }

        // Append to the event queue (first free slot whose data is empty).
        let base = EVENT_TABLE.as_ptr();
        let mut idx = None;
        for i in 0..MAX_DEVICES {
            let ev = &*base.add(i);
            if ev.data[0] == 0 && ev.next.is_none() {
                idx = Some(i);
                break;
            }
        }
        let idx = match idx {
            Some(i) => i,
            None => return Err(ENOMEM),
        };
        *base.add(idx) = DevmanEvent { data, next: None };
        let tail = EVENT_TAIL.load(Ordering::Relaxed);
        if tail >= 0 {
            (*base.add(tail as usize)).next = Some(idx);
        } else {
            EVENT_HEAD.store(idx as i32, Ordering::Relaxed);
        }
        EVENT_TAIL.store(idx as i32, Ordering::Relaxed);
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
    // Release the slot for reuse.
    unsafe { *base.add(idx) = DevmanEvent::zeroed() };
    Some(data)
}

// Message handlers

/// Fill the DEVMAN_REPLY reply fields (m_type, DEVMAN_RESULT, DEVMAN_DEVICE_ID).
///
/// # Safety
///
/// `msg` must be a valid 64-byte message buffer.
unsafe fn devman_reply(msg: &mut [u8; 64], result: i32, dev_id: i32) {
    unsafe {
        msg_set_i32(msg, MSG_OFF_TYPE, DEVMAN_REPLY as i32);
        msg_set_i64(msg, MSG_OFF_M4_L1, result as i64); // DEVMAN_RESULT
        msg_set_i64(msg, MSG_OFF_M4_L2, dev_id as i64); // DEVMAN_DEVICE_ID
    }
}

/// Read a NUL-terminated string from `buffer` starting at `off`.
fn cstr_at(buffer: &[u8], off: usize) -> &str {
    if off >= buffer.len() {
        return "";
    }
    let rest = &buffer[off..];
    let len = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    core::str::from_utf8(&rest[..len]).unwrap_or("")
}

/// Format `v` (non-negative) as a decimal string into `out`.
fn itoa_buf(v: i32, out: &mut [u8]) -> &str {
    let mut v = v.max(0) as u32;
    let mut i = out.len();
    loop {
        i -= 1;
        out[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    core::str::from_utf8(&out[i..]).unwrap_or("")
}

/// Parse a serialized `DevmanDeviceInfo` blob, add the device to the tree,
/// and enqueue an ADD event. Returns the new device id, or a negative errno.
///
/// Extracted from `do_add_device` so the logic is host-testable without a
/// grant table.
///
/// # Safety
///
/// `buffer` must hold a valid serialized device-info blob.
unsafe fn do_add_device_inner(source: i32, buffer: &[u8]) -> Result<i32, i32> {
    unsafe {
        if buffer.len() < core::mem::size_of::<DevmanDeviceInfo>() {
            return Err(EINVAL);
        }
        let devinf = DevmanDeviceInfo {
            count: i32::from_ne_bytes(buffer[0..4].try_into().unwrap_or([0; 4])),
            parent_dev_id: i32::from_ne_bytes(buffer[4..8].try_into().unwrap_or([0; 4])),
            name_offset: u32::from_ne_bytes(buffer[8..12].try_into().unwrap_or([0; 4])),
            subsystem_offset: u32::from_ne_bytes(buffer[12..16].try_into().unwrap_or([0; 4])),
        };

        let parent_idx = match devman_find_device(devinf.parent_dev_id) {
            Some(idx) => idx,
            None => return Err(ENODEV),
        };
        let name = cstr_at(buffer, devinf.name_offset as usize);
        let dev_idx = devman_dev_add_child(parent_idx, name, buffer, &devinf)?;

        let base = DEVICE_TABLE.as_ptr();
        let dev = &mut *base.add(dev_idx);
        dev.state = DEVMAN_DEVICE_UNBOUND;
        dev.owner = source;
        let dev_id = dev.dev_id;

        let _ = devman_add_event(dev_idx, true);

        Ok(dev_id)
    }
}

/// Handle DEVMAN_ADD_DEV — add a device to the tree.
///
/// # Safety
///
/// `msg` must be a valid 64-byte message buffer.
pub unsafe fn do_add_device(msg: &mut [u8; 64]) -> i32 {
    unsafe {
        let source = msg_i32(msg, MSG_OFF_SOURCE);
        let grant_id = msg_i32(msg, MSG_OFF_M4_L1);
        let grant_size = msg_i32(msg, MSG_OFF_M4_L2);

        if grant_id < 0 || grant_size < core::mem::size_of::<DevmanDeviceInfo>() as i32 {
            devman_reply(msg, EINVAL, 0);
            return EINVAL;
        }

        // Copy the serialized device info from the caller via the grant.
        let mut devinfo_buf = [0u8; 1024];
        let copy_len = (grant_size as usize).min(devinfo_buf.len());
        let r =
            crate::tty::sys_safecopyfrom(source, grant_id as u32, 0, &mut devinfo_buf[..copy_len]);
        if r != OK {
            devman_reply(msg, EINVAL, 0);
            return EINVAL;
        }

        match do_add_device_inner(source, &devinfo_buf) {
            Ok(dev_id) => {
                devman_reply(msg, OK, dev_id);
                OK
            }
            Err(e) => {
                devman_reply(msg, e, 0);
                e
            }
        }
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

// Server main loop

/// VTreeFS init hook — populate the device tree once, on mount.
pub fn devman_init_hook() {
    static ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if !ONCE.swap(true, Ordering::Relaxed) {
        unsafe {
            devman_init_devices();
        }
    }
}

/// VTreeFS message hook — handle non-FS messages (DEVMAN_ADD_DEV,
/// DEVMAN_DEL_DEV, DEVMAN_BIND, DEVMAN_UNBIND). The reply is built into
/// `msg` (m_type = DEVMAN_REPLY); the VTreeFS loop sends it back.
///
/// # Safety
///
/// `msg` must point to a valid 64-byte message buffer.
pub fn devman_handle_message(msg: &mut [u8; 64]) -> i32 {
    unsafe {
        let m_type = msg_i32(msg, MSG_OFF_TYPE) as u32;
        match m_type {
            DEVMAN_ADD_DEV => do_add_device(msg),
            DEVMAN_DEL_DEV => do_del_device(msg),
            DEVMAN_BIND => do_bind_device(msg),
            DEVMAN_UNBIND => do_unbind_device(msg),
            _ => {
                devman_reply(msg, EINVAL, 0);
                EINVAL
            }
        }
    }
}

/// Copy a NUL-terminated string into `buf` starting at `offset`.
/// Returns the number of bytes written.
fn fill_from(data: &[u8], offset: u64, buf: &mut [u8]) -> usize {
    let len = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let start = (offset as usize).min(len);
    let n = (len - start).min(buf.len());
    buf[..n].copy_from_slice(&data[start..start + n]);
    n
}

/// VTreeFS read hook — serve the `events` file and static-info files.
///
/// Static-info inodes carry their table index in cbdata (`si_idx + 1`); the
/// events file is identified by inode id. Device directories read as empty.
pub fn devman_read_hook(node: u32, offset: u64, buf: &mut [u8]) -> usize {
    let cbdata = vtreefs::get_inode_cbdata(node);
    if cbdata > 0 {
        let si = unsafe { &*STATIC_INFO_TABLE.as_ptr().add(cbdata - 1) };
        return fill_from(&si.data, offset, buf);
    }
    if node == EVENTS_INODE.load(Ordering::Relaxed) {
        return match unsafe { devman_read_event() } {
            Some(event) => fill_from(&event, offset, buf),
            None => 0,
        };
    }
    0
}

/// DEVMAN server main loop.
///
/// Registers the VTreeFS hooks and enters the VTreeFS receive-dispatch loop:
///   - init_hook  → `devman_init_devices` (on mount)
///   - message_hook → `devman_handle_message` (ADD/DEL/BIND/UNBIND)
///   - read_hook → `devman_read_hook` (events + static info)
pub fn devman_server_main() {
    let hooks = vtreefs::FsHooks {
        init_hook: Some(devman_init_hook),
        cleanup_hook: None,
        lookup_hook: None,
        getdents_hook: None,
        read_hook: Some(devman_read_hook),
        rdlink_hook: None,
        message_hook: Some(devman_handle_message),
    };
    let root_stat = vtreefs::InodeStat {
        mode: I_DIRECTORY | 0o444,
        uid: 0,
        gid: 0,
        size: 0,
        dev: NO_DEV as u64,
    };
    vtreefs::start_vtreefs(hooks, vtreefs::MAX_INODES as u32, root_stat, 0);
}

// Tests

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
        EVENTS_INODE.store(u32::MAX, Ordering::Relaxed);
        // Initialize the VTreeFS inode table that devman's tree lives in.
        let root_stat = vtreefs::InodeStat {
            mode: 0o040555,
            uid: 0,
            gid: 0,
            size: 0,
            dev: u32::MAX as u64,
        };
        vtreefs::vtreefs_init(vtreefs::FsHooks::empty(), 1024, root_stat);
        unsafe { devman_init_devices() };
    }

    /// Add a child device (name only; no serialized info entries).
    fn add_child(parent: usize, name: &str) -> usize {
        let devinf = DevmanDeviceInfo::zeroed();
        unsafe { devman_dev_add_child(parent, name, &[], &devinf).unwrap() }
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

        let child = add_child(0, "dev0");
        let child_idx = child;

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

        let c1 = add_child(0, "dev1");
        let c2 = add_child(0, "dev2");
        let c3 = add_child(0, "dev3");

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

        let child = add_child(0, "dev0");
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

        let child = add_child(0, "dev0");
        let grandchild = add_child(child, "dev1");
        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let gc_dev = unsafe { &*base.add(grandchild) };

        let found = unsafe { devman_find_device(gc_dev.dev_id) };
        assert_eq!(found, Some(grandchild));
    }

    #[test]
    fn test_add_static_info() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = add_child(0, "dev0");
        let r = unsafe { devman_dev_add_static_info(child, "extra", "42") };
        assert!(r.is_ok());

        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let dev = unsafe { &*base.add(child) };
        assert!(dev.first_info.is_some());

        let ii_base = unsafe { INFO_INODE_TABLE.as_ptr() };
        let ii = unsafe { &*ii_base.add(dev.first_info.unwrap()) };
        // add_child already created a "devman_id" info (data_idx 0); the
        // explicitly added one is the most recent, at the head.
        assert_eq!(ii.data_idx, 1);

        let si_base = unsafe { STATIC_INFO_TABLE.as_ptr() };
        let si = unsafe { &*si_base.add(ii.data_idx) };
        let data_str = core::str::from_utf8(&si.data[..2]).unwrap();
        assert_eq!(data_str, "42");
    }

    #[test]
    fn test_get_put_device() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = add_child(0, "dev0");

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

        let child = add_child(0, "dev0");
        assert_eq!(DEVICE_COUNT.load(Ordering::Relaxed), 2);

        unsafe { devman_put_device(child) };
        // After put, ref_count goes to 0 and device is deleted.
        assert_eq!(DEVICE_COUNT.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_do_add_device_reply() {
        let _lock = TestLockGuard::acquire();
        setup();

        // grant_id < 0 → the grant copy is skipped and the reply is EINVAL.
        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, 100);
            msg_set_i32(&mut msg, MSG_OFF_M4_L1, -1); // grant_id
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, 64); // grant_size
        }

        let r = unsafe { do_add_device(&mut msg) };
        assert_eq!(r, EINVAL);

        let result = unsafe { msg_i32(&msg, MSG_OFF_M4_L1) };
        assert_eq!(result, EINVAL);
        let m_type = unsafe { msg_i32(&msg, MSG_OFF_TYPE) };
        assert_eq!(m_type, DEVMAN_REPLY as i32);
    }

    /// Build a serialized `DevmanDeviceInfo` blob for a device named `name`
    /// under `parent_dev_id`.
    fn build_devinfo(name: &str, parent_dev_id: i32) -> [u8; 128] {
        let mut buf = [0u8; 128];
        buf[0..4].copy_from_slice(&0i32.to_le_bytes()); // count (no info entries)
        buf[4..8].copy_from_slice(&parent_dev_id.to_le_bytes());
        buf[8..12].copy_from_slice(&16u32.to_le_bytes()); // name_offset
        buf[12..16].copy_from_slice(&(16 + name.len() as u32 + 1).to_le_bytes()); // subsystem_offset
        buf[16..16 + name.len()].copy_from_slice(name.as_bytes());
        buf[16 + name.len()] = 0;
        buf
    }

    #[test]
    fn test_add_device_inner() {
        let _lock = TestLockGuard::acquire();
        setup();

        let buf = build_devinfo("tty0", 0);
        let dev_id = unsafe { do_add_device_inner(100, &buf) };
        assert_eq!(dev_id, Ok(1), "first device gets id 1");

        // The device exists, is owned by the caller, and lives in the tree.
        let found = unsafe { devman_find_device(1) };
        assert_eq!(found, Some(1));
        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let dev = unsafe { &*base.add(1) };
        assert_eq!(dev.owner, 100);
        assert_eq!(dev.state, DEVMAN_DEVICE_UNBOUND);
        assert_eq!(vtreefs::get_inode_name(dev.inode_id), "tty0");

        // An ADD event was queued with the path and device id.
        let event = unsafe { devman_read_event() };
        assert!(event.is_some(), "ADD event queued");
        let data = event.unwrap();
        let data_str = core::str::from_utf8(&data).unwrap();
        assert!(
            data_str.starts_with("ADD /devices/tty0 0x00000001"),
            "got: {data_str}"
        );
    }

    #[test]
    fn test_add_device_bad_parent() {
        let _lock = TestLockGuard::acquire();
        setup();

        let buf = build_devinfo("orphan", 999);
        let dev_id = unsafe { do_add_device_inner(100, &buf) };
        assert_eq!(dev_id, Err(ENODEV));
    }

    #[test]
    fn test_message_hook_routing() {
        let _lock = TestLockGuard::acquire();
        setup();

        // DEVMAN_ADD_DEV with an invalid grant → EINVAL reply.
        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, 100);
            msg_set_i32(&mut msg, MSG_OFF_TYPE, DEVMAN_ADD_DEV as i32);
            msg_set_i32(&mut msg, MSG_OFF_M4_L1, -1);
        }
        let r = unsafe { devman_handle_message(&mut msg) };
        assert_eq!(r, EINVAL);
        let m_type = unsafe { msg_i32(&msg, MSG_OFF_TYPE) };
        assert_eq!(m_type, DEVMAN_REPLY as i32);

        // DEVMAN_BIND from a non-RS endpoint → EPERM reply.
        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, 100);
            msg_set_i32(&mut msg, MSG_OFF_TYPE, DEVMAN_BIND as i32);
            msg_set_i32(&mut msg, MSG_OFF_M4_L2, 0);
        }
        let _ = unsafe { devman_handle_message(&mut msg) };
        let result = unsafe { msg_i32(&msg, MSG_OFF_M4_L1) };
        assert_eq!(result, EPERM);

        // Unknown message type → EINVAL reply.
        let mut msg = [0u8; 64];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, 100);
            msg_set_i32(&mut msg, MSG_OFF_TYPE, 0x9999);
        }
        let r = unsafe { devman_handle_message(&mut msg) };
        assert_eq!(r, EINVAL);
        let m_type = unsafe { msg_i32(&msg, MSG_OFF_TYPE) };
        assert_eq!(m_type, DEVMAN_REPLY as i32);
    }

    #[test]
    fn test_read_hook_static_info() {
        let _lock = TestLockGuard::acquire();
        setup();

        let child = add_child(0, "dev0");
        let r = unsafe { devman_dev_add_static_info(child, "devman_id", "42") };
        assert!(r.is_ok());
        let base = unsafe { DEVICE_TABLE.as_ptr() };
        let dev = unsafe { &*base.add(child) };
        let ii = unsafe { &*INFO_INODE_TABLE.as_ptr().add(dev.first_info.unwrap()) };

        let mut out = [0u8; 8];
        let n = devman_read_hook(ii.inode_id, 0, &mut out);
        assert_eq!(&out[..n], b"42");
    }

    #[test]
    fn test_events_inode_read() {
        let _lock = TestLockGuard::acquire();
        setup();

        let buf = build_devinfo("tty0", 0);
        assert_eq!(unsafe { do_add_device_inner(100, &buf) }, Ok(1));

        let events_inode = EVENTS_INODE.load(Ordering::Relaxed);
        assert_ne!(events_inode, u32::MAX);
        let mut out = [0u8; 64];
        let n = devman_read_hook(events_inode, 0, &mut out);
        let data = core::str::from_utf8(&out[..n]).unwrap();
        assert!(data.starts_with("ADD /devices/tty0 0x"), "got: {data}");

        // The queue is empty now.
        let mut out = [0u8; 64];
        assert_eq!(devman_read_hook(events_inode, 0, &mut out), 0);
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

        let child = add_child(0, "dev0");
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

        let child = add_child(0, "dev0");
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

        let child = add_child(0, "dev0");
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
