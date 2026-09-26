//! File block cache for the VM server.
//!
//! A fixed-capacity cache of clean file pages. An entry is identified either by a
//! device offset or by a file's identity, matching C's two indexes in `cache.c`:
//! the block paths address a page by `(dev, dev_offset)` (`find_cached_page_bydev`)
//! and the file fault path by `(dev, ino, ino_offset)` (`find_cached_page_byino`).
//!
//! The inode is load-bearing, not decoration. This port's file fault path both
//! *inserts* pages ([`finish_page`] in `vm/mod.rs`) and looks them up
//! (`start_file_page`), its `dev_offset` is a *file* offset, and `dev` is one
//! filesystem's number: a file page keyed by device offset alone aliases every
//! file's page 0 onto one entry. C does not hit that because its file pages enter
//! the cache from the filesystem's side, which knows a device offset.
//!
//! Each entry holds one reference on the page's `PhysBlock`, so a cached frame
//! survives any single process unmapping it (the process's `pb_unref` leaves the
//! cache's reference) and is freed only when the last mapping and the cache entry
//! both go away. Eviction is LRU over a doubly linked list threaded through the
//! fixed slot array — the VM server is single-threaded `no_std` and allocates
//! nothing at runtime (matching `pb.rs`'s static-table style).
//!
//! Cache entries are always *clean* file content: `finish_page` only inserts pages
//! whose whole 4 KiB lies inside the file, and only for read-only or `MAP_SHARED`
//! regions, so a cached frame is never dirtied by a MAP_PRIVATE write (a writable
//! MAP_PRIVATE file page keeps the private allocate+FDIO path and never enters the
//! cache).

use core::cell::UnsafeCell;

use crate::vm::pb;

/// Number of cached 4 KiB pages (16 MiB of file data). Sized from available
/// RAM; the exec working set (a few MiB of binaries) fits comfortably.
pub const CACHE_CAPACITY: usize = 4096;

/// The port's "no device" sentinel, which is **not** 0. C's `NO_DEV` is `0` (`cache.c`),
/// so a guard copied from C's value is wrong here: the port's sentinel is `0xffff`
/// (`arch_common::consts::NO_DEV`, `vfs/types.rs::NO_DEV`). The root filesystem is
/// *mounted* with device 0 (`vfs/mount.rs::mount_root` passes `dev = 0`), so 0 is a
/// number this port really uses — a file vnode's `v_dev` comes from the filesystem's own
/// reply (0x9e7 for the root FS on x86_64), but a filesystem whose pages did carry 0
/// would otherwise be silently uncacheable.
const NO_DEV: u32 = arch_common::consts::NO_DEV as u32;

/// C's `VMC_NO_INODE` (`minix/vm.h:88`): "to reference a disk block, no associated
/// file". A page carrying it belongs to the device-offset form, not the file one.
const VMC_NO_INODE: u32 = 0;

/// The device offset recorded for a file page, which has none: C's entries always
/// carry one because its producers are filesystem-side. Device offsets are
/// page-aligned (both block paths check `is_multiple_of(PAGE_SIZE)`), and this
/// value is not, so the device-offset form can never match a file entry — which is
/// what keeps the two keyings from seeing each other's pages.
const NO_DEV_OFFSET: u64 = u64::MAX;

/// No LRU neighbour (slot indices are `usize`; `NONE` marks the list ends).
const NONE: usize = usize::MAX;

#[derive(Debug, Clone, Copy)]
struct CacheEntry {
    dev: u32,
    ino: u32,
    /// Offset within the device, for an entry the device-offset form owns;
    /// [`NO_DEV_OFFSET`] for a file page, which has no device offset.
    dev_offset: u64,
    ino_offset: u64,
    /// Physical frame of the cached page.
    phys: u64,
    /// `PhysBlock` index; the cache holds one reference on it.
    pb: usize,
    /// LRU list neighbours (slot indices, `NONE` at the ends).
    older: usize,
    newer: usize,
}

/// Fixed-slot cache table with an LRU list threaded through the slots.
pub struct CacheTable<const N: usize> {
    slots: [Option<CacheEntry>; N],
    count: usize,
    /// Least- and most-recently-used slot indices (`NONE` when empty).
    lru_oldest: usize,
    lru_newest: usize,
}

