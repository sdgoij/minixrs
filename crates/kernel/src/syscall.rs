//! Basic userspace syscall handlers (task 5.41).
//!
//! These are raw POSIX syscalls called directly by userspace programs
//! via the `syscall` instruction. They run with register args:
//!   - rax = syscall number
//!   - rdi, rsi, rdx = arguments
//!   - return value in rax
//!
//! In the real Minix system, these are handled by the PM server through
//! IPC. For early boot, we stub them directly in the kernel to allow
//! basic userspace programs to run (getpid, write to serial, etc.).

use arch_common::ipc::Message;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Type for a basic syscall handler.
/// Takes the current process and register arguments, returns a value.
pub type BasicSyscallFn = unsafe fn(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64;

/// Maximum syscall number we handle.
pub const NR_BASIC_SYSCALLS: usize = 64;

struct BasicSyscallTable(UnsafeCell<[Option<BasicSyscallFn>; NR_BASIC_SYSCALLS]>);
unsafe impl Sync for BasicSyscallTable {}
impl BasicSyscallTable {
    const fn new(val: [Option<BasicSyscallFn>; NR_BASIC_SYSCALLS]) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut [Option<BasicSyscallFn>; NR_BASIC_SYSCALLS] {
        self.0.get()
    }
}

/// Dispatch table for basic syscalls.
/// Accessed via raw pointers to avoid Rust 2024 `static_mut_refs` issues.
static BASIC_SYSCALL_TABLE: BasicSyscallTable = BasicSyscallTable::new([None; NR_BASIC_SYSCALLS]);

/// Get a raw pointer to the syscall table.
fn syscall_table_ptr() -> *mut [Option<BasicSyscallFn>; NR_BASIC_SYSCALLS] {
    BASIC_SYSCALL_TABLE.get()
}

/// Simple bump allocator brk (0x3FE00000-0x3FF00000 region).
static CURRENT_BRK: AtomicU64 = AtomicU64::new(0x3FE00000);

/// Register a basic syscall handler.
///
/// # Safety
///
/// Must be called during initialization, before any userspace execution.
pub unsafe fn register_basic_syscall(nr: usize, handler: BasicSyscallFn) {
    unsafe {
        let table = syscall_table_ptr();
        if nr < NR_BASIC_SYSCALLS {
            let slot = (table as *mut Option<BasicSyscallFn>).add(nr);
            core::ptr::write(slot, Some(handler));
        }
    }
}

/// Dispatch a basic syscall. Returns the value to place in RAX.
///
/// Saves the per-process CR3 before dispatching, loads BOOT_CR3 so the
/// kernel has access to identity-mapped data, then restores the per-process
/// CR3 after the handler returns.
///
/// When BOOT_CR3 is still 0 (pre-init / test mode) the CR3 save/restore
/// is skipped entirely, since the privileged instructions would crash in
/// a host test binary.
///
/// # Safety
///
/// `caller` must point to a valid Proc.
pub unsafe fn dispatch_basic_syscall(
    caller: *mut crate::proc::Proc,
    nr: usize,
    args: &[u64; 6],
) -> i64 {
    // Per-process page tables preserve the kernel identity map via PD
    // deep-copy, so the kernel can access its own data AND user data
    // without switching CR3. The old CR3 save/restore is disabled.

    unsafe {
        let table = syscall_table_ptr() as *const Option<BasicSyscallFn>;
        if nr < NR_BASIC_SYSCALLS {
            let entry = core::ptr::read(table.add(nr));
            match entry {
                Some(handler) => handler(caller, args),
                None => -38,
            }
        } else {
            -38
        }
    }
}

// Syscall handlers (table in syscall_map)

/// SYS_read (2) — read from file descriptor.
/// fd=0: serial input (unless the process VFS-owns fd 0). fd>0: forward to VFS.
unsafe fn sys_read_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    let buf = args[1] as *mut u8;
    let count = args[2] as usize;
    if fd == 0 && (!caller.is_null() && (*caller).p_fd_vfs & 1 == 0) {
        // stdin → serial input (interrupt-driven via ser_input). The
        // tty server is the only process that reads the ring directly
        // (its own fd 0 stays non-VFS); a stray reader would steal
        // console input from tty (TTY.md 1C.4).
        if !caller.is_null() && (*caller).p_endpoint != TTY_PROC_NR {
            return -9; // EBADF
        }
        if buf.is_null() || count == 0 {
            return -14; // EFAULT
        }
        // Read one byte. The fd-0 read returns EAGAIN when the ring is
        // empty and the tty's do_read loop retries in user mode, where the
        // timer preempts it and schedules other runnable processes (e.g.
        // the input server's SYS_SETALARM virtio poll). A kernel-mode
        // busy-wait would starve them: the x86 read spins with IF=0 (no
        // timer), and the riscv/aarch64 timer only fires at the WFI window
        // anyway. The UART is drained into the ring here and by the
        // per-tick trap drains, so a retry finds the byte.
        let byte = {
            let mut got = crate::ser_input::try_read();
            if got.is_none() {
                // Feed the ring from the UART first (the ISR/timer drain
                // is disabled while we run), then re-check.
                while let Some(b) = crate::hal::poll_console() {
                    unsafe { crate::ser_input::push_byte(b) };
                }
                got = crate::ser_input::try_read();
            }
            match got {
                Some(b) => b,
                None => return -11, // EAGAIN: tty retries in user mode
            }
        };
        unsafe {
            core::ptr::write_volatile(buf, byte);
            core::arch::asm!("", options(nostack, preserves_flags));
        }
        1
    } else {
        // Forward to VFS. Use the caller's per-process send buffer, not
        // the shared kernel stack (the SENDREC blocks the caller; the next
        // process reuses the same kernel stack and a reply written there
        // would corrupt its live frame).
        let msg: &mut [u8; crate::proc::MESSAGE_SIZE] = &mut (*caller).p_sendmsg;
        msg.fill(0);
        msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
        msg[4..8].copy_from_slice(&0x100i32.to_le_bytes()); // VFS_READ = 0x100
        msg[8..12].copy_from_slice(&fd.to_le_bytes());
        msg[16..24].copy_from_slice(&(buf as u64).to_le_bytes());
        msg[24..28].copy_from_slice(&(count as u32).to_le_bytes());
        unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
    }
}

