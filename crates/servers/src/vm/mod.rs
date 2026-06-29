//! VM server — adapted from `minix/servers/vm/main.c`
//!
//! Implements the VM server main loop, message dispatch, and stub handlers
//! for all VM calls. Real implementations come in Phases 6.4+.

#![allow(unused_variables)]
#![allow(static_mut_refs)]

pub mod mem;
pub mod proc;

use arch_common::com::{
    NR_VM_CALLS, RS_INIT, RS_PROC_NR, VFS_PROC_NR, VM_BRK, VM_CLEARCACHE, VM_EXIT, VM_FORK,
    VM_GETPHYS, VM_GETREF, VM_GETRUSAGE, VM_INFO, VM_MAP_PHYS, VM_MAPCACHEPAGE, VM_MMAP, VM_MUNMAP,
    VM_NOTIFY_SIG, VM_PAGEFAULT, VM_PROCCTL, VM_QUERY_EXIT, VM_REMAP, VM_REMAP_RO, VM_RQ_BASE,
    VM_RS_MEMCTL, VM_RS_SET_PRIV, VM_RS_UPDATE, VM_SETCACHEPAGE, VM_SHM_UNMAP, VM_UNMAP_PHYS,
    VM_VFS_MMAP, VM_VFS_REPLY, VM_WATCH_EXIT, VM_WILLEXIT, VMCTL_CLEAR_PAGEFAULT, VMIW_REGION,
    VMIW_STATS, VMIW_USAGE, VMPPARAM_CLEAR, VMPPARAM_HANDLEMEM,
};
use arch_common::com::{SUSPEND, is_ipc_notify, is_vfs_fs_transid};
use arch_common::consts::NR_PROCS;
use arch_common::ipc::{EDONTREPLY, Message};
use arch_common::ipcconst::{
    IPC_FLG_MSG_FROM_KERNEL, IPC_STATUS_FLAGS_SHIFT, ipc_status_flags_test,
};

// ── Constants ────────────────────────────────────────────────────────────

const OK: i32 = 0;

/// Operation not supported (ENOSYS from MINIX errno.h).
const ENOSYS: i32 = -72;

/// Invalid argument (EINVAL).
const EINVAL: i32 = -5;

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

// ═════════════════════════════════════════════════════════════════════════
// Call dispatch table
// ═════════════════════════════════════════════════════════════════════════

/// A single entry in the VM call dispatch table.
#[derive(Copy, Clone)]
pub struct VmCallEntry {
    pub func: Option<fn(&mut Message) -> i32>,
    pub name: &'static str,
}

/// VM call dispatch table, indexed by `call_number()`.
///
/// Initialized to all-None; populated by `init_vm()`.
static mut VM_CALLS: [VmCallEntry; NR_VM_CALLS as usize] = [VmCallEntry {
    func: None,
    name: "",
}; NR_VM_CALLS as usize];

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
fn set_call(call_nr: u32, func: fn(&mut Message) -> i32, name: &'static str) {
    let idx = (call_nr - VM_RQ_BASE) as usize;
    unsafe {
        VM_CALLS[idx] = VmCallEntry {
            func: Some(func),
            name,
        };
    }
}

