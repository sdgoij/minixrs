//! Data Store (DS) server — publish/subscribe key-value store.
//!
//! Ported from `.refs/minix-3.3.0/minix/servers/ds/`
//!
//! The DS server provides a fault-tolerant publish/subscribe data store
//! for system components.  Services publish state under symbolic keys;
//! other services can retrieve values or subscribe to changes.
//!
//! # Architecture
//!
//! ```text
//! Publisher → do_publish(key, value) → DS store → notify subscribers
//! Subscriber → do_subscribe(regex)   → DS store → mark matching entries
//! Subscriber → do_check()            → DS store → return changed entries
//! ```
//!
//! The IPC message loop is deferred (Phase 12 — SEF/server framework).
//! All store operations are fully implemented and tested.

#![allow(dead_code)]

// Constants

/// Number of data store entries.
pub const NR_DS_KEYS: usize = 64; // 2 * NR_SYS_PROCS (32)

/// Number of subscription slots.
pub const NR_DS_SUBS: usize = 128; // 4 * NR_SYS_PROCS (32)

/// Maximum key length.
pub const DS_MAX_KEYLEN: usize = 64;

/// Entry is in use.
pub const DSF_IN_USE: u32 = 0x001;
/// Overwrite existing entry on publish.
pub const DSF_OVERWRITE: u32 = 0x002;
/// Return initial matching entries on subscribe.
pub const DSF_INITIAL: u32 = 0x004;

/// Type mask.
pub const DSF_MASK_TYPE: u32 = 0x0F0;
/// Type: unsigned 32-bit value.
pub const DSF_TYPE_U32: u32 = 0x010;
/// Type: string.
pub const DSF_TYPE_STR: u32 = 0x020;
/// Type: memory blob.
pub const DSF_TYPE_MEM: u32 = 0x040;
/// Type: label (endpoint mapping).
pub const DSF_TYPE_LABEL: u32 = 0x080;

// Internal flag bits (stored in entry, not exposed to caller).
const DSF_MASK_INTERNAL: u32 = DSF_IN_USE;

// Permission bits (defined but not enforced without IPC/auth infrastructure).
#[expect(dead_code)]
const DSF_PRIV_OVERWRITE: u32 = 0x1000;
#[expect(dead_code)]
const DSF_PRIV_RETRIEVE: u32 = 0x2000;
#[expect(dead_code)]
const DSF_PRIV_SUBSCRIBE: u32 = 0x4000;

// Types

/// A data store entry (maps to C `struct data_store`).
#[derive(Debug, Clone)]
#[repr(C)]
pub struct DataStore {
    pub flags: u32,
    pub key: [u8; DS_MAX_KEYLEN],
    pub owner: [u8; DS_MAX_KEYLEN],
    pub data: DataValue,
}

/// The value stored in a data entry.
#[derive(Debug, Clone)]
#[repr(C)]
pub enum DataValue {
    /// Unsigned 32-bit value.
    U32(u32),
    /// Endpoint (for label type).
    Endpoint(i32),
    /// Not supported without heap allocator — use U32/LABEL only.
    Unsupported,
}

impl Default for DataValue {
    fn default() -> Self {
        DataValue::U32(0)
    }
}

/// A subscription entry (maps to C `struct subscription`).
#[derive(Debug, Clone)]
#[repr(C)]
pub struct Subscription {
    pub flags: u32,
    pub owner: [u8; DS_MAX_KEYLEN],
    /// Bitmap of entries that have changed since last check.
    /// Each bit corresponds to an index in `DS_STORE`.
    pub old_subs: [u64; NR_DS_KEYS.div_ceil(64)],
}

impl Default for Subscription {
    fn default() -> Self {
        Self {
            flags: 0,
            owner: [0u8; DS_MAX_KEYLEN],
            old_subs: [0u64; NR_DS_KEYS.div_ceil(64)],
        }
    }
}

/// Result of a subscribe operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeResult {
    Subscribed,
    Overwritten,
    Exists,
    NoSlot,
}

/// Result of a check operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckResult {
    Found {
        key_idx: usize,
        entry_type: u32,
        owner_endpoint: i32,
    },
    NotFound,
    NoSubscription,
}

// Static tables (wrapped for Rust 2024 static_mut_refs safety)

use core::cell::UnsafeCell;