/// SYS_open (4) — open a file.
/// args[0] = path pointer, args[1] = path length, args[2] = flags.
/// Forwards to VFS via IPC. With O_CREAT it becomes VFS_CREAT (do_creat's
/// layout: name @ 8, name_len @ 16, flags @ 24, mode @ 28); otherwise
/// VFS_OPEN (do_open's layout: flags @ 8, path_addr @ 16, path_len @ 24).
unsafe fn sys_open_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0];
    let path_len = args[1] as u32;
    let flags = args[2] as i32;

    // Forward to VFS using the caller's per-process send buffer: the
    // SENDREC blocks the caller, and a reply delivered to the shared
    // kernel stack would be overwritten by the next process to run.
    let msg: &mut [u8; crate::proc::MESSAGE_SIZE] = &mut (*caller).p_sendmsg;
    msg.fill(0);
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    if flags & 0o100 != 0 {
        // O_CREAT set — the file may need to be created (VFS_CREAT).
        msg[4..8].copy_from_slice(&0x104i32.to_le_bytes()); // VFS_CREAT = 0x104
        msg[8..16].copy_from_slice(&path_ptr.to_le_bytes());
        msg[16..20].copy_from_slice(&path_len.to_le_bytes());
        msg[24..28].copy_from_slice(&flags.to_le_bytes());
        msg[28..32].copy_from_slice(&0o644i32.to_le_bytes()); // default create mode
    } else {
        msg[4..8].copy_from_slice(&0x103i32.to_le_bytes()); // VFS_OPEN = 0x103
        msg[8..12].copy_from_slice(&flags.to_le_bytes());
        msg[16..24].copy_from_slice(&path_ptr.to_le_bytes());
        msg[24..28].copy_from_slice(&path_len.to_le_bytes());
    }
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_close (5) — close a file descriptor.
/// Forwards to VFS via IPC. VFS's do_close reads fd at offset 8.
unsafe fn sys_close_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;

    let msg: &mut [u8; crate::proc::MESSAGE_SIZE] = &mut (*caller).p_sendmsg;
    msg.fill(0);
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    msg[4..8].copy_from_slice(&0x105i32.to_le_bytes()); // VFS_CLOSE = 0x105
    msg[8..12].copy_from_slice(&fd.to_le_bytes());
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_getpid (20) — return the caller's endpoint as PID.
unsafe fn sys_getpid_handler(caller: *mut crate::proc::Proc, _args: &[u64; 6]) -> i64 {
    unsafe { (*caller).p_endpoint as i64 }
}

// Pending exit notification queue
// When a process exits via sys_exit_handler, the kernel stores the exit
// info here and notifies PM via mini_notify. PM reads the queue to find
// which process exited and with what status.

const PENDING_EXIT_QUEUE_SIZE: usize = 16;

/// A pending exit notification.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct PendingExit {
    endpoint: i32,
    exit_status: i32,
}

struct PendingExitTable(UnsafeCell<[PendingExit; PENDING_EXIT_QUEUE_SIZE]>);
unsafe impl Sync for PendingExitTable {}
impl PendingExitTable {
    const fn new(val: [PendingExit; PENDING_EXIT_QUEUE_SIZE]) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut [PendingExit; PENDING_EXIT_QUEUE_SIZE] {
        self.0.get()
    }
}

/// Circular buffer of pending exits.
static PENDING_EXITS: PendingExitTable = PendingExitTable::new(
    [PendingExit {
        endpoint: 0,
        exit_status: 0,
    }; PENDING_EXIT_QUEUE_SIZE],
);

/// Head index (next slot to read).
static PE_HEAD: AtomicUsize = AtomicUsize::new(0);
/// Tail index (next slot to write).
static PE_TAIL: AtomicUsize = AtomicUsize::new(0);
/// Count of entries.
static PE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Push an exit notification. Returns true if queued, false if full.
unsafe fn push_pending_exit(endpoint: i32, exit_status: i32) -> bool {
    unsafe {
        let count = PE_COUNT.load(Ordering::Relaxed);
        if count >= PENDING_EXIT_QUEUE_SIZE {
            return false; // queue full, drop notification
        }
        let tail = PE_TAIL.load(Ordering::Relaxed);
        (*PENDING_EXITS.get())[tail] = PendingExit {
            endpoint,
            exit_status,
        };
        PE_TAIL.store((tail + 1) % PENDING_EXIT_QUEUE_SIZE, Ordering::Relaxed);
        PE_COUNT.store(count + 1, Ordering::Relaxed);
        true
    }
}

/// Pop an exit notification. Returns None if queue empty.
///
/// # Safety
///
/// Must be called with exclusive access to the pending exit queue.
/// Only the PM server should call this in response to a notification.
#[allow(unused)]
pub unsafe fn pop_pending_exit() -> Option<(i32, i32)> {
    unsafe {
        let count = PE_COUNT.load(Ordering::Relaxed);
        if count == 0 {
            return None;
        }
        let head = PE_HEAD.load(Ordering::Relaxed);
        let entry = (*PENDING_EXITS.get())[head];
        PE_HEAD.store((head + 1) % PENDING_EXIT_QUEUE_SIZE, Ordering::Relaxed);
        PE_COUNT.store(count - 1, Ordering::Relaxed);
        Some((entry.endpoint, entry.exit_status))
    }
}

/// SYS_exit (0) — terminate the current process.
/// Stores the exit status, sets SIGNALED+SIG_PENDING for PM to pick up
/// via SYS_GETKSIG, notifies PM, and frees the Proc slot.
unsafe fn sys_exit_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    unsafe {
        let exit_status = args[0] as i32;
        let endpoint = (*caller).p_endpoint;

        // Group exit: every thread of the process dies with it (main-thread
        // exit, `exit()` from any thread, or a fatal signal all end here).
        // Free the worker threads — including the caller when a worker
        // called exit() — then run the exit bookkeeping on the main slot:
        // PM finds the exit via SYS_GETKSIG on the process's endpoint, so
        // the SIGNALED/SIG_PENDING flags must live on the main slot.
        let main = crate::thread::group(caller);
        crate::thread::sweep_group(main);
        let rp = main;

        // Store exit status in p_signal_received for PM to read via SYS_GETKSIG.
        (*rp).p_signal_received = exit_status as u64;

        // A signal that pended (cause_sig) but was not yet delivered when the
        // process exited is moot: once the exit reply is read, PM would
        // otherwise treat the exit as a signal delivery, never zombify the
        // process, and the parent's waitpid hangs (observed: sigtest's 3rd ^C
        // racing its exit). Clear the map so an exit reply always has zero
        // pending bits.
        (*rp).p_pending = 0;

        // Set SIGNALED + SIG_PENDING so do_getksig_handler finds this process.
        // Matching C cause_sig(): RTS_SET(rp, RTS_SIGNALED | RTS_SIG_PENDING)
        // Also set SLOT_FREE so the slot can be reused after PM reads the exit.
        let sig_flags = crate::proc::RtsFlags::SIGNALED.bits()
            | crate::proc::RtsFlags::SIG_PENDING.bits()
            | crate::proc::RtsFlags::SLOT_FREE.bits();
        (*rp)
            .p_rts_flags
            .fetch_or(sig_flags, core::sync::atomic::Ordering::Relaxed);

        // Unlink the main slot from any caller queue it is blocked on as a
        // sender (a worker may have called exit while the main thread sat
        // in SEND to a server; that server's later receive must not wake a
        // dead slot). No-op when the main thread was not blocked.
        crate::system::clear_ipc(rp);

        // Push to pending exit queue.
        push_pending_exit(endpoint, exit_status);

        // Remove from run queue so pick_proc doesn't find a dead process.
        // RTS_SET above already dequeues if was runnable, but dequeue
        // again for safety (no-op if already dequeued).
        crate::sched::dequeue(rp);

        // Notify PM (the signal manager) that this process has exited.
        // Matching C: cause_sig() -> send_sig() -> mini_notify(proc_addr(SYSTEM), rp->p_endpoint)
        if let Some(sig_mgr_ep) = get_sig_manager(rp) {
            let _ = crate::ipc::mini_notify(arch_common::com::SYSTEM, sig_mgr_ep);
        } else {
            let _ = crate::ipc::mini_notify(arch_common::com::SYSTEM, arch_common::com::PM_PROC_NR);
        }
    }
    crate::system::EDONTREPLY as i64
}