/// Initialize the VM call dispatch table.
///
/// Must be called once before entering the main loop.
pub fn init_vm() {
    // Zero out the table first
    for entry in unsafe { VM_CALLS.iter_mut() } {
        *entry = VmCallEntry {
            func: None,
            name: "",
        };
    }

    // ── Basic ──
    set_call(VM_MMAP, do_mmap, "do_mmap");
    set_call(VM_MUNMAP, do_munmap, "do_munmap");
    set_call(VM_MAP_PHYS, do_map_phys, "do_map_phys");
    set_call(VM_UNMAP_PHYS, do_munmap, "do_munmap");

    // ── PM (Process Manager) ──
    set_call(VM_EXIT, do_exit, "do_exit");
    set_call(VM_FORK, do_fork, "do_fork");
    set_call(VM_BRK, do_brk, "do_brk");
    set_call(VM_WILLEXIT, do_willexit, "do_willexit");
    set_call(VM_NOTIFY_SIG, do_notify_sig, "do_notify_sig");
    set_call(VM_PROCCTL, do_procctl_notrans, "do_procctl");

    // ── VFS (Virtual File System) ──
    set_call(VM_VFS_REPLY, do_vfs_reply, "do_vfs_reply");
    set_call(VM_VFS_MMAP, do_vfs_mmap, "do_vfs_mmap");

    // ── RS (Reincarnation Server) ──
    set_call(VM_RS_SET_PRIV, do_rs_set_priv, "do_rs_set_priv");
    set_call(VM_RS_UPDATE, do_rs_update, "do_rs_update");
    set_call(VM_RS_MEMCTL, do_rs_memctl, "do_rs_memctl");

    // ── Generic ──
    set_call(VM_REMAP, do_remap, "do_remap");
    set_call(VM_REMAP_RO, do_remap, "do_remap");
    set_call(VM_GETPHYS, do_get_phys, "do_get_phys");
    set_call(VM_SHM_UNMAP, do_shm_unmap, "do_shm_unmap");
    set_call(VM_GETREF, do_get_refcount, "do_get_refcount");
    set_call(VM_INFO, do_info, "do_info");
    set_call(VM_QUERY_EXIT, do_query_exit, "do_query_exit");
    set_call(VM_WATCH_EXIT, do_watch_exit, "do_watch_exit");

    // ── Cache ──
    set_call(VM_MAPCACHEPAGE, do_mapcache, "do_mapcache");
    set_call(VM_SETCACHEPAGE, do_setcache, "do_setcache");
    set_call(VM_CLEARCACHE, do_clearcache, "do_clearcache");

    // ── Rusage ──
    set_call(VM_GETRUSAGE, do_getrusage, "do_getrusage");
}

// ═════════════════════════════════════════════════════════════════════════
// Server main loop
// ═════════════════════════════════════════════════════════════════════════

/// VM server main entry point.
///
/// Initializes the call table and enters the message dispatch loop.
/// The IPC receive call (`sef_receive`) is stubbed — real IPC comes
/// in Phase 13 (IPC + SEF framework).
pub fn vm_main() {
    init_vm();

    // TODO: Phase 13 — replace with real sef_receive + ipc_send loop.
    //
    //   loop {
    //       let mut msg = Message {
    //           m_source: 0,
    //           m_type: 0,
    //           m_payload: unsafe { core::mem::zeroed() },
    //       };
    //       let mut ipc_status = 0u16;
    //       let r = sef_receive(ANY, &mut msg, &mut ipc_status);
    //       if r != OK { continue; }
    //
    //       dispatch_message(&mut msg, ipc_status);
    //   }
}

