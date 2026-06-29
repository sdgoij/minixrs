//! Kernel debugging — adapted from `minix/kernel/debug.c`
//!
//! Provides flag-to-string conversion, process printing, debug IPC
//! hooks (with stats matrix), and the enhanced run queue checker.
//!
//! All functions are `no_std` compatible — they write into fixed-size
//! buffers rather than using formatted I/O.

use core::arch::asm;

use crate::r#priv::NR_SYS_CALLS;
use crate::proc::*;

// ─────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────

/// IPC stats matrix size (NR_PROCS + 1, with +1 for kernel slot).
const IPCPROCS: usize = NR_PROCS_TOTAL;

/// Kernel IPC slot number.
const KERNELIPC: usize = NR_PROCS_TOTAL - 1;

/// Number of top-talker slots for stats printing.
const PRINTSLOTS: usize = 20;

// ─────────────────────────────────────────────────────────────────────────
// IPC statistics matrix
// ─────────────────────────────────────────────────────────────────────────

/// IPC message count matrix.
///
/// `messages[src][dst]` = number of messages sent from `src` to `dst`.
/// Slot `KERNELIPC` is used for kernel-originated messages.
pub static mut IPC_MESSAGES: [[u32; IPCPROCS]; IPCPROCS] = [[0u32; IPCPROCS]; IPCPROCS];

/// Top-talker winners table.
#[derive(Debug, Clone, Copy, Default)]
pub struct IpcStatsEntry {
    pub src: usize,
    pub dst: usize,
    pub messages: u32,
}

/// Get the top IPC talkers since the last reset.
pub fn ipc_top_talkers() -> [IpcStatsEntry; PRINTSLOTS] {
    let mut winners = [IpcStatsEntry::default(); PRINTSLOTS];
    unsafe {
        let matrix = core::ptr::addr_of!(IPC_MESSAGES).cast::<u32>();
        for src in 0..IPCPROCS {
            for dst in 0..IPCPROCS {
                let n = *matrix.add(src * IPCPROCS + dst);
                if n == 0 {
                    continue;
                }
                // Find insertion position
                let mut w = PRINTSLOTS;
                while w > 0 && n > winners[w - 1].messages {
                    w -= 1;
                }
                if w >= PRINTSLOTS {
                    continue;
                }
                // Shift and insert
                let rem = PRINTSLOTS - w - 1;
                if rem > 0 {
                    winners.copy_within(w..PRINTSLOTS - 1, w + 1);
                }
                winners[w] = IpcStatsEntry {
                    src,
                    dst,
                    messages: n,
                };
            }
        }
    }
    winners
}

/// Reset the IPC message matrix.
pub fn ipc_reset_stats() {
    unsafe {
        let matrix = core::ptr::addr_of_mut!(IPC_MESSAGES).cast::<u32>();
        for i in 0..(IPCPROCS * IPCPROCS) {
            *matrix.add(i) = 0;
        }
    }
}

/// Clear IPC stats for a specific process slot.
///
/// # Safety
///
/// `slot` must be < `IPCPROCS` or the call is a no-op.
pub unsafe fn ipc_clear_slot(slot: usize) {
    if slot >= IPCPROCS {
        return;
    }
    unsafe {
        let matrix = core::ptr::addr_of_mut!(IPC_MESSAGES).cast::<u32>();
        for i in 0..IPCPROCS {
            *matrix.add(slot * IPCPROCS + i) = 0;
            *matrix.add(i * IPCPROCS + slot) = 0;
        }
    }
}