/// SYS_hangdump (63) — print each live process's IPC state to the serial
/// console. Diagnostic for wedged-server hangs: shows who is blocked on
/// whom (SENDING → p_sendto_e, RECEIVING → p_getfrom_e) so a deadlocked
/// server chain is visible from the shell.
unsafe fn sys_hang_dump_handler(_caller: *mut crate::proc::Proc, _args: &[u64; 6]) -> i64 {
    unsafe {
        use crate::proc::RtsFlags;

        fn put(b: u8) {
            crate::hal::serial_write_byte(b);
        }
        fn puts(s: &[u8]) {
            for &b in s {
                put(b);
            }
        }
        fn put_hex(v: i32) {
            for i in (0..8).rev() {
                put(b"0123456789abcdef"[((v >> (i * 4)) & 0xF) as usize]);
            }
        }
        fn put_hex64(v: u64) {
            for i in (0..16).rev() {
                put(b"0123456789abcdef"[((v >> (i * 4)) & 0xF) as usize]);
            }
        }

        // x86_64 TrapFrame layout: rip @ 160, rsp @ 168 (other arches save
        // the same fields at the same byte offsets in their own p_reg).
        fn frame_rip(rp: *const crate::proc::Proc) -> u64 {
            // SAFETY: `rp` is a live Proc; p_reg is a plain byte array.
            unsafe {
                let p = &(*(rp)).p_reg;
                u64::from_ne_bytes(p[160..168].try_into().unwrap_or([0; 8]))
            }
        }
        fn frame_rsp(rp: *const crate::proc::Proc) -> u64 {
            // SAFETY: `rp` is a live Proc; p_reg is a plain byte array.
            unsafe {
                let p = &(*(rp)).p_reg;
                u64::from_ne_bytes(p[168..176].try_into().unwrap_or([0; 8]))
            }
        }

        puts(b"--- hangdump ---\r\n");
        puts(b"  runq:");
        crate::sched::dump_queues(|ep| {
            put(b' ');
            put_hex(ep);
        });
        puts(b" free=");
        // Reuse the memstat plumbing: phys_free_pages is only callable from
        // ring 0, which we are in.
        put_hex(crate::vm::phys_free_pages() as i32);
        puts(b"\r\n");
        for i in 0..crate::proc::NR_PROCS_TOTAL {
            let rp = crate::table::proc_addr(i as i32);
            if rp.is_null() || (*rp).is_empty() {
                continue;
            }
            let rts = (*rp).p_rts_flags.load(Ordering::Relaxed);
            puts(b"  ");
            for &b in &(*rp).p_name {
                if b == 0 {
                    break;
                }
                put(b);
            }
            puts(b" ep=");
            put_hex((*rp).p_endpoint);
            puts(b" rts=");
            put_hex(rts as i32);
            let mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
            if mf != 0 {
                puts(b" mf=");
                put_hex(mf as i32);
            }
            if rts & RtsFlags::SENDING.bits() != 0 {
                puts(b" SENDTO=");
                put_hex((*rp).p_sendto_e);
            }
            if rts & RtsFlags::RECEIVING.bits() != 0 {
                puts(b" GETFROM=");
                put_hex((*rp).p_getfrom_e);
            }
            if rts & RtsFlags::SLOT_FREE.bits() == 0 && rts & RtsFlags::SIGNALED.bits() != 0 {
                puts(b" SIGNALED");
            }
            puts(b" rip=");
            put_hex64(frame_rip(rp));
            puts(b" rsp=");
            put_hex64(frame_rsp(rp));
            puts(b"\r\n");
        }
        puts(b"--- end hangdump ---\r\n");
    }
    0
}

/// SYS_write (3) — write to a file descriptor.
/// fd=1 (stdout), fd=2 (stderr) go to serial output, unless the process
/// VFS-owns them (dup2'd redirect), in which case the write is forwarded
/// to VFS. Regular file writes go through VFS via `minix_std::fs::write`.
///
/// # Safety
///
/// Must be called from ring 0 with a valid caller process pointer.
/// The buffer pointer in `args[1]` must be readable in the caller's address space.
pub unsafe fn sys_write_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    let count = args[2] as usize;
    let buf = args[1] as *const u8;
    if buf.is_null() {
        return -14; // EFAULT
    }
    if fd == 1 || fd == 2 {
        if !caller.is_null() && (*caller).p_fd_vfs & (1u32 << fd) != 0 {
            // VFS-owned fd 1/2 (dup2'd redirect): the write must reach the
            // redirect file, not the console.
            return unsafe { forward_write_to_vfs(caller, fd, buf, count) };
        }
        // Serial output shortcut for console writes.
        if count > 0 {
            for i in 0..count.min(256) {
                let c = unsafe { core::ptr::read_volatile(buf.add(i)) };
                if c == b'\n' {
                    crate::hal::serial_write_byte(b'\r');
                }
                crate::hal::serial_write_byte(c);
            }
        }
        count as i64
    } else {
        // fd 0 or regular files (fd >= 3): forward to VFS, mirroring
        // sys_read_handler. Without this, fd >= 3 writes returned EBADF
        // and cp silently dropped every copied byte.
        unsafe { forward_write_to_vfs(caller, fd, buf, count) }
    }
}