impl<const N: usize> CacheTable<N> {
    /// Create an empty table.
    pub const fn new() -> Self {
        Self {
            slots: [None; N],
            count: 0,
            lru_oldest: NONE,
            lru_newest: NONE,
        }
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.count
    }

    /// True when the table holds no entries.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Remove `slot` from the LRU list. `slot` must be a live entry.
    fn lru_remove(&mut self, slot: usize) {
        let (older, newer) = {
            let e = self.slots[slot].as_ref().expect("live slot in LRU list");
            (e.older, e.newer)
        };
        if older != NONE {
            self.slots[older].as_mut().expect("older slot live").newer = newer;
        } else {
            self.lru_oldest = newer;
        }
        if newer != NONE {
            self.slots[newer].as_mut().expect("newer slot live").older = older;
        } else {
            self.lru_newest = older;
        }
    }

    /// Push `slot` to the newest end of the LRU list. `slot` must be live
    /// and not already in the list.
    fn lru_push(&mut self, slot: usize) {
        let e = self.slots[slot].as_mut().expect("live slot in LRU list");
        e.older = self.lru_newest;
        e.newer = NONE;
        if self.lru_newest != NONE {
            self.slots[self.lru_newest]
                .as_mut()
                .expect("newest slot live")
                .newer = slot;
        } else {
            self.lru_oldest = slot;
        }
        self.lru_newest = slot;
    }

    /// Move `slot` to the newest end of the LRU list.
    fn lru_touch(&mut self, slot: usize) {
        self.lru_remove(slot);
        self.lru_push(slot);
    }

    /// Look up a live slot by `(dev, dev_offset)` — C's `find_cached_page_bydev`, the
    /// form for a page addressed by a *device* offset (the block paths). When `ino`
    /// is `Some`, restamp the entry's inode identity (C's `update_inohash`); when
    /// `touch`, move the entry to the newest end of the LRU list.
    fn slot_bydev(
        &mut self,
        dev: u32,
        dev_offset: u64,
        ino: Option<u32>,
        ino_offset: u64,
        touch: bool,
    ) -> Option<usize> {
        let hit = self
            .slots
            .iter()
            .position(|s| matches!(s, Some(e) if e.dev == dev && e.dev_offset == dev_offset))?;
        if let Some(ino) = ino {
            let e = self.slots[hit].as_mut().expect("hit slot live");
            e.ino = ino;
            e.ino_offset = ino_offset;
        }
        if touch {
            self.lru_touch(hit);
        }
        Some(hit)
    }

    /// Look up a live slot by `(dev, ino, ino_offset)` — C's
    /// `find_cached_page_byino`, the form for a *file* page.
    ///
    /// The file fault path has to use this one. Its `dev_offset` is a file offset
    /// and `dev` is one filesystem's number, so a file page keyed by device offset
    /// alone aliases every file's page 0 onto one entry.
    fn slot_byino(&mut self, dev: u32, ino: u32, ino_offset: u64, touch: bool) -> Option<usize> {
        let hit = self.slots.iter().position(
            |s| matches!(s, Some(e) if e.dev == dev && e.ino == ino && e.ino_offset == ino_offset),
        )?;
        if touch {
            self.lru_touch(hit);
        }
        Some(hit)
    }

    /// Look up a cached page by device offset; returns the frame's physical address
    /// and `PhysBlock` index, or `None`. The entry keeps its `PhysBlock` reference,
    /// so the returned frame is valid until the entry is evicted or cleared.
    pub fn find_bydev(
        &mut self,
        dev: u32,
        dev_offset: u64,
        ino: Option<u32>,
        ino_offset: u64,
        touch: bool,
    ) -> Option<(u64, usize)> {
        let slot = self.slot_bydev(dev, dev_offset, ino, ino_offset, touch)?;
        let e = self.slots[slot].as_ref().expect("found slot live");
        Some((e.phys, e.pb))
    }

    /// Look up a cached file page by `(dev, ino, ino_offset)`. See
    /// [`Self::slot_byino`] for why the file path keys on the inode.
    pub fn find_byino(
        &mut self,
        dev: u32,
        ino: u32,
        ino_offset: u64,
        touch: bool,
    ) -> Option<(u64, usize)> {
        let slot = self.slot_byino(dev, ino, ino_offset, touch)?;
        let e = self.slots[slot].as_ref().expect("found slot live");
        Some((e.phys, e.pb))
    }