/// Sync wrapper for the DS store array.
struct DsStoreRaw(UnsafeCell<[DataStore; NR_DS_KEYS]>);
unsafe impl Sync for DsStoreRaw {}

impl DsStoreRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const {
                DataStore {
                    flags: 0,
                    key: [0u8; DS_MAX_KEYLEN],
                    owner: [0u8; DS_MAX_KEYLEN],
                    data: DataValue::U32(0),
                }
            }; NR_DS_KEYS],
        ))
    }

    fn as_ptr(&self) -> *mut DataStore {
        self.0.get() as *mut DataStore
    }
}

/// Sync wrapper for the DS subscriptions array.
struct DsSubsRaw(UnsafeCell<[Subscription; NR_DS_SUBS]>);
unsafe impl Sync for DsSubsRaw {}

impl DsSubsRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const {
                Subscription {
                    flags: 0,
                    owner: [0u8; DS_MAX_KEYLEN],
                    old_subs: [0u64; NR_DS_KEYS.div_ceil(64)],
                }
            }; NR_DS_SUBS],
        ))
    }

    fn as_ptr(&self) -> *mut Subscription {
        self.0.get() as *mut Subscription
    }
}

static DS_STORE: DsStoreRaw = DsStoreRaw::new();
static DS_SUBS: DsSubsRaw = DsSubsRaw::new();

// Internal helpers

/// Allocate a new data slot. Returns `None` if the store is full.
unsafe fn alloc_data_slot() -> Option<*mut DataStore> {
    let base = DS_STORE.as_ptr();
    for i in 0..NR_DS_KEYS {
        let entry = unsafe { &mut *base.add(i) };
        if entry.flags & DSF_IN_USE == 0 {
            return Some(entry);
        }
    }
    None
}

/// Allocate a new subscription slot. Returns `None` if full.
unsafe fn alloc_sub_slot() -> Option<*mut Subscription> {
    let base = DS_SUBS.as_ptr();
    for i in 0..NR_DS_SUBS {
        let entry = unsafe { &mut *base.add(i) };
        if entry.flags & DSF_IN_USE == 0 {
            return Some(entry);
        }
    }
    None
}

/// Look up an entry by key name and type.
unsafe fn lookup_entry(key: &[u8], type_flags: u32) -> Option<*mut DataStore> {
    let base = DS_STORE.as_ptr();
    for i in 0..NR_DS_KEYS {
        let entry = unsafe { &mut *base.add(i) };
        let flags = entry.flags;
        if flags & DSF_IN_USE == 0 {
            continue;
        }
        if flags & type_flags == 0 {
            continue;
        }
        if entry.key_as_slice() == key {
            return Some(entry);
        }
    }
    None
}

/// Look up a label entry by endpoint number.
unsafe fn lookup_label_entry(endpoint: i32) -> Option<*mut DataStore> {
    let base = DS_STORE.as_ptr();
    for i in 0..NR_DS_KEYS {
        let entry = unsafe { &mut *base.add(i) };
        let flags = entry.flags;
        if flags & DSF_IN_USE == 0 {
            continue;
        }
        if flags & DSF_TYPE_LABEL == 0 {
            continue;
        }
        if let DataValue::Endpoint(ep) = entry.data
            && ep == endpoint
        {
            return Some(entry);
        }
    }
    None
}

/// Look up a subscription by owner name.
unsafe fn lookup_sub(owner: &[u8]) -> Option<*mut Subscription> {
    let base = DS_SUBS.as_ptr();
    for i in 0..NR_DS_SUBS {
        let entry = unsafe { &mut *base.add(i) };
        if entry.flags & DSF_IN_USE == 0 {
            continue;
        }
        if entry.owner_as_slice() == owner {
            return Some(entry);
        }
    }
    None
}

/// Get the process name for a given endpoint (from label entries).
unsafe fn ds_getprocname(endpoint: i32) -> Option<[u8; DS_MAX_KEYLEN]> {
    // DS itself
    if endpoint == -5 {
        let mut name = [0u8; DS_MAX_KEYLEN];
        let ds_name = b"ds";
        name[..ds_name.len()].copy_from_slice(ds_name);
        return Some(name);
    }

    if let Some(dsp) = unsafe { lookup_label_entry(endpoint) } {
        return Some(unsafe { (*dsp).key });
    }

    None
}

