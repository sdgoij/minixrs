//! VM server — adapted from `minix/servers/vm/main.c`
//!
//! Implements the VM server main loop, message dispatch, and stub handlers
//! for all VM calls. Real implementations come in Phases 6.4+.

#![allow(unused_variables)]
#![allow(dead_code)]

pub mod cow;
pub mod mem;
pub mod pb;
pub mod proc;
pub mod region;

use arch_common::com::{
    NR_VM_CALLS, RS_INIT, RS_PROC_NR, VFS_PROC_NR, VM_BRK, VM_CLEARCACHE, VM_EXEC_NEWMEM, VM_EXIT,
    VM_FORK, VM_GETPHYS, VM_GETREF, VM_GETRUSAGE, VM_INFO, VM_MAP_PHYS, VM_MAPCACHEPAGE, VM_MMAP,
    VM_MUNMAP, VM_NOTIFY_SIG, VM_PAGEFAULT, VM_PROCCTL, VM_QUERY_EXIT, VM_REMAP, VM_REMAP_RO,
    VM_RQ_BASE, VM_RS_MEMCTL, VM_RS_SET_PRIV, VM_RS_UPDATE, VM_SETCACHEPAGE, VM_SHM_UNMAP,
    VM_UNMAP_PHYS, VM_VFS_MMAP, VM_VFS_REPLY, VM_WATCH_EXIT, VM_WILLEXIT, VMCTL_CLEAR_PAGEFAULT,
    VMIW_REGION, VMIW_STATS, VMIW_USAGE, VMPPARAM_CLEAR, VMPPARAM_HANDLEMEM,
};
use arch_common::com::{SUSPEND, is_ipc_notify, is_vfs_fs_transid};
use arch_common::consts::NR_PROCS;
use arch_common::ipc::{EDONTREPLY, Message};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

const OK: i32 = 0;

/// Operation not supported (ENOSYS from MINIX errno.h).
const ENOSYS: i32 = -72;

/// Invalid argument (EINVAL).
const EINVAL: i32 = -5;

/// Resource temporarily unavailable (EAGAIN).
const EAGAIN: i32 = -11;

/// Cannot allocate memory (ENOMEM).
const ENOMEM: i32 = -12;

/// True if `ep` is a valid user-process endpoint of any generation.
///
/// Endpoints encode `(generation << 16) | slot`, so forked children (which
/// get generation 1 via `make_endpoint(new_gen, slot)`) carry values well
/// above `NR_PROCS`. The old `ep >= NR_PROCS` check rejected them.
fn is_user_ep(ep: i32) -> bool {
    kernel::table::is_ok_endpoint(ep) && kernel::table::endpoint_slot(ep) >= 0
}

// ---- Physical memory management via kernel calls ----
// VMINIX: VM manages physical memory through its own allocator.
// In our port, VM uses kernel call 62 (SYS_VM_PAGING) which
// runs in kernel context and accesses the kernel's allocator.
// This avoids the static-data-duplication issue (Blocker 5 class).

const VM_PAGING_CALL: i32 = 62;
const VM_PAGING_SUBCMD_OFF: usize = 8;
const VM_PAGING_COUNT_OFF: usize = 12;
const VM_PAGING_CR3_OFF: usize = 24;
const VM_PAGING_VA_OFF: usize = 32;
const VM_PAGING_PA_OFF: usize = 40;
const VM_PAGING_FLAGS_OFF: usize = 48;

const VM_PAGING_ALLOC: i32 = 1;
const VM_PAGING_FREE: i32 = 2;
const VM_PAGING_MAP: i32 = 3;
const VM_PAGING_UNMAP: i32 = 4;
const VM_PAGING_COPY: i32 = 6;
const VM_PAGING_FORK: i32 = 7;
const VM_PAGING_WALK_PAGE: i32 = 8;
const VM_PAGING_CLEAR: i32 = 9;
const VM_PAGING_MEMSTAT: i32 = 10;

/// Walk the page table identified by `cr3` at virtual address `va`
/// and return the PTE value. Runs in ring 0 via kernel call so it can
/// safely dereference physical page table addresses.
/// Returns the raw 64-bit PTE value, or 0 if the page is not mapped.
pub fn vm_walk_page(cr3: u64, va: u64) -> u64 {
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_WALK_PAGE.to_le_bytes());
    msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&cr3.to_le_bytes());
    msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8].copy_from_slice(&va.to_le_bytes());
    let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
    if r != 0 {
        return 0;
    }
    u64::from_le_bytes(
        msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    )
}

/// Allocate `count` contiguous physical pages via kernel call.
/// Returns the physical address of the first page, or 0 on failure.
pub fn vm_alloc_pages(count: usize) -> u64 {
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_ALLOC.to_le_bytes());
    msg[VM_PAGING_COUNT_OFF..VM_PAGING_COUNT_OFF + 4]
        .copy_from_slice(&(count as i32).to_le_bytes());
    let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
    if r != 0 {
        return 0;
    }
    u64::from_le_bytes(
        msg[VM_PAGING_PA_OFF..VM_PAGING_PA_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    )
}

/// Free `count` contiguous physical pages starting at `pa` via kernel call.
pub fn vm_free_pages(pa: u64, count: usize) -> i32 {
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_FREE.to_le_bytes());
    msg[VM_PAGING_PA_OFF..VM_PAGING_PA_OFF + 8].copy_from_slice(&pa.to_le_bytes());
    msg[VM_PAGING_COUNT_OFF..VM_PAGING_COUNT_OFF + 4]
        .copy_from_slice(&(count as i32).to_le_bytes());
    minix_rt::kernel_call(VM_PAGING_CALL, &mut msg)
}

/// Query the kernel allocator's free physical page count (memstat). VM's
/// own copy of the arch allocator covers the whole identity window and is
/// not authoritative; the kernel's is the real one.
pub fn vm_free_pages_query() -> i32 {
    #[cfg(target_os = "minix")]
    {
        let mut msg = [0u8; 64];
        msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
            .copy_from_slice(&VM_PAGING_MEMSTAT.to_le_bytes());
        let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
        if r != 0 {
            return -1;
        }
        i32::from_le_bytes(
            msg[VM_PAGING_COUNT_OFF..VM_PAGING_COUNT_OFF + 4]
                .try_into()
                .unwrap_or([0; 4]),
        )
    }
    #[cfg(not(target_os = "minix"))]
    {
        0 // host tests have no kernel to query
    }
}

/// VM's own page table root (CR3 physical address).
/// Set during `vm_init_boot` and used by `vm_mappages`/`vm_unmappages`
/// to map physical pages into VM's address space for direct access.
pub static VM_SELF_CR3: AtomicU64 = AtomicU64::new(0);

/// Next free virtual address in VM's address space for temporary mappings.
/// Starts at 0x7F0000000000 — well above the heap (0x3FE00000) and
/// below the kernel half (0xFFFF800000000000). Each call to `vm_find_hole`
/// bumps this by the requested number of pages.
///
/// Base is per-arch: just below the arch's user-space top so the scratch
/// mappings never collide with code/heap/mmap/stack (which all live in the
/// low GiBs). 4 GiB of headroom = 1M transient mappings. The old fixed
/// 0x7F0000000000 was x86-only and above riscv64's SV39 user top
/// (0x4000000000), so every self-map on riscv64 was rejected by the
/// kernel's MAP bounds check and demand-paging bailed. Page-aligned:
/// aarch64's MAX_USER_ADDRESS (2^44 - 1) ends in 0xFFF, so the raw
/// subtraction would hand out misaligned scratch VAs — map_page maps the
/// page containing the VA, and a 4KB read from the returned VA would cross
/// into an unmapped page (observed: vm_destroy's page-table walk faulted
/// and VM deadlocked with PAGEFAULT on hello exit).
static VM_NEXT_MAP_VA: AtomicU64 =
    AtomicU64::new((kernel::pagetable::MAX_USER_ADDRESS - 0x1_0000_0000) & !0xFFF);

/// LIFO of temporary-mapping VAs released by [`vm_unmappage`], so the
/// kernel's intermediate table pages for VM's own address space are reused
/// instead of every map marching `VM_NEXT_MAP_VA` upward through a fresh
/// page-table page. The kernel's `unmap_page` keeps intermediate tables
/// allocated, so without reuse each 512 self-maps leaked one physical PT
/// page (≈1.2 pages per `hello` exec). VM is single-threaded, so the list
/// needs no locking; single-page maps dominate (vm_mappages is unused).
const VM_MAP_VA_FREELIST_CAP: usize = 64;
static VM_MAP_VA_FREELIST: [AtomicU64; VM_MAP_VA_FREELIST_CAP] =
    [const { AtomicU64::new(0) }; VM_MAP_VA_FREELIST_CAP];
static VM_MAP_VA_FREELIST_LEN: AtomicUsize = AtomicUsize::new(0);

const PAGE_SIZE: u64 = 4096;

/// Find a range of `pages` consecutive virtual addresses in VM's own
/// address space for temporary physical page mappings.
///
/// Returns the starting VA. Single-page requests first reuse a VA returned
/// by a previous [`vm_unmappage`] (bounded by [`VM_MAP_VA_FREELIST_CAP`]);
/// multi-page requests (and a full free list) bump `VM_NEXT_MAP_VA`. The
/// 4 GiB headroom below the arch user top bounds the never-wrapping march.
pub fn vm_find_hole(pages: usize) -> u64 {
    if pages == 1 {
        let len = VM_MAP_VA_FREELIST_LEN.load(Ordering::Relaxed);
        if len > 0 {
            let va = VM_MAP_VA_FREELIST[len - 1].load(Ordering::Relaxed);
            VM_MAP_VA_FREELIST_LEN.store(len - 1, Ordering::Relaxed);
            return va;
        }
    }
    let bytes = (pages as u64) * PAGE_SIZE;
    VM_NEXT_MAP_VA.fetch_add(bytes, Ordering::Relaxed)
}

/// Map a single physical page `phys` into VM's address space at a
/// virtual address obtained from `vm_find_hole`, with the given `flags`.
///
/// Returns the virtual address on success, or 0 on failure.
/// After this call, the returned VA can be dereferenced from VM's
/// user-mode context to access the physical page.
pub fn vm_mappage(phys: u64, flags: u64) -> u64 {
    let va = vm_find_hole(1);
    let self_cr3 = VM_SELF_CR3.load(Ordering::Relaxed);
    if self_cr3 == 0 {
        return 0;
    }
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_MAP.to_le_bytes());
    msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&self_cr3.to_le_bytes());
    msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8].copy_from_slice(&va.to_le_bytes());
    msg[VM_PAGING_PA_OFF..VM_PAGING_PA_OFF + 8].copy_from_slice(&phys.to_le_bytes());
    msg[VM_PAGING_FLAGS_OFF..VM_PAGING_FLAGS_OFF + 8].copy_from_slice(&flags.to_le_bytes());
    let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
    if r != 0 {
        return 0;
    }
    va
}

/// Unmap a page from VM's address space at `va` and return it to the
/// temporary-mapping VA pool for reuse.
pub fn vm_unmappage(va: u64) -> i32 {
    let self_cr3 = VM_SELF_CR3.load(Ordering::Relaxed);
    if self_cr3 == 0 {
        return -1;
    }
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_UNMAP.to_le_bytes());
    msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&self_cr3.to_le_bytes());
    msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8].copy_from_slice(&va.to_le_bytes());
    let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
    if r == 0 && va != 0 {
        let len = VM_MAP_VA_FREELIST_LEN.load(Ordering::Relaxed);
        if len < VM_MAP_VA_FREELIST_CAP {
            VM_MAP_VA_FREELIST[len].store(va, Ordering::Relaxed);
            VM_MAP_VA_FREELIST_LEN.store(len + 1, Ordering::Relaxed);
        }
    }
    r
}

/// Map `count` consecutive physical pages into VM's address space.
/// Returns the starting VA, or 0 on failure.
pub fn vm_mappages(phys: u64, count: usize, flags: u64) -> u64 {
    let va = vm_find_hole(count);
    let self_cr3 = VM_SELF_CR3.load(Ordering::Relaxed);
    if self_cr3 == 0 {
        return 0;
    }
    for i in 0..count {
        let mut msg = [0u8; 64];
        msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
            .copy_from_slice(&VM_PAGING_MAP.to_le_bytes());
        msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&self_cr3.to_le_bytes());
        msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8]
            .copy_from_slice(&(va + (i as u64) * PAGE_SIZE).to_le_bytes());
        msg[VM_PAGING_PA_OFF..VM_PAGING_PA_OFF + 8]
            .copy_from_slice(&(phys + (i as u64) * PAGE_SIZE).to_le_bytes());
        msg[VM_PAGING_FLAGS_OFF..VM_PAGING_FLAGS_OFF + 8].copy_from_slice(&flags.to_le_bytes());
        let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
        if r != 0 {
            return 0;
        }
    }
    va
}

/// Unmap `count` pages from VM's address space starting at `va`.
pub fn vm_unmappages(va: u64, count: usize) {
    let self_cr3 = VM_SELF_CR3.load(Ordering::Relaxed);
    if self_cr3 == 0 {
        return;
    }
    for i in 0..count {
        let mut msg = [0u8; 64];
        msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
            .copy_from_slice(&VM_PAGING_UNMAP.to_le_bytes());
        msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&self_cr3.to_le_bytes());
        msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8]
            .copy_from_slice(&(va + (i as u64) * PAGE_SIZE).to_le_bytes());
        let _ = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
    }
}

/// Map a single physical page `pa` at virtual address `va` in the page table
/// identified by `cr3`. The kernel runs in ring 0 and can access all physical
/// memory. The caller must ensure `cr3` is a valid page table root and `va`
/// is within the user address range.
///
/// `flags` should contain permission bits (MAP_USER, MAP_WRITE, etc.) but
/// NOT MAP_PRESENT — the kernel's `map_page` adds PRESENT automatically.
/// Returns 0 on success, negative errno on failure.
pub fn vm_map_page_in(cr3: u64, va: u64, pa: u64, flags: u64) -> i32 {
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_MAP.to_le_bytes());
    msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&cr3.to_le_bytes());
    msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8].copy_from_slice(&va.to_le_bytes());
    msg[VM_PAGING_PA_OFF..VM_PAGING_PA_OFF + 8].copy_from_slice(&pa.to_le_bytes());
    msg[VM_PAGING_FLAGS_OFF..VM_PAGING_FLAGS_OFF + 8].copy_from_slice(&flags.to_le_bytes());
    minix_rt::kernel_call(VM_PAGING_CALL, &mut msg)
}

/// Unmap a single 4K page at `va` from the page table identified by `cr3`.
/// The kernel runs in ring 0 and can access all physical memory. The caller
/// must ensure `cr3` is a valid page table root and `va` is within the user
/// address range. Returns 0 on success, negative errno on failure.
pub fn vm_unmap_page_in(cr3: u64, va: u64) -> i32 {
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_UNMAP.to_le_bytes());
    msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&cr3.to_le_bytes());
    msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8].copy_from_slice(&va.to_le_bytes());
    minix_rt::kernel_call(VM_PAGING_CALL, &mut msg)
}