    /// Insert (or replace) the entry for `(dev, dev_offset)` — the device-offset
    /// form, as the block paths reach it. The cache takes one reference on `pb`; a
    /// replaced or evicted entry's reference is released.
    ///
    /// `dev == 0` is refused here because that is C's `NO_DEV`. On this port 0 is a
    /// real device number — the root filesystem's — so this form cannot cache
    /// anything on the root FS; a filesystem that wants its blocks cached there has
    /// to decide this again (see [`Self::insert_byino`] for what the file path
    /// does). Nothing sends `VM_MAPCACHEPAGE`/`VM_SETCACHEPAGE` today, so this form
    /// has no caller outside the tests.
    pub fn insert_bydev(
        &mut self,
        dev: u32,
        dev_offset: u64,
        ino: u32,
        ino_offset: u64,
        phys: u64,
        pb: usize,
    ) {
        if dev == 0 {
            return;
        }
        let found = self.slot_bydev(dev, dev_offset, None, 0, false);
        self.insert_entry(
            found,
            CacheEntry {
                dev,
                dev_offset,
                ino,
                ino_offset,
                phys,
                pb,
                older: NONE,
                newer: NONE,
            },
        );
    }

    /// Insert (or replace) the entry for a file page, keyed by `(dev, ino,
    /// ino_offset)` — C's `addcache` as the file fault path reaches it.
    ///
    /// The device part of the guard is the port's `NO_DEV`, *not* C's `0`: 0 is a number
    /// this port uses (the root filesystem is mounted with it), so C's value would make a
    /// whole filesystem's file pages silently uncacheable. `ino == VMC_NO_INODE` is
    /// refused instead — a page with no file belongs to the device-offset form — and so is
    /// a device-less file (`NO_DEV`).
    pub fn insert_byino(&mut self, dev: u32, ino: u32, ino_offset: u64, phys: u64, pb: usize) {
        if dev == NO_DEV || ino == VMC_NO_INODE {
            return;
        }
        let found = self.slot_byino(dev, ino, ino_offset, false);
        self.insert_entry(
            found,
            CacheEntry {
                dev,
                dev_offset: NO_DEV_OFFSET,
                ino,
                ino_offset,
                phys,
                pb,
                older: NONE,
                newer: NONE,
            },
        );
    }

    /// Place a new entry, or refresh `found`'s. Shared by the two forms above, which
    /// differ only in how an existing entry is found.
    fn insert_entry(&mut self, found: Option<usize>, e: CacheEntry) {
        // Existing key: refresh it. The same frame only needs its identity
        // restamped; a different frame moves the cache's reference.
        if let Some(slot) = found {
            let same_frame = self.slots[slot].as_ref().expect("live slot").phys == e.phys;
            let cached = self.slots[slot].as_mut().expect("live slot");
            cached.ino = e.ino;
            cached.ino_offset = e.ino_offset;
            if !same_frame {
                let old_pb = cached.pb;
                cached.phys = e.phys;
                cached.pb = e.pb;
                pb::pb_ref(e.pb);
                pb::pb_unref(old_pb);
            }
            self.lru_touch(slot);
            return;
        }
        // New key: evict the LRU oldest when full.
        if self.count == N {
            let victim = self.lru_oldest;
            debug_assert!(victim != NONE, "full table must have an oldest");
            let victim_pb = self.slots[victim].as_ref().expect("victim slot live").pb;
            self.lru_remove(victim);
            self.slots[victim] = None;
            self.count -= 1;
            pb::pb_unref(victim_pb);
        }
        let slot = self
            .slots
            .iter()
            .position(|s| s.is_none())
            .expect("a free slot after eviction");
        pb::pb_ref(e.pb);
        self.slots[slot] = Some(e);
        self.count += 1;
        self.lru_push(slot);
    }

    /// Remove every entry for `dev`, releasing each frame's cache reference.
    /// Mappings in live processes keep their own reference, so the frames
    /// survive until those processes unmap them.
    pub fn clear_bydev(&mut self, dev: u32) {
        let mut i = 0;
        while i < N {
            if let Some(e) = self.slots[i].as_ref()
                && e.dev == dev
            {
                let pb = e.pb;
                self.lru_remove(i);
                self.slots[i] = None;
                self.count -= 1;
                pb::pb_unref(pb);
                continue; // slot `i` is free; do not advance
            }
            i += 1;
        }
    }
}

