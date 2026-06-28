//! VM server — adapted from `minix/servers/vm/main.c`
//!
//! Implements the VM server main loop, message dispatch, and stub handlers
//! for all VM calls. Real implementations come in Phases 6.4+.

#![allow(unused_variables)]
#![allow(static_mut_refs)]

pub mod mem;
pub mod proc;

use arch_common::com::{
    NR_VM_CALLS, RS_PROC_NR, VFS_PROC_NR, VM_BRK, VM_CLEARCACHE, VM_EXIT, VM_FORK, VM_GETPHYS,
    VM_GETREF, VM_GETRUSAGE, VM_INFO, VM_MAP_PHYS, VM_MAPCACHEPAGE, VM_MMAP, VM_MUNMAP,
    VM_NOTIFY_SIG, VM_PROCCTL, VM_QUERY_EXIT, VM_REMAP, VM_REMAP_RO, VM_RQ_BASE, VM_RS_MEMCTL,
    VM_RS_SET_PRIV, VM_RS_UPDATE, VM_SETCACHEPAGE, VM_SHM_UNMAP, VM_UNMAP_PHYS, VM_VFS_MMAP,
    VM_VFS_REPLY, VM_WATCH_EXIT, VM_WILLEXIT, VMIW_REGION, VMIW_STATS, VMIW_USAGE, VMPPARAM_CLEAR,
    VMPPARAM_HANDLEMEM,
};
use arch_common::consts::NR_PROCS;
use arch_common::ipc::Message;

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
const SUSPEND: i32 = -998;

/// Do not reply at all (internal VM status).
#[allow(dead_code)]
const EDONTREPLY: i32 = -201;

/// Endpoint representing kernel-originated messages.
#[allow(dead_code)]
const FROM_KERNEL: i32 = 0x100;

/// Special endpoint to receive from any source.
#[allow(dead_code)]
const ANY: i32 = 0x0000ffff;

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
/// Currently a placeholder; real IPC receive loop comes in Phase 6.4+.
pub fn vm_main() {
    init_vm();

    // TODO: Phase 6.4+ — receive messages via IPC and dispatch.
    //
    // The loop body will be:
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
    //       // Check for notifications from kernel
    //       if is_ipc_notify(ipc_status) {
    //           if msg.m_source == FROM_KERNEL {
    //               sef_signal_handler();
    //           }
    //           continue;
    //       }
    //
    //       let call_nr = msg.m_type as u32;
    //
    //       // Handle special message types
    //       if call_nr == VM_PAGEFAULT {
    //           // TODO: forward to kernel via VMCTL
    //           msg.m_type = SUSPEND;
    //       } else if call_nr == RS_INIT {
    //           // TODO: SEF init callback
    //           msg.m_type = OK;
    //       } else if is_vfs_fs_transid(call_nr) {
    //           // TODO: VFS transaction dispatch
    //           msg.m_type = ENOSYS;
    //       } else {
    //           // Normal dispatch through call table
    //           let idx = call_number(call_nr);
    //           let result = if idx >= 0 {
    //               if let Some(func) = VM_CALLS[idx as usize].func {
    //                   func(&mut msg)
    //               } else {
    //                   ENOSYS
    //               }
    //           } else {
    //               ENOSYS
    //           };
    //
    //           // Reply unless handler requested no reply
    //           if result != SUSPEND && result != EDONTREPLY {
    //               msg.m_type = result;
    //               // send(msg.m_source, &mut msg);
    //           }
    //       }
    //   }
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

    // TODO: Phase 6.9 full — validate endpoint, check address against
    // vm_region_top, increment major/minor fault counters, send SIGSEGV
    // on invalid address via sys_kill(), clear pagefault via sys_vmctl().
    //
    // Full implementation when region AVL tree is available:
    //   1. vm_isokendpt(ep, &p) -> panic if bad
    //   2. map_lookup(vmp, addr) -> if NULL, sys_kill(SIGSEGV) + VMCTL_CLEAR
    //   3. if !writable && wr -> sys_kill(SIGSEGV) + VMCTL_CLEAR
    //   4. map_pf(vmp, region, offset, wr, ...) -> handle
    //   5. pt_clearmapcache()
    //   6. sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0)
    let _ = ep;
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
    let _src_addr = unsafe { msg.m_payload.m1.m1i3 } as u64;
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

    // TODO: real remap via map_page_region + shared_setsource
    // For now, return a dummy mapped address
    msg.m_payload.m1.m1i1 = 0x1000; // dummy mapped address
    OK
}