/// Split any identity huge pages covering `[va, va + pages*4096)` and clear
/// every 4KB entry in the range, so any access (user or kernel mode) faults.
/// Per-process page tables copy the kernel's identity map; without clearing,
/// a lazy region above 1 GiB silently aliases the supervisor identity page
/// (writes land beyond RAM, reads return garbage).
pub fn vm_clear_range(cr3: u64, va: u64, pages: u64) -> i32 {
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_CLEAR.to_le_bytes());
    msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&cr3.to_le_bytes());
    msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8].copy_from_slice(&va.to_le_bytes());
    msg[VM_PAGING_COUNT_OFF..VM_PAGING_COUNT_OFF + 4]
        .copy_from_slice(&(pages as i32).to_le_bytes());
    minix_rt::kernel_call(VM_PAGING_CALL, &mut msg)
}

/// Copy `count` physical pages from `src_pa` to `dst_pa` via kernel call.
/// The kernel runs in ring 0 and copies via the identity map.
pub fn vm_copy_pages(src_pa: u64, dst_pa: u64, count: usize) -> i32 {
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_COPY.to_le_bytes());
    msg[VM_PAGING_PA_OFF..VM_PAGING_PA_OFF + 8].copy_from_slice(&src_pa.to_le_bytes());
    msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&dst_pa.to_le_bytes());
    msg[VM_PAGING_COUNT_OFF..VM_PAGING_COUNT_OFF + 4]
        .copy_from_slice(&(count as i32).to_le_bytes());
    minix_rt::kernel_call(VM_PAGING_CALL, &mut msg)
}

/// Create a child page table by cloning the parent's via kernel call.
/// The kernel (ring 0) walks the parent's page table, allocates a new PML4
/// and intermediate pages, and maps all user pages in the child.
/// Returns the child's CR3 (physical address of PML4), or 0 on failure.
pub fn vm_fork_pagetable(parent_cr3: u64) -> u64 {
    let mut msg = [0u8; 64];
    msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
        .copy_from_slice(&VM_PAGING_FORK.to_le_bytes());
    msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8].copy_from_slice(&parent_cr3.to_le_bytes());
    let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
    if r != 0 {
        return 0;
    }
    u64::from_le_bytes(
        msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    )
}

/// Process flags
#[allow(dead_code)]
const VMF_EXITING: u32 = 0x01;
#[allow(dead_code)]
const VMF_WATCHEXIT: u32 = 0x02;
#[allow(dead_code)]
const VMF_EXIT_QUERY: u32 = 0x04;

/// Reply later via a different message (internal VM status).
#[allow(dead_code)]
const _SUSPEND: i32 = -998;

/// Do not reply at all (internal VM status).
#[allow(dead_code)]
const _EDONTREPLY: i32 = -201;

/// Endpoint representing kernel-originated messages.
#[allow(dead_code)]
const _FROM_KERNEL: i32 = 0x100;

/// Special endpoint to receive from any source.
#[allow(dead_code)]
const _ANY: i32 = 0x0000ffff;

// Call dispatch table

/// A single entry in the VM call dispatch table.
#[derive(Copy, Clone)]
pub struct VmCallEntry {
    pub func: Option<fn(&mut Message) -> i32>,
    pub name: &'static str,
}

struct VmCallsCell(UnsafeCell<[VmCallEntry; NR_VM_CALLS as usize]>);
unsafe impl Sync for VmCallsCell {}
impl VmCallsCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [VmCallEntry {
                func: None,
                name: "",
            }; NR_VM_CALLS as usize],
        ))
    }
    fn get(&self) -> *mut [VmCallEntry; NR_VM_CALLS as usize] {
        self.0.get()
    }
}

/// VM call dispatch table, indexed by `call_number()`.
///
/// Initialized to all-None; populated by `init_vm()`.
static VM_CALLS: VmCallsCell = VmCallsCell::new();

/// Map a message type to a 0-based dispatch table index.
///
/// Returns `-1` if the type is outside the `VM_RQ_BASE` range.
pub fn call_number(c: u32) -> i32 {
    if (VM_RQ_BASE..VM_RQ_BASE + NR_VM_CALLS).contains(&c) {
        (c - VM_RQ_BASE) as i32
    } else {
        -1
    }
}

/// Set a single entry in the dispatch table.
pub fn set_call(msg_type: u32, func: fn(&mut Message) -> i32, name: &'static str) {
    let idx = call_number(msg_type);
    if idx >= 0 {
        unsafe {
            let p = core::ptr::addr_of_mut!((*VM_CALLS.get())[idx as usize]);
            core::ptr::write(
                p,
                VmCallEntry {
                    func: Some(func),
                    name,
                },
            );
        }
    }
}

/// Initialize the VM call dispatch table.
///
/// Must be called once before entering the main loop.
pub fn init_vm() {
    // Zero out the table first
    for entry in unsafe { (*VM_CALLS.get()).iter_mut() } {
        *entry = VmCallEntry {
            func: None,
            name: "",
        };
    }

    set_call(VM_MMAP, do_mmap, "do_mmap");
    set_call(VM_MUNMAP, do_munmap, "do_munmap");
    set_call(VM_MAP_PHYS, do_map_phys, "do_map_phys");
    set_call(VM_UNMAP_PHYS, do_munmap, "do_munmap");

    set_call(VM_EXIT, do_exit, "do_exit");
    set_call(VM_FORK, do_fork, "do_fork");
    set_call(VM_BRK, do_brk, "do_brk");
    set_call(VM_WILLEXIT, do_willexit, "do_willexit");
    set_call(VM_NOTIFY_SIG, do_notify_sig, "do_notify_sig");
    set_call(VM_PROCCTL, do_procctl_notrans, "do_procctl");
    set_call(VM_EXEC_NEWMEM, do_exec_newmem, "do_exec_newmem");

    set_call(VM_VFS_REPLY, do_vfs_reply, "do_vfs_reply");
    set_call(VM_VFS_MMAP, do_vfs_mmap, "do_vfs_mmap");

    set_call(VM_RS_SET_PRIV, do_rs_set_priv, "do_rs_set_priv");
    set_call(VM_RS_UPDATE, do_rs_update, "do_rs_update");
    set_call(VM_RS_MEMCTL, do_rs_memctl, "do_rs_memctl");

    set_call(VM_REMAP, do_remap, "do_remap");
    set_call(VM_REMAP_RO, do_remap, "do_remap");
    set_call(VM_GETPHYS, do_get_phys, "do_get_phys");
    set_call(VM_SHM_UNMAP, do_shm_unmap, "do_shm_unmap");
    set_call(VM_GETREF, do_get_refcount, "do_get_refcount");
    set_call(VM_INFO, do_info, "do_info");
    set_call(VM_QUERY_EXIT, do_query_exit, "do_query_exit");
    set_call(VM_WATCH_EXIT, do_watch_exit, "do_watch_exit");

    set_call(VM_MAPCACHEPAGE, do_mapcache, "do_mapcache");
    set_call(VM_SETCACHEPAGE, do_setcache, "do_setcache");
    set_call(VM_CLEARCACHE, do_clearcache, "do_clearcache");

    set_call(VM_GETRUSAGE, do_getrusage, "do_getrusage");

    // Initialize vmproc entries for all boot processes.
    vm_init_boot();
}