impl<const N: usize> Default for CacheTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

struct CacheTableCell<const N: usize>(UnsafeCell<CacheTable<N>>);
unsafe impl<const N: usize> Sync for CacheTableCell<N> {}
impl<const N: usize> CacheTableCell<N> {
    const fn new() -> Self {
        Self(UnsafeCell::new(CacheTable::new()))
    }
    fn get(&self) -> *mut CacheTable<N> {
        self.0.get()
    }
}

static CACHE_TABLE: CacheTableCell<CACHE_CAPACITY> = CacheTableCell::new();

/// Find a cached page by device offset; returns `(phys, pb)` or `None`. See
/// [`CacheTable::find_bydev`].
pub fn cache_find_bydev(
    dev: u32,
    dev_offset: u64,
    ino: Option<u32>,
    ino_offset: u64,
    touch: bool,
) -> Option<(u64, usize)> {
    unsafe { (*CACHE_TABLE.get()).find_bydev(dev, dev_offset, ino, ino_offset, touch) }
}

/// Find a cached file page by `(dev, ino, ino_offset)`. See
/// [`CacheTable::find_byino`].
pub fn cache_find_byino(dev: u32, ino: u32, ino_offset: u64, touch: bool) -> Option<(u64, usize)> {
    if dev == NO_DEV || ino == VMC_NO_INODE {
        return None;
    }
    unsafe { (*CACHE_TABLE.get()).find_byino(dev, ino, ino_offset, touch) }
}

/// Insert a page keyed by device offset. See [`CacheTable::insert_bydev`].
pub fn cache_insert_bydev(
    dev: u32,
    dev_offset: u64,
    ino: u32,
    ino_offset: u64,
    phys: u64,
    pb: usize,
) {
    unsafe {
        (*CACHE_TABLE.get()).insert_bydev(dev, dev_offset, ino, ino_offset, phys, pb);
    }
}

/// Insert a file page keyed by `(dev, ino, ino_offset)`. See
/// [`CacheTable::insert_byino`].
pub fn cache_insert_byino(dev: u32, ino: u32, ino_offset: u64, phys: u64, pb: usize) {
    unsafe {
        (*CACHE_TABLE.get()).insert_byino(dev, ino, ino_offset, phys, pb);
    }
}

/// Drop every cached page of `dev`. See [`CacheTable::clear_bydev`].
pub fn cache_clear_bydev(dev: u32) {
    unsafe {
        (*CACHE_TABLE.get()).clear_bydev(dev);
    }
}