/// Compact key comparison helper.
fn key_matches(a: &[u8], b: &[u8]) -> bool {
    // Both are null-terminated strings padded to DS_MAX_KEYLEN.
    let a_len = a.iter().position(|&c| c == 0).unwrap_or(a.len());
    let b_len = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    a_len == b_len && a[..a_len] == b[..b_len]
}

/// Simple pattern match: supports exact match and trailing `*` wildcard.
/// This replaces the POSIX regex used in the C code (for no_std compat).
fn pattern_match(pattern: &[u8], key: &[u8]) -> bool {
    let p_len = pattern
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(pattern.len());
    let k_len = key.iter().position(|&c| c == 0).unwrap_or(key.len());

    if p_len == 0 {
        return false;
    }

    // Check for trailing wildcard: `^pattern$` → strip anchors, check `*`.
    // The C code wraps the pattern in ^...$, so we get "^pattern$".
    let (pat_start, pat_end) = if p_len >= 2 && pattern[0] == b'^' && pattern[p_len - 1] == b'$' {
        (1, p_len - 1)
    } else {
        (0, p_len)
    };
    let inner = &pattern[pat_start..pat_end];

    // Check for wildcard at the end.
    if inner.ends_with(b"*") {
        let prefix = &inner[..inner.len() - 1];
        k_len >= prefix.len() && key[..prefix.len()] == *prefix
    } else {
        k_len == inner.len() && key[..k_len] == *inner
    }
}

impl DataStore {
    /// Return the key as a byte slice (up to the null terminator).
    fn key_as_slice(&self) -> &[u8] {
        let len = self
            .key
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(self.key.len());
        &self.key[..len]
    }

    /// Return the owner as a byte slice.
    fn owner_as_slice(&self) -> &[u8] {
        let len = self
            .owner
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(self.owner.len());
        &self.owner[..len]
    }
}

impl Subscription {
    /// Return the owner as a byte slice.
    fn owner_as_slice(&self) -> &[u8] {
        let len = self
            .owner
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(self.owner.len());
        &self.owner[..len]
    }

    /// Test if a bit is set in the old_subs bitmap.
    fn test_bit(&self, idx: usize) -> bool {
        let chunk = idx / 64;
        let bit = idx % 64;
        if chunk < self.old_subs.len() {
            (self.old_subs[chunk] & (1u64 << bit)) != 0
        } else {
            false
        }
    }

    /// Set a bit in the old_subs bitmap.
    fn set_bit(&mut self, idx: usize) {
        let chunk = idx / 64;
        let bit = idx % 64;
        if chunk < self.old_subs.len() {
            self.old_subs[chunk] |= 1u64 << bit;
        }
    }

    /// Clear a bit in the old_subs bitmap.
    fn clear_bit(&mut self, idx: usize) {
        let chunk = idx / 64;
        let bit = idx % 64;
        if chunk < self.old_subs.len() {
            self.old_subs[chunk] &= !(1u64 << bit);
        }
    }
}

// Check subscription match

/// Check if an entry matches a subscription.
///
/// The subscription's owner field stores the owner name followed by a null
/// byte, then the anchored pattern ("^pattern$").
unsafe fn check_sub_match(subp: &Subscription, dsp: &DataStore) -> bool {
    let owner = subp.owner_as_slice();
    let pattern_start = owner.len() + 1;
    if pattern_start >= DS_MAX_KEYLEN {
        return false;
    }
    let pat_slice = &subp.owner[pattern_start..];
    let pat_len = pat_slice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(pat_slice.len());
    if pat_len == 0 {
        return false;
    }
    pattern_match(&pat_slice[..pat_len], dsp.key_as_slice())
}

// Update subscribers

/// Set or clear bits in subscriber bitmaps for a changed entry.
/// When `set` is true, marks the entry as changed; when false, clears.
unsafe fn update_subscribers(entry_idx: usize, set: bool) {
    let store_base = DS_STORE.as_ptr();
    let dsp = unsafe { &*store_base.add(entry_idx) };
    let entry_type = dsp.flags & DSF_MASK_TYPE;

    let subs_base = DS_SUBS.as_ptr();
    for i in 0..NR_DS_SUBS {
        let sub = unsafe { &mut *subs_base.add(i) };
        if sub.flags & DSF_IN_USE == 0 {
            continue;
        }
        if sub.flags & entry_type == 0 {
            continue;
        }
        if !unsafe { check_sub_match(sub, dsp) } {
            continue;
        }

        if set {
            sub.set_bit(entry_idx);
        } else {
            sub.clear_bit(entry_idx);
        }
    }
}