/// Record an IPC message in the stats matrix.
unsafe fn ipc_record(src_slot: usize, dst_slot: usize) {
    if src_slot >= IPCPROCS || dst_slot >= IPCPROCS {
        return;
    }
    unsafe {
        let matrix = core::ptr::addr_of_mut!(IPC_MESSAGES).cast::<u32>();
        let idx = src_slot * IPCPROCS + dst_slot;
        let val = *matrix.add(idx);
        *matrix.add(idx) = val.wrapping_add(1);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// IPC message type identification (partial — covers common types)
// ─────────────────────────────────────────────────────────────────────────

/// Return a human-readable name for a message type, if known.
pub fn mtypename(mtype: i32) -> Option<&'static str> {
    // Kernel call range
    if mtype >= crate::system::KERNEL_CALL
        && mtype < crate::system::KERNEL_CALL + NR_SYS_CALLS as i32
    {
        let idx = (mtype - crate::system::KERNEL_CALL) as usize;
        unsafe {
            let names = crate::glo::IPC_CALL_NAMES.get();
            return (*names)[idx];
        }
    }
    // Common notification types
    match mtype {
        -10 => Some("NOTIFY_MESSAGE"),
        -11 => Some("SCHEDULING_NO_QUANTUM"),
        0 => Some("OK"),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Flag-to-string conversion
// ─────────────────────────────────────────────────────────────────────────

/// Convert RTS flags to a string representation.
pub fn rtsflagstr(flags: u32, buf: &mut [u8]) -> &str {
    let mut pos = 0;
    macro_rules! append {
        ($name:expr) => {
            if pos > 0 && pos < buf.len() {
                buf[pos] = b' ';
                pos += 1;
            }
            let name = $name.as_bytes();
            let end = (pos + name.len()).min(buf.len());
            buf[pos..end].copy_from_slice(&name[..end - pos]);
            pos = end;
        };
    }
    if flags == 0 {
        if !buf.is_empty() {
            buf[0] = b'0';
            pos = 1;
        }
    } else {
        if flags & RtsFlags::SLOT_FREE.bits() != 0 {
            append!("RTS_SLOT_FREE");
        }
        if flags & RtsFlags::PROC_STOP.bits() != 0 {
            append!("RTS_PROC_STOP");
        }
        if flags & RtsFlags::SENDING.bits() != 0 {
            append!("RTS_SENDING");
        }
        if flags & RtsFlags::RECEIVING.bits() != 0 {
            append!("RTS_RECEIVING");
        }
        if flags & RtsFlags::SIGNALED.bits() != 0 {
            append!("RTS_SIGNALED");
        }
        if flags & RtsFlags::SIG_PENDING.bits() != 0 {
            append!("RTS_SIG_PENDING");
        }
        if flags & RtsFlags::P_STOP.bits() != 0 {
            append!("RTS_P_STOP");
        }
        if flags & RtsFlags::NO_PRIV.bits() != 0 {
            append!("RTS_NO_PRIV");
        }
        if flags & RtsFlags::NO_ENDPOINT.bits() != 0 {
            append!("RTS_NO_ENDPOINT");
        }
        if flags & RtsFlags::VMINHIBIT.bits() != 0 {
            append!("RTS_VMINHIBIT");
        }
        if flags & RtsFlags::PAGEFAULT.bits() != 0 {
            append!("RTS_PAGEFAULT");
        }
        if flags & RtsFlags::VMREQUEST.bits() != 0 {
            append!("RTS_VMREQUEST");
        }
        if flags & RtsFlags::VMREQTARGET.bits() != 0 {
            append!("RTS_VMREQTARGET");
        }
        if flags & RtsFlags::PREEMPTED.bits() != 0 {
            append!("RTS_PREEMPTED");
        }
        if flags & RtsFlags::NO_QUANTUM.bits() != 0 {
            append!("RTS_NO_QUANTUM");
        }
        if flags & RtsFlags::BOOTINHIBIT.bits() != 0 {
            append!("RTS_BOOTINHIBIT");
        }
    }
    let len = pos.min(buf.len());
    core::str::from_utf8(&buf[..len]).unwrap_or("(bad utf-8)")
}

/// Convert misc flags to a string representation.
pub fn miscflagstr(flags: u32, buf: &mut [u8]) -> &str {
    let mut pos = 0;
    macro_rules! append {
        ($name:expr) => {
            if pos > 0 && pos < buf.len() {
                buf[pos] = b' ';
                pos += 1;
            }
            let name = $name.as_bytes();
            let end = (pos + name.len()).min(buf.len());
            buf[pos..end].copy_from_slice(&name[..end - pos]);
            pos = end;
        };
    }
    if flags == 0 {
        if !buf.is_empty() {
            buf[0] = b'0';
            pos = 1;
        }
    } else {
        if flags & MiscFlags::REPLY_PEND.bits() != 0 {
            append!("MF_REPLY_PEND");
        }
        if flags & MiscFlags::VIRT_TIMER.bits() != 0 {
            append!("MF_VIRT_TIMER");
        }
        if flags & MiscFlags::PROF_TIMER.bits() != 0 {
            append!("MF_PROF_TIMER");
        }
        if flags & MiscFlags::KCALL_RESUME.bits() != 0 {
            append!("MF_KCALL_RESUME");
        }
        if flags & MiscFlags::DELIVERMSG.bits() != 0 {
            append!("MF_DELIVERMSG");
        }
        if flags & MiscFlags::SIG_DELAY.bits() != 0 {
            append!("MF_SIG_DELAY");
        }
        if flags & MiscFlags::SC_ACTIVE.bits() != 0 {
            append!("MF_SC_ACTIVE");
        }
        if flags & MiscFlags::SC_DEFER.bits() != 0 {
            append!("MF_SC_DEFER");
        }
        if flags & MiscFlags::SC_TRACE.bits() != 0 {
            append!("MF_SC_TRACE");
        }
        if flags & MiscFlags::FPU_INITIALIZED.bits() != 0 {
            append!("MF_FPU_INITIALIZED");
        }
        if flags & MiscFlags::SENDING_FROM_KERNEL.bits() != 0 {
            append!("MF_SENDING_FROM_KERNEL");
        }
        if flags & MiscFlags::CONTEXT_SET.bits() != 0 {
            append!("MF_CONTEXT_SET");
        }
        if flags & MiscFlags::SPROF_SEEN.bits() != 0 {
            append!("MF_SPROF_SEEN");
        }
        if flags & MiscFlags::FLUSH_TLB.bits() != 0 {
            append!("MF_FLUSH_TLB");
        }
        if flags & MiscFlags::SENDA_VM_MISS.bits() != 0 {
            append!("MF_SENDA_VM_MISS");
        }
        if flags & MiscFlags::STEP.bits() != 0 {
            append!("MF_STEP");
        }
    }
    let len = pos.min(buf.len());
    core::str::from_utf8(&buf[..len]).unwrap_or("(bad utf-8)")
}

// ─────────────────────────────────────────────────────────────────────────
// Scheduler name
// ─────────────────────────────────────────────────────────────────────────

/// Return the name of a process's scheduler.
///
/// # Safety
///
/// `rp` must point to a valid `Proc` or be null.
pub unsafe fn schedulerstr(rp: *mut Proc, buf: &mut [u8]) -> &str {
    unsafe {
        if rp.is_null() || (*rp).p_scheduler.is_null() {
            let name = b"KERNEL";
            let len = name.len().min(buf.len());
            buf[..len].copy_from_slice(&name[..len]);
            return core::str::from_utf8(&buf[..len]).unwrap_or("KERNEL");
        }
        let sched = (*rp).p_scheduler;
        let mut pos = 0;
        for &c in &(*sched).p_name {
            if c == 0 || pos >= buf.len() {
                break;
            }
            buf[pos] = c as u8;
            pos += 1;
        }
        core::str::from_utf8(&buf[..pos]).unwrap_or("(invalid)")
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Process pointer validation
// ─────────────────────────────────────────────────────────────────────────

/// Check if a process pointer is valid.
///
/// # Safety
///
/// `p` must be either null or point within the process table bounds.
pub unsafe fn proc_ptr_ok(p: *const Proc) -> bool {
    unsafe {
        if p.is_null() {
            return false;
        }
        let base = crate::table::proc_table_base();
        let end = base.add(NR_PROCS_TOTAL);
        if p < base || p >= end {
            return false;
        }
        if !(p as usize).is_multiple_of(core::mem::align_of::<Proc>()) {
            return false;
        }
        core::ptr::addr_of!((*p).p_magic).read() == PMAGIC
    }
}

// ─────────────────────────────────────────────────────────────────────────
// print_proc
// ─────────────────────────────────────────────────────────────────────────

/// Write a human-readable process description into the provided buffer.
///
/// # Safety
///
/// `rp` must point to a valid `Proc` or be null.
pub unsafe fn print_proc(rp: *mut Proc, buf: &mut [u8]) -> &str {
    if rp.is_null() {
        let msg = b"(null)";
        let len = msg.len().min(buf.len());
        buf[..len].copy_from_slice(&msg[..len]);
        return core::str::from_utf8(&buf[..len]).unwrap_or("");
    }
    unsafe {
        let mut pos = 0;
        macro_rules! write_str {
            ($s:expr) => {
                let s = $s.as_bytes();
                let end = (pos + s.len()).min(buf.len());
                buf[pos..end].copy_from_slice(&s[..end - pos]);
                pos = end;
            };
        }
        // Process number
        let nr = (*rp).p_nr;
        if nr >= 0 {
            let s = itoa(nr as u32, &mut buf[pos..]);
            pos += s.len();
        } else {
            buf[pos] = b'-';
            pos += 1;
            let s = itoa((-nr) as u32, &mut buf[pos..]);
            pos += s.len();
        }
        write_str!(": ");
        // Name
        for &c in &(*rp).p_name {
            if c == 0 || pos >= buf.len() {
                break;
            }
            buf[pos] = c as u8;
            pos += 1;
        }
        write_str!(" ep=");
        // Endpoint
        let ep = (*rp).p_endpoint;
        if ep >= 0 {
            let s = itoa(ep as u32, &mut buf[pos..]);
            pos += s.len();
        } else {
            buf[pos] = b'-';
            pos += 1;
            let s = itoa((-ep) as u32, &mut buf[pos..]);
            pos += s.len();
        }
        let len = pos.min(buf.len());
        core::str::from_utf8(&buf[..len]).unwrap_or("")
    }
}

/// Simple integer-to-ASCII conversion (no_std compatible).
fn itoa(mut n: u32, buf: &mut [u8]) -> &str {
    if buf.is_empty() {
        return "";
    }
    if n == 0 {
        buf[0] = b'0';
        return "0";
    }
    let mut pos = 0;
    while n > 0 && pos < buf.len() {
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
        pos += 1;
    }
    buf[..pos].reverse();
    let len = pos.min(buf.len());
    core::str::from_utf8(&buf[..len]).unwrap_or("")
}

// ─────────────────────────────────────────────────────────────────────────
// Debug IPC hooks
// ─────────────────────────────────────────────────────────────────────────

/// Resolve a process to a stats matrix slot.
unsafe fn proc_to_slot(rp: *mut Proc) -> usize {
    unsafe {
        if rp.is_null() {
            return KERNELIPC;
        }
        let slot = ((*rp).p_nr + crate::proc::NR_TASKS as i32) as usize;
        if slot >= IPCPROCS { KERNELIPC } else { slot }
    }
}

/// Debug hook: kernel call message dispatched.
///
/// # Safety
///
/// `proc_` must point to a valid `Proc` or be null.
pub unsafe fn hook_ipc_msgkcall(msg: &[u8; MESSAGE_SIZE], proc_: *mut Proc) {
    unsafe {
        let src = proc_to_slot(proc_);
        ipc_record(src, KERNELIPC);
        // mtypename lookup would be done here in a debug dump build
        let _ = msg;
    }
}

/// Debug hook: kernel call result.
///
/// # Safety
///
/// `proc_` must point to a valid `Proc` or be null.
pub unsafe fn hook_ipc_msgkresult(msg: &[u8; MESSAGE_SIZE], proc_: *mut Proc) {
    unsafe {
        let dst = proc_to_slot(proc_);
        ipc_record(KERNELIPC, dst);
        let _ = msg;
    }
}

/// Debug hook: message received by a process.
///
/// # Safety
///
/// `src` and `dst` must point to valid `Proc` or be null.
pub unsafe fn hook_ipc_msgrecv(msg: &[u8; MESSAGE_SIZE], src: *mut Proc, dst: *mut Proc) {
    unsafe {
        let s = proc_to_slot(src);
        let d = proc_to_slot(dst);
        ipc_record(s, d);
        let _ = msg;
    }
}

/// Debug hook: message sent by a process.
///
/// # Safety
///
/// `src` and `dst` must point to valid `Proc` or be null.
pub unsafe fn hook_ipc_msgsend(msg: &[u8; MESSAGE_SIZE], src: *mut Proc, dst: *mut Proc) {
    unsafe {
        let s = proc_to_slot(src);
        let d = proc_to_slot(dst);
        ipc_record(s, d);
        let _ = msg;
    }
}

/// Debug hook: IPC clear (zero stats for a process).
///
/// # Safety
///
/// `p` must point to a valid `Proc` or be null.
pub unsafe fn hook_ipc_clear(p: *mut Proc) {
    unsafe {
        let slot = proc_to_slot(p);
        ipc_clear_slot(slot);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// proc_stacktrace — dump call stack for diagnostics (Phase 8.9)
// ─────────────────────────────────────────────────────────────────────────

/// Dump a process's call stack to the kernel message buffer.
///
/// Walks the x86_64 kernel stack via saved RBP frames:
///   frame layout: [saved RBP (8 bytes)] [return address (8 bytes)]
/// The initial RBP is obtained inline (current process) or from kernel
/// stack (non-current process via RSP after checking BOOT_CR3).
///
/// Prints up to 50 frames, then stops. Output goes to KMESSAGES buffer.
///
/// # Safety
///
/// `rp` must point to a valid Proc with a kernel stack in identity-mapped
/// memory. Must be called on BOOT_CR3.
/// Dump a process's call stack to the kernel message buffer.
///
/// Walks the x86_64 kernel stack via saved RBP frames:
///   frame layout: [saved RBP (8 bytes)] [return address (8 bytes)]
/// Prints up to 50 frames, then stops. Output goes to KMESSAGES buffer.
///
/// # Safety
///
/// `rp` must point to a valid Proc with a kernel stack in identity-mapped
/// memory. Must be called on BOOT_CR3.
pub unsafe fn proc_stacktrace(rp: *const Proc) {
    unsafe {
        let name_ptr = &(*rp).p_name as *const i8;
        let ep = (*rp).p_endpoint;
        let rip = (*rp).p_reg.rip;
        let rsp = (*rp).p_reg.rsp;

        // Format header manually into a stack buffer: "name (ep=N) rip=0x... rsp=0x...\n"
        let mut buf = [0u8; 160];
        let mut pos = 0;
        // Append name
        for i in 0..15 {
            let c = *name_ptr.add(i) as u8;
            if c == 0 {
                break;
            }
            if pos < buf.len() {
                buf[pos] = c;
                pos += 1;
            }
        }
        let hdr = format_u64(ep as u64);
        append_str(&mut buf, &mut pos, b" (ep=");
        append_str(&mut buf, &mut pos, &hdr);
        append_str(&mut buf, &mut pos, b") rip=0x");
        let rip_hex = hex64(rip);
        append_str(&mut buf, &mut pos, &rip_hex);
        append_str(&mut buf, &mut pos, b" rsp=0x");
        let rsp_hex = hex64(rsp);
        append_str(&mut buf, &mut pos, &rsp_hex);
        append_str(&mut buf, &mut pos, b"\n");

        append_kmess(&buf[..pos]);

        let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
        if boot_cr3 == 0 {
            append_kmess(b"  (stacktrace unavailable in test mode)\n");
            return;
        }

        // Walk the kernel RBP chain.
        // On x86_64, kernel functions push rbp; mov rbp, rsp at entry.
        // Since the kernel stack is identity-mapped, we can read directly.
        // The caller's stack frame is where we start; read RBP via inline asm.
        let mut rbp: u64;
        asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack));

        append_kmess(b"  Stack trace:\n");

        let mut n = 0;
        while rbp != 0 && n < 50 {
            let next_rbp = core::ptr::read_volatile(rbp as *const u64);
            let ret_addr = core::ptr::read_volatile((rbp + 8) as *const u64);

            // Format: "    #N: 0xXXXXXXXXXXXXXXXX\n"
            let mut line = [0u8; 30];
            let mut lp = 0;
            append_str(&mut line, &mut lp, b"    #");
            let n_str = format_u64(n as u64);
            append_str(&mut line, &mut lp, &n_str);
            append_str(&mut line, &mut lp, b": 0x");
            let ra_hex = hex64(ret_addr);
            append_str(&mut line, &mut lp, &ra_hex);
            if lp < line.len() {
                line[lp] = b'\n';
                lp += 1;
            }
            append_kmess(&line[..lp]);

            if next_rbp != 0 && next_rbp <= rbp {
                append_kmess(b"    (stack corruption)\n");
                break;
            }
            rbp = next_rbp;
            n += 1;
        }

        if n >= 50 {
            append_kmess(b"  (truncated after 50 frames)\n");
        }
    }
}

/// Format a u64 as a decimal string (no allocation).
fn format_u64(mut v: u64) -> [u8; 20] {
    let mut buf = [0u8; 20];
    if v == 0 {
        buf[0] = b'0';
        return buf;
    }
    let mut i = 20;
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let mut out = [0u8; 20];
    let len = 20 - i;
    out[..len].copy_from_slice(&buf[i..]);
    out
}

/// Format a u64 as a 16-digit lowercase hex string.
fn hex64(v: u64) -> [u8; 16] {
    let mut buf = [0u8; 16];
    let hex_chars = b"0123456789abcdef";
    for i in 0..16 {
        buf[i] = hex_chars[((v >> (60 - i * 4)) & 0xF) as usize];
    }
    buf
}

/// Append a byte slice to a buffer at position `pos`, advancing `pos`.
fn append_str(buf: &mut [u8], pos: &mut usize, s: &[u8]) {
    for &b in s {
        if *pos < buf.len() {
            buf[*pos] = b;
            *pos += 1;
        }
    }
}

/// Append bytes to the kernel messages buffer.
fn append_kmess(bytes: &[u8]) {
    unsafe {
        let km = crate::glo::KMESSAGES.get();
        let next = &mut (*km).km_next;
        let size = &mut (*km).km_size;
        let km_buf = &raw mut (*km).km_buf as *mut u8;
        for &b in bytes {
            let idx = *next as usize;
            if idx < 10000 {
                core::ptr::write(km_buf.add(idx), b);
                *next += 1;
            }
        }
        *size = (*next).min(10000i32);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::proc_init;

    #[test]
    fn test_rtsflagstr_empty() {
        let mut buf = [0u8; 200];
        assert_eq!(rtsflagstr(0, &mut buf), "0");
    }

    #[test]
    fn test_rtsflagstr_sending() {
        let mut buf = [0u8; 200];
        let s = rtsflagstr(RtsFlags::SENDING.bits(), &mut buf);
        assert!(s.contains("RTS_SENDING"));
    }

    #[test]
    fn test_rtsflagstr_multiple() {
        let mut buf = [0u8; 200];
        let flags = RtsFlags::SENDING.bits() | RtsFlags::RECEIVING.bits();
        let s = rtsflagstr(flags, &mut buf);
        assert!(s.contains("RTS_SENDING"));
        assert!(s.contains("RTS_RECEIVING"));
    }

    #[test]
    fn test_miscflagstr_empty() {
        let mut buf = [0u8; 200];
        assert_eq!(miscflagstr(0, &mut buf), "0");
    }

    #[test]
    fn test_miscflagstr_fpu() {
        let mut buf = [0u8; 200];
        let s = miscflagstr(MiscFlags::FPU_INITIALIZED.bits(), &mut buf);
        assert!(s.contains("MF_FPU_INITIALIZED"));
    }

    #[test]
    fn test_schedulerstr_kernel() {
        unsafe {
            let mut buf = [0u8; 100];
            assert_eq!(schedulerstr(core::ptr::null_mut(), &mut buf), "KERNEL");
        }
    }

    #[test]
    fn test_proc_ptr_ok() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            assert!(proc_ptr_ok(rp));
        }
    }

    #[test]
    fn test_proc_ptr_ok_null() {
        unsafe {
            assert!(!proc_ptr_ok(core::ptr::null()));
        }
    }

    #[test]
    fn test_print_proc() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut buf = [0u8; 256];
            let s = print_proc(rp, &mut buf);
            assert!(!s.is_empty());
        }
    }

    #[test]
    fn test_print_proc_null() {
        unsafe {
            let mut buf = [0u8; 256];
            assert_eq!(print_proc(core::ptr::null_mut(), &mut buf), "(null)");
        }
    }

    #[test]
    fn test_itoa() {
        let mut buf = [0u8; 20];
        assert_eq!(itoa(0, &mut buf), "0");
        assert_eq!(itoa(123, &mut buf), "123");
        assert_eq!(itoa(99999, &mut buf), "99999");
    }

    #[test]
    fn test_ipc_record_increments() {
        unsafe {
            ipc_reset_stats();
            hook_ipc_msgsend(
                &[0u8; MESSAGE_SIZE],
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            );
            hook_ipc_msgsend(
                &[0u8; MESSAGE_SIZE],
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            );
            let matrix = core::ptr::addr_of!(IPC_MESSAGES).cast::<u32>();
            let kernel_to_kernel = *matrix.add(KERNELIPC * IPCPROCS + KERNELIPC);
            assert!(
                kernel_to_kernel >= 2,
                "expected >= 2 kernel<->kernel msgs, got {}",
                kernel_to_kernel
            );
        }
    }

    #[test]
    fn test_ipc_record_stress() {
        unsafe {
            ipc_reset_stats();
            proc_init();
            ipc_reset_stats();
            let rp0 = crate::table::proc_addr(0);
            let rp1 = crate::table::proc_addr(1);
            // Send 5 messages from 0 -> 1
            for _ in 0..5 {
                hook_ipc_msgsend(&[0u8; MESSAGE_SIZE], rp0, rp1);
            }
            let slot0 = proc_to_slot(rp0);
            let slot1 = proc_to_slot(rp1);
            let matrix = core::ptr::addr_of!(IPC_MESSAGES).cast::<u32>();
            let count = *matrix.add(slot0 * IPCPROCS + slot1);
            assert_eq!(
                count, 5,
                "expected 5 msgs from proc 0 -> proc 1, got {}",
                count
            );
        }
    }

    #[test]
    fn test_ipc_top_talkers_after_records() {
        unsafe {
            ipc_reset_stats();
            let rp0 = crate::table::proc_addr(0);
            let rp1 = crate::table::proc_addr(1);
            for _ in 0..3 {
                hook_ipc_msgsend(&[0u8; MESSAGE_SIZE], rp0, rp1);
            }
            let top = ipc_top_talkers();
            // The first entry should be our 3-message pair
            assert!(
                top[0].messages >= 3,
                "expected top talker >= 3 msgs, got {}",
                top[0].messages
            );
        }
    }

    #[test]
    fn test_ipc_reset_clears() {
        unsafe {
            ipc_reset_stats();
            hook_ipc_msgkcall(&[0u8; MESSAGE_SIZE], core::ptr::null_mut());
            hook_ipc_msgkcall(&[0u8; MESSAGE_SIZE], core::ptr::null_mut());
            ipc_reset_stats();
            let matrix = core::ptr::addr_of!(IPC_MESSAGES).cast::<u32>();
            let total: u32 = (0..IPCPROCS * IPCPROCS).map(|i| *matrix.add(i)).sum();
            assert_eq!(total, 0, "expected all zeros after reset, got {}", total);
        }
    }

    #[test]
    fn test_ipc_clear_slot_clears() {
        unsafe {
            ipc_reset_stats();
            proc_init();
            ipc_reset_stats();
            let rp0 = crate::table::proc_addr(0);
            let rp1 = crate::table::proc_addr(1);
            hook_ipc_msgsend(&[0u8; MESSAGE_SIZE], rp0, rp1);
            let slot = proc_to_slot(rp0);
            ipc_clear_slot(slot);
            let matrix = core::ptr::addr_of!(IPC_MESSAGES).cast::<u32>();
            let total: u32 = (0..IPCPROCS)
                .map(|i| *matrix.add(slot * IPCPROCS + i) + *matrix.add(i * IPCPROCS + slot))
                .sum();
            assert_eq!(total, 0, "expected slot row+col cleared, got {}", total);
        }
    }

    #[test]
    fn test_mtypename_notify() {
        assert_eq!(mtypename(-10), Some("NOTIFY_MESSAGE"));
    }

    #[test]
    fn test_mtypename_unknown() {
        assert_eq!(mtypename(-999), None);
    }

    #[test]
    fn test_hook_ipc_clear_does_not_crash() {
        unsafe {
            hook_ipc_clear(core::ptr::null_mut());
        }
    }
}
