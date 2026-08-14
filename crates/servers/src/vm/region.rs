//! Virtual memory region abstraction for the VM server.
//!
//! Tracks contiguous virtual address ranges backed by physical pages.
//! For Phase 2, uses a flat array of regions per process (no AVL tree).

/// Maximum number of regions per process.
/// Boot processes have 2-3 (code, data/brk, stack). Allow room for mmap.
pub const MAX_REGIONS: usize = 16;

/// A single contiguous virtual memory region with physical backing.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VirRegion {
    /// Start virtual address (page-aligned).
    pub vaddr: u64,
    /// Size in bytes (multiple of PAGE_SIZE).
    pub length: u64,
    /// Region flags (VR_WRITABLE, VR_ANON, VR_DIRECT, VR_PRESENT, VR_FILE).
    pub flags: u32,
    /// Number of physical pages backing this region.
    pub npages: u32,
    /// Physical addresses of backing pages (up to MAX_PHYS_PAGES).
    ///
    /// For anonymous regions with lazy allocation, phys_pages may be
    /// mostly zero until page faults trigger allocation.
    pub phys_pages: [u64; MAX_PHYS_PAGES],
    /// For VR_FILE regions: device of the backing file (from FDLOOKUP).
    pub dev: u32,
    /// For VR_FILE regions: inode number of the backing file.
    pub ino: u32,
    /// For VR_FILE regions: VM file descriptor (dup'd into VFS's own fproc
    /// by FDLOOKUP); FDIO reads use it and teardown closes it via FDCLOSE.
    pub fd: i32,
    /// For VR_FILE regions: page-aligned file offset of the first mapped
    /// byte (region.vaddr + i*PAGE maps file bytes file_offset + i*PAGE).
    pub file_offset: u64,
    /// For VR_FILE regions: absolute file offset at which this region's
    /// file content ends (file_offset + filesz for exec segments;
    /// min(mapped-end, EOF) for user mmap). Pages whose file offset is at
    /// or past this are supplied as zero pages (bss tails, holes, beyond
    /// EOF) instead of being read from the file.
    pub file_size: u64,
}

/// Maximum physical pages tracked inline per region.
/// 16 pages = 64 KB; larger regions page-fault and add entries dynamically.
pub const MAX_PHYS_PAGES: usize = 16;

// Region flags

/// Region is writable.
pub const VR_WRITABLE: u32 = 0x01;
/// Region is readable.
pub const VR_READABLE: u32 = 0x02;
/// Anonymous memory (zero-fill, no file backing).
pub const VR_ANON: u32 = 0x04;
/// Direct physical mapping (I/O, device memory).
pub const VR_DIRECT: u32 = 0x08;
/// Region has physical pages present (vs. lazy).
pub const VR_PRESENT: u32 = 0x10;
/// Region is a data segment (brk heap).
pub const VR_DATA: u32 = 0x20;
/// Region is backed by a file; pages fault in from the file via the
/// VM↔VFS FDIO protocol instead of being allocated from the heap.
pub const VR_FILE: u32 = 0x40;
/// Region is executable (exec text segment). Non-executable file
/// regions (rodata/data) are pre-faulted at exec so VFS's kernel-mode
/// copies of the image (vircopy of user buffers) hit present pages.
pub const VR_EXEC: u32 = 0x80;
/// Region maps cached file blocks (do_mapcache): a writable, fully
/// present window over frames the VM block cache shares with a
/// filesystem. Not file-backed (no fd, no FDIO) — teardown frees the
/// frames through their PhysBlock references like any other mapping.
pub const VR_CACHE: u32 = 0x100;

impl VirRegion {
    /// Create a new region descriptor.
    pub const fn new(vaddr: u64, length: u64, flags: u32) -> Self {
        Self {
            vaddr,
            length,
            flags,
            npages: 0,
            phys_pages: [0u64; MAX_PHYS_PAGES],
            dev: 0,
            ino: 0,
            fd: -1,
            file_offset: 0,
            file_size: 0,
        }
    }

    /// Create a new file-backed region descriptor.
    #[allow(clippy::too_many_arguments)]
    pub const fn new_file(
        vaddr: u64,
        length: u64,
        flags: u32,
        dev: u32,
        ino: u32,
        fd: i32,
        file_offset: u64,
        file_size: u64,
    ) -> Self {
        Self {
            vaddr,
            length,
            flags,
            npages: 0,
            phys_pages: [0u64; MAX_PHYS_PAGES],
            dev,
            ino,
            fd,
            file_offset,
            file_size,
        }
    }

