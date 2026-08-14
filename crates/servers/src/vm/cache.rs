//! File block cache for the VM server.
//!
//! A fixed-capacity cache of clean file pages, keyed by `(dev, dev_offset)`
//! with `(ino, ino_offset)` carried for identity (C's `cache.c`). Each entry
//! holds one reference on the page's `PhysBlock`, so a cached frame survives
//! any single process unmapping it (the process's `pb_unref` leaves the
//! cache's reference) and is freed only when the last mapping and the cache
//! entry both go away. Eviction is LRU over a doubly linked list threaded
//! through the fixed slot array — the VM server is single-threaded `no_std`
//! and allocates nothing at runtime (matching `pb.rs`'s static-table style).
//!
//! Cache entries are always *clean* file content: `map_file_page` only
//! inserts pages whose whole 4 KiB lies inside the file, and only for
//! read-only regions, so a cached frame is never dirtied by a
//! MAP_PRIVATE write (writable file pages keep the private allocate+FDIO
//! path and never enter the cache).

use core::cell::UnsafeCell;

use crate::vm::pb;

/// Number of cached 4 KiB pages (16 MiB of file data). Sized from available
/// RAM; the exec working set (a few MiB of binaries) fits comfortably.
pub const CACHE_CAPACITY: usize = 4096;

/// No LRU neighbour (slot indices are `usize`; `NONE` marks the list ends).
const NONE: usize = usize::MAX;

#[derive(Debug, Clone, Copy)]
struct CacheEntry {
    dev: u32,
    ino: u32,
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