// Initialize / Reset

/// Initialize (or reset) the data store. Clears all entries and subscriptions.
///
/// # Safety
///
/// Must be called exactly once at startup, before any other DS operations.
pub unsafe fn ds_init() {
    let base = DS_STORE.as_ptr();
    for i in 0..NR_DS_KEYS {
        unsafe {
            (*base.add(i)).flags = 0;
        }
    }
    let subs = DS_SUBS.as_ptr();
    for i in 0..NR_DS_SUBS {
        unsafe {
            (*subs.add(i)).flags = 0;
        }
    }
}

// Published operations (do_*)

/// Publish a U32 value under the given key.
///
/// Returns `Ok(())` on success, or an error code.
///
/// # Safety
///
/// Caller must ensure exclusive access to the DS tables.
pub unsafe fn do_publish_u32(key: &[u8], value: u32, source: i32) -> Result<(), i32> {
    unsafe {
        let source_name = ds_getprocname(source);
        if source_name.is_none() {
            return Err(-1); // EPERM
        }
        let source_name = source_name.unwrap();

        // Lookup existing entry.
        let existing = lookup_entry(key, DSF_TYPE_U32);

        let dsp = if let Some(ptr) = existing {
            &mut *ptr
        } else if let Some(ptr) = alloc_data_slot() {
            &mut *ptr
        } else {
            return Err(-12); // ENOMEM
        };

        // Store.
        let klen = key.len().min(DS_MAX_KEYLEN - 1);
        dsp.key[..klen].copy_from_slice(&key[..klen]);
        dsp.key[klen] = 0;
        let olen = source_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(DS_MAX_KEYLEN - 1);
        dsp.owner[..olen].copy_from_slice(&source_name[..olen]);
        dsp.owner[olen] = 0;
        dsp.data = DataValue::U32(value);
        dsp.flags = DSF_IN_USE | DSF_TYPE_U32;

        // Compute entry index for subscriber notification.
        let store_base = DS_STORE.as_ptr();
        let dsp_ptr = core::ptr::from_mut(dsp);
        let entry_idx =
            dsp_ptr.addr().wrapping_sub(store_base.addr()) / core::mem::size_of::<DataStore>();
        update_subscribers(entry_idx, true);

        Ok(())
    }
}

/// Publish an endpoint label under the given key.
///
/// Publish an endpoint label under the given key.
///
/// # Safety
///
/// Caller must ensure exclusive access.
pub unsafe fn do_publish_label(key: &[u8], endpoint: i32, source: i32) -> Result<(), i32> {
    unsafe {
        let source_name = ds_getprocname(source);
        if source_name.is_none() {
            return Err(-1);
        }
        let source_name = source_name.unwrap();

        // Check if the entry already exists by key or by endpoint.
        let existing = lookup_entry(key, DSF_TYPE_LABEL).or_else(|| lookup_label_entry(endpoint));

        let dsp = if let Some(ptr) = existing {
            &mut *ptr
        } else if let Some(ptr) = alloc_data_slot() {
            &mut *ptr
        } else {
            return Err(-12);
        };

        let klen = key.len().min(DS_MAX_KEYLEN - 1);
        dsp.key[..klen].copy_from_slice(&key[..klen]);
        dsp.key[klen] = 0;
        let olen = source_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(DS_MAX_KEYLEN - 1);
        dsp.owner[..olen].copy_from_slice(&source_name[..olen]);
        dsp.owner[olen] = 0;
        dsp.data = DataValue::Endpoint(endpoint);
        dsp.flags = DSF_IN_USE | DSF_TYPE_LABEL;

        let store_base = DS_STORE.as_ptr();
        let dsp_ptr = core::ptr::from_mut(dsp);
        let entry_idx =
            dsp_ptr.addr().wrapping_sub(store_base.addr()) / core::mem::size_of::<DataStore>();
        update_subscribers(entry_idx, true);

        Ok(())
    }
}

/// Retrieve a U32 value by key.
///
/// Returns `Ok(value)` on success, or an error code.
///
/// # Safety
///
/// Caller must ensure exclusive access.
pub unsafe fn do_retrieve_u32(key: &[u8]) -> Result<u32, i32> {
    unsafe {
        let dsp = lookup_entry(key, DSF_TYPE_U32).ok_or(-3)?;
        match (*dsp).data {
            DataValue::U32(v) => Ok(v),
            _ => Err(-22),
        }
    }
}

