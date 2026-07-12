//! Physical page reference-counting module.
//!
//! Provides `PhysBlock` — a reference-counted physical page — and
//! `PhysBlockTable`, a fixed-capacity table of `PhysBlock` entries.
//! Multiple `VirRegion` entries (in the same or different processes)
//! can share a `PhysBlock`, enabling Copy-on-Write (COW) after fork.
//!
//! The table holds up to 1024 entries, which is sufficient for typical
//! boot and fork workloads. Entries are never moved after allocation;
//! the index returned by `pb_new` remains valid until `pb_unref` frees it.

/// A reference-counted physical page.
#[derive(Debug, Clone, Copy)]
pub struct PhysBlock {
    /// Physical address (page-aligned).
    pub phys: u64,
    /// Number of references (> 1 means shared / COW-protected).
    pub refcount: u8,
}

/// A fixed-capacity table of `PhysBlock` entries.
pub struct PhysBlockTable {
    blocks: [Option<PhysBlock>; 1024],
    count: u16,
}

impl PhysBlockTable {
    /// Create an empty table.
    pub const fn new() -> Self {
        Self {
            blocks: [None; 1024],
            count: 0,
        }
    }

    /// Allocate a new `PhysBlock` for the given physical address.
    /// Returns the index on success, `None` if the table is full.
    pub fn alloc(&mut self, phys: u64) -> Option<usize> {
        for (idx, slot) in self.blocks.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(PhysBlock { phys, refcount: 1 });
                self.count += 1;
                return Some(idx);
            }
        }
        None
    }

    /// Increment the reference count of the block at `idx`.
    /// Returns `true` if the block exists and was bumped.
    /// Returns `false` if the slot is empty or `idx` is out of range.
    pub fn reference(&mut self, idx: usize) -> bool {
        match self.blocks.get_mut(idx).and_then(|s| s.as_mut()) {
            Some(block) => {
                block.refcount = block.refcount.saturating_add(1);
                true
            }
            None => false,
        }
    }

    /// Decrement the reference count of the block at `idx`.
    ///
    /// If the refcount reaches 0, the slot is freed and the physical page
    /// is deallocated (via `kernel::vm::free_mem`).
    pub fn unreference(&mut self, idx: usize) {
        let block = match self.blocks.get_mut(idx).and_then(|s| s.as_mut()) {
            Some(block) => block,
            None => return,
        };

        if block.refcount > 1 {
            block.refcount -= 1;
            return;
        }

        // Refcount is 1 (only reference) — remove and free the physical page.
        if let Some(block) = self.blocks[idx].take() {
            self.count -= 1;
            let pa = block.phys;
            if pa != 0 {
                crate::vm::vm_free_pages(pa, 1);
            }
        }
    }

    /// Get a shared reference to the block at `idx`.
    pub fn get(&self, idx: usize) -> Option<&PhysBlock> {
        self.blocks.get(idx).and_then(|s| s.as_ref())
    }

    /// Find a block by physical address. Returns the index if found.
    pub fn find(&self, phys: u64) -> Option<usize> {
        self.blocks.iter().position(|s| match s {
            Some(b) => b.phys == phys,
            None => false,
        })
    }

    /// Return the number of active entries.
    pub fn len(&self) -> u16 {
        self.count
    }

    /// Return true if the table is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl Default for PhysBlockTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Global PhysBlock table, accessible from the single-threaded VM server.
use core::cell::UnsafeCell;

struct PbTableCell(UnsafeCell<PhysBlockTable>);
unsafe impl Sync for PbTableCell {}
impl PbTableCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(PhysBlockTable::new()))
    }
    fn get(&self) -> *mut PhysBlockTable {
        self.0.get()
    }
}

static PB_TABLE: PbTableCell = PbTableCell::new();

/// Allocate a new PhysBlock. Returns the index, or `None` if full.
pub fn pb_new(phys: u64) -> Option<usize> {
    unsafe { (*PB_TABLE.get()).alloc(phys) }
}