/// Build a VFS_WRITE request in the caller's per-process send buffer and
/// block on the reply. Used for VFS-owned fd 1/2 (dup2'd redirects) and
/// for regular file descriptors (fd >= 3), whose writes must reach the
/// filesystem rather than the serial console.
///
/// # Safety
///
/// `caller` must point to a valid Proc and `buf` must be readable for
/// `count` bytes in the caller's address space.
unsafe fn forward_write_to_vfs(
    caller: *mut crate::proc::Proc,
    fd: i32,
    buf: *const u8,
    count: usize,
) -> i64 {
    // The reply lands in the same per-process buffer: mini_receive re-points
    // p_delivermsg_vir at m_ptr (p_sendmsg) before blocking, and setting it
    // up front covers a reply arriving during the send phase.
    let msg: &mut [u8; crate::proc::MESSAGE_SIZE] = &mut (*caller).p_sendmsg;
    msg.fill(0);
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    msg[4..8].copy_from_slice(&0x101i32.to_le_bytes()); // VFS_WRITE = 0x101
    msg[8..12].copy_from_slice(&fd.to_le_bytes());
    msg[16..24].copy_from_slice(&(buf as u64).to_le_bytes());
    msg[24..32].copy_from_slice(&(count as u64).to_le_bytes());
    msg[32..40].copy_from_slice(&0u64.to_le_bytes()); // position (unused)

    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_setfdvfs (53) — mark fd 0..2 as VFS-owned (on=1) or serial (on=0).
/// The shell calls this after dup2'ing a redirect file onto fd 1 so the
/// exec'd command's write(1) reaches the file instead of the serial console.
unsafe fn sys_setfdvfs_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    if !(0..=2).contains(&fd) {
        return -9; // EBADF
    }
    if !caller.is_null() {
        if args[1] != 0 {
            (*caller).p_fd_vfs |= 1u32 << fd;
        } else {
            (*caller).p_fd_vfs &= !(1u32 << fd);
        }
    }
    0
}

/// SYS_brk (13) — change data segment size.
/// Simple bump allocator in 0x3FE00000-0x3FF00000 region.
unsafe fn sys_brk_handler(_caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let new_brk = args[0];
    if new_brk == 0 {
        // Query current break
        CURRENT_BRK.load(Ordering::Relaxed) as i64
    } else if (0x3FE00000..0x3FF00000).contains(&new_brk) {
        CURRENT_BRK.store(new_brk, Ordering::Relaxed);
        new_brk as i64
    } else {
        -12i64 // ENOMEM
    }
}

/// VFS server endpoint.
const VFS_PROC_NR: i32 = 1;
/// TTY server endpoint — the only process allowed to read the kernel
/// serial ring directly (its fd 0 stays non-VFS).
const TTY_PROC_NR: i32 = 5;

/// Build the common header of a path-based VFS request in the caller's
/// send buffer: destination endpoint, call number, path pointer @8, path
/// length @16 (the m7 convention VFS's `do_*` path handlers parse). The
/// caller fills any extra fields (mode, owner, …) before sending.
unsafe fn vfs_path_msg(
    caller: *mut crate::proc::Proc,
    vfs_call: i32,
    path_ptr: u64,
    path_len: u32,
) -> &'static mut [u8; crate::proc::MESSAGE_SIZE] {
    let msg: &mut [u8; crate::proc::MESSAGE_SIZE] = &mut (*caller).p_sendmsg;
    msg.fill(0);
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    msg[4..8].copy_from_slice(&vfs_call.to_le_bytes());
    msg[8..16].copy_from_slice(&path_ptr.to_le_bytes());
    msg[16..20].copy_from_slice(&path_len.to_le_bytes());
    msg
}

/// SYS_mkdir (40) — create a directory.
/// args[0] = path pointer, args[1] = path length, args[2] = mode.
unsafe fn sys_mkdir_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let msg = unsafe { vfs_path_msg(caller, 0x109, args[0], args[1] as u32) }; // VFS_MKDIR
    msg[24..28].copy_from_slice(&(args[2] as i32).to_le_bytes()); // mode
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_unlink (41) — remove a file.
/// args[0] = path pointer, args[1] = path length.
unsafe fn sys_unlink_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let msg = unsafe { vfs_path_msg(caller, 0x107, args[0], args[1] as u32) }; // VFS_UNLINK
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_rmdir (42) — remove a directory.
/// args[0] = path pointer, args[1] = path length.
unsafe fn sys_rmdir_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let msg = unsafe { vfs_path_msg(caller, 0x112, args[0], args[1] as u32) }; // VFS_RMDIR
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_link (43) — create a hard link.
/// args[0] = old path pointer, args[1] = old length, args[2] = new path
/// pointer, args[3] = new length.
unsafe fn sys_link_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let msg = unsafe { vfs_path_msg(caller, 0x106, args[0], args[1] as u32) }; // VFS_LINK
    msg[24..32].copy_from_slice(&args[2].to_le_bytes()); // new path @24
    msg[32..36].copy_from_slice(&(args[3] as u32).to_le_bytes()); // new len @32
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_chmod (44) — change file mode.
/// args[0] = path pointer, args[1] = path length, args[2] = mode.
unsafe fn sys_chmod_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let msg = unsafe { vfs_path_msg(caller, 0x10B, args[0], args[1] as u32) }; // VFS_CHMOD
    msg[24..28].copy_from_slice(&(args[2] as i32).to_le_bytes()); // mode
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_chown (45) — change file owner.
/// args[0] = path pointer, args[1] = path length, args[2] = owner,
/// args[3] = group.
unsafe fn sys_chown_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let msg = unsafe { vfs_path_msg(caller, 0x10C, args[0], args[1] as u32) }; // VFS_CHOWN
    msg[24..28].copy_from_slice(&(args[2] as i32).to_le_bytes()); // owner
    msg[32..36].copy_from_slice(&(args[3] as i32).to_le_bytes()); // group
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_mknod (56) — create a device node.
/// args[0] = path pointer, args[1] = path length, args[2] = mode,
/// args[3] = device number.
unsafe fn sys_mknod_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let msg = unsafe { vfs_path_msg(caller, 0x10A, args[0], args[1] as u32) }; // VFS_MKNOD
    msg[24..28].copy_from_slice(&(args[2] as i32).to_le_bytes()); // mode
    msg[32..36].copy_from_slice(&(args[3] as i32).to_le_bytes()); // dev
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

/// SYS_getdents (57) — read directory entries.
/// Forwards to VFS via IPC. VFS's do_getdents reads:
///   fd at offset 8, buf_addr at offset 16, count at offset 24.
unsafe fn sys_getdents_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    let buf_ptr = args[1];
    let count = args[2] as u32;

    let msg: &mut [u8; crate::proc::MESSAGE_SIZE] = &mut (*caller).p_sendmsg;
    msg.fill(0);
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    msg[4..8].copy_from_slice(&0x11Di32.to_le_bytes()); // VFS_GETDENTS = 0x11D
    msg[8..12].copy_from_slice(&fd.to_le_bytes());
    msg[16..24].copy_from_slice(&buf_ptr.to_le_bytes());
    msg[24..28].copy_from_slice(&count.to_le_bytes());
    unsafe { crate::ipc::syscall_sendrec_status(caller, msg) }
}

// IPC syscall handlers (46-49)

/// SYS_IPC_SEND (46) — blocking send a message to a process.
unsafe fn sys_ipc_send_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let dest = args[0] as i32;
    let msg_ptr = args[1] as *mut u8;
    if msg_ptr.is_null() {
        return -14; // EFAULT
    }
    // do_sync_ipc reads destination from msg[0..4]
    unsafe { core::ptr::write_unaligned(msg_ptr as *mut i32, dest) };
    unsafe { crate::ipc::do_sync_ipc(caller, msg_ptr, crate::ipc::SEND) as i64 }
}