/// Initialize vmproc entries for all boot processes.
///
/// Records the initial data segment boundaries so that do_brk can
/// track per-process heap state. The initial brk starts at the
/// pre-allocated heap base (0x3FE00000) that the kernel maps during boot.
fn vm_init_boot() {
    use arch_common::consts::NR_PROCS;

    // Query the kernel for each process slot via SYS_VM_PAGING / VM_PAGING_QUERY_PROC.
    // The kernel has the real Proc table; VM cannot access it directly because
    // the kernel crate's static data becomes a separate BSS copy in VM's binary.
    // This matches MINIX's approach: VM uses sys_getkinfo to retrieve boot info.
    const VM_PAGING_CALL: i32 = 62;
    const VM_PAGING_QUERY_PROC: i32 = 5;
    const VM_PAGING_SUBCMD_OFF: usize = 8;
    const VM_PAGING_COUNT_OFF: usize = 12;
    // Output offsets (match do_vm_paging_handler):
    //   VM_PAGING_CR3_OFF (24) = in_use (u64, 0 or 1)
    //   VM_PAGING_VA_OFF  (32) = endpoint (u64)
    //   VM_PAGING_PA_OFF  (40) = CR3 (u64)
    const VM_PAGING_INUSE_OFF: usize = 24;
    const VM_PAGING_EP_OFF: usize = 32;
    const VM_PAGING_CR3_OFF: usize = 40;

    for slot in 0..NR_PROCS {
        let mut msg = [0u8; 64];
        msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
            .copy_from_slice(&VM_PAGING_QUERY_PROC.to_le_bytes());
        msg[VM_PAGING_COUNT_OFF..VM_PAGING_COUNT_OFF + 4]
            .copy_from_slice(&(slot as i32).to_le_bytes());

        let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
        if r != 0 {
            continue;
        }

        let in_use = u64::from_le_bytes(
            msg[VM_PAGING_INUSE_OFF..VM_PAGING_INUSE_OFF + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );
        if in_use == 0 {
            continue;
        }

        let ep = u64::from_le_bytes(
            msg[VM_PAGING_EP_OFF..VM_PAGING_EP_OFF + 8]
                .try_into()
                .unwrap_or([0; 8]),
        ) as i32;
        let cr3 = u64::from_le_bytes(
            msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );

        // Save VM's own CR3 for self-mapping physical pages.
        // The kernel returns the full endpoint (gen << 16 + slot),
        // so extract the slot portion, not the raw endpoint value.
        let slot = kernel::table::endpoint_slot(ep);
        if slot == arch_common::com::VM_PROC_NR && cr3 != 0 {
            VM_SELF_CR3.store(cr3, Ordering::Relaxed);
        }

        if let Some(vmp) = unsafe { proc::vmproc_alloc(ep) } {
            vmp.vm_region_top = kernel::hal::user_heap_base();
            vmp.vm_pml4_phys = cr3;
            // Create a data segment region for the pre-allocated brk heap.
            let data_region = region::VirRegion::new(
                kernel::hal::user_heap_base(),
                0x100000u64, // 1 MB
                region::VR_READABLE
                    | region::VR_WRITABLE
                    | region::VR_ANON
                    | region::VR_PRESENT
                    | region::VR_DATA,
            );
            vmp.vm_regions.insert(data_region);
        }
    }
}

// Server main loop

/// VM server main entry point.
///
/// Initializes the call table, boots vmproc table, and enters the
/// message dispatch loop.
pub fn vm_main() {
    // Initialize the PhysicalAllocator for this server's copy of the
    // kernel crate (which contains arch-specific code). Each server
    // binary has its own copy; the kernel's copy was init'd in kmain.
    #[cfg(target_os = "minix")]
    unsafe {
        // Physical memory range for page table allocation.
        // On x86_64: RAM is identity-mapped at 0x400000-0x40000000 (1 GB);
        // stop below the ACPI/reserved window at the top of the range.
        // On RISC-V (QEMU virt): RAM starts at 0x80000000.
        #[cfg(target_arch = "x86_64")]
        let range = (0x400000u64, 0x3FA00000u64);
        #[cfg(target_arch = "riscv64")]
        let range = (0x81000000u64, 0x0F000000u64);
        #[cfg(target_arch = "aarch64")]
        let range = (0x41000000u64, 0x0F000000u64); // 256MB RAM: 16MB kernel, 240MB free
        kernel::hal::init_phys_alloc(range.0, range.1);
    }
    // On aarch64 the server-side copy of the arch allocator is never
    // initialized (init_phys_alloc is a no-op), yet the teardown walks
    // (free_address_space / free_user_range) consult the low-GB alias
    // window to avoid freeing shared alias frames. The window is defined by
    // the KERNEL's allocator (the per-process tables are built from it), so
    // query it via kernel call 62 / VM_PAGING_MEMINFO and cache it.
    #[cfg(all(target_os = "minix", target_arch = "aarch64"))]
    {
        const VM_PAGING_CALL: i32 = 62;
        const VM_PAGING_MEMINFO: i32 = 11;
        const VM_PAGING_SUBCMD_OFF: usize = 8;
        const VM_PAGING_CR3_OFF: usize = 24;
        const VM_PAGING_VA_OFF: usize = 32;
        let mut msg = [0u8; 64];
        msg[VM_PAGING_SUBCMD_OFF..VM_PAGING_SUBCMD_OFF + 4]
            .copy_from_slice(&VM_PAGING_MEMINFO.to_le_bytes());
        let r = minix_rt::kernel_call(VM_PAGING_CALL, &mut msg);
        if r == 0 {
            let base = u64::from_le_bytes(
                msg[VM_PAGING_CR3_OFF..VM_PAGING_CR3_OFF + 8]
                    .try_into()
                    .unwrap_or([0; 8]),
            );
            let usable = u64::from_le_bytes(
                msg[VM_PAGING_VA_OFF..VM_PAGING_VA_OFF + 8]
                    .try_into()
                    .unwrap_or([0; 8]),
            );
            kernel::hal::set_alias_window(base, usable);
        }
    }
    init_vm();

    #[cfg(target_os = "minix")]
    {
        const RECEIVE_CALL: u64 = 47;
        #[allow(dead_code)]
        const SEND_CALL: u64 = 46;
        const ANY: i32 = 0x0000ffff;

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            // Receive a message from any sender.
            // syscall2 returns the sender's endpoint via write_retval (RAX).
            let src = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64)
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            // Dispatch the call. dispatch_message handles setting msg.m_type
            // to the result and (via ipc_send_stub) sending the reply.
            // The stub is a no-op; the main loop sends the actual reply via SEND.
            let status = dispatch_message(&mut msg, 0);

            // Send the reply if the handler didn't request no-reply.
            if status != SUSPEND && status != EDONTREPLY {
                msg.m_type = status;
                unsafe {
                    minix_rt::syscall2(
                        minix_rt::SENDNB_CALL,
                        src_ep as u64,
                        &mut msg as *mut Message as u64,
                    );
                }
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // No-op on host builds — dispatch is tested directly
    }
}

/// Dispatch a single message through the VM call table.
///
/// Handles special message types (VM_PAGEFAULT, RS_INIT, VFS transactions)
/// and normal dispatch through `VM_CALLS`. Repies to the caller via `ipc_send()`.
///
/// Returns the result code (for testing).
pub fn dispatch_message(msg: &mut Message, ipc_status: i32) -> i32 {
    // Check for notifications.
    // The ipc_status parameter is not available (main loop passes 0),
    // so also check m_type directly for NOTIFY_MESSAGE (-10).
    if is_ipc_notify(ipc_status) || msg.m_type == arch_common::com::NOTIFY_MESSAGE as i32 {
        sef_signal_handler();
        return EDONTREPLY;
    }

    let call_nr = msg.m_type as u32;

    // Handle special message types.
    if call_nr == VM_PAGEFAULT {
        // Handle page fault: allocate page, map it, clear fault.
        do_pagefaults(msg);
        // The faulting process is resumed via sys_vmctl(CLEAR_PAGEFAULT)
        // inside do_pagefaults. No reply to the kernel is needed.
        return EDONTREPLY;
    }

    if call_nr == RS_INIT {
        // TODO: Phase 13 — SEF init callback.
        msg.m_type = OK;
        let _ = ipc_send_stub(msg.m_source, msg);
        return OK;
    }

    if is_vfs_fs_transid(call_nr) {
        // TODO: Phase 13 — VFS transaction dispatch.
        msg.m_type = ENOSYS;
        let _ = ipc_send_stub(msg.m_source, msg);
        return ENOSYS;
    }

    // Normal dispatch through call table.
    let idx = call_number(call_nr);
    let result = if idx >= 0 {
        let entry = unsafe { &(*VM_CALLS.get())[idx as usize] };
        if let Some(func) = entry.func {
            func(msg)
        } else {
            ENOSYS
        }
    } else {
        ENOSYS
    };

    // Reply unless handler requested no reply.
    if result != SUSPEND && result != EDONTREPLY {
        msg.m_type = result;
        let _ = ipc_send_stub(msg.m_source, msg);
    }

    result
}

/// Stub for `ipc_send` — sends a message to a process.
///
/// Real implementation in Phase 13: calls kernel IPC send.
fn ipc_send_stub(_dest: i32, _msg: &Message) -> Result<(), i32> {
    // TODO: Phase 13 — actual IPC send via kernel.
    Ok(())
}

/// Execute boot process (stub).
///
/// Loads and starts the initial user-space process during boot.
/// Called once during system initialization after the VM server starts.
pub fn exec_bootproc() {
    // TODO: Phase 7 — execute boot process with ELF loading
}

/// SEF signal handler callback.
///
/// Handles kernel signals delivered to the VM server.
/// Iterates all process slots to find pending page faults
/// (stored by the kernel's #PF handler) and processes them.
pub fn sef_signal_handler() {
    // Process pending page faults by querying the kernel via
    // SYS_VMCTL(VMCTL_MEMREQ_GET). Iterate all active Vmproc
    // entries and check each one for pending fault data.
    unsafe {
        proc::for_each_active_vmproc(|vmp| {
            let ep = vmp.vm_endpoint;
            if let Ok((addr, error_code)) = minix_rt::sys_vmctl_memreq_get(ep) {
                handle_pagefault_for(ep, addr, error_code);
            }
        });
    }
}

// Page fault handling (Phase 6.9 — port of pagefaults.c)

// PFERR_* constants from C's VPF_FLAGS decoding
#[allow(dead_code)]
const PFERR_NOPAGE: u32 = 0;
#[allow(dead_code)]
const PFERR_WRITE: u32 = 0x01;
#[allow(dead_code)]
const PFERR_PROT: u32 = 0x02;
#[allow(dead_code)]
const PFERR_READ: u32 = 0x04;

// Signal numbers
#[allow(dead_code)]
const SIGSEGV: i32 = 11;
#[allow(dead_code)]
const SIGABRT: i32 = 6;

/// Handle a page fault forwarded from the kernel.
///
/// The kernel delivers VM_PAGEFAULT messages when a user-space process
/// accesses an unmapped virtual address. VM must:
/// 1. Look up the faulting address in the process's region list
/// 2. If the region exists and access is valid, allocate + map a page
/// 3. If invalid (unmapped address, write to read-only), send SIGSEGV
///
/// Message format:
///   m9.m9l1 = faulting virtual address (VPF_ADDR)
///   m9.m9l2 = fault flags (VPF_FLAGS: PFERR_WRITE, PFERR_READ, etc.)
pub fn do_pagefaults(msg: &mut Message) {
    let ep = msg.m_source;
    let addr = unsafe { msg.m_payload.m9.m9l1 } as u64;
    let flags = unsafe { msg.m_payload.m9.m9l2 } as u32;
    handle_pagefault_for(ep, addr, flags);
}

/// Core page fault handler shared by message dispatch and notification path.
///
/// Processes a page fault for `ep` at `addr` with the given `error_code`
/// (the CPU page fault error code bits).
fn handle_pagefault_for(ep: i32, addr: u64, error_code: u32) {
    let is_write = error_code & PFERR_WRITE != 0;
    let is_prot_fault = error_code & PFERR_PROT != 0;

    // Validate the endpoint via the Vmproc table.
    let vmp = match unsafe { proc::vmproc_lookup(ep) } {
        Some(vmp) => vmp,
        None => {
            sys_kill(ep, SIGSEGV);
            unsafe {
                mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
            }
            return;
        }
    };

    // Use vm_get_addrspace which prefers the kernel's authoritative CR3
    // (updated by the exec path) over Vmproc's potentially stale value.
    let cr3 = unsafe { proc::vm_get_addrspace(ep) };
    if cr3 == 0 {
        sys_kill(ep, SIGSEGV);
        unsafe {
            mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
        }
        return;
    }

    // Handle COW faults via PTE walk (not region lookup). After an exec,
    // VM's region cache is stale (init's pre-exec layout) and doesn't reflect
    // the new binary's mappings. Walking the PTE directly is authoritative.
    if is_prot_fault && is_write {
        let pte_val = crate::vm::vm_walk_page(cr3, addr);
        // Only a present *user* page with a write fault is a COW candidate.
        // A present page without the user bit (e.g. the kernel's
        // supervisor-only identity huge pages the exec'd page table copies
        // on RISC-V) falls through to the normal demand-paging path below,
        // which maps a fresh user page over it. The user/writable checks go
        // through HAL helpers because aarch64 encodes access in AP[2:1], not
        // in separate U/RW bits (a COW leaf is AP=11 — EL0-accessible,
        // read-only).
        if pte_val & kernel::pagetable::PG_P != 0
            && kernel::hal::pte_is_user(pte_val)
            && !kernel::hal::pte_is_writable(pte_val)
        {
            // Present, user, read-only with write fault → COW.
            if cow::handle_cow_fault(vmp, addr) != 0 {
                sys_kill(ep, SIGSEGV);
            }
            unsafe {
                mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
            }
            return;
        }
        // Not a COW candidate. Two cases land here and must fall through
        // to the demand-paging path below rather than SIGSEGV:
        //  1. Not-present pages: the RISC-V trap always sets the "present"
        //     bit in the synthesized error code, so not-present store
        //     faults arrive with is_prot_fault set (and aarch64's raw ESR
        //     sets both flag bits for leaf-level faults).
        //  2. Supervisor-only identity pages the exec'd page table copies
        //     on RISC-V (and the low-GB alias on aarch64): writing the
        //     mmap heap at mmap_base() hits one of these blocks.
        // The region lookup below then maps a fresh user page over the
        // address when a region covers it, and SIGSEGVs otherwise.
    }

    // Non-COW fault: find region for demand paging.
    let region = vmp.vm_regions.find(addr);
    let region = match region {
        Some(r) => r,
        None => {
            sys_kill(ep, SIGSEGV);
            unsafe {
                mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
            }
            return;
        }
    };

    // File-backed region: demand the page from the file instead of the
    // zero-fill heap path below. The first fault after exec pre-faults the
    // non-executable file regions (rodata/data) so VFS's kernel-mode copies
    // of the image (vircopy of user buffers) hit present pages.
    if region.flags & region::VR_FILE != 0 {
        if vmp.prefault_exec {
            vmp.prefault_exec = false;
            // The pre-fault covers the faulting page when it lies in a
            // non-executable region; map_file_page skips present pages, so
            // the call below only maps the page when it is still absent
            // (a VR_EXEC text page, which the pre-fault leaves lazy).
            if !prefault_vfs_file_regions(ep, vmp, cr3) {
                return;
            }
        }
        if map_file_page(ep, vmp, cr3, addr) {
            unsafe {
                mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
            }
        }
        return;
    }

    // Demand-paging: allocate a physical page, zero-fill, and map it.
    // Physical memory is managed by the kernel; route through kernel call 62
    // (VM_PAGING) rather than calling kernel functions directly, which would
    // operate on a duplicated copy of the kernel allocator inside VM's binary.
    let page_size: u64 = 4096;
    let page_addr = addr & !(page_size - 1);
    let pa = crate::vm::vm_alloc_pages(1);
    if pa == 0 {
        sys_kill(ep, SIGSEGV);
        unsafe {
            mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
        }
        return;
    }

    // Zero-fill the page via a temporary mapping in VM's own address space.
    let tmp_va = crate::vm::vm_mappage(
        pa,
        kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE,
    );
    if tmp_va == 0 {
        crate::vm::vm_free_pages(pa, 1);
        sys_kill(ep, SIGSEGV);
        unsafe {
            mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
        }
        return;
    }
    unsafe {
        core::ptr::write_bytes(tmp_va as *mut u8, 0, page_size as usize);
    }
    crate::vm::vm_unmappage(tmp_va);

    let mut pt_flags = kernel::pagetable::MAP_USER;
    if region.flags & region::VR_WRITABLE != 0 {
        pt_flags |= kernel::pagetable::MAP_WRITE;
    }
    if region.flags & region::VR_EXEC != 0 {
        pt_flags |= kernel::pagetable::MAP_EXEC;
    }

    if crate::vm::vm_map_page_in(cr3, page_addr, pa, pt_flags) == 0 {
        pb::pb_new(pa);
        if let Some(vmp) = unsafe { proc::vmproc_lookup(ep) }
            && let Some(r) = vmp.vm_regions.find_mut(page_addr)
        {
            r.add_page(page_addr, pa);
        }
        unsafe {
            mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
        }
    } else {
        crate::vm::vm_free_pages(pa, 1);
        sys_kill(ep, SIGSEGV);
        unsafe {
            mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
        }
    }
}

/// Map one page of a file-backed region: allocate a fresh page, map it
/// writable at the VA, ask VFS to read the file block into it (FDIO — the
/// magic grant write lands in the target's CR3), then downgrade to the
/// region's permissions. Pages at or past EOF (holes, `.bss` tails) stay
/// zero-filled; the FDIO read only fills the in-file portion. Shared by the
/// demand-fault path and the exec pre-fault; the caller resolves the fault
/// with VMCTL_CLEAR_PAGEFAULT on success. Returns false if the process was
/// SIGSEGV'd (kill + fault-clear already done).
fn map_file_page(ep: i32, vmp: &mut proc::Vmproc, cr3: u64, addr: u64) -> bool {
    let page_size: u64 = 4096;
    let page_addr = addr & !(page_size - 1);

    // The exec pre-fault may have mapped this page already (the faulting
    // page lies in a non-executable region it covered); skip it so the page
    // isn't double-allocated. In the plain demand-fault path only PROT
    // faults arrive with a present page, which map as before.
    if crate::vm::vm_walk_page(cr3, page_addr) & kernel::pagetable::PG_P != 0 {
        return true;
    }

    // Extract the fields we need up front so the region borrow ends before
    // the blocking FDIO request.
    let (fd, file_off, file_size, writable, exec) = {
        let region = match vmp.vm_regions.find_mut(page_addr) {
            Some(r) => r,
            None => {
                sys_kill(ep, SIGSEGV);
                unsafe {
                    mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
                }
                return false;
            }
        };
        (
            region.fd,
            region.file_offset_at(page_addr),
            region.file_size,
            region.flags & region::VR_WRITABLE != 0,
            region.flags & region::VR_EXEC != 0,
        )
    };

    let pa = crate::vm::vm_alloc_pages(1);
    if pa == 0 {
        sys_kill(ep, SIGSEGV);
        unsafe {
            mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
        }
        return false;
    }

    // Zero-fill the page via a temporary mapping in VM's own address space.
    let tmp_va = crate::vm::vm_mappage(
        pa,
        kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE,
    );
    if tmp_va == 0 {
        crate::vm::vm_free_pages(pa, 1);
        sys_kill(ep, SIGSEGV);
        unsafe {
            mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
        }
        return false;
    }
    unsafe {
        core::ptr::write_bytes(tmp_va as *mut u8, 0, page_size as usize);
    }
    crate::vm::vm_unmappage(tmp_va);

    // Map writable so MFS's SAFECOPYTO (through the target's CR3) lands in
    // the page, then downgrade to the region's permissions below.
    if crate::vm::vm_map_page_in(
        cr3,
        page_addr,
        pa,
        kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE,
    ) != 0
    {
        crate::vm::vm_free_pages(pa, 1);
        sys_kill(ep, SIGSEGV);
        unsafe {
            mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
        }
        return false;
    }

    if file_off < file_size {
        let mut reply = [0u8; 64];
        let r = vfs_request_sync(
            arch_common::com::VMVFSREQ_FDIO as i32,
            fd,
            ep,
            file_off,
            page_addr,
            page_size as u32,
            &mut reply,
        );
        if r != 0 {
            let _ = crate::vm::vm_unmap_page_in(cr3, page_addr);
            crate::vm::vm_free_pages(pa, 1);
            sys_kill(ep, SIGSEGV);
            unsafe {
                mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
            }
            return false;
        }
        // The region's in-file end can fall mid-page (a segment whose
        // filesz is not page-aligned, or a file shorter than the mapped
        // view): FDIO filled the whole page from the file, whose tail
        // sections (.strtab, next segment) are not part of the memory
        // image. Zero everything past the in-file end.
        let in_file = file_size - file_off;
        if in_file < page_size {
            let tmp_va = crate::vm::vm_mappage(
                pa,
                kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE,
            );
            if tmp_va == 0 {
                let _ = crate::vm::vm_unmap_page_in(cr3, page_addr);
                crate::vm::vm_free_pages(pa, 1);
                sys_kill(ep, SIGSEGV);
                unsafe {
                    mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
                }
                return false;
            }
            unsafe {
                core::ptr::write_bytes(
                    (tmp_va + in_file) as *mut u8,
                    0,
                    (page_size - in_file) as usize,
                );
            }
            crate::vm::vm_unmappage(tmp_va);
        }
    }

    // Downgrade to the region's permissions (read-only exec pages must not
    // stay writable; writable MAP_PRIVATE segments keep MAP_WRITE; exec
    // segments keep the execute bit, which SV39 requires for instruction
    // fetch — a text page without X faults forever on RISC-V).
    let mut pt_flags = kernel::pagetable::MAP_USER;
    if writable {
        pt_flags |= kernel::pagetable::MAP_WRITE;
    }
    if exec {
        pt_flags |= kernel::pagetable::MAP_EXEC;
    }
    let _ = crate::vm::vm_map_page_in(cr3, page_addr, pa, pt_flags);

    pb::pb_new(pa);
    if let Some(vmp) = unsafe { proc::vmproc_lookup(ep) }
        && let Some(r) = vmp.vm_regions.find_mut(page_addr)
    {
        r.add_page(page_addr, pa);
    }
    true
}

/// Pre-fault every non-executable file region of an exec'd image (rodata,
/// data, bss) so VFS's kernel-mode copies of the image (vircopy of user
/// buffers, e.g. the shell's `# ` prompt in rodata) hit present pages.
/// Text stays lazy — its faults are user-mode instruction fetches, which
/// have a working resume path. Runs once, on the first file-region fault
/// after exec. Returns false if a page failed and the process was
/// SIGSEGV'd.
fn prefault_vfs_file_regions(ep: i32, vmp: &mut proc::Vmproc, cr3: u64) -> bool {
    let page_size: u64 = 4096;
    let mut ranges: [(u64, u64); region::MAX_REGIONS] = [(0, 0); region::MAX_REGIONS];
    let mut n = 0usize;
    for r in vmp.vm_regions.regions.iter().flatten() {
        if r.flags & region::VR_FILE != 0
            && r.flags & region::VR_EXEC == 0
            && n < region::MAX_REGIONS
        {
            ranges[n] = (r.vaddr, r.end());
            n += 1;
        }
    }
    for &(start, end) in &ranges[..n] {
        let mut page = start;
        while page < end {
            if !map_file_page(ep, vmp, cr3, page) {
                return false;
            }
            page += page_size;
        }
    }
    true
}

/// Send a signal to a process via the kernel.
///
/// Validates endpoint and signal number, sets SIG_PENDING+SIGNALED flags,
/// and enqueues the process for signal delivery.
pub fn sys_kill(ep: i32, sig: i32) -> i32 {
    if !(0..=127).contains(&sig) {
        return EINVAL;
    }
    let slot = kernel::table::endpoint_slot(ep);
    // cause_sig (not send_sig): the target is a user process, so the
    // signal must go through its signal manager — set RTS_SIGNALED |
    // RTS_SIG_PENDING and notify PM (SIGKSIG). send_sig only records
    // the bit in s_sig_pending and notifies SYSTEM, so a SIGSEGV'd
    // process would keep running (and re-faulting) forever.
    #[cfg(target_os = "minix")]
    unsafe {
        kernel::system::cause_sig(slot, sig);
    }
    OK
}

/// Clear the page fault flag on a process, reactivating it.
pub fn clear_pagefault(ep: i32) -> i32 {
    // Forward VMCTL_CLEAR_PAGEFAULT to the kernel: the kernel's
    // do_vmctl_handler clears RTS_PAGEFAULT on the real Proc struct and
    // re-enqueues the faulting process if it becomes runnable.
    unsafe { mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0) }
}