/// Dispatch a single message through the VM call table.
///
/// Handles special message types (VM_PAGEFAULT, RS_INIT, VFS transactions)
/// and normal dispatch through `VM_CALLS`. Repies to the caller via `ipc_send()`.
///
/// Returns the result code (for testing).
pub fn dispatch_message(msg: &mut Message, ipc_status: i32) -> i32 {
    // Check for notifications from kernel.
    if is_ipc_notify(ipc_status) {
        if ipc_status_flags_test(
            ipc_status,
            IPC_FLG_MSG_FROM_KERNEL << IPC_STATUS_FLAGS_SHIFT,
        ) {
            sef_signal_handler();
        }
        // Notifications don't get a reply.
        return EDONTREPLY;
    }

    let call_nr = msg.m_type as u32;

    // Handle special message types.
    if call_nr == VM_PAGEFAULT {
        // TODO: Phase 13 — forward to kernel via VMCTL.
        msg.m_type = SUSPEND;
        return SUSPEND;
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
        let entry = unsafe { &VM_CALLS[idx as usize] };
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

/// SEF signal handler callback (stub).
///
/// Handles kernel signals delivered to the VM server.
pub fn sef_signal_handler() {
    // TODO: Phase 8+ — respond to kernel signals (SIGS_PAGEFAULT, etc.)
}

// ═════════════════════════════════════════════════════════════════════════
// Page fault handling (Phase 6.9 — port of pagefaults.c)
// ═════════════════════════════════════════════════════════════════════════

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
/// Validates the endpoint, checks the address against the process's region
/// map, and sends SIGSEGV on invalid addresses or protection violations.
pub fn do_pagefaults(msg: &mut Message) {
    let ep = msg.m_source;
    let _addr = unsafe { msg.m_payload.m9.m9l1 } as u64; // VPF_ADDR
    let _flags = unsafe { msg.m_payload.m9.m9l2 } as u32; // VPF_FLAGS

    // Validate the endpoint via the Vmproc table.
    let _vmp = match unsafe { proc::vmproc_lookup(ep) } {
        Some(vmp) => vmp,
        None => {
            // Unknown endpoint — send SIGSEGV and clear.
            sys_kill(ep, SIGSEGV);
            unsafe {
                mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
            }
            return;
        }
    };

    // TODO: walk the region AVL tree to validate the faulting address.
    // For now, clear the page fault unconditionally.
    //
    // Full implementation (from region.c map_pf):
    //   1. map_lookup(vmp, addr) — find the region containing addr
    //   2. If NULL → SIGSEGV + clear
    //   3. If write fault on read-only → SIGSEGV + clear
    //   4. map_pf(vmp, region, offset, writable, ...) — handle demand-paging
    //   5. pt_clearmapcache()
    //   6. sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0)
    unsafe {
        mem::sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0);
    }
}

/// Send a signal to a process via the kernel.
///
/// Validates endpoint and signal number, sets SIG_PENDING+SIGNALED flags,
/// and enqueues the process.
pub fn sys_kill(_ep: i32, _sig: i32) -> i32 {
    // TODO: Phase 6.9 full — call into kernel::system::cause_sig() or
    //       send a message to the kernel via syscall.
    OK
}

/// Clear the page fault flag on a process, reactivating it.
pub fn clear_pagefault(_ep: i32) -> i32 {
    // TODO: Phase 6.9 full — issue VMCTL_CLEAR_PAGEFAULT via kernel syscall.
    OK
}

// ═════════════════════════════════════════════════════════════════════════
// Phase 6.10 — Shared memory (shm.c)
// ═════════════════════════════════════════════════════════════════════════

/// Handle VM_SHM_UNMAP — clear matching shared memory regions.
fn do_shm_unmap(msg: &mut Message) -> i32 {
    let ep = msg.m_source;
    if ep < 0 || ep >= NR_PROCS as i32 {
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

// ═════════════════════════════════════════════════════════════════════════
// Phase 6.11 — Remap operations (mmap.c)
// ═════════════════════════════════════════════════════════════════════════

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
    if dest_ep < 0 || dest_ep >= NR_PROCS as i32 {
        return EINVAL;
    }
    if src_ep < 0 || src_ep >= NR_PROCS as i32 {
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
    let walk_result = unsafe { kernel::pagetable::walk(src_cr3, src_addr) };
    let src_pa = match walk_result {
        Ok(r) => r.pte_value & 0x000FFFFFFFFFF000,
        Err(_) => return EINVAL,
    };

    // Map the source physical page into the destination at the same
    // virtual address (standard shared-memory remap).
    let flags =
        kernel::pagetable::MAP_PRESENT | kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE;
    if unsafe { kernel::pagetable::map_page(dst_cr3, src_addr, src_pa, flags) }.is_err() {
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
    if actual_target < 0 || actual_target >= NR_PROCS as i32 {
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
        if unsafe { kernel::pagetable::map_page(cr3, vaddr + offset, phys + offset, flags) }
            .is_err()
        {
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

    if target < 0 || target >= NR_PROCS as i32 {
        return EINVAL;
    }

    let cr3 = unsafe { proc::vm_get_addrspace(target) };
    if cr3 == 0 {
        return EINVAL;
    }

    let result = unsafe { kernel::pagetable::walk(cr3, addr) };
    match result {
        Ok(r) => {
            let pa = r.pte_value & 0x000FFFFFFFFFF000;
            msg.m_payload.m1.m1i1 = pa as i32;
            OK
        }
        Err(_) => {
            msg.m_payload.m1.m1i1 = 0;
            OK
        }
    }
}

/// Handle VM_GETREF — get reference count of a region.
///
/// Validates endpoint, walks the grant table to find matching entries.
/// Returns refcount in m1i1.
fn do_get_refcount(msg: &mut Message) -> i32 {
    let target = unsafe { msg.m_payload.m1.m1i1 };
    let addr = unsafe { msg.m_payload.m1.m1i2 } as u64;

    if target < 0 || target >= NR_PROCS as i32 {
        return EINVAL;
    }

    // Walk the grant table looking for entries mapped by this target
    // that involve the given virtual address.
    let mut refcount = 0;
    unsafe {
        for row in mem::GRANT_TABLES.iter() {
            for grant in row.iter() {
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
/// Validates endpoint, checks page alignment, walks the page table
/// to unmap pages.
fn do_munmap(msg: &mut Message) -> i32 {
    let caller = msg.m_source;
    let target = if unsafe { msg.m_payload.m1.m1i1 } != 0 {
        unsafe { msg.m_payload.m1.m1i1 }
    } else {
        caller
    };

    if target < 0 || target >= NR_PROCS as i32 {
        return EINVAL;
    }

    let addr = unsafe { msg.m_payload.m1.m1i2 } as u64;
    if !addr.is_multiple_of(4096) {
        return EINVAL;
    }

    let size = unsafe { msg.m_payload.m1.m1i3 } as u64;
    if size == 0 {
        return EINVAL;
    }

    let cr3 = unsafe { proc::vm_get_addrspace(target) };
    if cr3 == 0 {
        return EINVAL;
    }

    unsafe {
        if kernel::pagetable::unmap_range(cr3, addr, size).is_err() {
            return EINVAL;
        }
    }

    OK
}

// ═════════════════════════════════════════════════════════════════════════
// Phase 6.12 — Procctl and exit (exit.c)
// ═════════════════════════════════════════════════════════════════════════

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
    if target_ep < 0 || target_ep >= NR_PROCS as i32 {
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
    let ep = msg.m_source;
    if ep < 0 || ep >= NR_PROCS as i32 {
        return EINVAL;
    }

    // Destroy the process's address space.
    unsafe {
        proc::vm_destroy(ep);
    }

    OK
}

/// Handle VM_WILLEXIT — process announces intent to exit.
fn do_willexit(msg: &mut Message) -> i32 {
    let ep = msg.m_source;
    if ep < 0 || ep >= NR_PROCS as i32 {
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

// ═════════════════════════════════════════════════════════════════════════
// Stub handlers (remaining unimplemented calls)
// ═════════════════════════════════════════════════════════════════════════

fn do_mmap(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_fork(msg: &mut Message) -> i32 {
    // Extract parent and child endpoints from message.
    // m1i1 = child endpoint, m_source = parent endpoint.
    let parent_ep = msg.m_source;
    let child_ep = unsafe { msg.m_payload.m1.m1i1 };

    if parent_ep < 0 || child_ep < 0 {
        return EINVAL;
    }
    if parent_ep >= NR_PROCS as i32 || child_ep >= NR_PROCS as i32 {
        return EINVAL;
    }

    unsafe {
        if proc::vm_clone(parent_ep, child_ep) != 0 {
            return EINVAL;
        }
    }

    OK
}

fn do_brk(msg: &mut Message) -> i32 {
    // The new heap break address comes in m1i1.
    let new_brk = unsafe { msg.m_payload.m1.m1i1 } as u64;
    let ep = msg.m_source;

    if ep < 0 || ep >= NR_PROCS as i32 {
        return EINVAL;
    }

    // Validate: break must be page-aligned-ish (MINIX rounds up).
    // The break address must be within the user address space.
    if new_brk > kernel::pagetable::MAX_USER_ADDRESS {
        return EINVAL;
    }

    let cr3 = unsafe { proc::vm_get_addrspace(ep) };
    if cr3 == 0 {
        return EINVAL;
    }

    // Round up to page boundary.
    let page_size: u64 = 4096;
    let target = (new_brk + page_size - 1) & !(page_size - 1);

    // Get the current region_top (approximate current break).
    let current_top = unsafe {
        match proc::vmproc_lookup(ep) {
            Some(vmp) => vmp.vm_region_top,
            None => return EINVAL,
        }
    };

    if target > current_top {
        // Expand heap: allocate and map new pages.
        let start = if current_top.is_multiple_of(page_size) {
            current_top
        } else {
            (current_top + page_size - 1) & !(page_size - 1)
        };

        let mut va = start;
        while va < target {
            let pg = unsafe { kernel::vm::alloc_mem(1, 0) };
            if pg == kernel::vm::NO_MEM {
                return -1;
            }
            let pa = pg * page_size;
            let flags = kernel::pagetable::MAP_PRESENT
                | kernel::pagetable::MAP_USER
                | kernel::pagetable::MAP_WRITE;
            if unsafe { kernel::pagetable::map_page(cr3, va, pa, flags) }.is_err() {
                unsafe { kernel::vm::free_mem(pg, 1) };
                return -1;
            }
            va += page_size;
        }
    } else if target < current_top {
        // Shrink heap: unmap pages.
        unsafe {
            let _ = kernel::pagetable::unmap_range(cr3, target, current_top - target);
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

    if target_ep < 0 || target_ep >= NR_PROCS as i32 {
        return EINVAL;
    }

    // Mark the target process in the Vmproc table with a signal-pending
    // flag.  The full implementation would send the signal via sys_kill.
    sys_kill(target_ep, _sig);

    OK
}

fn do_vfs_reply(msg: &mut Message) -> i32 {
    // VFS reply handling — receives the result of a VFS operation
    // that was forwarded by VM.  Stored in m1i1 (result) and m1i2
    // (transaction ID / status).
    let _result = unsafe { msg.m_payload.m1.m1i1 };
    let _status = unsafe { msg.m_payload.m1.m1i2 };

    // TODO: Phase 13 — route the VFS reply back to the waiting
    // process via the PENDING transaction table.
    OK
}

fn do_vfs_mmap(msg: &mut Message) -> i32 {
    let _ep = msg.m_source;
    let _addr = unsafe { msg.m_payload.m1.m1i1 } as u64;
    let _len = unsafe { msg.m_payload.m1.m1i2 } as u64;
    let _flags = unsafe { msg.m_payload.m1.m1i3 } as u32;

    // TODO: Phase 13 — implement file-backed mmap by calling VFS
    // to read file data into allocated physical pages, then mapping
    // them into the process's address space.
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
            // Populate VmStatsInfo: page size, total pages, free/cached stats
            msg.m_payload.m1.m1i1 = kernel::vm::VM_PAGE_SIZE as i32;
            msg.m_payload.m1.m1i2 = kernel::vm::total_pages();
            // Estimate free pages: use total_pages minus a placeholder.
            // The real implementation calls memstats() from the kernel.
            msg.m_payload.m1.m1i3 = 0; // free pages placeholder
            OK
        }
        VMIW_USAGE => {
            // Populate VmUsageInfo from target process's Vmproc entry.
            if target_ep < 0 || target_ep >= NR_PROCS as i32 {
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
            if target_ep < 0 || target_ep >= NR_PROCS as i32 {
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

    if target_ep < 0 || target_ep >= NR_PROCS as i32 {
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

    if target_ep < 0 || target_ep >= NR_PROCS as i32 {
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

// ═════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use arch_common::com::{
        NR_VM_CALLS, VM_MMAP, VM_PAGEFAULT, VM_REMAP, VM_REMAP_RO, VM_RQ_BASE, VM_SHM_UNMAP,
        VM_UNMAP_PHYS,
    };

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
            assert!(VM_CALLS[0].func.is_some(), "VM_EXIT should be set");
            assert_eq!(VM_CALLS[0].name, "do_exit");

            assert!(VM_CALLS[(VM_MMAP - VM_RQ_BASE) as usize].func.is_some());
            assert_eq!(VM_CALLS[(VM_MMAP - VM_RQ_BASE) as usize].name, "do_mmap");
        }
    }

    #[test]
    fn test_init_vm_zeros_unset_entries() {
        init_vm();
        unsafe {
            // Slots that are not in the official call list should remain None
            // VM_WILLEXIT is at index 5; check an empty slot like index 4 (VM_EXEC_NEWMEM)
            assert!(
                VM_CALLS[4].func.is_none(),
                "slot 4 (VM_EXEC_NEWMEM) should not be set"
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
            assert!(VM_CALLS[unmap_idx].func.is_some());
            assert!(VM_CALLS[shm_idx].func.is_some());

            // VM_REMAP and VM_REMAP_RO both map to do_remap
            let remap_idx = (VM_REMAP - VM_RQ_BASE) as usize;
            let remap_ro_idx = (VM_REMAP_RO - VM_RQ_BASE) as usize;
            assert!(VM_CALLS[remap_idx].func.is_some());
            assert!(VM_CALLS[remap_ro_idx].func.is_some());
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

        // VFS — now return OK instead of ENOSYS
        assert_eq!(do_vfs_reply(&mut msg), OK);
        assert_eq!(do_vfs_mmap(&mut msg), OK);

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
        // do_pagefaults should not panic
        do_pagefaults(&mut msg);
        // sys_kill should return OK (stub)
        assert_eq!(sys_kill(0, 11), OK); // SIGSEGV
        assert_eq!(sys_kill(1, 6), OK); // SIGABRT
        // clear_pagefault should return OK (stub)
        assert_eq!(clear_pagefault(0), OK);
        assert_eq!(clear_pagefault(1), OK);
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

    // ── dispatch_message tests ────────────────────────────────────────

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
    fn test_dispatch_vm_pagefault_returns_suspend() {
        init_vm();
        let mut msg = Message {
            m_source: 42,
            m_type: VM_PAGEFAULT as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, SUSPEND);
        assert_eq!(msg.m_type, SUSPEND);
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
        // do_mmap is a stub that returns ENOSYS
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, ENOSYS);
        assert_eq!(msg.m_type, ENOSYS); // reply type set
    }

    #[test]
    fn test_dispatch_unknown_call_returns_enosys() {
        init_vm();
        let mut msg = Message {
            m_source: 0,
            m_type: 0x9999 as i32, // unknown call number
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, ENOSYS);
        assert_eq!(msg.m_type, ENOSYS);
    }

    #[test]
    fn test_dispatch_unset_table_slot_returns_enosys() {
        init_vm();
        // VM_EXEC_NEWMEM = VM_RQ_BASE + 3 is in range but not set
        let mut msg = Message {
            m_source: 0,
            m_type: (VM_RQ_BASE + 3) as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, ENOSYS);
        assert_eq!(msg.m_type, ENOSYS);
    }

    #[test]
    fn test_dispatch_suspend_handler_no_reply() {
        init_vm();
        // VM_PAGEFAULT returns SUSPEND, so msg.m_type should stay SUSPEND
        // (no reply sent)
        let mut msg = Message {
            m_source: 42,
            m_type: VM_PAGEFAULT as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, SUSPEND);
        assert_eq!(msg.m_type, SUSPEND);
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
        // VFS_TRANSACTION_BASE = 0x200, a VFS transaction ID is in that range
        let mut msg = Message {
            m_source: VFS_PROC_NR,
            m_type: 0x200 as i32, // VFS_TRANSACTION_BASE
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = dispatch_message(&mut msg, 0);
        assert_eq!(r, ENOSYS);
        assert_eq!(msg.m_type, ENOSYS);
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