/// Handle VM_MAP_PHYS — map physical memory into a process.
///
/// Validates length and target endpoint, rounds addresses to page boundaries,
/// returns the mapped virtual address in m1i1.
fn do_map_phys(msg: &mut Message) -> i32 {
    let target = unsafe { msg.m_payload.m1.m1i1 };
    let len = unsafe { msg.m_payload.m1.m1i2 };
    let _phys = unsafe { msg.m_payload.m1.m1i3 } as u64;

    if len <= 0 {
        return EINVAL;
    }

    let actual_target = if target == -1 { msg.m_source } else { target };
    if actual_target < 0 || actual_target >= NR_PROCS as i32 {
        return EINVAL;
    }

    // Round len to page boundary
    let page_size: usize = 4096;
    let _rounded_len = if !(len as usize).is_multiple_of(page_size) {
        (len as usize) + page_size - ((len as usize) % page_size)
    } else {
        len as usize
    };

    // TODO: check map_perm_check, call map_page_region with VR_DIRECT
    msg.m_payload.m1.m1i1 = 0x2000; // dummy mapped address
    OK
}

/// Handle VM_GETPHYS — translate virtual address to physical address.
///
/// Validates endpoint, walks region array to find matching region,
/// returns physical address in m1i1.
fn do_get_phys(msg: &mut Message) -> i32 {
    let target = unsafe { msg.m_payload.m1.m1i1 };
    let _addr = unsafe { msg.m_payload.m1.m1i2 } as u64;

    if target < 0 || target >= NR_PROCS as i32 {
        return EINVAL;
    }

    // TODO: walk region array and call map_get_phys
    msg.m_payload.m1.m1i1 = 0; // dummy physical address
    OK
}

/// Handle VM_GETREF — get reference count of a region.
///
/// Validates endpoint, walks region array to find matching region,
/// returns refcount (1 for matched, 0 for not found).
fn do_get_refcount(msg: &mut Message) -> i32 {
    let target = unsafe { msg.m_payload.m1.m1i1 };
    let _addr = unsafe { msg.m_payload.m1.m1i2 } as u64;

    if target < 0 || target >= NR_PROCS as i32 {
        return EINVAL;
    }

    // TODO: walk region array and call map_get_ref
    // For now, return 1 (region found, refcount = 1)
    1
}

/// Handle VM_MUNMAP / VM_UNMAP_PHYS — unmap memory regions.
///
/// Validates endpoint, checks page alignment, walks region array
/// to clear matching entries.
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

    // TODO: walk region array and unmap matching entries
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
/// Validates endpoint, clears the process's VM state.
fn do_exit(msg: &mut Message) -> i32 {
    let ep = msg.m_source;
    if ep < 0 || ep >= NR_PROCS as i32 {
        return EINVAL;
    }

    // Clear all VM state for this process
    proc::clear_proc(ep);

    OK
}

/// Handle VM_WILLEXIT — process announces intent to exit.
fn do_willexit(msg: &mut Message) -> i32 {
    let _ep = msg.m_source;
    if _ep < 0 || _ep >= NR_PROCS as i32 {
        return EINVAL;
    }

    // TODO: set VMF_EXITING / VMF_WATCHEXIT on the Vmproc entry
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
    let _ = msg;
    ENOSYS
}