/// Increment the refcount of the block at `idx`.
pub fn pb_ref(idx: usize) -> bool {
    unsafe { (*PB_TABLE.get()).reference(idx) }
}

/// Decrement the refcount of the block at `idx`, freeing the physical
/// page if the refcount reaches 0.
pub fn pb_unref(idx: usize) {
    unsafe { (*PB_TABLE.get()).unreference(idx) }
}

/// Get a reference to the block at `idx`.
pub fn pb_get(idx: usize) -> Option<&'static PhysBlock> {
    unsafe { (*PB_TABLE.get()).get(idx) }
}

/// Find a block by physical address. Returns the index, or `None`.
pub fn pb_find(phys: u64) -> Option<usize> {
    unsafe { (*PB_TABLE.get()).find(phys) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pb_new_and_get() {
        let mut table = PhysBlockTable::new();
        let idx = table.alloc(0x1000).unwrap();
        let block = table.get(idx).unwrap();
        assert_eq!(block.phys, 0x1000);
        assert_eq!(block.refcount, 1);
    }

    #[test]
    fn test_pb_ref_increments() {
        let mut table = PhysBlockTable::new();
        let idx = table.alloc(0x2000).unwrap();
        assert!(table.reference(idx));
        assert_eq!(table.get(idx).unwrap().refcount, 2);
        assert!(table.reference(idx));
        assert_eq!(table.get(idx).unwrap().refcount, 3);
    }

    #[test]
    fn test_pb_ref_invalid_index() {
        let mut table = PhysBlockTable::new();
        assert!(!table.reference(9999));
    }

    #[test]
    fn test_pb_unref_decrements() {
        let mut table = PhysBlockTable::new();
        // Use phys=0 so unreference doesn't call free_mem on fake pages.
        let idx = table.alloc(0).unwrap();
        table.reference(idx);
        assert_eq!(table.get(idx).unwrap().refcount, 2);
        // Decrement once: still > 1
        table.unreference(idx);
        assert_eq!(table.get(idx).unwrap().refcount, 1);
        // Decrement again: refcount becomes 0, entry freed
        table.unreference(idx);
        assert!(table.get(idx).is_none());
    }

    #[test]
    fn test_pb_find_by_phys() {
        let mut table = PhysBlockTable::new();
        table.alloc(0x1000);
        let idx_b = table.alloc(0x2000).unwrap();
        table.alloc(0x3000);
        assert_eq!(table.find(0x2000), Some(idx_b));
        assert_eq!(table.find(0x4000), None);
    }

    #[test]
    fn test_pb_table_full() {
        let mut table = PhysBlockTable::new();
        for i in 0..1024 {
            let phys = (i as u64 + 1) * 0x1000;
            assert!(table.alloc(phys).is_some(), "slot {} should accept", i);
        }
        assert!(table.alloc(0xDEAD000).is_none());
    }

    #[test]
    fn test_pb_refcount_saturation() {
        let mut table = PhysBlockTable::new();
        let idx = table.alloc(0x5000).unwrap();
        // start at 1, each reference() adds 1 (saturating at 255)
        for _ in 0..250 {
            table.reference(idx);
        }
        // 1 + 250 = 251
        assert_eq!(table.get(idx).unwrap().refcount, 251);
        // Saturate at u8::MAX
        for _ in 0..10 {
            table.reference(idx);
        }
        assert_eq!(table.get(idx).unwrap().refcount, u8::MAX);
    }

    #[test]
    fn test_pb_global_functions() {
        // Global functions should be callable without panicking.
        // Use phys=0 to avoid kernel::vm::free_mem on unallocated pages.
        let idx = pb_new(0).unwrap();
        assert!(pb_get(idx).is_some());
        assert!(pb_ref(idx));
        pb_unref(idx);
        // After one unref, refcount goes from 2 to 1 (still valid)
        assert!(pb_get(idx).is_some());
        pb_unref(idx);
        // After second unref, entry should be freed
        // (phys=0 means pageno=0, so free_mem is skipped)
        assert!(pb_get(idx).is_none());
    }
}