    /// File offset of the page containing `addr` within this file region.
    pub const fn file_offset_at(&self, addr: u64) -> u64 {
        let page_size: u64 = 4096;
        let page = (addr - self.vaddr) & !(page_size - 1);
        self.file_offset + page
    }

    /// End address (exclusive).
    pub const fn end(&self) -> u64 {
        self.vaddr + self.length
    }

    /// Check if an address falls within this region.
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.vaddr && addr < self.end()
    }

    /// Check if this region overlaps another.
    pub fn overlaps(&self, other: &VirRegion) -> bool {
        self.vaddr < other.end() && other.vaddr < self.end()
    }

    /// Record a physical page at the given virtual address offset.
    /// Returns the index, or None if the array is full.
    pub fn add_page(&mut self, _vaddr: u64, phys: u64) -> Option<usize> {
        let idx = self.npages as usize;
        if idx >= MAX_PHYS_PAGES {
            return None;
        }
        self.phys_pages[idx] = phys;
        self.npages += 1;
        Some(idx)
    }

    /// Find the physical page for a virtual address within this region.
    pub fn phys_at(&self, addr: u64) -> Option<u64> {
        if !self.contains(addr) {
            return None;
        }
        let page_size: u64 = 4096;
        let offset = (addr - self.vaddr) / page_size;
        let idx = offset as usize;
        if idx < MAX_PHYS_PAGES && idx < self.npages as usize {
            let pa = self.phys_pages[idx];
            if pa != 0 {
                return Some(pa);
            }
        }
        None
    }
}

// Region list management

/// A flat array of virtual regions (no AVL tree for Phase 2).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct RegionList {
    pub regions: [Option<VirRegion>; MAX_REGIONS],
}

impl RegionList {
    /// Create an empty region list.
    pub const fn new() -> Self {
        Self {
            regions: [None; MAX_REGIONS],
        }
    }

    /// Insert a region into the list. Returns None on success,
    /// or Some(region) if the list is full or overlaps an existing region.
    pub fn insert(&mut self, region: VirRegion) -> Option<VirRegion> {
        // Check for overlap
        for existing in self.regions.iter().flatten() {
            if existing.overlaps(&region) {
                return Some(region);
            }
        }
        // Find a free slot
        for slot in self.regions.iter_mut() {
            if slot.is_none() {
                *slot = Some(region);
                return None;
            }
        }
        // Full
        Some(region)
    }

    /// Find the region containing the given virtual address.
    pub fn find(&self, vaddr: u64) -> Option<&VirRegion> {
        self.regions.iter().flatten().find(|r| r.contains(vaddr))
    }

    /// Find the region containing the given virtual address (mutable).
    pub fn find_mut(&mut self, vaddr: u64) -> Option<&mut VirRegion> {
        self.regions
            .iter_mut()
            .flatten()
            .find(|r| r.contains(vaddr))
    }

    /// Remove a region by virtual address. Returns the removed region, or None.
    pub fn remove(&mut self, vaddr: u64) -> Option<VirRegion> {
        for slot in self.regions.iter_mut() {
            if let Some(r) = slot
                && r.vaddr == vaddr
            {
                return slot.take();
            }
        }
        None
    }

    /// Number of regions in the list.
    pub fn len(&self) -> usize {
        self.regions.iter().flatten().count()
    }