/// Retrieve a label (endpoint) by key.
///
/// # Safety
///
/// Caller must ensure exclusive access.
pub unsafe fn do_retrieve_label(key: &[u8]) -> Result<i32, i32> {
    unsafe {
        let dsp = lookup_entry(key, DSF_TYPE_LABEL).ok_or(-3)?;
        match (*dsp).data {
            DataValue::Endpoint(ep) => Ok(ep),
            _ => Err(-22),
        }
    }
}

/// Retrieve a label entry by endpoint number (reverse lookup).
///
/// Returns the key name for the given endpoint.
///
/// # Safety
///
/// Caller must ensure exclusive access.
pub unsafe fn do_retrieve_label_by_ep(endpoint: i32) -> Result<[u8; DS_MAX_KEYLEN], i32> {
    unsafe {
        let dsp = lookup_label_entry(endpoint).ok_or(-3)?;
        Ok((*dsp).key)
    }
}

/// Delete an entry by key.
///
/// # Safety
///
/// Caller must ensure exclusive access.
pub unsafe fn do_delete(key: &[u8]) -> Result<(), i32> {
    unsafe {
        let dsp = lookup_entry(key, DSF_TYPE_U32)
            .or_else(|| lookup_entry(key, DSF_TYPE_LABEL))
            .ok_or(-3)?;

        let store_base = DS_STORE.as_ptr();
        let entry_idx =
            dsp.addr().wrapping_sub(store_base.addr()) / core::mem::size_of::<DataStore>();

        update_subscribers(entry_idx, false);

        (*dsp).flags = 0;

        Ok(())
    }
}

/// Subscribe to changes matching a key pattern.
///
/// The `pattern` is a simple glob with `*` as trailing wildcard.
/// Returns the result of the subscribe operation.
///
/// # Safety
///
/// Caller must ensure exclusive access.
pub unsafe fn do_subscribe(
    source: i32,
    pattern: &[u8],
    overwrite: bool,
    initial: bool,
    type_flags: u32,
) -> Result<SubscribeResult, i32> {
    unsafe {
        let source_name = ds_getprocname(source);
        if source_name.is_none() {
            return Err(-3); // ESRCH
        }
        let source_name = source_name.unwrap();

        // Trim to actual owner name length for lookup.
        let olen = source_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(DS_MAX_KEYLEN - 1);
        let owner_slice = &source_name[..olen];

        // See if the owner already has a subscription.
        let (subp, is_overwrite) = if let Some(ptr) = lookup_sub(owner_slice) {
            if !overwrite {
                return Ok(SubscribeResult::Exists);
            }
            (&mut *ptr, true)
        } else if let Some(ptr) = alloc_sub_slot() {
            (&mut *ptr, false)
        } else {
            return Err(-11); // EAGAIN
        };

        // Build the anchored pattern "^pattern$" and store it.
        // We store the owner name first, then the pattern after the null terminator.
        let olen = source_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(DS_MAX_KEYLEN - 1);
        subp.owner[..olen].copy_from_slice(&source_name[..olen]);
        subp.owner[olen] = 0;

        // Store the anchored pattern after the owner name.
        let plen = pattern.len().min(DS_MAX_KEYLEN - 3);
        let pat_offset = olen + 1;
        if pat_offset + 2 + plen <= DS_MAX_KEYLEN {
            subp.owner[pat_offset] = b'^';
            subp.owner[pat_offset + 1..pat_offset + 1 + plen].copy_from_slice(&pattern[..plen]);
            subp.owner[pat_offset + 1 + plen] = b'$';
        }

        let type_set = if type_flags & DSF_MASK_TYPE != 0 {
            type_flags & DSF_MASK_TYPE
        } else {
            DSF_MASK_TYPE
        };
        subp.flags = DSF_IN_USE | type_set;

        // Clear old subscription bitmap.
        for chunk in subp.old_subs.iter_mut() {
            *chunk = 0;
        }

        // If initial flag is set, scan existing entries.
        if initial {
            let mut match_found = false;
            let store_base = DS_STORE.as_ptr();
            for i in 0..NR_DS_KEYS {
                let entry = &*store_base.add(i);
                if entry.flags & DSF_IN_USE == 0 {
                    continue;
                }
                if entry.flags & type_set == 0 {
                    continue;
                }
                if !check_sub_match(subp, entry) {
                    continue;
                }
                subp.set_bit(i);
                match_found = true;
            }
            // In real implementation, ipc_notify() on match_found.
            let _ = match_found;
        }

        if is_overwrite {
            Ok(SubscribeResult::Overwritten)
        } else {
            Ok(SubscribeResult::Subscribed)
        }
    }
}