/// SYS_IPC_RECEIVE (47) — receive a message from a process.
/// src = ANY (0x0000ffff) to receive from anyone.
unsafe fn sys_ipc_receive_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let src = args[0] as i32;
    let msg_ptr = args[1] as *mut u8;
    if msg_ptr.is_null() {
        return -14; // EFAULT
    }
    // Set delivery address so delivermsg can copy directly-delivered message.
    unsafe { (*caller).p_delivermsg_vir = msg_ptr as u64 };
    // do_sync_ipc reads source from msg[0..4]
    unsafe { core::ptr::write_unaligned(msg_ptr as *mut i32, src) };
    unsafe { crate::ipc::do_sync_ipc(caller, msg_ptr, crate::ipc::RECEIVE) as i64 }
}

/// SYS_IPC_SENDREC (48) — send then receive (atomic request-reply).
unsafe fn sys_ipc_sendrec_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let dest = args[0] as i32;
    let msg_ptr = args[1] as *mut u8;
    if msg_ptr.is_null() {
        return -14; // EFAULT
    }
    // Set delivery address so delivermsg can copy reply to user buffer.
    unsafe { (*caller).p_delivermsg_vir = msg_ptr as u64 };
    // do_sync_ipc reads destination from msg[0..4]
    unsafe { core::ptr::write_unaligned(msg_ptr as *mut i32, dest) };
    unsafe { crate::ipc::do_sync_ipc(caller, msg_ptr, crate::ipc::SENDREC) as i64 }
}

/// SYS_IPC_SENDNB (51) — non-blocking send a message to a process.
/// Same as SEND (46) but does not block if the destination is not receiving.
unsafe fn sys_ipc_sendnb_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let dest = args[0] as i32;
    let msg_ptr = args[1] as *mut u8;
    if msg_ptr.is_null() {
        return -14; // EFAULT
    }
    unsafe { core::ptr::write_unaligned(msg_ptr as *mut i32, dest) };
    unsafe { crate::ipc::do_sync_ipc(caller, msg_ptr, crate::ipc::SENDNB) as i64 }
}

/// SYS_IPC_SENDA (52) — send asynchronous messages.
///
/// args[0] = unused (0)
/// args[1] = pointer to a message buffer containing table ptr and size
///
/// The message buffer layout (set by asynsend3 wrapper):
///   [8..16] = table pointer (u64)
///   [16..24] = table size (u64)
unsafe fn sys_ipc_senda_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let msg_ptr = args[1] as *mut u8;
    if msg_ptr.is_null() {
        return -14; // EFAULT
    }
    unsafe {
        crate::ipc::ipc_senda_handler(
            caller,
            &mut *msg_ptr.cast::<[u8; crate::proc::MESSAGE_SIZE]>(),
        ) as i64
    }
}

// Thread syscalls (54, 55, 58, 59)

/// SYS_thread_create (54) — create a thread in the calling process.
/// args: [0] = entry, [1] = stack top, [2] = arg (first argument register).
/// Returns the new thread's tid (> 0) or a negative errno.
unsafe fn sys_thread_create_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    if caller.is_null() {
        return -38; // ENOSYS
    }
    unsafe { crate::thread::create(caller, args[0], args[1], args[2]) as i64 }
}

/// SYS_thread_exit (55) — terminate the calling thread. The main thread
/// (tid 0) calling this exits the whole process (status in args[0]).
unsafe fn sys_thread_exit_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    if caller.is_null() {
        return -38;
    }
    unsafe {
        if (*caller).p_tid == 0 {
            return sys_exit_handler(caller, args);
        }
        crate::thread::exit(caller) as i64
    }
}

/// SYS_thread_join (58) — wait for a thread of the calling process to exit.
/// args[0] = tid. Returns 0 when the thread has exited, or a negative errno.
unsafe fn sys_thread_join_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    if caller.is_null() {
        return -38;
    }
    unsafe { crate::thread::join(caller, args[0] as u32) as i64 }
}

/// SYS_thread_yield (59) — yield the CPU to another runnable thread.
unsafe fn sys_thread_yield_handler(caller: *mut crate::proc::Proc, _args: &[u64; 6]) -> i64 {
    if caller.is_null() {
        return -38;
    }
    unsafe { crate::thread::yield_self(caller) };
    0
}

/// SYS_thread_set_tls (60) — set this thread's thread pointer (TLS base).
/// args[0] = the TLS base: FS base on x86_64, tpidr_el0 on AArch64; unused
/// on RISC-V (tp is a saved GPR the thread library sets directly). Stored
/// per-thread and reloaded on every context switch; also applied to the
/// calling thread immediately.
unsafe fn sys_thread_set_tls_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    if caller.is_null() {
        return -38;
    }
    let tls = args[0];
    unsafe {
        (*caller).p_tls = tls;
        crate::hal::set_tls_current(tls);
    }
    0
}

/// SYS_futex_wait (61) — block this thread until `SYS_futex_wake` on the
/// same address. args[0] = user futex address, args[1] = expected value.
/// Returns 0 when woken, -EAGAIN when `*addr != expected`, -EFAULT for an
/// unmapped address.
unsafe fn sys_futex_wait_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    if caller.is_null() {
        return -38;
    }
    unsafe { crate::thread::futex_wait(caller, args[0], args[1]) as i64 }
}

/// SYS_futex_wake (62) — wake up to args[1] threads blocked in
/// `SYS_futex_wait` on the address args[0]. Returns the number woken.
unsafe fn sys_futex_wake_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    if caller.is_null() {
        return -38;
    }
    unsafe { crate::thread::futex_wake(args[0], args[1] as u32) as i64 }
}