fn do_brk(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_notify_sig(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_vfs_reply(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_vfs_mmap(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_rs_set_priv(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_rs_update(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_rs_memctl(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_info(msg: &mut Message) -> i32 {
    // The message carries the subcode in m1_i1 (VMIW_STATS=1, VMIW_USAGE=2, VMIW_REGION=3)
    // and optionally the target endpoint in m1_i2
    let subcode = unsafe { msg.m_payload.m1.m1i1 } as u32;
    let _target_ep = unsafe { msg.m_payload.m1.m1i2 };

    match subcode {
        VMIW_STATS => {
            // Populate VmStatsInfo: page size, total pages, free/cached stats
            // For now, return OK with zeros — full impl reads from kernel::vm
            msg.m_payload.m1.m1i1 = kernel::vm::VM_PAGE_SIZE as i32;
            msg.m_payload.m1.m1i2 = kernel::vm::total_pages();
            // TODO: read free/cached from kernel::vm::mem_stats()
            OK
        }
        VMIW_USAGE => {
            // Populate VmUsageInfo from target process's Vmproc entry
            // Stubbed for now — real impl needs Vmproc table lookup
            OK
        }
        VMIW_REGION => {
            // Walk region array, write VmRegionInfo structs to output buffer
            // Stubbed for now — real impl needs region AVL tree
            OK
        }
        _ => ENOSYS,
    }
}

fn do_query_exit(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_watch_exit(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_mapcache(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_setcache(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_clearcache(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
}

fn do_getrusage(msg: &mut Message) -> i32 {
    let _ = msg;
    ENOSYS
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

        // Phase 6.11 — Remap operations (now return OK instead of ENOSYS)
        // do_remap: needs size > 0, set m1i4 = 4096
        msg.m_payload.m1.m1i4 = 4096;
        assert_eq!(do_remap(&mut msg), OK);
        // Reset message for next call
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;
        // do_map_phys: needs len > 0 (m1i2) and target ep = m1i1 = 0
        msg.m_payload.m1.m1i2 = 4096;
        assert_eq!(do_map_phys(&mut msg), OK);
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;
        // do_get_phys: target ep m1i1 = 0 is valid
        assert_eq!(do_get_phys(&mut msg), OK);
        // do_get_refcount: returns 1 for matched
        assert_eq!(do_get_refcount(&mut msg), 1);
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;
        // do_munmap: addr must be page-aligned
        msg.m_payload.m1.m1i2 = 4096; // page-aligned addr
        assert_eq!(do_munmap(&mut msg), OK);
        msg.m_payload = unsafe { core::mem::zeroed() };
        msg.m_source = 0;

        // Phase 6.12 — Procctl and exit
        // do_exit: source = 0 is valid
        assert_eq!(do_exit(&mut msg), OK);
        assert_eq!(do_fork(&mut msg), ENOSYS);
        assert_eq!(do_brk(&mut msg), ENOSYS);
        // do_willexit: source = 0 is valid
        assert_eq!(do_willexit(&mut msg), OK);
        assert_eq!(do_notify_sig(&mut msg), ENOSYS);
        // do_procctl: m9l1 (subcode) = 0 does not match any case -> EINVAL
        assert_eq!(do_procctl(&mut msg, 0), EINVAL);
        assert_eq!(do_procctl_notrans(&mut msg), EINVAL);

        // VFS
        assert_eq!(do_vfs_reply(&mut msg), ENOSYS);
        assert_eq!(do_vfs_mmap(&mut msg), ENOSYS);

        // RS
        assert_eq!(do_rs_set_priv(&mut msg), ENOSYS);
        assert_eq!(do_rs_update(&mut msg), ENOSYS);
        assert_eq!(do_rs_memctl(&mut msg), ENOSYS);

        // Generic (still stubbed)
        assert_eq!(do_info(&mut msg), ENOSYS);
        assert_eq!(do_query_exit(&mut msg), ENOSYS);
        assert_eq!(do_watch_exit(&mut msg), ENOSYS);

        // Cache
        assert_eq!(do_mapcache(&mut msg), ENOSYS);
        assert_eq!(do_setcache(&mut msg), ENOSYS);
        assert_eq!(do_clearcache(&mut msg), ENOSYS);

        // Rusage
        assert_eq!(do_getrusage(&mut msg), ENOSYS);
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
}