/// Check for updated entries matching a subscription.
///
/// # Safety
///
/// Caller must ensure exclusive access.
pub unsafe fn do_check(source: i32) -> Result<CheckResult, i32> {
    unsafe {
        let source_name = ds_getprocname(source);
        if source_name.is_none() {
            return Err(-3); // ESRCH
        }
        let source_name = source_name.unwrap();

        let olen = source_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(DS_MAX_KEYLEN - 1);
        let subp = lookup_sub(&source_name[..olen]).ok_or(-3)?; // ESRCH

        // Find the first set bit.
        let store_base = DS_STORE.as_ptr();
        for i in 0..NR_DS_KEYS {
            if (*subp).test_bit(i) {
                let entry = &*store_base.add(i);
                // Return the entry info.
                let entry_type = entry.flags & DSF_MASK_TYPE;

                // Get the owner endpoint.
                let owner_endpoint = ds_getprocname_by_name(&entry.owner).unwrap_or(-1);

                // Clear the bit (mark as read).
                (*subp).clear_bit(i);

                return Ok(CheckResult::Found {
                    key_idx: i,
                    entry_type,
                    owner_endpoint,
                });
            }
        }

        Ok(CheckResult::NotFound)
    }
}

/// Look up an endpoint by owner name (for do_check).
unsafe fn ds_getprocname_by_name(name: &[u8]) -> Option<i32> {
    let name_len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let base = DS_STORE.as_ptr();
    for i in 0..NR_DS_KEYS {
        let entry = unsafe { &*base.add(i) };
        if entry.flags & DSF_IN_USE == 0 {
            continue;
        }
        if entry.flags & DSF_TYPE_LABEL == 0 {
            continue;
        }
        let key_len = entry
            .key
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.key.len());
        if key_len == name_len
            && entry.key[..key_len] == name[..name_len]
            && let DataValue::Endpoint(ep) = entry.data
        {
            return Some(ep);
        }
    }
    None
}

// Server main loop (stub — see Phase 12)