/// SYS_KERNEL_CALL (50) — invoke a kernel call on the SYSTEM task.
///
/// args[0] = call_nr (kernel call number, e.g. 0 for SYS_FORK)
/// args[1] = pointer to a Message struct
///
/// The Message struct should have:
///   m_source = 0 (will be overwritten with KERNEL_CALL + call_nr)
///   m_type = 0 (will be overwritten with caller endpoint)
///   m_payload = kernel call payload fields
///
/// After the call, the Message struct is updated with the kernel's reply
/// (result code in bytes 0-3, reply fields in m_payload).
///
/// # Safety
///
/// `caller` must point to a valid `Proc` struct. `args` must contain a valid
/// message buffer pointer at `args[1]`.
pub unsafe fn sys_kernel_call_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let call_nr = args[0] as i32;
    let msg_ptr = args[1] as *mut u8;
    if msg_ptr.is_null() {
        return -14; // EFAULT
    }
    unsafe {
        // The kernel identity map is preserved in all per-process page
        // tables via PD deep-copy, so we can read/write the caller's
        // message buffer without switching CR3.  No CR3 save/restore
        // needed — see dispatch_basic_syscall comment.

        // Copy user message into kernel buffer.
        // Use Message struct size (56 bytes), not MESSAGE_SIZE (64), because
        // send_kernel_call may pass a Message struct directly (the compiler
        // can optimize away the 64-byte intermediate buffer). Reading 64 bytes
        // would ingest 8 bytes of adjacent stack garbage.
        let mut kbuf = [0u8; crate::proc::MESSAGE_SIZE];
        // Copy only Message size (56 bytes), not MESSAGE_SIZE (64), because
        // the caller's buffer may be a Message struct, not a raw 64-byte buffer.
        let copy_sz = core::mem::size_of::<Message>().min(crate::proc::MESSAGE_SIZE);
        core::ptr::copy_nonoverlapping(msg_ptr, kbuf.as_mut_ptr(), copy_sz);
        // Set call number at bytes 0-3 (for kernel_call_dispatch)
        let call_val = (crate::system::KERNEL_CALL as u32 + call_nr as u32) as i32;
        kbuf[0..4].copy_from_slice(&call_val.to_ne_bytes());
        // Set source endpoint at bytes 4-7
        let src_ep = (*caller).p_endpoint;
        kbuf[4..8].copy_from_slice(&src_ep.to_ne_bytes());
        // Set delivery address for result copy-back.
        // Save the previous p_delivermsg_vir so a blocked RECEIVE's
        // buffer address isn't lost when a kernel call overwrites it.
        let saved_vir = (*caller).p_delivermsg_vir;
        (*caller).p_delivermsg_vir = msg_ptr as u64;
        let result = crate::system::kernel_call_dispatch(caller, &mut kbuf);
        // Copy result back to user
        crate::system::kernel_call_finish(caller, &mut kbuf, result);
        // Restore the original delivery address
        (*caller).p_delivermsg_vir = saved_vir;
        result as i64
    }
}

/// SYS_IPC_NOTIFY (49) — send an asynchronous notification.
unsafe fn sys_ipc_notify_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let dest = args[0] as i32;
    let mut msg_buf = [0u8; 64];
    unsafe {
        core::ptr::write_unaligned(msg_buf.as_mut_ptr() as *mut i32, dest);
        crate::ipc::do_sync_ipc(caller, msg_buf.as_mut_ptr(), crate::ipc::NOTIFY) as i64
    }
}

/// Result of loading a new executable image into a process.
#[derive(Debug, Clone, Copy)]
pub struct ExecLoadResult {
    /// Entry point (PC) of the new image.
    pub entry: u64,
    /// User stack pointer (RSP) of the new image.
    pub rsp: u64,
}