    /// Check if the list is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check if the list is full.
    pub fn is_full(&self) -> bool {
        self.regions.iter().all(|r| r.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_region_new() {
        let r = VirRegion::new(0x1000, 0x4000, VR_WRITABLE | VR_ANON);
        assert_eq!(r.vaddr, 0x1000);
        assert_eq!(r.length, 0x4000);
        assert_eq!(r.end(), 0x5000);
        assert!(r.contains(0x1000));
        assert!(r.contains(0x4FFF));
        assert!(!r.contains(0x5000));
        assert!(!r.contains(0x0FFF));
    }

    #[test]
    fn test_region_overlaps() {
        let a = VirRegion::new(0x1000, 0x4000, VR_ANON);
        let b = VirRegion::new(0x3000, 0x2000, VR_ANON); // overlaps
        let c = VirRegion::new(0x6000, 0x1000, VR_ANON); // no overlap
        assert!(a.overlaps(&b));
        assert!(b.overlaps(&a));
        assert!(!a.overlaps(&c));
    }

    #[test]
    fn test_region_add_page() {
        let mut r = VirRegion::new(0x1000, 0x4000, VR_ANON);
        assert!(r.add_page(0x1000, 0x8000).is_some());
        assert_eq!(r.npages, 1);
        assert_eq!(r.phys_pages[0], 0x8000);
        assert_eq!(r.phys_at(0x1000), Some(0x8000));
        assert_eq!(r.phys_at(0x2000), None); // not added yet
    }

    #[test]
    fn test_region_list_insert_and_find() {
        let mut list = RegionList::new();
        let r = VirRegion::new(0x1000, 0x4000, VR_ANON);
        assert!(list.insert(r).is_none());
        assert_eq!(list.len(), 1);
        assert!(list.find(0x1000).is_some());
        assert!(list.find(0x1000).unwrap().contains(0x2000));
        assert!(list.find(0x6000).is_none());
    }

    #[test]
    fn test_region_list_overlap_rejected() {
        let mut list = RegionList::new();
        let a = VirRegion::new(0x1000, 0x4000, VR_ANON);
        let b = VirRegion::new(0x3000, 0x1000, VR_ANON);
        assert!(list.insert(a).is_none());
        assert!(list.insert(b).is_some()); // rejected — overlaps
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn test_region_list_remove() {
        let mut list = RegionList::new();
        let r = VirRegion::new(0x1000, 0x4000, VR_ANON);
        list.insert(r);
        assert!(list.remove(0x1000).is_some());
        assert_eq!(list.len(), 0);
        assert!(list.remove(0x1000).is_none());
    }

    #[test]
    fn test_region_list_full() {
        let mut list = RegionList::new();
        for i in 0..MAX_REGIONS {
            let r = VirRegion::new((i as u64 + 1) * 0x10000, 0x1000, VR_ANON);
            assert!(list.insert(r).is_none(), "slot {} should accept", i);
        }
        assert!(list.is_full());
        let extra = VirRegion::new(0xFFFFFFF000, 0x1000, VR_ANON);
        assert!(list.insert(extra).is_some()); // full
    }

    #[test]
    fn test_region_flags() {
        assert_eq!(VR_WRITABLE, 0x01);
        assert_eq!(VR_READABLE, 0x02);
        assert_eq!(VR_ANON, 0x04);
        assert_eq!(VR_DIRECT, 0x08);
        assert_eq!(VR_PRESENT, 0x10);
        assert_eq!(VR_DATA, 0x20);
        assert_eq!(VR_FILE, 0x40);
    }

    #[test]
    fn test_region_new_file_fields() {
        let r = VirRegion::new_file(
            0x1000,
            0x4000,
            VR_READABLE | VR_FILE,
            0x100,
            42,
            7,
            0x2000,
            0x10000,
        );
        assert_eq!(r.dev, 0x100);
        assert_eq!(r.ino, 42);
        assert_eq!(r.fd, 7);
        assert_eq!(r.file_offset, 0x2000);
        assert_eq!(r.file_size, 0x10000);
        assert_eq!(r.flags & VR_FILE, VR_FILE);
        // Anonymous regions get neutral file fields.
        let anon = VirRegion::new(0x1000, 0x4000, VR_ANON);
        assert_eq!(anon.fd, -1);
        assert_eq!(anon.file_offset, 0);
        assert_eq!(anon.file_size, 0);
    }

    #[test]
    fn test_region_file_offset_at() {
        let r = VirRegion::new_file(0x1000, 0x4000, VR_FILE, 0, 0, 0, 0x2000, 0x10000);
        // First page of the region maps file bytes at file_offset.
        assert_eq!(r.file_offset_at(0x1000), 0x2000);
        // Page-aligned faults anywhere inside a page resolve to that page.
        assert_eq!(r.file_offset_at(0x1FFF), 0x2000);
        assert_eq!(r.file_offset_at(0x2000), 0x3000);
        assert_eq!(r.file_offset_at(0x4FFF), 0x5000);
    }
}