    /// Look up a live slot by `(dev, dev_offset)`. When `ino` is `Some`,
    /// restamp the entry's inode identity (C's `update_inohash`); when
    /// `touch`, move the entry to the newest end of the LRU list.
    fn find_slot(
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

    /// Look up a cached page by `(dev, dev_offset)`; returns the frame's
    /// physical address and `PhysBlock` index, or `None`. The entry keeps
    /// its `PhysBlock` reference, so the returned frame is valid until the
    /// entry is evicted or cleared.
    pub fn find(
        &mut self,
        dev: u32,
        dev_offset: u64,
        ino: Option<u32>,
        ino_offset: u64,
        touch: bool,
    ) -> Option<(u64, usize)> {
        let slot = self.find_slot(dev, dev_offset, ino, ino_offset, touch)?;
        let e = self.slots[slot].as_ref().expect("found slot live");
        Some((e.phys, e.pb))
    }

    /// Insert (or replace) the entry for `(dev, dev_offset)`. The cache
    /// takes one reference on `pb`; a replaced or evicted entry's reference
    /// is released. Entries with `dev == 0` (`NO_DEV`) are refused.
    pub fn insert(
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
        // Existing key: refresh it. The same frame only needs its identity
        // restamped; a different frame moves the cache's reference.
        if let Some(slot) = self.find_slot(dev, dev_offset, None, 0, false) {
            let same_frame = self.slots[slot].as_ref().expect("live slot").phys == phys;
            let e = self.slots[slot].as_mut().expect("live slot");
            e.ino = ino;
            e.ino_offset = ino_offset;
            if !same_frame {
                let old_pb = e.pb;
                e.phys = phys;
                e.pb = pb;
                pb::pb_ref(pb);
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
        pb::pb_ref(pb);
        self.slots[slot] = Some(CacheEntry {
            dev,
            ino,
            dev_offset,
            ino_offset,
            phys,
            pb,
            older: NONE,
            newer: NONE,
        });
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

/// Find a cached page by `(dev, dev_offset)`; returns `(phys, pb)` or
/// `None`. See [`CacheTable::find`].
pub fn cache_find(
    dev: u32,
    dev_offset: u64,
    ino: Option<u32>,
    ino_offset: u64,
    touch: bool,
) -> Option<(u64, usize)> {
    unsafe { (*CACHE_TABLE.get()).find(dev, dev_offset, ino, ino_offset, touch) }
}

/// Insert a page into the cache. See [`CacheTable::insert`].
pub fn cache_insert(dev: u32, dev_offset: u64, ino: u32, ino_offset: u64, phys: u64, pb: usize) {
    unsafe {
        (*CACHE_TABLE.get()).insert(dev, dev_offset, ino, ino_offset, phys, pb);
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
        t.insert(1, 0x1000, 42, 0x2000, 0x4000, pb);
        assert_eq!(t.len(), 1);
        assert_eq!(t.find(1, 0x1000, None, 0, false), Some((0x4000, pb)));
        // A different dev or offset misses.
        assert_eq!(t.find(2, 0x1000, None, 0, false), None);
        assert_eq!(t.find(1, 0x2000, None, 0, false), None);
        t.clear_bydev(1);
        assert!(t.is_empty());
    }

    #[test]
    fn test_find_restamps_inode() {
        let mut t = CacheTable::<8>::new();
        let pb = pb::pb_new(0).unwrap();
        t.insert(1, 0x1000, 42, 0x2000, 0x4000, pb);
        // find with a different ino updates the identity.
        assert_eq!(
            t.find(1, 0x1000, Some(7), 0x3000, false),
            Some((0x4000, pb))
        );
        let slot = t.find_slot(1, 0x1000, None, 0, false).unwrap();
        assert_eq!(t.slots[slot].as_ref().unwrap().ino, 7);
        assert_eq!(t.slots[slot].as_ref().unwrap().ino_offset, 0x3000);
    }

    #[test]
    fn test_insert_replaces_same_key() {
        let mut t = CacheTable::<8>::new();
        let pb1 = pb::pb_new(0).unwrap();
        let pb2 = pb::pb_new(0).unwrap();
        t.insert(1, 0x1000, 0, 0, 0x4000, pb1);
        t.insert(1, 0x1000, 0, 0, 0x5000, pb2);
        assert_eq!(t.len(), 1);
        assert_eq!(t.find(1, 0x1000, None, 0, false), Some((0x5000, pb2)));
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
            t.insert(1, (i as u64) * 0x1000, 0, 0, (i as u64 + 1) * 0x1000, pb);
        }
        // Touching entry 0 makes it newest.
        assert_eq!(t.find(1, 0, None, 0, true), Some((0x1000, pbs[0])));
        // Inserting a fifth entry evicts the LRU oldest: entry 1. The
        // eviction releases the cache's reference (back to the owner's 1).
        let pb5 = pb::pb_new(0).unwrap();
        t.insert(1, 4 * 0x1000, 0, 0, 5 * 0x1000, pb5);
        assert_eq!(t.len(), 4);
        assert_eq!(t.find(1, 0, None, 0, false), Some((0x1000, pbs[0])));
        assert_eq!(t.find(1, 0x1000, None, 0, false), None);
        assert_eq!(pb::pb_get(pbs[1]).map(|b| b.refcount), Some(1));
        pb::pb_unref(pbs[1]);
        assert!(pb::pb_get(pbs[1]).is_none());
        assert_eq!(
            t.find(1, 4 * 0x1000, None, 0, false),
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
        t.insert(1, 0x1000, 0, 0, 0x4000, pb1);
        t.insert(2, 0x1000, 0, 0, 0x5000, pb2);
        t.clear_bydev(1);
        assert_eq!(t.len(), 1);
        assert_eq!(t.find(1, 0x1000, None, 0, false), None);
        assert_eq!(t.find(2, 0x1000, None, 0, false), Some((0x5000, pb2)));
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
    fn test_insert_refuses_no_dev() {
        let mut t = CacheTable::<8>::new();
        let pb = pb::pb_new(0).unwrap();
        t.insert(0, 0x1000, 0, 0, 0x4000, pb);
        assert!(t.is_empty());
        // No reference was taken for the refused entry.
        assert_eq!(pb::pb_get(pb).map(|b| b.refcount), Some(1));
        pb::pb_unref(pb);
    }

    #[test]
    fn test_global_functions() {
        let pb = pb::pb_new(0).unwrap();
        let before = cache_len();
        cache_insert(9, 0x1000, 3, 0x2000, 0x4000, pb);
        assert_eq!(cache_len(), before + 1);
        assert_eq!(cache_find(9, 0x1000, None, 0, false), Some((0x4000, pb)));
        cache_clear_bydev(9);
        assert_eq!(cache_len(), before);
        assert_eq!(cache_find(9, 0x1000, None, 0, false), None);
        pb::pb_unref(pb);
    }
}