/// Number of live cache entries.
pub fn cache_len() -> usize {
    unsafe { (*CACHE_TABLE.get()).len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_find() {
        let mut t = CacheTable::<8>::new();
        assert!(t.is_empty());
        // phys=0 keeps pb_unref from calling vm_free_pages on host.
        let pb = pb::pb_new(0).unwrap();
        t.insert_bydev(1, 0x1000, 42, 0x2000, 0x4000, pb);
        assert_eq!(t.len(), 1);
        assert_eq!(t.find_bydev(1, 0x1000, None, 0, false), Some((0x4000, pb)));
        // A different dev or offset misses.
        assert_eq!(t.find_bydev(2, 0x1000, None, 0, false), None);
        assert_eq!(t.find_bydev(1, 0x2000, None, 0, false), None);
        t.clear_bydev(1);
        assert!(t.is_empty());
    }

    #[test]
    fn test_find_restamps_inode() {
        let mut t = CacheTable::<8>::new();
        let pb = pb::pb_new(0).unwrap();
        t.insert_bydev(1, 0x1000, 42, 0x2000, 0x4000, pb);
        // find with a different ino updates the identity.
        assert_eq!(
            t.find_bydev(1, 0x1000, Some(7), 0x3000, false),
            Some((0x4000, pb))
        );
        let slot = t.slot_bydev(1, 0x1000, None, 0, false).unwrap();
        assert_eq!(t.slots[slot].as_ref().unwrap().ino, 7);
        assert_eq!(t.slots[slot].as_ref().unwrap().ino_offset, 0x3000);
    }

    #[test]
    fn test_insert_replaces_same_key() {
        let mut t = CacheTable::<8>::new();
        let pb1 = pb::pb_new(0).unwrap();
        let pb2 = pb::pb_new(0).unwrap();
        t.insert_bydev(1, 0x1000, 0, 0, 0x4000, pb1);
        t.insert_bydev(1, 0x1000, 0, 0, 0x5000, pb2);
        assert_eq!(t.len(), 1);
        assert_eq!(t.find_bydev(1, 0x1000, None, 0, false), Some((0x5000, pb2)));
        // The replacement released the cache's reference on the old frame
        // (back to the owner's 1) and took one on the new frame.
        assert_eq!(pb::pb_get(pb1).map(|b| b.refcount), Some(1));
        assert_eq!(pb::pb_get(pb2).map(|b| b.refcount), Some(2));
        // Dropping the owner reference frees the old frame; clearing the
        // cache frees the new one.
        pb::pb_unref(pb1);
        assert!(pb::pb_get(pb1).is_none());
        t.clear_bydev(1);
        assert_eq!(pb::pb_get(pb2).map(|b| b.refcount), Some(1));
        pb::pb_unref(pb2);
        assert!(pb::pb_get(pb2).is_none());
    }

    #[test]
    fn test_lru_eviction() {
        let mut t = CacheTable::<4>::new();
        let mut pbs = [0usize; 4];
        for (i, slot) in pbs.iter_mut().enumerate() {
            let pb = pb::pb_new(0).unwrap();
            *slot = pb;
            t.insert_bydev(1, (i as u64) * 0x1000, 0, 0, (i as u64 + 1) * 0x1000, pb);
        }
        // Touching entry 0 makes it newest.
        assert_eq!(t.find_bydev(1, 0, None, 0, true), Some((0x1000, pbs[0])));
        // Inserting a fifth entry evicts the LRU oldest: entry 1. The
        // eviction releases the cache's reference (back to the owner's 1).
        let pb5 = pb::pb_new(0).unwrap();
        t.insert_bydev(1, 4 * 0x1000, 0, 0, 5 * 0x1000, pb5);
        assert_eq!(t.len(), 4);
        assert_eq!(t.find_bydev(1, 0, None, 0, false), Some((0x1000, pbs[0])));
        assert_eq!(t.find_bydev(1, 0x1000, None, 0, false), None);
        assert_eq!(pb::pb_get(pbs[1]).map(|b| b.refcount), Some(1));
        pb::pb_unref(pbs[1]);
        assert!(pb::pb_get(pbs[1]).is_none());
        assert_eq!(
            t.find_bydev(1, 4 * 0x1000, None, 0, false),
            Some((5 * 0x1000, pb5))
        );
        t.clear_bydev(1);
        pb::pb_unref(pb5);
        pb::pb_unref(pbs[0]);
        pb::pb_unref(pbs[2]);
        pb::pb_unref(pbs[3]);
    }

    #[test]
    fn test_clear_bydev_only_clears_that_dev() {
        let mut t = CacheTable::<8>::new();
        let pb1 = pb::pb_new(0).unwrap();
        let pb2 = pb::pb_new(0).unwrap();
        t.insert_bydev(1, 0x1000, 0, 0, 0x4000, pb1);
        t.insert_bydev(2, 0x1000, 0, 0, 0x5000, pb2);
        t.clear_bydev(1);
        assert_eq!(t.len(), 1);
        assert_eq!(t.find_bydev(1, 0x1000, None, 0, false), None);
        assert_eq!(t.find_bydev(2, 0x1000, None, 0, false), Some((0x5000, pb2)));
        // Only the cleared dev's cache reference was released.
        assert_eq!(pb::pb_get(pb1).map(|b| b.refcount), Some(1));
        pb::pb_unref(pb1);
        assert!(pb::pb_get(pb1).is_none());
        assert_eq!(pb::pb_get(pb2).map(|b| b.refcount), Some(2));
        t.clear_bydev(2);
        assert!(t.is_empty());
        assert_eq!(pb::pb_get(pb2).map(|b| b.refcount), Some(1));
        pb::pb_unref(pb2);
        assert!(pb::pb_get(pb2).is_none());
    }

    #[test]
    fn test_insert_bydev_refuses_device_zero() {
        // The device-offset form keeps C's guard verbatim (`NO_DEV` is 0 in C),
        // even though 0 is a real device number on this port. Nothing sends
        // VM_SETCACHEPAGE today; file pages go through `insert_byino`.
        let mut t = CacheTable::<8>::new();
        let pb = pb::pb_new(0).unwrap();
        t.insert_bydev(0, 0x1000, 0, 0, 0x4000, pb);
        assert!(t.is_empty());
        // No reference was taken for the refused entry.
        assert_eq!(pb::pb_get(pb).map(|b| b.refcount), Some(1));
        pb::pb_unref(pb);
    }

    #[test]
    fn test_insert_byino_accepts_the_root_device_and_refuses_no_dev() {
        // Device 0 is the root filesystem's on this port, so refusing it is what
        // left the cache empty and every file page private. Refused instead are the
        // port's `NO_DEV` (0xffff) and C's `VMC_NO_INODE` (0).
        let mut t = CacheTable::<8>::new();
        let pb = pb::pb_new(0).unwrap();
        t.insert_byino(0, 42, 0x1000, 0x4000, pb);
        assert_eq!(t.len(), 1);
        assert_eq!(t.find_byino(0, 42, 0x1000, false), Some((0x4000, pb)));
        t.insert_byino(NO_DEV, 42, 0x2000, 0x5000, pb);
        t.insert_byino(0, VMC_NO_INODE, 0x2000, 0x5000, pb);
        assert_eq!(t.len(), 1);
        assert_eq!(t.find_byino(NO_DEV, 42, 0x2000, false), None);
        assert_eq!(t.find_byino(0, VMC_NO_INODE, 0x2000, false), None);
        t.clear_bydev(0);
        assert!(t.is_empty());
        pb::pb_unref(pb);
    }

    #[test]
    fn test_byino_key_includes_the_inode() {
        // Two files on one device at the same file offset are different pages. The
        // device-offset form would have shared one entry between them.
        let mut t = CacheTable::<8>::new();
        let pb1 = pb::pb_new(0).unwrap();
        let pb2 = pb::pb_new(0).unwrap();
        t.insert_byino(0, 11, 0, 0x4000, pb1);
        t.insert_byino(0, 12, 0, 0x5000, pb2);
        assert_eq!(t.len(), 2);
        assert_eq!(t.find_byino(0, 11, 0, false), Some((0x4000, pb1)));
        assert_eq!(t.find_byino(0, 12, 0, false), Some((0x5000, pb2)));
        // And an inode-keyed insert of the same key replaces rather than duplicates.
        t.insert_byino(0, 11, 0, 0x6000, pb1);
        assert_eq!(t.len(), 2);
        assert_eq!(t.find_byino(0, 11, 0, false), Some((0x6000, pb1)));
        t.clear_bydev(0);
        pb::pb_unref(pb1);
        pb::pb_unref(pb2);
    }

    #[test]
    fn test_the_two_keys_do_not_see_each_other() {
        // A file page and a block page can carry the same offset and device; each
        // form must find only its own entry. File entries store `NO_DEV_OFFSET`,
        // which no device offset can equal.
        let mut t = CacheTable::<8>::new();
        let pb1 = pb::pb_new(0).unwrap();
        let pb2 = pb::pb_new(0).unwrap();
        t.insert_byino(7, 11, 0x1000, 0x4000, pb1);
        t.insert_bydev(7, 0x1000, 11, 0x1000, 0x5000, pb2);
        assert_eq!(t.len(), 2);
        assert_eq!(t.find_byino(7, 11, 0x1000, false), Some((0x4000, pb1)));
        assert_eq!(t.find_bydev(7, 0x1000, None, 0, false), Some((0x5000, pb2)));
        t.clear_bydev(7);
        pb::pb_unref(pb1);
        pb::pb_unref(pb2);
    }

    #[test]
    fn test_global_functions() {
        let pb = pb::pb_new(0).unwrap();
        let before = cache_len();
        cache_insert_byino(0, 3, 0x1000, 0x4000, pb);
        assert_eq!(cache_len(), before + 1);
        assert_eq!(cache_find_byino(0, 3, 0x1000, false), Some((0x4000, pb)));
        assert_eq!(cache_find_byino(0, 4, 0x1000, false), None);
        cache_clear_bydev(0);
        assert_eq!(cache_len(), before);
        assert_eq!(cache_find_byino(0, 3, 0x1000, false), None);
        pb::pb_unref(pb);
    }
}
