//! Copy-on-write bookkeeping across a fork, and the write faults that finish it.
//!
//! The kernel's `vm_paging_fork` builds the child's page tables as a deep copy
//! of the parent's and clears the write bit on both sides' writable user leaves,
//! so after a fork every present page is shared and read-only in both processes.
//! `cow_setup_fork` then takes a reference on each shared frame: that count is
//! what tells a teardown the frame has another owner, and what a write fault
//! consults to decide whether the writer needs a copy.
//!
//! `handle_cow_fault` is the other half. A write fault on a present read-only
//! user page either re-enables writability in place (the frame has one owner, or
//! the region shares it on purpose) or gives the writer a private copy.
//!
//! The copying is lazy on purpose. It used to happen here, for every page of the
//! child, at fork time, which made a fork cost the whole address space: a
//! compositor spawning a client copied 248 MiB in three kernel calls per page and
//! could not do it at all in the 512 MiB the other gates run in.

use crate::vm::pb;
use crate::vm::proc::Vmproc;
use kernel::hal::{pte_is_user, pte_is_writable, pte_set_writable, pte_to_phys, pte_user_owned};
use kernel::pagetable::{PG_P, PG_PS};

/// Flags for reaching a page-table page from VM's own address space.
///
/// `MAP_PRESENT | MAP_USER` alone produces a V|U PTE that SV39 decodes as a table
/// pointer (R=W=X=0), so the PTE reads fault. `MAP_READ` is 0 on x86/aarch64 (no
/// read bit) and PTE_R on RISC-V.
const TABLE_MAP_FLAGS: u64 =
    kernel::pagetable::MAP_PRESENT | kernel::pagetable::MAP_USER | kernel::pagetable::MAP_READ;

/// User-half entries of the top-level table.
const USER_ENTRIES: usize = 256;

const PAGE_SIZE: u64 = 4096;

/// Non-leaf entries per table.
const ENTRIES: usize = 512;

/// Take a reference on every frame the child shares with its parent.
///
/// The kernel has already built the child's tables and made both sides'
/// writable user leaves read-only, so every present leaf the child owns is a
/// shared frame. Nothing is copied here; the first write to a page copies it.
///
/// A frame the refcount window does not cover cannot be recorded as shared, so
/// that page is copied for the child instead, which is what a fork used to do
/// for all of them.
///
/// Called from VM's do_fork after the kernel created the child page table but
/// before the child runs.
pub(crate) unsafe fn cow_setup_fork(child_cr3: u64, child_ep: i32) -> i32 {
    if child_cr3 == 0 {
        return -1;
    }
    // No page tables means nothing is shared: the child's memory is a copy, as
    // on wasm32.
    let Some(top) = kernel::hal::pt_levels().checked_sub(1) else {
        return 0;
    };
    unsafe { register_table(child_cr3, child_cr3, child_ep, top, 0) }
}

/// Walk one level of `cr3`'s user half, registering every shared leaf.
///
/// Returns 0, or -1 when a table page could not be mapped into VM. A table that
/// cannot be read leaves its pages unregistered, which the fault path treats as
/// "not shared" and resolves by copying: safe, at the cost of a copy.
unsafe fn register_table(cr3: u64, phys: u64, ep: i32, level: u32, va_base: u64) -> i32 {
    use crate::vm::{vm_mappage, vm_unmappage};

    let va = vm_mappage(phys, TABLE_MAP_FLAGS);
    if va == 0 {
        return -1;
    }
    let table = va as *const u64;
    let step = PAGE_SIZE << (level * 9);
    let mut result = 0;

    for idx in 0..ENTRIES {
        let entry = unsafe { core::ptr::read(table.add(idx)) };
        if entry & PG_P == 0 {
            continue;
        }
        let entry_va = va_base + (idx as u64) * step;
        if level == 0 {
            if pte_is_user(entry) {
                unsafe { share_or_copy(cr3, ep, entry_va, entry) };
            }
            continue;
        }
        // A block entry above the bottom level is a huge mapping: the kernel's
        // identity map, a device block, or the low-GB alias. None of them maps a
        // frame this process owns, so there is nothing to share.
        if entry & PG_PS != 0 {
            continue;
        }
        if unsafe { register_table(cr3, pte_to_phys(entry), ep, level - 1, entry_va) } != 0 {
            result = -1;
        }
    }

    vm_unmappage(va);
    result
}

/// Record that `ep`'s page at `va` now shares the frame `pte` maps.
unsafe fn share_or_copy(cr3: u64, ep: i32, va: u64, pte: u64) {
    let phys = pte_to_phys(pte);
    if phys == 0 {
        return;
    }
    // Frames the process does not own are not the fork's to free, so a
    // reference here would only pin them: alias and identity leaves
    // (`pte_user_owned` is the same predicate teardown uses) and device frames
    // (`VR_DIRECT`, which teardown skips).
    if !pte_user_owned(pte, va) || in_direct_region(ep, va) {
        return;
    }

    let shared = match pb::pb_find(phys) {
        Some(pfn) => pb::pb_ref(pfn),
        None => match pb::pb_new(phys) {
            Some(pfn) => pb::pb_ref(pfn),
            None => false,
        },
    };
    if shared {
        return;
    }

    // Outside the refcount window: the frame cannot be recorded as shared, so
    // the child gets its own copy and the parent keeps the frame. The child's
    // first write then needs no fault, and neither teardown can free the other
    // side's page.
    let new_phys = crate::vm::vm_alloc_pages(1);
    if new_phys != 0
        && crate::vm::vm_copy_pages(phys, new_phys, 1) == 0
        && crate::vm::vm_map_page_in(cr3, va, new_phys, pte_set_writable(pte)) == 0
    {
        return;
    }
    if new_phys != 0 {
        crate::vm::vm_free_pages(new_phys, 1);
    }
    report_untracked_share();
}