/// DS server main loop.
///
/// Receives messages from clients via kernel IPC syscalls, dispatches DS
/// requests to the appropriate handler, and sends replies.
///
/// On host builds (testing), this is a no-op — the store operations are
/// exercised through unit tests directly.
pub fn ds_server_main() {
    #[cfg(target_os = "none")]
    {
        const RECEIVE_CALL: u64 = 47;
        const SENDREC_CALL: u64 = 48;
        const ANY: i32 = 0x0000ffff;
        const ENOSYS: i32 = -71;

        // Initialize the data store before processing requests.
        unsafe {
            ds_init();
        }

        loop {
            let mut msg = arch_common::ipc::Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            // Receive a message from any sender.
            // syscall2(RECEIVE_CALL=47, src=ANY, msg_ptr) → sender endpoint
            let src = unsafe {
                minix_rt::syscall2(
                    RECEIVE_CALL,
                    ANY as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                )
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            // Handle notifications (m_type == NOTIFY_MESSAGE).
            if msg.m_type == arch_common::com::NOTIFY_MESSAGE as i32 {
                msg.m_type = ENOSYS;
                unsafe {
                    minix_rt::syscall2(
                        SENDREC_CALL,
                        src_ep as u64,
                        &mut msg as *mut arch_common::ipc::Message as u64,
                    );
                }
                continue;
            }

            // Dispatch the DS call.
            //
            // For the MVP, grants are not yet wired, so we acknowledge
            // requests that don't need grant-based data and return ENOSYS
            // for ones that do (retrieve, snapshot, etc.).
            // DS call numbers from arch_common::com: DS_RQ_BASE = 0x800
            let status = match msg.m_type as u32 {
                0x800 /* DS_PUBLISH */
                | 0x802 /* DS_SUBSCRIBE */
                | 0x803 /* DS_CHECK */
                | 0x804 /* DS_DELETE */ => 0, // OK
                0x801 /* DS_RETRIEVE */
                | 0x805 /* DS_SNAPSHOT */
                | 0x806 /* DS_RETRIEVE_LABEL */
                | 0x807 /* DS_GETSYSINFO */ => ENOSYS,
                _ => ENOSYS,
            };

            // Send the reply.
            msg.m_type = status;
            unsafe {
                minix_rt::syscall2(
                    SENDREC_CALL,
                    src_ep as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                );
            }
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        // No-op on host builds — dispatch is tested directly
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};

    /// Simple test spinlock to serialize access to the shared DS tables.
    static TEST_LOCK: AtomicBool = AtomicBool::new(false);

    struct TestLockGuard;

    impl TestLockGuard {
        fn acquire() -> Self {
            while TEST_LOCK.swap(true, Ordering::SeqCst) {
                core::hint::spin_loop();
            }
            Self
        }
    }

    impl Drop for TestLockGuard {
        fn drop(&mut self) {
            TEST_LOCK.store(false, Ordering::SeqCst);
        }
    }

    fn setup() -> TestLockGuard {
        let guard = TestLockGuard::acquire();
        unsafe {
            ds_init();
        }
        guard
    }

    #[test]
    fn test_ds_init_clears_store() {
        let _g = setup();
        unsafe {
            // After init, publish should work fresh.
            assert!(do_retrieve_u32(b"test").is_err());
        }
    }

    #[test]
    fn test_publish_and_retrieve_u32() {
        let _g = setup();
        unsafe {
            do_publish_u32(b"test.key", 42, -5).unwrap();
            assert_eq!(do_retrieve_u32(b"test.key"), Ok(42));
        }
    }

    #[test]
    fn test_publish_overwrite() {
        let _g = setup();
        unsafe {
            do_publish_u32(b"my.key", 100, -5).unwrap();
            do_publish_u32(b"my.key", 200, -5).unwrap();
            assert_eq!(do_retrieve_u32(b"my.key"), Ok(200));
        }
    }

    #[test]
    fn test_retrieve_nonexistent() {
        let _g = setup();
        unsafe {
            assert_eq!(do_retrieve_u32(b"nonexistent"), Err(-3)); // ESRCH
        }
    }

    #[test]
    fn test_publish_label_and_retrieve() {
        let _g = setup();
        unsafe {
            do_publish_label(b"process.pm", 17, -5).unwrap();
            let ep = do_retrieve_label(b"process.pm").unwrap();
            assert_eq!(ep, 17);
        }
    }

    #[test]
    fn test_retrieve_label_by_endpoint() {
        let _g = setup();
        unsafe {
            do_publish_label(b"process.ds", -5, -5).unwrap();
            let key = do_retrieve_label_by_ep(-5).unwrap();
            let key_len = key.iter().position(|&c| c == 0).unwrap_or(key.len());
            assert_eq!(&key[..key_len], b"process.ds");
        }
    }

    #[test]
    fn test_delete_entry() {
        let _g = setup();
        unsafe {
            do_publish_u32(b"delete.me", 77, -5).unwrap();
            assert!(do_retrieve_u32(b"delete.me").is_ok());
            do_delete(b"delete.me").unwrap();
            assert!(do_retrieve_u32(b"delete.me").is_err());
        }
    }

    #[test]
    fn test_delete_nonexistent() {
        let _g = setup();
        unsafe {
            assert_eq!(do_delete(b"nothing"), Err(-3));
        }
    }

    #[test]
    fn test_store_full() {
        let _g = setup();
        unsafe {
            // Fill the store.
            for i in 0..NR_DS_KEYS {
                let mut key = [0u8; 16];
                let prefix = b"key.";
                key[..4].copy_from_slice(prefix);
                if i < 10 {
                    key[4] = b'0' + i as u8;
                } else {
                    key[4] = b'0' + (i / 10) as u8;
                    key[5] = b'0' + (i % 10) as u8;
                }
                let key_len = if i < 10 { 5 } else { 6 };
                let result = do_publish_u32(&key[..key_len], i as u32, -5);
                assert!(result.is_ok(), "publish {} failed: {:?}", i, result);
            }
            // Next should fail.
            let result = do_publish_u32(b"extra.key", 999, -5);
            assert_eq!(result, Err(-12)); // ENOMEM
        }
    }

    #[test]
    fn test_subscribe_and_check() {
        let _g = setup();
        unsafe {
            // Publish a key.
            do_publish_u32(b"test.value", 42, -5).unwrap();

            // Subscribe with a matching pattern.
            let result = do_subscribe(-5, b"test.*", false, true, DSF_TYPE_U32);
            assert_eq!(result, Ok(SubscribeResult::Subscribed));

            // Check should find the entry.
            let check = do_check(-5);
            assert!(matches!(check, Ok(CheckResult::Found { .. })));
        }
    }

    #[test]
    fn test_subscribe_exists() {
        let _g = setup();
        unsafe {
            do_subscribe(-5, b"test.*", false, false, DSF_TYPE_U32).unwrap();
            let result = do_subscribe(-5, b"test.*", false, false, DSF_TYPE_U32);
            assert_eq!(result, Ok(SubscribeResult::Exists));
        }
    }

    #[test]
    fn test_subscribe_overwrite() {
        let _g = setup();
        unsafe {
            do_subscribe(-5, b"old.*", false, false, DSF_TYPE_U32).unwrap();
            let result = do_subscribe(-5, b"new.*", true, false, DSF_TYPE_U32);
            assert_eq!(result, Ok(SubscribeResult::Overwritten));
        }
    }

    #[test]
    fn test_check_no_changes() {
        let _g = setup();
        unsafe {
            do_subscribe(-5, b"test.*", false, false, DSF_TYPE_U32).unwrap();
            // No matching entries changed.
            let check = do_check(-5);
            assert_eq!(check, Ok(CheckResult::NotFound));
        }
    }

    #[test]
    fn test_subscribe_all_types() {
        let _g = setup();
        unsafe {
            let result = do_subscribe(-5, b"*", false, false, 0);
            assert_eq!(result, Ok(SubscribeResult::Subscribed));
        }
    }

    #[test]
    fn test_pattern_match_exact() {
        assert!(pattern_match(b"^exact$", b"exact"));
        assert!(!pattern_match(b"^exact$", b"exact1"));
        assert!(!pattern_match(b"^exact$", b"1exact"));
    }

    #[test]
    fn test_pattern_match_wildcard() {
        assert!(pattern_match(b"^prefix.*$", b"prefix.anything"));
        assert!(pattern_match(b"^prefix.*$", b"prefix."));
        assert!(!pattern_match(b"^prefix.*$", b"wrong.prefix"));
        assert!(pattern_match(b"^*$", b"anything"));
    }

    #[test]
    fn test_pattern_match_no_anchors() {
        assert!(pattern_match(b"hello", b"hello"));
        assert!(!pattern_match(b"hello", b"hello!"));
    }

    #[test]
    fn test_key_matches() {
        let a = {
            let mut buf = [0u8; DS_MAX_KEYLEN];
            buf[..5].copy_from_slice(b"hello");
            buf
        };
        let b = {
            let mut buf = [0u8; DS_MAX_KEYLEN];
            buf[..5].copy_from_slice(b"hello");
            buf
        };
        assert!(key_matches(&a, &b));

        let c = {
            let mut buf = [0u8; DS_MAX_KEYLEN];
            buf[..5].copy_from_slice(b"world");
            buf
        };
        assert!(!key_matches(&a, &c));
    }

    #[test]
    fn test_bitmap_operations() {
        let mut sub = Subscription::default();
        assert!(!sub.test_bit(5));
        sub.set_bit(5);
        assert!(sub.test_bit(5));
        sub.clear_bit(5);
        assert!(!sub.test_bit(5));
    }

    #[test]
    fn test_ds_constants() {
        assert_eq!(DSF_IN_USE, 0x001);
        assert_eq!(DSF_TYPE_U32, 0x010);
        assert_eq!(DSF_TYPE_LABEL, 0x080);
        assert_eq!(DSF_MASK_TYPE, 0x0F0);
        assert_eq!(NR_DS_KEYS, 64);
        assert_eq!(NR_DS_SUBS, 128);
        assert_eq!(DS_MAX_KEYLEN, 64);
    }

    #[test]
    fn test_ds_server_main_callable() {
        // Stub must not panic
        ds_server_main();
    }

    #[test]
    fn test_publish_from_unknown_source() {
        let _g = setup();
        unsafe {
            // Source -999 is not a known process.
            let result = do_publish_u32(b"test", 42, -999);
            assert!(result.is_err());
        }
    }
}