/// Load a raw ELF binary into a target process, replacing its image.
/// Install a new executable image into `rp`'s address space.
///
/// VM has registered the executable's PT_LOAD segments as lazy file-backed
/// regions (VM_EXEC_NEWMEM + VM_VFS_MMAP). The kernel builds a fresh page
/// table rooted at a copy of the boot identity map (`exec_create_root`),
/// clears the code range `[code_start, code_end)` so the file regions fault
/// on first touch instead of aliasing identity RAM, maps the stack and brk
/// heap into it, builds the user stack frame from `argv_strs`/`envp_strs`,
/// and programs the target's registers (entry, RSP, argc, argv). It does
/// NOT make the target runnable — the caller decides when the process
/// resumes at the new entry point.
///
/// Returns the entry point and stack pointer on success, or a negative
/// errno.
///
/// # Safety
///
/// `rp` must point to a valid, in-use `Proc`. `code_start`/`code_end` must
/// be page-aligned and cover every PT_LOAD segment.
pub unsafe fn exec_elf_for_target(
    rp: *mut crate::proc::Proc,
    entry: u64,
    code_start: u64,
    code_end: u64,
    argv_strs: &[&str],
    envp_strs: &[&str],
) -> Result<ExecLoadResult, i64> {
    unsafe {
        if code_end <= code_start || code_start & 0xFFF != 0 || code_end & 0xFFF != 0 {
            return Err(-22); // EINVAL
        }

        // Architecture-specific user stack base.
        let user_stack_base: u64 = crate::hal::user_stack_base();
        let user_stack_size: usize = crate::hal::user_stack_size();

        let stack_start = user_stack_base & !0xFFF;
        let stack_end = (user_stack_base + user_stack_size as u64 + 0xFFF) & !0xFFF;

        // Build new page table root (arch-specific layout; copies the boot
        // identity map so the kernel stays executable on this CR3).
        let boot_cr3_val = crate::pagetable::boot_cr3();
        let root = match crate::hal::exec_create_root(boot_cr3_val) {
            0 => return Err(-12),
            r => r,
        };

        // Clear the code range in the fresh table: exec_create_root copies
        // the identity map with user access in the low window, so without
        // clearing, the lazy file regions would silently alias identity RAM
        // (no fault fires). Each access now faults and VM demand-pages from
        // the file regions.
        let mut va = code_start;
        while va < code_end {
            let _ = crate::pagetable::clear_page(root, va);
            va += 0x1000;
        }

        // Allocate physical pages for the stack and write the exec frame
        // directly into them under the boot identity map. Writing at the
        // user-stack VA would land beyond RAM below 256 MiB on x86
        // (0x0FE00000), 2.3 GiB on RISC-V and 252 MiB on AArch64, so the
        // frame is built at the allocated physical pages and the RSP plus
        // the argv/envp string pointers are converted to the user VA.
        let stack_pages = ((stack_end - stack_start) / 4096) as usize;
        let phys_stack_base = match crate::hal::alloc_phys_contig(stack_pages) {
            Some(b) => b,
            None => return Err(-12),
        };
        let saved_cr3 = crate::hal::read_cr3();
        crate::hal::write_cr3(boot_cr3_val);
        let phys_rsp = match crate::elf::setup_user_stack_full(
            phys_stack_base + user_stack_size as u64,
            user_stack_size,
            argv_strs,
            envp_strs,
        ) {
            Ok(rsp) => rsp,
            Err(_) => {
                crate::hal::write_cr3(saved_cr3);
                return Err(-38);
            }
        };
        let user_rsp = user_stack_base + (phys_rsp - phys_stack_base);
        // setup_user_stack_full stored the string pointers as absolute
        // addresses in the phys frame; remap them to the user stack VA.
        let delta = user_stack_base.wrapping_sub(phys_stack_base);
        let argc = argv_strs.len().min(63);
        for i in 0..argc {
            let slot = phys_rsp + 8 + (i as u64) * 8;
            let val = core::ptr::read_volatile(slot as *mut u64);
            core::ptr::write_volatile(slot as *mut u64, val.wrapping_add(delta));
        }
        let argv_null = phys_rsp + 8 + (argc as u64) * 8;
        let envc = envp_strs.len().min(63);
        for j in 0..envc {
            let slot = argv_null + 8 + (j as u64) * 8;
            let val = core::ptr::read_volatile(slot as *mut u64);
            core::ptr::write_volatile(slot as *mut u64, val.wrapping_add(delta));
        }
        crate::hal::write_cr3(saved_cr3);

        // Map the user stack into the new root: VA → allocated PA.
        let user_flags = crate::hal::pte_user_flags();
        let mut va = stack_start;
        let mut pa = phys_stack_base;
        while va < stack_end {
            if crate::pagetable::map_page(root, va, pa, user_flags).is_err() {
                return Err(-12);
            }
            va += 0x1000;
            pa += 0x1000;
        }

        // Map the brk heap range (heap base .. base + 1 MiB) with private
        // physical pages so the exec'd process's bump allocator (minix-rt
        // hardcodes this range) has real backing. VM registered the matching
        // VR_DATA region at VM_EXEC_NEWMEM time, so heap faults are resolved
        // by VM's demand-paging as the brk grows.
        let brk_start = crate::hal::user_heap_base();
        let brk_end = brk_start + 0x100000u64;
        let brk_pages = ((brk_end - brk_start) / 4096) as usize;
        if let Some(brk_phys) = crate::hal::alloc_phys_contig(brk_pages) {
            let mut brk_va = brk_start;
            let mut brk_pa = brk_phys;
            while brk_va < brk_end {
                if crate::pagetable::map_page(root, brk_va, brk_pa, user_flags).is_err() {
                    break;
                }
                brk_va += 0x1000;
                brk_pa += 0x1000;
            }
        }

        // Set the new page table.
        core::ptr::write_volatile(&mut (*rp).p_seg.p_cr3, root);

        // Architecture-specific register setup.
        let rsp_fb = if user_rsp == 0 {
            user_stack_base + user_stack_size as u64 - 0x30
        } else {
            user_rsp
        };
        let argc = argv_strs.len() as u64;
        let argv_ptr = rsp_fb + 8;

        crate::hal::exec_init_regs(&mut (*rp).p_reg, entry, rsp_fb, argc, argv_ptr);
        (*rp).p_misc_flags.fetch_or(
            crate::proc::MiscFlags::CONTEXT_SET.bits(),
            core::sync::atomic::Ordering::SeqCst,
        );

        // Clean up legacy misc flags
        {
            use crate::proc::MiscFlags;
            // Clear MF_DELIVERMSG if set.
            let old_mf = (*rp)
                .p_misc_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            (*rp).p_misc_flags.store(
                old_mf & !MiscFlags::DELIVERMSG.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            // Mark FPU regs as not significant.
            let old_mf2 = (*rp)
                .p_misc_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            (*rp).p_misc_flags.store(
                old_mf2 & !MiscFlags::FPU_INITIALIZED.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            crate::hal::release_fpu(rp as *mut core::ffi::c_void);
        }

        Ok(ExecLoadResult { entry, rsp: rsp_fb })
    }
}

/// Get the signal manager endpoint for a process.
///
/// Returns the endpoint of the signal manager (typically PM) for the
/// given process, or `None` if the process has no valid signal manager.
///
/// Matching C: `priv(rp)->s_sig_mgr` in `cause_sig()` (system.c).
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
unsafe fn get_sig_manager(rp: *const crate::proc::Proc) -> Option<i32> {
    unsafe {
        let priv_ptr = (*rp).p_priv;
        if priv_ptr.is_null() {
            return None;
        }
        let mgr = (*priv_ptr).s_sig_mgr;
        // s_sig_mgr stores the proc_nr (e.g. PM_PROC_NR = 0).
        // For generation-0 boot processes, the kernel endpoint equals
        // the proc_nr (make_endpoint(0, n) = n). Check if valid.
        if mgr < 0 || mgr >= crate::proc::NR_PROCS as i32 {
            return None;
        }
        Some(mgr)
    }
}

/// Initialize basic syscall handlers.
///
/// # Safety
///
/// Must be called exactly once during boot.
pub unsafe fn init_basic_syscalls() {
    unsafe {
        // Syscall numbers match POSIX convention (minix-rt constants):
        // 0 = exit, 2 = read, 3 = write, 4 = open, 5 = close,
        // 9 = ... no, wait. Let me use the CORRECT mapping.
        // The userland (minix-rt) uses:
        //   NR_EXIT=0, NR_READ=2, NR_WRITE=3, NR_OPEN=4, NR_CLOSE=5
        //   NR_GETPID=20, NR_BRK=36
        // The kernel handles these syscalls.
        register_basic_syscall(0, sys_exit_handler); // NR_EXIT
        register_basic_syscall(2, sys_read_handler); // NR_READ
        register_basic_syscall(3, sys_write_handler); // NR_WRITE
        register_basic_syscall(4, sys_open_handler); // NR_OPEN
        register_basic_syscall(5, sys_close_handler); // NR_CLOSE
        register_basic_syscall(20, sys_getpid_handler); // NR_GETPID
        register_basic_syscall(36, sys_brk_handler); // NR_BRK
        register_basic_syscall(40, sys_mkdir_handler); // NR_MKDIR
        register_basic_syscall(41, sys_unlink_handler); // NR_UNLINK
        register_basic_syscall(42, sys_rmdir_handler); // NR_RMDIR
        register_basic_syscall(43, sys_link_handler); // NR_LINK
        register_basic_syscall(44, sys_chmod_handler); // NR_CHMOD
        register_basic_syscall(45, sys_chown_handler); // NR_CHOWN
        register_basic_syscall(56, sys_mknod_handler); // NR_MKNOD
        register_basic_syscall(57, sys_getdents_handler); // NR_GETDENTS
        // IPC syscalls (from minix-std): 46=SEND, 47=RECEIVE, 48=SENDREC, 49=NOTIFY
        register_basic_syscall(46, sys_ipc_send_handler); // SEND
        register_basic_syscall(47, sys_ipc_receive_handler); // RECEIVE
        register_basic_syscall(48, sys_ipc_sendrec_handler); // SENDREC
        register_basic_syscall(49, sys_ipc_notify_handler); // NOTIFY
        register_basic_syscall(50, sys_kernel_call_handler); // NR_KERNEL_CALL
        register_basic_syscall(51, sys_ipc_sendnb_handler); // SENDNB
        register_basic_syscall(52, sys_ipc_senda_handler); // SENDA
        register_basic_syscall(53, sys_setfdvfs_handler); // NR_SETFDVFS
        // Thread syscalls
        register_basic_syscall(54, sys_thread_create_handler); // NR_THREAD_CREATE
        register_basic_syscall(55, sys_thread_exit_handler); // NR_THREAD_EXIT
        register_basic_syscall(58, sys_thread_join_handler); // NR_THREAD_JOIN
        register_basic_syscall(59, sys_thread_yield_handler); // NR_THREAD_YIELD
        register_basic_syscall(60, sys_thread_set_tls_handler); // NR_THREAD_SET_TLS
        register_basic_syscall(61, sys_futex_wait_handler); // NR_FUTEX_WAIT
        register_basic_syscall(62, sys_futex_wake_handler); // NR_FUTEX_WAKE
        register_basic_syscall(63, sys_hang_dump_handler); // NR_HANGDUMP
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::proc_init;

    #[test]
    fn test_getpid_returns_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            (*rp).p_endpoint = 42;
            let args = [0u64; 6];
            assert_eq!(sys_getpid_handler(rp, &args), 42);
        }
    }

    #[test]
    #[ignore = "requires ring 0 (I/O port access)"]
    fn test_write_stdout_returns_count() {
        unsafe {
            let buf = [0u8; 10];
            let args = [1u64, buf.as_ptr() as u64, 10u64, 0, 0, 0];
            let rp = crate::table::proc_addr(0);
            assert_eq!(sys_write_handler(rp, &args), 10);
        }
    }

    #[test]
    fn test_write_regular_fd_null_buf_returns_efault() {
        // Writes to regular fds (>= 3) forward to VFS, which validates the
        // fd (VFS's do_write returns EBADF for a bad one). The kernel-side
        // EFAULT check still applies before routing.
        unsafe {
            let args = [3u64, 0u64, 10u64, 0, 0, 0]; // fd 3, null buf
            let rp = crate::table::proc_addr(0);
            assert_eq!(sys_write_handler(rp, &args), -14);
        }
    }

    #[test]
    fn test_write_null_buf_returns_efault() {
        unsafe {
            let args = [1u64, 0u64, 10u64, 0, 0, 0]; // null buf
            let rp = crate::table::proc_addr(0);
            assert_eq!(sys_write_handler(rp, &args), -14);
        }
    }

    #[test]
    fn test_brk_query_returns_current() {
        unsafe {
            proc_init();
            CURRENT_BRK.store(0x3FE01000, Ordering::Relaxed);
            let args = [0u64, 0, 0, 0, 0, 0];
            assert_eq!(sys_brk_handler(core::ptr::null_mut(), &args), 0x3FE01000i64);
        }
    }

    #[test]
    fn test_brk_set_valid() {
        unsafe {
            CURRENT_BRK.store(0x3FE00000, Ordering::Relaxed);
            let args = [0x3FE02000u64, 0, 0, 0, 0, 0];
            assert_eq!(sys_brk_handler(core::ptr::null_mut(), &args), 0x3FE02000i64);
            assert_eq!(CURRENT_BRK.load(Ordering::Relaxed), 0x3FE02000);
        }
    }

    #[test]
    fn test_brk_out_of_range() {
        unsafe {
            let args = [0x40000000u64, 0, 0, 0, 0, 0];
            assert_eq!(sys_brk_handler(core::ptr::null_mut(), &args), -12);
        }
    }

    #[test]
    fn test_dispatch_unknown_syscall_returns_enosys() {
        unsafe {
            let rp = crate::table::proc_addr(0);
            assert_eq!(dispatch_basic_syscall(rp, 999, &[0u64; 6]), -38);
        }
    }

    #[test]
    fn test_exit_frees_slot_and_stores_status() {
        unsafe {
            proc_init();
            #[cfg(target_arch = "x86_64")]
            crate::hal::init_cpulocals();
            // Exit a non-PM process (proc 1) so the SIGKSIG notification to
            // PM (proc 0) does not target the exiting process itself.
            let rp = crate::table::proc_addr(1);
            (*rp).p_nr = 1;
            (*rp).p_endpoint = 100;
            (*rp).p_pending = 0;
            (*rp)
                .p_rts_flags
                .store(0, core::sync::atomic::Ordering::Relaxed);
            let args = [42u64, 0, 0, 0, 0, 0];
            let result = sys_exit_handler(rp, &args);
            assert_eq!(result, crate::system::EDONTREPLY as i64);
            // Should free the Proc slot
            let flags = (*rp)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            assert!(
                flags & crate::proc::RtsFlags::SLOT_FREE.bits() != 0,
                "exit should free the Proc slot"
            );
            // Should store exit status in p_signal_received
            assert_eq!((*rp).p_signal_received, 42);
            // Invariant PM's SYS_GETKSIG loop relies on: an exit carries the
            // status in p_signal_received and NEVER sets p_pending (only
            // cause_sig does). A signal-only reply has p_pending != 0 and
            // p_signal_received == 0; exit_proc must only run for the former.
            assert_eq!((*rp).p_pending, 0, "exit must not set p_pending");
            // Should have queued a pending exit notification
            let pending = pop_pending_exit();
            assert_eq!(pending, Some((100, 42)));
        }
    }

    #[test]
    fn test_init_registers_getpid() {
        unsafe {
            proc_init();
            init_basic_syscalls();
            let rp = crate::table::proc_addr(0);
            (*rp).p_endpoint = 42;
            assert_eq!(dispatch_basic_syscall(rp, 20, &[0u64; 6]), 42);
        }
    }

    #[test]
    fn test_init_registers_brk() {
        unsafe {
            CURRENT_BRK.store(0x3FE00000, Ordering::Relaxed);
            init_basic_syscalls();
            assert_eq!(
                dispatch_basic_syscall(core::ptr::null_mut(), 36, &[0u64, 0, 0, 0, 0, 0]),
                0x3FE00000i64
            );
        }
    }

    #[test]
    fn test_handler_signatures() {
        fn _check(_: BasicSyscallFn) {}
        _check(sys_getpid_handler);
        _check(sys_exit_handler);
        _check(sys_write_handler);
        _check(sys_brk_handler);
    }

    #[test]
    fn test_pending_exit_queue_empty() {
        unsafe {
            // Drain any leftover from previous tests
            while pop_pending_exit().is_some() {}
            assert!(pop_pending_exit().is_none());
        }
    }

    #[test]
    fn test_pending_exit_queue_roundtrip() {
        unsafe {
            // Drain any leftover
            while pop_pending_exit().is_some() {}
            assert!(push_pending_exit(42, 7));
            assert!(push_pending_exit(43, 8));
            assert_eq!(pop_pending_exit(), Some((42, 7)));
            assert_eq!(pop_pending_exit(), Some((43, 8)));
            assert!(pop_pending_exit().is_none());
        }
    }

    #[test]
    fn test_pending_exit_queue_full() {
        unsafe {
            while pop_pending_exit().is_some() {}
            // Fill the queue
            for i in 0..PENDING_EXIT_QUEUE_SIZE {
                assert!(push_pending_exit(i as i32, 0));
            }
            // Next push should fail
            assert!(!push_pending_exit(999, 0));
            // Drain
            for _ in 0..PENDING_EXIT_QUEUE_SIZE {
                assert!(pop_pending_exit().is_some());
            }
            assert!(pop_pending_exit().is_none());
        }
    }
}