/// True if `va` lies in a `VR_DIRECT` region of `ep`: a device frame, which no
/// allocator owns and no teardown may free.
fn in_direct_region(ep: i32, va: u64) -> bool {
    unsafe { crate::vm::proc::vmproc_lookup(ep) }.is_some_and(|vmp| {
        vmp.vm_regions
            .find(va)
            .is_some_and(|r| r.flags & crate::vm::region::VR_DIRECT != 0)
    })
}

/// Say once that a shared page had to be copied because the refcount table does
/// not cover its frame, so a fork that paid for it is not silent about why.
fn report_untracked_share() {
    #[cfg(target_os = "minix")]
    {
        use core::sync::atomic::{AtomicBool, Ordering};
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            let msg = b"vm: frame refcounts do not cover a forked page; it was copied\n";
            unsafe { minix_rt::write(1, msg.as_ptr(), msg.len()) };
        }
    }
}

/// Handle a COW page fault for process `vmp` at `fault_addr`.
///
/// Called when a write page fault occurs on a present, read-only user page.
///
/// 1. A region that shares its frames on purpose (`VR_SHARED`, `VR_DIRECT`)
///    becomes writable in place: there is nothing to copy, and for a device
///    frame there is no allocator page to free.
/// 2. Otherwise the frame's reference count decides. One reference means this is
///    the only owner and writability can just be restored; more than one means
///    someone else maps the frame and this writer needs a private copy.
///
/// Returns 0 on success, -1 on failure.
pub(crate) fn handle_cow_fault(vmp: &mut Vmproc, fault_addr: u64) -> i32 {
    use crate::vm::proc::vm_get_addrspace;
    let cr3 = unsafe { vm_get_addrspace(vmp.vm_endpoint) };
    if cr3 == 0 {
        return -1;
    }

    let page_addr = fault_addr & !(PAGE_SIZE - 1);

    // A frame shared on purpose rather than copied. VR_SHARED is a MAP_SHARED
    // file page whose frame belongs to the file cache: both processes must keep
    // writing it, so writability is re-enabled in place (the region flags
    // survived the fork through vm_clone's region copy). VR_DIRECT is device
    // memory: copying it would give the process RAM in place of the device.
    if vmp.vm_regions.find(page_addr).is_some_and(|r| {
        r.flags & (crate::vm::region::VR_SHARED | crate::vm::region::VR_DIRECT) != 0
    }) {
        let pte = crate::vm::vm_walk_page(cr3, page_addr);
        if pte != 0 && pte & PG_P != 0 && !pte_is_writable(pte) {
            return crate::vm::vm_map_page_in(
                cr3,
                page_addr,
                pte_to_phys(pte),
                pte_set_writable(pte),
            );
        }
        return 0;
    }

    let pte_val = crate::vm::vm_walk_page(cr3, page_addr);
    if pte_val == 0 || pte_val & PG_P == 0 {
        return -1;
    }
    if pte_is_writable(pte_val) {
        // Already writable — nothing to do.
        return 0;
    }

    let phys_addr = pte_to_phys(pte_val);
    if phys_addr == 0 {
        return -1;
    }

    let pb_idx = match pb::pb_find(phys_addr) {
        Some(idx) => Some(idx),
        None => {
            // No count for this frame. That is what an untracked frame looks
            // like (outside the refcount window, or a page the fork could not
            // reach), and the frame may still belong to another process, so a
            // private copy is the only sound answer: handing the frame out
            // writable here is how a forked child's write reached its parent's
            // heap (.notes/wayland.md).
            let new_phys = crate::vm::vm_alloc_pages(1);
            if new_phys == 0 {
                return -1;
            }
            if crate::vm::vm_copy_pages(phys_addr, new_phys, 1) != 0 {
                crate::vm::vm_free_pages(new_phys, 1);
                return -1;
            }
            if crate::vm::vm_map_page_in(cr3, page_addr, new_phys, pte_set_writable(pte_val)) != 0
            {
                crate::vm::vm_free_pages(new_phys, 1);
                return -1;
            }
            pb::pb_new(new_phys);
            return 0;
        }
    };
    let Some(pb_idx) = pb_idx else {
        return -1;
    };

    // A count of zero means the frame was released under us, which no fault
    // should observe.
    let refcount = pb::pb_count(pb_idx);
    if refcount == 0 {
        return -1;
    }

    if refcount == 1 {
        // The only owner: writability can simply come back.
        return crate::vm::vm_map_page_in(cr3, page_addr, phys_addr, pte_set_writable(pte_val));
    }

    // More than one owner: copy, and move this process's reference to the copy.
    let new_phys = crate::vm::vm_alloc_pages(1);
    if new_phys == 0 {
        return -1;
    }
    if crate::vm::vm_copy_pages(phys_addr, new_phys, 1) != 0 {
        crate::vm::vm_free_pages(new_phys, 1);
        return -1;
    }
    if crate::vm::vm_map_page_in(cr3, page_addr, new_phys, pte_set_writable(pte_val)) != 0 {
        crate::vm::vm_free_pages(new_phys, 1);
        return -1;
    }
    pb::pb_unref(pb_idx);
    pb::pb_new(new_phys);
    0
}