// Phase 6.10 — Shared memory (shm.c)

/// Handle VM_SHM_UNMAP — clear matching shared memory regions.
fn do_shm_unmap(msg: &mut Message) -> i32 {
    let ep = msg.m_source;
    if !is_user_ep(ep) {
        return EINVAL;
    }
    let _addr = unsafe { msg.m_payload.m1.m1i1 } as u64;
    // TODO: walk region array and clear matching shared memory entries
    OK
}

/// Handle IPC_SHMGET — shared memory get request (stub).
#[allow(dead_code)]
fn do_shm_get(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

/// Handle IPC_SHMAT — shared memory attach (stub).
#[allow(dead_code)]
fn do_shm_at(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

// Phase 6.11 — Remap operations (mmap.c)

/// Handle VM_REMAP / VM_REMAP_RO — remap a shared region.
///
/// Validates endpoints and source address/size, rounds size to page boundary,
/// returns the mapped virtual address in m1i1.
fn do_remap(msg: &mut Message) -> i32 {
    let _caller = msg.m_source;
    let dest_ep = unsafe { msg.m_payload.m1.m1i1 };
    let src_ep = unsafe { msg.m_payload.m1.m1i2 };
    let src_addr = unsafe { msg.m_payload.m1.m1i3 } as u64;
    let mut _size = unsafe { msg.m_payload.m1.m1i4 } as usize;

    // Validate endpoints
    if !is_user_ep(dest_ep) {
        return EINVAL;
    }
    if !is_user_ep(src_ep) {
        return EINVAL;
    }

    // Round size to page boundary
    let page_size: usize = 4096;
    if !_size.is_multiple_of(page_size) {
        _size += page_size - (_size % page_size);
    }

    if _size == 0 {
        return EINVAL;
    }

    // Get the destination process's CR3.
    let dst_cr3 = unsafe { proc::vm_get_addrspace(dest_ep) };
    if dst_cr3 == 0 {
        return EINVAL;
    }

    // Look up the source physical address by walking its page table.
    let src_cr3 = unsafe { proc::vm_get_addrspace(src_ep) };
    if src_cr3 == 0 {
        return EINVAL;
    }

    // Walk the source page table to get the physical address of src_addr.
    // Via the kernel (ring 0): a direct `kernel::pagetable::walk` is fine,
    // but `map_page`/`tlb_flush_page` below execute `invlpg`, which is
    // privileged and #GPs from VM's user context.
    let src_pa = crate::vm::vm_walk_page(src_cr3, src_addr) & 0x000FFFFFFFFFF000;
    if src_pa == 0 {
        return EINVAL;
    }

    // Map the source physical page into the destination at the same
    // virtual address (standard shared-memory remap).
    let flags =
        kernel::pagetable::MAP_PRESENT | kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE;
    if crate::vm::vm_map_page_in(dst_cr3, src_addr, src_pa, flags) != 0 {
        return EINVAL;
    }

    // Return the mapped virtual address.
    msg.m_payload.m1.m1i1 = src_addr as i32;
    OK
}

/// Handle VM_MAP_PHYS — map physical memory into a process.
///
/// Validates length and target endpoint, rounds addresses to page boundaries,
/// and maps the physical page into the target process's address space.
fn do_map_phys(msg: &mut Message) -> i32 {
    let target = unsafe { msg.m_payload.m1.m1i1 };
    let len = unsafe { msg.m_payload.m1.m1i2 };
    let phys = unsafe { msg.m_payload.m1.m1i3 } as u64;

    if len <= 0 {
        return EINVAL;
    }

    let actual_target = if target == -1 { msg.m_source } else { target };
    if !is_user_ep(actual_target) {
        return EINVAL;
    }

    // Round len to page boundary.
    let page_size: u64 = 4096;
    let rounded_len = if !(len as u64).is_multiple_of(page_size) {
        (len as u64) + page_size - ((len as u64) % page_size)
    } else {
        len as u64
    };

    // Get the target process's CR3.
    let cr3 = unsafe { proc::vm_get_addrspace(actual_target) };
    if cr3 == 0 {
        return EINVAL;
    }

    // The caller provides the desired virtual address (stored in m1i4 or
    // uses an internal VM allocation). For now, use the same virtual address
    // as the physical address (identity mapping).
    let vaddr = phys;
    let flags =
        kernel::pagetable::MAP_PRESENT | kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE;

    let mapped_vaddr = vaddr;
    for offset in (0..rounded_len).step_by(page_size as usize) {
        // Via the kernel (ring 0): `kernel::pagetable::map_page` calls
        // `tlb_flush_page` (`invlpg`), which is privileged and #GPs from
        // VM's user context.
        if crate::vm::vm_map_page_in(cr3, vaddr + offset, phys + offset, flags) != 0 {
            return EINVAL;
        }
    }

    msg.m_payload.m1.m1i1 = mapped_vaddr as i32;
    OK
}

/// Handle VM_GETPHYS — translate virtual address to physical address.
///
/// Validates endpoint, walks the page table to find the physical address,
/// returns it in m1i1.
fn do_get_phys(msg: &mut Message) -> i32 {
    let target = unsafe { msg.m_payload.m1.m1i1 };
    let addr = unsafe { msg.m_payload.m1.m1i2 } as u64;

    if !is_user_ep(target) {
        return EINVAL;
    }

    let cr3 = unsafe { proc::vm_get_addrspace(target) };
    if cr3 == 0 {
        return EINVAL;
    }

    // Walk the page table to get the physical address of the given
    // virtual address (via the kernel).
    let pte = crate::vm::vm_walk_page(cr3, addr);
    let pa = pte & 0x000FFFFFFFFFF000;
    msg.m_payload.m1.m1i1 = pa as i32;
    OK
}

/// Handle VM_GETREF — get reference count of a region.
///
/// Validates endpoint, walks the grant table to find matching entries.
/// Returns refcount in m1i1.
fn do_get_refcount(msg: &mut Message) -> i32 {
    let target = unsafe { msg.m_payload.m1.m1i1 };
    let addr = unsafe { msg.m_payload.m1.m1i2 } as u64;

    if !is_user_ep(target) {
        return EINVAL;
    }

    // Walk the grant table looking for entries mapped by this target
    // that involve the given virtual address.
    let mut refcount = 0;
    unsafe {
        let tables = mem::GRANT_TABLES.get();
        for i in 0..mem::MAX_ENDPOINTS {
            for grant in (*tables)[i].iter() {
                if grant.g_grantor == target && grant.g_vaddr == addr && grant.g_grantor != 0 {
                    refcount += 1;
                }
            }
        }
    }

    if refcount > 0 {
        refcount
    } else {
        // Fall back to returning 1 (matched) for any valid target,
        // same behavior as the C stub when no region walk is available.
        1
    }
}

/// Handle VM_MUNMAP / VM_UNMAP_PHYS — unmap memory regions.
///
/// Message format (from minix-std vmem.rs):
///   raw[12..20] = length (u64)
///   raw[20..28] = address (u64)
///
/// Removes the region from tracking, unmaps physical pages from
/// the page table, and frees physical pages.
fn do_munmap(msg: &mut Message) -> i32 {
    let ep = msg.m_source;
    if !is_user_ep(ep) {
        return EINVAL;
    }

    let raw = unsafe { &msg.m_payload.raw };
    let length = u64::from_ne_bytes(raw[MMAP_LEN..MMAP_LEN + 8].try_into().unwrap_or([0; 8]));
    let addr = u64::from_ne_bytes(raw[MMAP_ADDR..MMAP_ADDR + 8].try_into().unwrap_or([0; 8]));

    if length == 0 || !addr.is_multiple_of(4096) {
        return EINVAL;
    }

    let len_aligned = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let end_addr = addr + len_aligned;
    if end_addr > kernel::pagetable::MAX_USER_ADDRESS || end_addr < addr {
        return EINVAL;
    }

    let cr3 = unsafe { proc::vm_get_addrspace(ep) };
    if cr3 == 0 {
        return EINVAL;
    }

    // Find and remove the region at this address.
    unsafe {
        if let Some(vmp) = proc::vmproc_lookup(ep) {
            // Remove the region from tracking; close a file region's vmfd
            // once no other process's region references the same file.
            if let Some(removed) = vmp.vm_regions.remove(addr)
                && removed.flags & region::VR_FILE != 0
            {
                fdref_close_if_unused(removed.dev, removed.ino, removed.fd, -1);
            }
        }
    }

    // Return the region's physical pages to the allocator BEFORE unmapping
    // the PTEs: free_user_range walks the page table to find the frames
    // (COW-shared frames and shared identity leaves are kept by the walk),
    // so the entries must still be present when it runs. The unmap loop
    // below then clears the PTEs so a later fault cannot map a freed frame.
    unsafe {
        crate::vm::proc::free_user_range(cr3, addr, addr + len_aligned);
    }

    // Unmap pages from the page table, one at a time via the kernel (ring
    // 0). `kernel::pagetable::unmap_range` calls `tlb_flush_page`
    // (`invlpg`), which is privileged and #GPs from VM's user context.
    let mut va = addr;
    while va < addr + len_aligned {
        let _ = crate::vm::vm_unmap_page_in(cr3, va);
        va += PAGE_SIZE;
    }

    // Set m1i1 = 0 so vm_call reads a positive result (0 = success).
    msg.m_payload.m1.m1i1 = 0;
    OK
}

// Phase 6.12 — Procctl and exit (exit.c)

/// Handle VM_PROCCTL — process control operations.
///
/// Reads VMPPARAM subcode from m9.m9l1 and dispatches:
///   VMPPARAM_CLEAR (1): validates source is RS or VFS, clears proc
///   VMPPARAM_HANDLEMEM (2): validates source is VFS, stubbed
fn do_procctl(msg: &mut Message, transid: u32) -> i32 {
    let _ = transid;
    let subcode = unsafe { msg.m_payload.m9.m9l1 } as u32;

    // Validate target endpoint from m9.m9l2
    let target_ep = unsafe { msg.m_payload.m9.m9l2 } as i32;
    if !is_user_ep(target_ep) {
        return EINVAL;
    }

    match subcode {
        VMPPARAM_CLEAR => {
            // Only RS and VFS may clear a process
            if msg.m_source != RS_PROC_NR && msg.m_source != VFS_PROC_NR {
                return EINVAL;
            }
            // Clear process, reallocate page table, bind it
            proc::clear_proc(target_ep);
            // pt_new and pt_bind are unsafe — call them here
            unsafe {
                let _ = proc::pt_new(target_ep);
                let _ = proc::pt_bind(target_ep);
            }
            OK
        }
        VMPPARAM_HANDLEMEM => {
            // Only VFS may handle memory
            if msg.m_source != VFS_PROC_NR {
                return EINVAL;
            }
            // TODO: call handle_memory_start() with VFS IPC
            OK
        }
        _ => EINVAL,
    }
}

fn do_procctl_notrans(msg: &mut Message) -> i32 {
    do_procctl(msg, 0)
}

/// Handle VM_EXIT — process exit notification.
///
/// Validates endpoint, destroys the process's VM state.
fn do_exit(msg: &mut Message) -> i32 {
    let ep = unsafe { msg.m_payload.m1.m1i1 };
    if !is_user_ep(ep) {
        return EINVAL;
    }

    // Close file-region vmfds that no other process references before
    // destroying the address space (fork children may share them).
    fdref_close_regions(ep);

    // Destroy the process's address space.
    unsafe {
        proc::vm_destroy(ep);
    }

    OK
}

/// Handle VM_WILLEXIT — process announces intent to exit.
fn do_willexit(msg: &mut Message) -> i32 {
    let ep = msg.m_source;
    if !is_user_ep(ep) {
        return EINVAL;
    }

    // Set VMF_EXITING flag on the Vmproc entry.
    unsafe {
        if let Some(vmp) = proc::vmproc_lookup(ep) {
            vmp.vm_flags |= proc::VMF_EXITING;
        }
    }

    OK
}

// Stub handlers (remaining unimplemented calls)

/// Message offset constants for VM_MMAP / VM_MUNMAP, matching
/// `minix-std`'s vmem.rs buffer layout. Offsets are relative to
/// the start of m_payload.raw ([u8; 48]).
const MMAP_PROT: usize = 4; // i32 — bytes 12-15 of message
const MMAP_FLAGS: usize = 8; // i32 — bytes 16-19
const MMAP_LEN: usize = 12; // u64 — bytes 20-27
const MMAP_ADDR: usize = 20; // u64 — bytes 28-35
const MMAP_FD: usize = 28; // i32 — bytes 36-39

// MAP_FIXED (matches `minix-std::vmem::MAP_FIXED`): map at the exact
// requested address.
const MAP_FIXED: u32 = 0x10;

// MAP_ANONYMOUS (matches `minix-std::vmem::MAP_ANONYMOUS`).
const MAP_ANONYMOUS: u32 = 0x20;

// VM_MMAP file-offset field (i64 at absolute byte 40 = payload offset 32).
const MMAP_OFFSET: usize = 32;

// VMâ†’VFS request protocol (VFS_VMCALL message; M10 layout, absolute
// message-byte offsets — matches `vfs/consts.rs`).
const VFS_VMCALL: i32 = 0x100 + 38; // VFS_BASE + 38
const VMCALL_REQ_OFF: usize = 16;
const VMCALL_FD_OFF: usize = 20;
const VMCALL_REQID_OFF: usize = 24;
const VMCALL_ENDPOINT_OFF: usize = 28;
const VMCALL_OFFSET_OFF: usize = 8;
const VMCALL_FAULTVA_OFF: usize = 32;
const VMCALL_LENGTH_OFF: usize = 48;

// Reply (VM_VFS_REPLY) payload offsets.
const VMV_RESULT_OFF: usize = 20;
const VMV_DEV_OFF: usize = 28;
const VMV_INO_OFF: usize = 32;
const VMV_FD_OFF: usize = 40;
const VMV_SIZE_PAGES_OFF: usize = 48;

/// Send a synchronous VMâ†’VFS request (FDLOOKUP/FDCLOSE/FDIO) and wait for
/// the VM_VFS_REPLY. VM is single-threaded, so blocking inside a handler is
/// safe: VFS processes the request (forwarding to MFS if needed) and
/// replies, waking VM's SENDREC. This replaces C MINIX's async
/// request/callback machinery with a synchronous call.
///
/// Returns the reply's VMV_RESULT; on OK the full reply is left in `reply`
/// for the caller to read the remaining VMV_* fields.
fn vfs_request_sync(
    req: i32,
    fd: i32,
    ep: i32,
    offset: u64,
    fault_va: u64,
    length: u32,
    reply: &mut [u8; 64],
) -> i32 {
    let mut msg = [0u8; 64];
    msg[4..8].copy_from_slice(&VFS_VMCALL.to_le_bytes());
    msg[VMCALL_REQ_OFF..VMCALL_REQ_OFF + 4].copy_from_slice(&req.to_le_bytes());
    msg[VMCALL_FD_OFF..VMCALL_FD_OFF + 4].copy_from_slice(&fd.to_le_bytes());
    msg[VMCALL_REQID_OFF..VMCALL_REQID_OFF + 4].copy_from_slice(&0u32.to_le_bytes());
    msg[VMCALL_ENDPOINT_OFF..VMCALL_ENDPOINT_OFF + 4].copy_from_slice(&ep.to_le_bytes());
    msg[VMCALL_OFFSET_OFF..VMCALL_OFFSET_OFF + 8].copy_from_slice(&offset.to_le_bytes());
    msg[VMCALL_FAULTVA_OFF..VMCALL_FAULTVA_OFF + 8].copy_from_slice(&fault_va.to_le_bytes());
    msg[VMCALL_LENGTH_OFF..VMCALL_LENGTH_OFF + 4].copy_from_slice(&length.to_le_bytes());
    let r = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::VFS_PROC_NR as u64,
            msg.as_mut_ptr() as u64,
        )
    };
    if r < 0 {
        return r as i32;
    }
    reply.copy_from_slice(&msg);
    i32::from_le_bytes(
        msg[VMV_RESULT_OFF..VMV_RESULT_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    )
}

/// True if any active process other than `exclude_ep` has a file region
/// referencing (dev, ino, fd). fork clones regions verbatim, so a fork
/// sibling's region keeps the shared vmfd alive; the dying process's own
/// regions must not (they are about to be destroyed and would otherwise
/// keep the vmfd open forever).
fn vmfd_is_referenced(dev: u32, ino: u32, fd: i32, exclude_ep: i32) -> bool {
    let mut referenced = false;
    unsafe {
        proc::for_each_active_vmproc(|vmp| {
            if vmp.vm_endpoint == exclude_ep {
                return;
            }
            for r in vmp.vm_regions.regions.iter().flatten() {
                if r.flags & region::VR_FILE != 0 && r.dev == dev && r.ino == ino && r.fd == fd {
                    referenced = true;
                    break;
                }
            }
        });
    }
    referenced
}

/// Close a VM file descriptor (FDCLOSE) once no region in any process other
/// than `exclude_ep` references its (dev, ino, fd) — a lightweight fdref:
/// fork clones regions verbatim, so the same vmfd is shared until the last
/// user goes away. The exec/exit path passes the dying endpoint as
/// `exclude_ep` because its own regions are still present while the scan
/// runs and would otherwise keep the vmfd alive forever (VFS's vmfd table
/// fills 1 fd per exec). Callers that already removed the region pass -1.
fn fdref_close_if_unused(dev: u32, ino: u32, fd: i32, exclude_ep: i32) {
    if fd < 0 {
        return;
    }
    if !vmfd_is_referenced(dev, ino, fd, exclude_ep) {
        let mut reply = [0u8; 64];
        let _ = vfs_request_sync(
            arch_common::com::VMVFSREQ_FDCLOSE as i32,
            fd,
            arch_common::com::VFS_PROC_NR,
            0,
            0,
            0,
            &mut reply,
        );
    }
}

/// Close every file region of `ep` that no other process references.
/// Used on exec (region list reset) and process exit.
fn fdref_close_regions(ep: i32) {
    let mut to_close: [(u32, u32, i32); region::MAX_REGIONS] = [(0, 0, -1); region::MAX_REGIONS];
    let mut n = 0usize;
    unsafe {
        if let Some(vmp) = proc::vmproc_lookup(ep) {
            for r in vmp.vm_regions.regions.iter().flatten() {
                if r.flags & region::VR_FILE != 0 && n < region::MAX_REGIONS {
                    to_close[n] = (r.dev, r.ino, r.fd);
                    n += 1;
                }
            }
        }
    }
    for &(dev, ino, fd) in &to_close[..n] {
        fdref_close_if_unused(dev, ino, fd, ep);
    }
}

// Anonymous-mmap search base, above the brk heap and the exec/stack
// regions; per-arch so it stays clear of the kernel's identity map
// (aarch64: user space is the low 1 GiB, PUD[0]).

/// Find the first free virtual range of `len_aligned` bytes at or above
/// [`kernel::hal::mmap_base`], skipping all existing regions of the process.
///
/// Regions don't overlap (the list enforces that on insert), so a range is
/// free iff no region overlaps it; the scan jumps past the end of any
/// blocking region on each iteration.
fn mmap_find_hole(regions: &region::RegionList, len_aligned: u64) -> Option<u64> {
    let mut candidate = kernel::hal::mmap_base();
    loop {
        let end = candidate + len_aligned;
        if end > kernel::pagetable::MAX_USER_ADDRESS || end < candidate {
            return None;
        }
        let blocking_end = regions
            .regions
            .iter()
            .flatten()
            .filter(|r| r.vaddr < end && candidate < r.end())
            .map(|r| r.end())
            .max();
        match blocking_end {
            Some(e) => candidate = (e + PAGE_SIZE - 1) & !(PAGE_SIZE - 1),
            None => return Some(candidate),
        }
    }
}

/// Handle VM_MMAP — map memory into a process.
///
/// Message format (from minix-std vmem.rs):
///   raw[4..8]   = prot flags (PROT_READ, PROT_WRITE)
///   raw[8..12]  = map flags (MAP_ANONYMOUS, MAP_PRIVATE, MAP_FIXED)
///   raw[12..20] = length (u64)
///   raw[20..28] = desired address (u64, 0 = system chooses)
///   raw[28..32] = fd (i32, -1 for anonymous)
///   raw[32..40] = file offset (i64)
///
/// Return: m1i1|m1i2 (message bytes 8..16, u64) = mapped address on
/// success, m_type = errno on failure.
fn do_mmap(msg: &mut Message) -> i32 {
    let ep = msg.m_source;
    if !is_user_ep(ep) {
        return EINVAL;
    }

    let cr3 = unsafe { proc::vm_get_addrspace(ep) };
    if cr3 == 0 {
        return EINVAL;
    }

    let raw = unsafe { &msg.m_payload.raw };
    let prot = i32::from_ne_bytes(raw[MMAP_PROT..MMAP_PROT + 4].try_into().unwrap_or([0; 4]));
    let map_flags =
        i32::from_ne_bytes(raw[MMAP_FLAGS..MMAP_FLAGS + 4].try_into().unwrap_or([0; 4]));
    let length = u64::from_ne_bytes(raw[MMAP_LEN..MMAP_LEN + 8].try_into().unwrap_or([0; 8]));
    let addr = u64::from_ne_bytes(raw[MMAP_ADDR..MMAP_ADDR + 8].try_into().unwrap_or([0; 8]));
    let fd = i32::from_ne_bytes(raw[MMAP_FD..MMAP_FD + 4].try_into().unwrap_or([0; 4]));
    let file_offset = i64::from_ne_bytes(
        raw[MMAP_OFFSET..MMAP_OFFSET + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );

    if length == 0 || length > kernel::pagetable::MAX_USER_ADDRESS {
        return EINVAL;
    }

    // Round length up to page boundary.
    let len_aligned = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    // Determine virtual address: an explicit hint (with or without
    // MAP_FIXED) is honored, an anonymous request finds the first free hole.
    let vaddr = if addr != 0 {
        addr
    } else if map_flags as u32 & MAP_FIXED != 0 {
        return EINVAL;
    } else {
        let vmp = match unsafe { proc::vmproc_lookup(ep) } {
            Some(vmp) => vmp,
            None => return EINVAL,
        };
        match mmap_find_hole(&vmp.vm_regions, len_aligned) {
            Some(va) => va,
            None => return EINVAL,
        }
    };

    // Page-align the address.
    let page_addr = vaddr & !(PAGE_SIZE - 1);

    // File-backed mapping: fd >= 0 without MAP_ANONYMOUS (matching C's
    // do_mmap: `fd == -1 || (flags & MAP_ANON)` selects anonymous).
    if fd >= 0 && map_flags as u32 & MAP_ANONYMOUS == 0 {
        return do_mmap_file(
            ep,
            cr3,
            prot,
            map_flags as u32,
            len_aligned,
            page_addr,
            fd,
            file_offset,
            msg,
        );
    }

    // Validate the address range is within bounds.
    let end_addr = page_addr + len_aligned;
    if end_addr > kernel::pagetable::MAX_USER_ADDRESS || end_addr < page_addr {
        return EINVAL;
    }

    // Check for overlap with existing regions.
    if let Some(vmp) = unsafe { proc::vmproc_lookup(ep) } {
        let new_r = region::VirRegion::new(page_addr, len_aligned, region::VR_ANON);
        if vmp.vm_regions.find(page_addr).is_some() || vmp.vm_regions.find(end_addr - 1).is_some() {
            return EINVAL;
        }
        let mut region = new_r;
        region.flags |= region::VR_READABLE;
        if prot & 0x02 != 0 {
            region.flags |= region::VR_WRITABLE;
        }
        if prot & 0x04 != 0 {
            region.flags |= region::VR_EXEC;
        }

        if vmp.vm_regions.insert(region).is_some() {
            return EAGAIN;
        }
    } else {
        return EINVAL;
    }

    // Lazy region: no physical pages are allocated at map time. Pages are
    // faulted in on access (user-mode faults, and kernel-mode faults via the
    // Phase 1-4 fault machinery). The per-process page tables copy the
    // kernel's 0..32 GiB identity map; windows above 1 GiB are supervisor-
    // only, so without clearing them a kernel-mode copy into the region
    // would silently alias the identity mapping (writes land beyond RAM,
    // reads return garbage) instead of faulting. Clear the region's entries
    // so every access — user or kernel — faults and VM maps a real page.
    let _ = crate::vm::vm_clear_range(cr3, page_addr, len_aligned / PAGE_SIZE);

    // Write mapped address into m1i1|m1i2 (u64 at message bytes 8..16)
    // for vm_call to read.
    msg.m_payload.m1.m1i1 = (page_addr & 0xFFFF_FFFF) as i32;
    msg.m_payload.m1.m1i2 = (page_addr >> 32) as i32;
    OK
}

/// File-backed VM_MMAP: resolve the fd via FDLOOKUP, create a lazy VR_FILE
/// region, and return the mapped address (page-aligned base plus the
/// unaligned file-offset head).
#[allow(clippy::too_many_arguments)]
fn do_mmap_file(
    ep: i32,
    cr3: u64,
    prot: i32,
    map_flags: u32,
    len_aligned: u64,
    page_addr: u64,
    fd: i32,
    file_offset: i64,
    msg: &mut Message,
) -> i32 {
    // Resolve the fd to a VM fd + file identity via VFS.
    let mut reply = [0u8; 64];
    let r = vfs_request_sync(
        arch_common::com::VMVFSREQ_FDLOOKUP as i32,
        fd,
        ep,
        0,
        0,
        0,
        &mut reply,
    );
    if r != 0 {
        return r;
    }
    let vmfd = i32::from_le_bytes(
        reply[VMV_FD_OFF..VMV_FD_OFF + 4]
            .try_into()
            .unwrap_or([0xFF; 4]),
    );
    let dev = u32::from_le_bytes(
        reply[VMV_DEV_OFF..VMV_DEV_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    );
    let ino = u32::from_le_bytes(
        reply[VMV_INO_OFF..VMV_INO_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    );
    let size_pages = u64::from_le_bytes(
        reply[VMV_SIZE_PAGES_OFF..VMV_SIZE_PAGES_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let file_size = size_pages.saturating_mul(PAGE_SIZE);

    // Align the file offset down to a page; the caller's view starts
    // file_page_off bytes into the region's first page (C mmap_file).
    let file_off = file_offset.max(0) as u64;
    let file_page_off = file_off & (PAGE_SIZE - 1);
    let file_off_aligned = file_off - file_page_off;

    // The region must cover the unaligned head too.
    let region_len = len_aligned + if file_page_off != 0 { PAGE_SIZE } else { 0 };
    let end_addr = page_addr + region_len;
    if end_addr > kernel::pagetable::MAX_USER_ADDRESS || end_addr < page_addr {
        return EINVAL;
    }

    // In-file end for the region's mapped view: file bytes exist only up
    // to the whole file's size; pages at or past that are zero-filled.
    let infile_end = (file_off_aligned + region_len).min(file_size);

    let mut flags = region::VR_READABLE | region::VR_FILE;
    if prot & 0x02 != 0 {
        flags |= region::VR_WRITABLE;
    }
    if prot & 0x04 != 0 {
        flags |= region::VR_EXEC;
    }
    let new_r = region::VirRegion::new_file(
        page_addr,
        region_len,
        flags,
        dev,
        ino,
        vmfd,
        file_off_aligned,
        infile_end,
    );

    unsafe {
        if let Some(vmp) = proc::vmproc_lookup(ep) {
            // MAP_FIXED semantics (C mmap_region → map_unmap_range):
            // existing regions overlapping the new range are replaced, not
            // rejected, so mmap(MAP_FIXED) over an old mapping works.
            let mut removed: [Option<region::VirRegion>; region::MAX_REGIONS] =
                [None; region::MAX_REGIONS];
            let mut n = 0usize;
            let mut i = 0usize;
            while i < region::MAX_REGIONS {
                let overlaps = vmp.vm_regions.regions[i]
                    .as_ref()
                    .is_some_and(|r| r.overlaps(&new_r));
                if overlaps {
                    if let Some(r) = vmp.vm_regions.regions[i].take()
                        && n < region::MAX_REGIONS
                    {
                        removed[n] = Some(r);
                        n += 1;
                    }
                    continue;
                }
                i += 1;
            }
            if vmp.vm_regions.insert(new_r).is_some() {
                return EAGAIN;
            }
            for r in removed[..n].iter().flatten() {
                if r.flags & region::VR_FILE != 0 {
                    fdref_close_if_unused(r.dev, r.ino, r.fd, -1);
                }
            }
        } else {
            return EINVAL;
        }
    }

    // Clear identity PTEs so the lazy file pages fault (no-op on page
    // tables without a low-window identity copy, e.g. exec'd images).
    let _ = crate::vm::vm_clear_range(cr3, page_addr, region_len / PAGE_SIZE);

    let mapped = page_addr + file_page_off;
    msg.m_payload.m1.m1i1 = (mapped & 0xFFFF_FFFF) as i32;
    msg.m_payload.m1.m1i2 = (mapped >> 32) as i32;
    OK
}

fn do_fork(msg: &mut Message) -> i32 {
    // Message format (matching C: VMF_ENDPOINT / VMF_SLOTNO):
    //   m_source = PM_PROC_NR (sender, set by IPC — not the parent!)
    //   m1.m1i1  = parent endpoint (VMF_ENDPOINT)
    //   m1.m1i2  = child slot number (VMF_SLOTNO)
    // Reply:
    //   m1.m1i1  = child endpoint (VMF_CHILD_ENDPOINT)

    let parent_ep = unsafe { msg.m_payload.m1.m1i1 };
    let child_slot = unsafe { msg.m_payload.m1.m1i2 };
    if parent_ep < 0 || child_slot < 0 || child_slot >= NR_PROCS as i32 {
        return EINVAL;
    }

    // Phase 1: Allocate Vmproc for child and create a private page table
    // (deep copy of all user pages). VM_clone uses the slot number as a
    // temporary endpoint; the real endpoint is created by sys_fork below.
    let temp_ep: i32 = child_slot;
    if unsafe { proc::vm_clone(parent_ep, temp_ep) } != 0 {
        return EINVAL;
    }

    // Only inherit VMF_INUSE flag; clear any other flags that may
    // have been set on the pre-allocated slot. Matching C fork.c line 84:
    //   vmc->vm_flags &= VMF_INUSE;
    // Also reset ACL if the parent had a system (non-user) ACL.
    // Matching C: acl_fork(vmc) at fork.c line 87.
    if let Some(child_vmp) = unsafe { proc::vmproc_lookup(temp_ep) } {
        child_vmp.vm_flags &= proc::VMF_INUSE;
        proc::acl_fork(child_vmp);
    }

    // Call SYS_FORK to create the kernel Proc entry.
    const PFF_VMINHIBIT: u32 = 0x01;
    let result = minix_rt::sys_fork(parent_ep, child_slot, PFF_VMINHIBIT);
    let (child_ep, msgaddr) = match result {
        Ok((ep, ma)) => (ep, ma),
        Err(_) => {
            unsafe { proc::vmproc_free(temp_ep) };
            return EAGAIN;
        }
    };
    // Update child Vmproc with the real endpoint returned by the kernel.
    if let Some(vmp) = unsafe { proc::vmproc_lookup(temp_ep) } {
        vmp.vm_endpoint = child_ep;
    }

    // Set the child's CR3 via SYS_VMCTL(VMCTL_SETADDRSPACE).
    // This also clears VMINHIBIT and enqueues the child.
    if let Some(child_vmp) = unsafe { proc::vmproc_lookup(child_ep) } {
        let child_cr3 = child_vmp.vm_pml4_phys;
        if child_cr3 != 0 {
            let _ = unsafe { minix_rt::sys_vmctl_set_addspace(child_ep, child_cr3) };
        }
        // COW bookkeeping: the fork COW-protects the child's shared frames
        // (aarch64 now included), so walk the tables and register the shared
        // frames in the PhysBlock table before the child runs.
        let parent_cr3_val = unsafe { proc::vm_get_addrspace(parent_ep) };
        if parent_cr3_val != 0 && child_cr3 != 0 {
            let _ = unsafe { cow::cow_setup_fork(parent_cr3_val, child_cr3) };
        }
    }

    // Handle memory for the message buffer — pre-fault COW pages so the
    // kernel's fork-reply copy lands in a private writable page, not a
    // read-only shared frame (matches C's handle_memory_once after fork).
    // On aarch64 this is what lets virtual_copy write the child's msg page
    // without faulting on the AP=11 COW leaf.
    if let Some(child_vmp) = unsafe { proc::vmproc_lookup(child_ep) } {
        const PAGE_SIZE: u64 = 4096;
        let msg_va = msgaddr;
        let msg_end = msgaddr + 56;
        let mut va = msg_va & !(PAGE_SIZE - 1);
        while va < msg_end {
            let _ = cow::handle_cow_fault(child_vmp, va);
            va = va.wrapping_add(PAGE_SIZE);
        }
        if let Some(parent_vmp) = unsafe { proc::vmproc_lookup(parent_ep) } {
            let mut va = msg_va & !(PAGE_SIZE - 1);
            while va < msg_end {
                let _ = cow::handle_cow_fault(parent_vmp, va);
                va = va.wrapping_add(PAGE_SIZE);
            }
        }
    }

    // Reply with child endpoint in m1i1 (matching C VMF_CHILD_ENDPOINT).
    msg.m_payload.m1.m1i1 = child_ep;
    OK
}

/// Handle VM_EXEC_NEWMEM — create a new address space for exec.
///
/// Clears the target's old region list (closing file-region vmfds that no
/// other process shares) and re-establishes the pre-allocated brk heap so
/// the exec chain's VM_VFS_MMAPs land cleanly. The page table itself is
/// built by the kernel's SYS_EXEC_LOAD (`exec_create_root` + cleared code
/// range): a VM-side `pt_new` table only carries the kernel's high half and
/// cannot host the kernel's low-half identity code, so it must NOT be bound
/// here.
fn do_exec_newmem(msg: &mut Message) -> i32 {
    let ep = unsafe { msg.m_payload.m1.m1i1 };
    if !is_user_ep(ep) {
        return EINVAL;
    }

    // Old file regions die with the old image; close their vmfds unless a
    // fork sibling still uses them.
    fdref_close_regions(ep);

    // Reclaim the old address space: the kernel's SYS_EXEC_LOAD builds a
    // fresh root (exec_create_root) and overwrites p_cr3 without touching
    // the Vmproc, so without this the pre-exec table (a fork COW copy or
    // the boot table) would leak and its PhysBlock refs would pin the
    // parent's pages for the child's whole lifetime.
    unsafe {
        proc::free_exec_old_addrspace(ep);
    }

    unsafe {
        // Clear old regions — the exec'd image registers its own below.
        if let Some(vmp) = proc::vmproc_lookup(ep) {
            for i in 0..crate::vm::region::MAX_REGIONS {
                vmp.vm_regions.regions[i] = None;
            }
            // Re-establish the pre-allocated brk heap (the kernel's exec
            // path maps heap base .. base + 1 MiB with physical pages before
            // the image runs) so `brk`/`sbrk` keep returning heap addresses
            // and faults inside the heap can be demand-paged. Mirrors
            // `vm_init_boot`'s setup for boot processes.
            let heap_region = crate::vm::region::VirRegion::new(
                kernel::hal::user_heap_base(),
                0x100000u64, // 1 MB
                crate::vm::region::VR_READABLE
                    | crate::vm::region::VR_WRITABLE
                    | crate::vm::region::VR_ANON
                    | crate::vm::region::VR_PRESENT
                    | crate::vm::region::VR_DATA,
            );
            let _ = vmp.vm_regions.insert(heap_region);
            vmp.vm_region_top = kernel::hal::user_heap_base() + 0x100000u64;
        }
    }

    OK
}

fn do_brk(msg: &mut Message) -> i32 {
    let new_brk = unsafe { msg.m_payload.m1.m1i1 } as u64;
    let ep = msg.m_source;

    if !is_user_ep(ep) {
        return EINVAL;
    }

    // addr 0 is a query — return the current break without modification
    if new_brk == 0 {
        let current = unsafe {
            match proc::vmproc_lookup(ep) {
                Some(vmp) => vmp.vm_region_top,
                None => return EINVAL,
            }
        };
        msg.m_payload.m1.m1i1 = current as i32;
        return OK;
    }

    // Validate: break must be within the user address space and below the
    // per-arch heap limit (on aarch64 the kernel's EL1-only identity map
    // starts at 0x40000000, so the heap must stop before it).
    if new_brk > kernel::pagetable::MAX_USER_ADDRESS || new_brk > kernel::hal::user_heap_limit() {
        return EINVAL;
    }

    let cr3 = unsafe { proc::vm_get_addrspace(ep) };
    if cr3 == 0 {
        return EINVAL;
    }

    let page_size: u64 = 4096;
    let target = (new_brk + page_size - 1) & !(page_size - 1);

    let current_top = unsafe {
        match proc::vmproc_lookup(ep) {
            Some(vmp) => vmp.vm_region_top,
            None => return EINVAL,
        }
    };

    if target > current_top {
        // Expand heap: allocate and map new pages.
        // Pages in the pre-allocated range (0x3FE00000..0x3FF00000) are
        // already mapped by the kernel during boot. Only allocate pages
        // beyond that range.
        let prealloc_end: u64 = kernel::hal::user_heap_base() + 0x100000;
        let alloc_start = if current_top < prealloc_end {
            prealloc_end
        } else {
            current_top
        };

        let flags = kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE;
        let mut va = alloc_start;
        while va < target {
            let pa = crate::vm::vm_alloc_pages(1);
            if pa == 0 {
                return EAGAIN;
            }
            if crate::vm::vm_map_page_in(cr3, va, pa, flags) != 0 {
                crate::vm::vm_free_pages(pa, 1);
                return EAGAIN;
            }
            va += page_size;
        }
    } else if target < current_top {
        // Shrink heap: unmap pages.
        // Don't unmap pages within the pre-allocated range.
        let prealloc_start: u64 = kernel::hal::user_heap_base();
        let unmap_end = current_top;
        let unmap_start = target.max(prealloc_start);
        if unmap_end > unmap_start {
            let mut va = unmap_start;
            while va < unmap_end {
                let _ = crate::vm::vm_unmap_page_in(cr3, va);
                va += page_size;
            }
        }
    }

    // Update the region_top.
    unsafe {
        if let Some(vmp) = proc::vmproc_lookup(ep) {
            vmp.vm_region_top = target;
        }
    }

    msg.m_payload.m1.m1i1 = target as i32;
    OK
}

fn do_notify_sig(msg: &mut Message) -> i32 {
    // The target process is identified by m_source (the sender is the
    // process manager / PM). m1i1 contains the target endpoint.
    let target_ep = unsafe { msg.m_payload.m1.m1i1 };
    // m1i2 contains the signal number to deliver.
    let _sig = unsafe { msg.m_payload.m1.m1i2 };

    if !is_user_ep(target_ep) {
        return EINVAL;
    }

    // Mark the target process in the Vmproc table with a signal-pending
    // flag.  The full implementation would send the signal via sys_kill.
    sys_kill(target_ep, _sig);

    OK
}

fn do_vfs_reply(msg: &mut Message) -> i32 {
    // VM→VFS requests in this port are synchronous: vfs_request_sync blocks
    // in sendrec, so VFS's VM_VFS_REPLY is consumed inline by that call and
    // never arrives here as a fresh message. The C design routes async
    // replies through a PENDING transaction table (vfs.c do_vfs_reply); the
    // sync design deliberately has no such table, so an out-of-band reply is
    // a protocol error. Decline to answer it (SUSPEND), matching C's
    // "don't reply to the reply" convention.
    let _ = msg;
    SUSPEND
}

fn do_vfs_mmap(msg: &mut Message) -> i32 {
    // VM_VFS_MMAP (VFS → VM): create a lazy file-backed region for one
    // exec'd PT_LOAD segment. The fd is a VM fd in VFS's own fproc, so no
    // FDLOOKUP round-trip is needed (matching C's mmap_file for exec).
    let raw = unsafe { &msg.m_payload.raw };
    let who = i32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0; 4]));
    let fd = i32::from_ne_bytes(raw[4..8].try_into().unwrap_or([0xFF; 4]));
    let protflags = i32::from_ne_bytes(raw[8..12].try_into().unwrap_or([0; 4]));
    let len = u64::from_ne_bytes(raw[12..20].try_into().unwrap_or([0; 8]));
    let vaddr = u64::from_ne_bytes(raw[20..28].try_into().unwrap_or([0; 8]));
    let foffset = u64::from_ne_bytes(raw[28..36].try_into().unwrap_or([0; 8]));
    // In-file end for the segment (p_offset + p_filesz, sent by VFS):
    // pages at or past this are zero-filled (bss), not read from the file.
    let infile_end = u64::from_ne_bytes(raw[36..44].try_into().unwrap_or([0; 8]));
    let dev = u32::from_ne_bytes(raw[44..48].try_into().unwrap_or([0; 4]));
    let ino = u32::from_ne_bytes(raw[48..52].try_into().unwrap_or([0; 4]));

    if !is_user_ep(who) || fd < 0 {
        return EINVAL;
    }
    let cr3 = unsafe { proc::vm_get_addrspace(who) };
    if cr3 == 0 {
        return EINVAL;
    }
    if len == 0 || len > kernel::pagetable::MAX_USER_ADDRESS {
        return EINVAL;
    }

    // Page-align the file offset; the region base is the page-aligned
    // segment vaddr and the mapping view starts file_page_off bytes in
    // (ELF guarantees p_vaddr % PAGE == p_offset % PAGE).
    let page_size: u64 = 4096;
    let file_page_off = foffset & (page_size - 1);
    let file_off_aligned = foffset - file_page_off;
    let page_addr = vaddr & !(page_size - 1);
    // Region length must be page-aligned so the last partial page's faults
    // (up to the rounded-up end) still fall inside the region.
    let region_len = (len + file_page_off + page_size - 1) & !(page_size - 1);
    let end_addr = page_addr + region_len;
    if end_addr > kernel::pagetable::MAX_USER_ADDRESS || end_addr < page_addr {
        return EINVAL;
    }

    let mut flags = region::VR_READABLE | region::VR_FILE;
    if protflags & 0x02 != 0 {
        flags |= region::VR_WRITABLE;
    }
    if protflags & 0x04 != 0 {
        flags |= region::VR_EXEC;
    }
    let new_r = region::VirRegion::new_file(
        page_addr,
        region_len,
        flags,
        dev,
        ino,
        fd,
        file_off_aligned,
        infile_end,
    );

    unsafe {
        if let Some(vmp) = proc::vmproc_lookup(who) {
            // C's mmap_region with MAP_FIXED calls map_unmap_range first:
            // carve the new range out of any overlapping region so adjacent
            // PT_LOAD segments (a tiny segment sharing pages with a larger
            // one) can map. Trim instead of dropping the whole region:
            // adjacent segments legitimately share the last partial page (a
            // data segment whose memsz rounding spans the bss start page),
            // and removing the whole region would leave its exclusive pages
            // (the .data first page) with no region — the next fault on them
            // SIGSEGVs. Fully-covered regions are dropped and their vmfd
            // released; the trimmed region keeps its fd (same file).
            let mut removed: [Option<region::VirRegion>; region::MAX_REGIONS] =
                [None; region::MAX_REGIONS];
            let mut n = 0usize;
            let mut i = 0usize;
            while i < region::MAX_REGIONS {
                let overlaps = vmp.vm_regions.regions[i]
                    .as_ref()
                    .is_some_and(|r| r.overlaps(&new_r));
                if !overlaps {
                    i += 1;
                    continue;
                }
                // SAFETY: overlaps() above established the entry is Some.
                let r = vmp.vm_regions.regions[i].as_mut().unwrap();
                if new_r.vaddr <= r.vaddr {
                    // The new region covers the old region's head.
                    if new_r.end() >= r.end() {
                        // Fully covered — replace it.
                        let old = vmp.vm_regions.regions[i].take().unwrap();
                        if n < region::MAX_REGIONS {
                            removed[n] = Some(old);
                            n += 1;
                        }
                    } else {
                        // Trim the head (not exercised by exec's ascending
                        // segment order; kept for completeness).
                        let cut = new_r.end() - r.vaddr;
                        let pages = (cut / 4096) as usize;
                        r.vaddr = new_r.end();
                        r.length -= cut;
                        r.npages = r.npages.saturating_sub(pages as u32);
                        for j in 0..r.npages as usize {
                            r.phys_pages[j] = r.phys_pages[j + pages];
                        }
                        for j in r.npages as usize..region::MAX_PHYS_PAGES {
                            r.phys_pages[j] = 0;
                        }
                    }
                } else {
                    // The new region starts inside the old one — trim the
                    // old region's tail up to the new region's start.
                    r.length = new_r.vaddr - r.vaddr;
                    if r.length == 0 {
                        let old = vmp.vm_regions.regions[i].take().unwrap();
                        if n < region::MAX_REGIONS {
                            removed[n] = Some(old);
                            n += 1;
                        }
                    }
                }
                i += 1;
            }
            if vmp.vm_regions.insert(new_r).is_some() {
                return EAGAIN;
            }
            // Executed image: the first fault pre-faults the non-executable
            // file regions (rodata/data) so VFS's kernel-mode copies of the
            // image (vircopy of user buffers) hit present pages.
            vmp.prefault_exec = true;
            for r in removed[..n].iter().flatten() {
                if r.flags & region::VR_FILE != 0 {
                    fdref_close_if_unused(r.dev, r.ino, r.fd, -1);
                }
            }
        } else {
            return EINVAL;
        }
    }

    // Clear identity PTEs so the lazy file pages fault (no-op on exec'd
    // page tables, which have no low-window identity copy).
    let _ = crate::vm::vm_clear_range(cr3, page_addr, region_len / page_size);

    msg.m_payload.m1.m1i1 = (page_addr & 0xFFFF_FFFF) as i32;
    msg.m_payload.m1.m1i2 = (page_addr >> 32) as i32;
    OK
}

fn do_rs_set_priv(msg: &mut Message) -> i32 {
    // RS sets the privilege/call mask for a process.
    // The target endpoint is in m1i1, the call mask bitmap
    // is in m1i2 and m1i3.
    let _target_ep = unsafe { msg.m_payload.m1.m1i1 };
    let _call_mask_lo = unsafe { msg.m_payload.m1.m1i2 } as u64;
    let _call_mask_hi = unsafe { msg.m_payload.m1.m1i3 } as u64;

    // TODO: When ACL infrastructure is available, store the call
    // mask on the Vmproc entry so that acl_check() can authorize
    // VM calls per-process.
    OK
}

fn do_rs_update(msg: &mut Message) -> i32 {
    // RS updates a process's VM state after live update.
    // The target endpoint is in m1i1.
    let _target_ep = unsafe { msg.m_payload.m1.m1i1 };

    // TODO: Phase 14 — handle live update: swap Vmproc entries
    // and page table references between old and new instances.
    OK
}

fn do_rs_memctl(msg: &mut Message) -> i32 {
    // RS memory control — pins memory or makes memory visible to VM.
    // Subcode in m1i1: 0 = VM_RS_MEM_PIN, 1 = VM_RS_MEM_MAKE_VM.
    let _subcode = unsafe { msg.m_payload.m1.m1i1 };
    let _target_ep = unsafe { msg.m_payload.m1.m1i2 };

    // TODO: Phase 14 — implement memory pinning and VM-managed
    // region transitions for live update support.
    OK
}

fn do_info(msg: &mut Message) -> i32 {
    // The message carries the subcode in m1_i1 (VMIW_STATS=1, VMIW_USAGE=2, VMIW_REGION=3)
    // and optionally the target endpoint in m1_i2
    let subcode = unsafe { msg.m_payload.m1.m1i1 } as u32;
    let target_ep = unsafe { msg.m_payload.m1.m1i2 };

    match subcode {
        VMIW_STATS => {
            // Populate VmStatsInfo: page size, total pages, free pages.
            msg.m_payload.m1.m1i1 = kernel::vm::VM_PAGE_SIZE as i32;
            msg.m_payload.m1.m1i2 = kernel::vm::total_pages();
            // Free pages from the kernel's real allocator (VM's own copy of
            // the arch allocator covers the whole identity window and is not
            // authoritative).
            msg.m_payload.m1.m1i3 = vm_free_pages_query();
            OK
        }
        VMIW_USAGE => {
            // Populate VmUsageInfo from target process's Vmproc entry.
            if !is_user_ep(target_ep) {
                return EINVAL;
            }
            unsafe {
                if let Some(vmp) = proc::vmproc_lookup(target_ep) {
                    // Total memory (vm_total) — approximate from region_top
                    msg.m_payload.m1.m1i1 = (vmp.vm_region_top / 4096) as i32;
                    // Minor page faults
                    msg.m_payload.m1.m1i2 = vmp.vm_minor_page_fault as i32;
                    // Major page faults
                    msg.m_payload.m1.m1i3 = vmp.vm_major_page_fault as i32;
                } else {
                    // No Vmproc entry — return zeros.
                    msg.m_payload.m1.m1i1 = 0;
                    msg.m_payload.m1.m1i2 = 0;
                    msg.m_payload.m1.m1i3 = 0;
                }
            }
            OK
        }
        VMIW_REGION => {
            // Walk region array, write VmRegionInfo structs to output buffer
            // Stubbed for now — real impl needs region AVL tree
            if !is_user_ep(target_ep) {
                return EINVAL;
            }
            msg.m_payload.m1.m1i1 = 0; // count of regions
            OK
        }
        _ => ENOSYS,
    }
}

fn do_query_exit(msg: &mut Message) -> i32 {
    // Query whether a process has exited.
    // The target endpoint is in m1i1.
    let _target_ep = unsafe { msg.m_payload.m1.m1i1 };

    // TODO: Phase 14 — look up the queryexit table to see if the
    // target process has exited and return its exit status.
    // For now, return EINVAL since no process is in the table.
    EINVAL
}

fn do_watch_exit(msg: &mut Message) -> i32 {
    // Register to be notified when a process exits.
    // The target endpoint is in m1i1, the watcher is msg.m_source.
    let _target_ep = unsafe { msg.m_payload.m1.m1i1 };
    let _watcher_ep = msg.m_source;

    // Set the VMF_WATCHEXIT flag on the target Vmproc entry.
    unsafe {
        if let Some(vmp) = proc::vmproc_lookup(_target_ep) {
            vmp.vm_flags |= proc::VMF_WATCHEXIT;
        }
    }

    OK
}

fn do_mapcache(msg: &mut Message) -> i32 {
    // Map a cache page into a process.
    // m1i1 = target endpoint, m1i2 = cache block number,
    // m1i3 = flags (e.g., write permission).
    let target_ep = unsafe { msg.m_payload.m1.m1i1 };
    let _block = unsafe { msg.m_payload.m1.m1i2 } as u64;
    let _flags = unsafe { msg.m_payload.m1.m1i3 } as u32;

    if !is_user_ep(target_ep) {
        return EINVAL;
    }

    let cr3 = unsafe { proc::vm_get_addrspace(target_ep) };
    if cr3 == 0 {
        return EINVAL;
    }

    // TODO: Phase 14 — look up the cache page by block number,
    // allocate a free virtual address in the cache region,
    // and map the page with map_page().
    msg.m_payload.m1.m1i1 = 0; // return the virtual address
    OK
}

fn do_setcache(msg: &mut Message) -> i32 {
    // Set a cache block for a process.
    // m1i1 = cache block number, m1i2 = physical address.
    let _block = unsafe { msg.m_payload.m1.m1i1 } as u64;
    let _phys = unsafe { msg.m_payload.m1.m1i2 } as u64;

    // TODO: Phase 14 — allocate a cache page entry and associate
    // it with the given block number and physical address.
    OK
}

fn do_clearcache(msg: &mut Message) -> i32 {
    // Clear cache pages for a process.
    // m1i1 = target endpoint.
    let _target_ep = unsafe { msg.m_payload.m1.m1i1 };

    // TODO: Phase 14 — walk the cache page table for the target
    // process and unmap / free all cache pages.
    OK
}

fn do_getrusage(msg: &mut Message) -> i32 {
    // Get resource usage for a process.
    // m1i1 = target endpoint.
    let target_ep = unsafe { msg.m_payload.m1.m1i1 };

    if !is_user_ep(target_ep) {
        return EINVAL;
    }

    unsafe {
        if let Some(vmp) = proc::vmproc_lookup(target_ep) {
            // Populate resource usage fields from Vmproc counters.
            // m1i1 = max RSS (vm_total_max approximated as vm_region_top),
            // m1i2 = minor page faults, m1i3 = major page faults.
            msg.m_payload.m1.m1i1 = (vmp.vm_region_top / 4096) as i32;
            msg.m_payload.m1.m1i2 = vmp.vm_minor_page_fault as i32;
            msg.m_payload.m1.m1i3 = vmp.vm_major_page_fault as i32;
            OK
        } else {
            EINVAL
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use arch_common::com::{
        NR_VM_CALLS, VM_MMAP, VM_PAGEFAULT, VM_REMAP, VM_REMAP_RO, VM_RQ_BASE, VM_SHM_UNMAP,
        VM_UNMAP_PHYS,
    };
    use arch_common::types::Endpoint;

    #[test]
    fn test_call_number_in_range() {
        // VM_RQ_BASE itself should map to index 0
        assert_eq!(call_number(VM_RQ_BASE), 0);
        // Last valid call
        assert_eq!(
            call_number(VM_RQ_BASE + NR_VM_CALLS - 1),
            (NR_VM_CALLS - 1) as i32
        );
    }

    #[test]
    fn test_call_number_out_of_range() {
        assert_eq!(call_number(VM_RQ_BASE - 1), -1);
        assert_eq!(call_number(VM_RQ_BASE + NR_VM_CALLS), -1);
        // VM_PAGEFAULT is outside the table range
        assert_eq!(call_number(VM_PAGEFAULT), -1);
        assert_eq!(call_number(0), -1);
        assert_eq!(call_number(u32::MAX), -1);
    }

    #[test]
    fn test_init_vm_populates_table() {
        init_vm();
        unsafe {
            // Spot-check a few entries
            assert!((*VM_CALLS.get())[0].func.is_some(), "VM_EXIT should be set");
            assert_eq!((*VM_CALLS.get())[0].name, "do_exit");

            assert!(
                (*VM_CALLS.get())[(VM_MMAP - VM_RQ_BASE) as usize]
                    .func
                    .is_some()
            );
            assert_eq!(
                (*VM_CALLS.get())[(VM_MMAP - VM_RQ_BASE) as usize].name,
                "do_mmap"
            );
        }
    }

    #[test]
    fn test_init_vm_zeros_unset_entries() {
        init_vm();
        unsafe {
            // Slots that are not in the official call list should remain None
            // VM_WILLEXIT is at index 5; check an empty slot like index 4 (VM_RQ_BASE + 4)
            assert!(
                (*VM_CALLS.get())[4].func.is_none(),
                "slot 4 should not be set"
            );
        }
    }

    #[test]
    fn test_init_vm_deduped_handlers() {
        init_vm();
        unsafe {
            // VM_UNMAP_PHYS maps to do_munmap, VM_SHM_UNMAP maps to do_shm_unmap
            let unmap_idx = (VM_UNMAP_PHYS - VM_RQ_BASE) as usize;
            let shm_idx = (VM_SHM_UNMAP - VM_RQ_BASE) as usize;
            assert!((*VM_CALLS.get())[unmap_idx].func.is_some());
            assert!((*VM_CALLS.get())[shm_idx].func.is_some());

            // VM_REMAP and VM_REMAP_RO both map to do_remap
            let remap_idx = (VM_REMAP - VM_RQ_BASE) as usize;
            let remap_ro_idx = (VM_REMAP_RO - VM_RQ_BASE) as usize;
            assert!((*VM_CALLS.get())[remap_idx].func.is_some());
            assert!((*VM_CALLS.get())[remap_ro_idx].func.is_some());
        }
    }

    #[test]
    fn test_all_stub_handlers_return_enosys() {
        let mut msg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };

        // Phase 6.10 — Shared memory
        assert_eq!(do_shm_unmap(&mut msg), OK);
        assert_eq!(do_shm_get(&mut msg), ENOSYS);
        assert_eq!(do_shm_at(&mut msg), ENOSYS);

        // Phase 6.11 — Remap operations (now return OK or EINVAL)
        // do_remap: dest_ep = m1i1 = 0, src_ep = m1i2 = 0,
        // but with no page table allocated for ep 0, it returns EINVAL.
        msg.m_payload.m1.m1i4 = 4096;
        assert_eq!(do_remap(&mut msg), EINVAL); // no Vmproc for ep 0
        // Reset message for next call
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;
        // do_map_phys: needs len > 0 (m1i2) and target ep = m1i1 = 0
        // But with no page table allocated, it returns EINVAL.
        msg.m_payload.m1.m1i2 = 4096;
        assert_eq!(do_map_phys(&mut msg), EINVAL);
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;
        // do_get_phys: target ep m1i1 = 0 has no page table in test mode,
        // so it returns EINVAL.
        assert_eq!(do_get_phys(&mut msg), EINVAL);
        // do_get_refcount: returns 1 for any valid target
        assert_eq!(do_get_refcount(&mut msg), 1);
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;
        // do_munmap: addr must be page-aligned, but no page table
        msg.m_payload.m1.m1i2 = 4096; // page-aligned addr
        msg.m_payload.m1.m1i3 = 4096; // size
        // With no CR3 available, returns EINVAL
        assert_eq!(do_munmap(&mut msg), EINVAL);
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;

        // Phase 6.12 — Procctl and exit
        // do_exit: source = 0 is valid
        assert_eq!(do_exit(&mut msg), OK);
        assert_eq!(do_fork(&mut msg), EINVAL); // requires child endpoint in m1i1
        msg.m_payload.m1.m1i1 = 1; // child endpoint
        assert_eq!(do_fork(&mut msg), EINVAL); // parent 0 and child 1 not yet in Vmproc
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;
        // do_brk requires a valid region_top
        msg.m_payload.m1.m1i1 = 0x10000;
        assert_eq!(do_brk(&mut msg), EINVAL); // no Vmproc for ep 0
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;
        // do_willexit: source = 0 is valid
        assert_eq!(do_willexit(&mut msg), OK);
        assert_eq!(do_notify_sig(&mut msg), OK);
        // do_procctl: m9l1 (subcode) = 0 does not match any case -> EINVAL
        assert_eq!(do_procctl(&mut msg, 0), EINVAL);
        assert_eq!(do_procctl_notrans(&mut msg), EINVAL);

        // VFS — do_vfs_reply rejects out-of-band replies (SUSPEND = no
        // reply; the sync protocol never leaves a pending async request);
        // do_vfs_mmap is a real handler (needs a valid target endpoint,
        // which a zeroed message lacks).
        assert_eq!(do_vfs_reply(&mut msg), SUSPEND);
        assert_eq!(do_vfs_mmap(&mut msg), EINVAL);

        // RS — now return OK instead of ENOSYS
        assert_eq!(do_rs_set_priv(&mut msg), OK);
        assert_eq!(do_rs_update(&mut msg), OK);
        assert_eq!(do_rs_memctl(&mut msg), OK);

        // do_info with no subcode set -> ENOSYS
        assert_eq!(do_info(&mut msg), ENOSYS);
        do_info(&mut msg);

        // Query exit — now returns EINVAL (no queryexit table)
        assert_eq!(do_query_exit(&mut msg), EINVAL);

        // Watch exit — now returns OK
        assert_eq!(do_watch_exit(&mut msg), OK);

        // Cache — do_mapcache needs valid endpoint in m1i1
        assert_eq!(do_mapcache(&mut msg), EINVAL); // no m1i1 set
        msg.m_payload.m1.m1i1 = 0; // valid ep but no page table
        assert_eq!(do_mapcache(&mut msg), EINVAL); // no page table
        msg.m_payload = unsafe { core::mem::zeroed() };
        assert_eq!(do_setcache(&mut msg), OK);
        assert_eq!(do_clearcache(&mut msg), OK);

        // Rusage — needs valid ep in m1i1
        assert_eq!(do_getrusage(&mut msg), EINVAL); // no m1i1 set
    }

    #[test]
    fn test_vm_calls_table_size() {
        assert_eq!(NR_VM_CALLS, 48);
    }

    #[test]
    fn test_do_exec_newmem_resets_stale_table_and_reestablishes_heap() {
        unsafe {
            let ep: Endpoint = 89;
            // Build a Vmproc by hand (host has no boot CR3, so vm_create
            // cannot be used): give it a stale pre-exec root and an old
            // image's file region, as a forked child about to exec would
            // have.
            let vmp = proc::vmproc_alloc(ep).expect("vmproc alloc");
            vmp.vm_pml4_phys = 0x12345000;
            let old = crate::vm::region::VirRegion::new(
                0x1000000,
                0x200000,
                crate::vm::region::VR_READABLE
                    | crate::vm::region::VR_WRITABLE
                    | crate::vm::region::VR_FILE
                    | crate::vm::region::VR_PRESENT,
            );
            let _ = vmp.vm_regions.insert(old);

            let mut msg = Message {
                m_source: arch_common::com::PM_PROC_NR,
                m_type: arch_common::com::VM_EXEC_NEWMEM as i32,
                m_payload: core::mem::zeroed(),
            };
            msg.m_payload.m1.m1i1 = ep;
            let r = do_exec_newmem(&mut msg);
            assert_eq!(r, OK);

            let vmp = proc::vmproc_lookup(ep).expect("vmproc after exec_newmem");
            // The pre-exec table is reclaimed (kernel CR3 query returns 0
            // on host, so only the recorded root is observable): it must not
            // dangle for a later vm_destroy to double-free.
            assert_eq!(vmp.vm_pml4_phys, 0, "stale root must be cleared");
            let mut heap_seen = false;
            let mut file_seen = false;
            for r in vmp.vm_regions.regions.iter().flatten() {
                if r.flags & crate::vm::region::VR_DATA != 0 {
                    heap_seen = true;
                }
                if r.flags & crate::vm::region::VR_FILE != 0 {
                    file_seen = true;
                }
            }
            assert!(heap_seen, "heap region must be re-established");
            assert!(!file_seen, "old image's file regions must be cleared");
            proc::vmproc_free(ep);
        }
    }

    #[test]
    fn test_vm_find_hole_reuses_released_va() {
        // Single-page holes march upward until a VA is released back;
        // the next request must reuse it instead of bumping VM_NEXT_MAP_VA
        // (which would otherwise consume a fresh kernel PT page per 512
        // self-maps — the per-exec leak).
        let a = vm_find_hole(1);
        let b = vm_find_hole(1);
        assert_eq!(b, a + PAGE_SIZE, "unreleased holes march upward");

        // Simulate vm_unmappage's release (host has no kernel to unmap).
        let len = VM_MAP_VA_FREELIST_LEN.load(Ordering::Relaxed);
        assert!(len < VM_MAP_VA_FREELIST_CAP);
        VM_MAP_VA_FREELIST[len].store(b, Ordering::Relaxed);
        VM_MAP_VA_FREELIST_LEN.store(len + 1, Ordering::Relaxed);

        let c = vm_find_hole(1);
        assert_eq!(c, b, "released VA must be reused before the bump allocator");
    }

    #[test]
    fn test_fdref_scan_excludes_the_dying_process() {
        unsafe {
            let a: Endpoint = 71;
            let b: Endpoint = 72;
            proc::vmproc_alloc(a).expect("vmproc A");
            proc::vmproc_alloc(b).expect("vmproc B");
            // A and B are fork siblings sharing vmfd 3 for file (dev 7,
            // ino 9); B also mapped the same file later through a fresh
            // vmfd 4. Regions stay in the list while the processes live.
            let file = |vaddr: u64, fd: i32| {
                crate::vm::region::VirRegion::new_file(
                    vaddr,
                    0x1000,
                    crate::vm::region::VR_READABLE | crate::vm::region::VR_FILE,
                    7,
                    9,
                    fd,
                    0,
                    0x1000,
                )
            };
            proc::vmproc_lookup(a)
                .unwrap()
                .vm_regions
                .insert(file(0x2000000, 3));
            proc::vmproc_lookup(a)
                .unwrap()
                .vm_regions
                .insert(file(0x2000000, 3));
            {
                let vmp = proc::vmproc_lookup(b).unwrap();
                vmp.vm_regions.insert(file(0x3000000, 3));
                vmp.vm_regions.insert(file(0x4000000, 4));
            }

            // A and B share fd 3 (fork clones regions verbatim): each keeps
            // it alive for the other, so neither's own region is treated as
            // the sole reference when the other dies.
            assert!(
                vmfd_is_referenced(7, 9, 3, a),
                "sibling B must keep the shared vmfd alive when A dies"
            );
            assert!(
                vmfd_is_referenced(7, 9, 3, b),
                "sibling A must keep the shared vmfd alive when B dies"
            );
            // fd 4 belongs to B alone: excluding B means nobody references
            // it, so the exec/exit path would send FDCLOSE.
            assert!(
                !vmfd_is_referenced(7, 9, 4, b),
                "B's sole region must not self-reference"
            );
            assert!(
                vmfd_is_referenced(7, 9, 4, -1),
                "B's own region references fd 4 when not excluded"
            );
            proc::vmproc_free(a);
            proc::vmproc_free(b);
        }
    }

    #[test]
    fn test_do_info_vmiw_stats() {
        let mut msg = Message {
            m_source: 0,
            m_type: VM_INFO as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        // VMIW_STATS = 1 in m1i1
        msg.m_payload.m1.m1i1 = VMIW_STATS as i32;
        let rc = do_info(&mut msg);
        assert_eq!(rc, OK);
        // Should have filled page size and total pages
        unsafe {
            assert!(msg.m_payload.m1.m1i1 > 0);
        }
    }

    #[test]
    fn test_do_info_vmiw_usage() {
        let mut msg = Message {
            m_source: 0,
            m_type: VM_INFO as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        msg.m_payload.m1.m1i1 = VMIW_USAGE as i32;
        assert_eq!(do_info(&mut msg), OK);
    }

    #[test]
    fn test_do_info_vmiw_region() {
        let mut msg = Message {
            m_source: 0,
            m_type: VM_INFO as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        msg.m_payload.m1.m1i1 = VMIW_REGION as i32;
        assert_eq!(do_info(&mut msg), OK);
    }

    #[test]
    fn test_do_info_unknown_subcode() {
        let mut msg = Message {
            m_source: 0,
            m_type: VM_INFO as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        // Unknown subcode should return ENOSYS
        msg.m_payload.m1.m1i1 = 99;
        assert_eq!(do_info(&mut msg), ENOSYS);
    }

    #[test]
    fn test_pagefault_functions_are_callable() {
        let mut msg = Message {
            m_source: 0,
            m_type: VM_PAGEFAULT as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        // do_pagefaults should not panic with a bad endpoint
        do_pagefaults(&mut msg);
        // sys_kill now calls kernel::system::send_sig which may fail in
        // test context (no valid priv structure for random proc numbers).
        // Just verify it doesn't panic.
        let _ = sys_kill(42, SIGSEGV);
        // clear_pagefault forwards VMCTL_CLEAR_PAGEFAULT to the kernel.
        // On host there is no kernel, so it reports failure (-1) instead
        // of the OK it returns on target.
        assert_eq!(clear_pagefault(0), -1);
        assert_eq!(clear_pagefault(1), -1);
    }

    #[test]
    fn test_constants_match() {
        assert_eq!(ENOSYS, -72);
        assert_eq!(EINVAL, -5);
        assert_eq!(SIGSEGV, 11);
        assert_eq!(SIGABRT, 6);
    }

    #[test]
    fn test_init_and_main_are_callable() {
        // Smoke test: these should not panic
        vm_main();
        exec_bootproc();
        sef_signal_handler();
    }

    #[test]
    fn test_dispatch_notification_returns_edontreply() {
        init_vm();
        let mut msg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        // Use a valid notification status: call type = NOTIFY (4), no flags.
        let notif_status: i32 = 4; // NOTIFY call number
        let r = dispatch_message(&mut msg, notif_status);
        assert_eq!(r, EDONTREPLY);
    }

    #[test]
    fn test_dispatch_vm_pagefault_returns_edontreply() {
        init_vm();
        let mut msg = Message {
            m_source: 42,
            m_type: VM_PAGEFAULT as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        // do_pagefaults handles the fault and returns EDONTREPLY
        // (no reply needed since the faulting process is resumed via
        // sys_vmctl(CLEAR_PAGEFAULT) internally)
        assert_eq!(r, EDONTREPLY);
    }

    #[test]
    fn test_dispatch_rs_init_returns_ok() {
        init_vm();
        let mut msg = Message {
            m_source: RS_PROC_NR,
            m_type: RS_INIT as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, OK);
        assert_eq!(msg.m_type, OK);
    }

    #[test]
    fn test_dispatch_known_call_dispatches_handler() {
        init_vm();
        let mut msg = Message {
            m_source: 0,
            m_type: VM_MMAP as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        // do_mmap is now a real implementation that validates the message.
        // With a zeroed message (no valid vaddr/length), it returns EINVAL.
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_dispatch_unknown_call_returns_enosys() {
        init_vm();
        let mut msg = Message {
            m_source: 0,
            m_type: 0x9999, // unknown call number
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, ENOSYS);
        assert_eq!(msg.m_type, ENOSYS);
    }

    #[test]
    fn test_dispatch_unset_table_slot_returns_enosys() {
        init_vm();
        // VM_RQ_BASE + 4 is in range but not set
        let mut msg = Message {
            m_source: 0,
            m_type: (VM_RQ_BASE + 4) as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, ENOSYS);
        assert_eq!(msg.m_type, ENOSYS);
    }

    #[test]
    fn test_dispatch_suspend_handler_no_reply() {
        init_vm();
        // VM_PAGEFAULT returns EDONTREPLY (fault handled internally,
        // no reply sent back to the kernel)
        let mut msg = Message {
            m_source: 42,
            m_type: VM_PAGEFAULT as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, EDONTREPLY);
    }

    #[test]
    fn test_ipc_send_stub_does_not_panic() {
        let msg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        assert!(ipc_send_stub(42, &msg).is_ok());
    }

    #[test]
    fn test_dispatch_vfs_transaction_returns_enosys() {
        init_vm();
        // A VFS transaction ID is in the 0xB00..0xBFF range
        // (VFS_TRANSACTION_BASE).
        let mut msg = Message {
            m_source: VFS_PROC_NR,
            m_type: 0xB00, // VFS_TRANSACTION_BASE
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, ENOSYS);
        assert_eq!(msg.m_type, ENOSYS);
    }

    #[test]
    fn test_do_vfs_reply_returns_suspend() {
        // The sync VM→VFS protocol never leaves a pending async request,
        // so an out-of-band VM_VFS_REPLY is rejected without replying.
        init_vm();
        let mut msg = Message {
            m_source: VFS_PROC_NR,
            m_type: VM_VFS_REPLY as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, SUSPEND);
    }

    #[test]
    fn test_dispatch_calls_init_vm_if_not_called() {
        // Ensure that dispatch doesn't panic even if init_vm wasn't called
        // (table will have all None entries -> ENOSYS)
        // Note: we call init_vm anyway since static state persists
        init_vm();
        let mut msg = Message {
            m_source: 0,
            m_type: VM_RQ_BASE as i32, // VM_EXIT
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        // VM_EXIT handler returns OK
        assert_eq!(r, OK);
    }
}
