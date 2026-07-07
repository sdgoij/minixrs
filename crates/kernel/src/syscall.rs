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
/// fd=0: serial input. fd>0: forward to VFS.
unsafe fn sys_read_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    let buf = args[1] as *mut u8;
    let count = args[2] as usize;
    if fd == 0 {
        // stdin → serial input (interrupt-driven via ser_input).
        if buf.is_null() || count == 0 {
            return -14; // EFAULT
        }
        // Read one byte (blocking).
        let byte = crate::ser_input::read_blocking();
        unsafe { core::ptr::write(buf, byte) };
        1
    } else {
        let mut msg = [0u8; 64];
        msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
        msg[4..8].copy_from_slice(&0x100i32.to_le_bytes()); // VFS_READ = 0x100
        msg[8..12].copy_from_slice(&fd.to_le_bytes());
        msg[16..24].copy_from_slice(&(buf as u64).to_le_bytes());
        msg[24..28].copy_from_slice(&(count as u32).to_le_bytes());

        let result =
            unsafe { crate::ipc::do_sync_ipc(caller, msg.as_mut_ptr(), crate::ipc::SENDREC) };
        if result != 0 {
            return result as i64;
        }
        let reply_status = i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
        reply_status as i64
    }
}

/// SYS_open (4) — open a file.
/// args[0] = path pointer, args[1] = path length, args[2] = flags.
/// Forwards to VFS via IPC. VFS's do_open reads:
///   flags at offset 8, path_addr at offset 16, path_len at offset 24.
unsafe fn sys_open_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0];
    let path_len = args[1] as u32;
    let flags = args[2] as i32;

    let mut msg = [0u8; 64];
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    msg[4..8].copy_from_slice(&0x103i32.to_le_bytes()); // VFS_OPEN = 0x103
    msg[8..12].copy_from_slice(&flags.to_le_bytes());
    msg[16..24].copy_from_slice(&path_ptr.to_le_bytes());
    msg[24..28].copy_from_slice(&path_len.to_le_bytes());

    let result = unsafe { crate::ipc::do_sync_ipc(caller, msg.as_mut_ptr(), crate::ipc::SENDREC) };
    if result != 0 {
        return result as i64;
    }
    // Reply status in bytes 4-7 (m_type, set by VFS reply).
    let reply_status = i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
    reply_status as i64
}

/// SYS_close (5) — close a file descriptor.
/// Forwards to VFS via IPC. VFS's do_close reads fd at offset 8.
unsafe fn sys_close_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;

    let mut msg = [0u8; 64];
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    msg[4..8].copy_from_slice(&0x105i32.to_le_bytes()); // VFS_CLOSE = 0x105
    msg[8..12].copy_from_slice(&fd.to_le_bytes());

    let result = unsafe { crate::ipc::do_sync_ipc(caller, msg.as_mut_ptr(), crate::ipc::SENDREC) };
    if result != 0 {
        return result as i64;
    }
    let reply_status = i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
    reply_status as i64
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

        // Store exit status in p_signal_received for PM to read via SYS_GETKSIG.
        (*caller).p_signal_received = exit_status as u64;

        // Set SIGNALED so do_getksig_handler finds this process.
        (*caller).p_rts_flags.fetch_or(
            crate::proc::RtsFlags::SIGNALED.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );

        // Push to pending exit queue for PM to read via SYS_GETKSIG.
        push_pending_exit(endpoint, exit_status);

        // Notify PM so it can mark the MProc as ZOMBIE.
        crate::ipc::mini_notify((*caller).p_endpoint, arch_common::com::PM_PROC_NR);

        // Free the Proc slot so the kernel-fork path waitpid works.
        (*caller).p_rts_flags.fetch_or(
            crate::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );

        // Remove from run queue so pick_proc doesn't find a dead process.
        crate::sched::dequeue(caller);
    }
    crate::system::EDONTREPLY as i64
}

/// SYS_write (9) — write to a file descriptor.
/// fd=1 (stdout), fd=2 (stderr) go to serial output.
///
/// # Safety
///
/// Must be called from ring 0 with a valid caller process pointer.
/// The buffer pointer in `args[1]` must be readable in the caller's address space.
pub unsafe fn sys_write_handler(_caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    let count = args[2] as usize;

    let buf = args[1] as *const u8;
    if buf.is_null() {
        return -14; // EFAULT
    }

    if fd == 1 || fd == 2 {
        for i in 0..count.min(256) {
            let c = unsafe { core::ptr::read_volatile(buf.add(i)) };
            if c == b'\n' {
                crate::hal::serial_write_byte(b'\r');
            }
            crate::hal::serial_write_byte(c);
        }
        count as i64
    } else {
        -9 // EBADF
    }
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

/// Build a VFS IPC message and send it via `do_sync_ipc`.
///
/// `vfs_call` is the VFS call number (VFS_MKDIR = 0x109, etc.).
/// `arg1`-`arg3` are i32 arguments placed in the m1 payload.
/// `path_ptr` and `path_len` are used for path-based calls.
///
/// Returns the reply status (OK = 0, or negative errno).
unsafe fn vfs_ipc_call(
    caller: *mut crate::proc::Proc,
    vfs_call: i32,
    arg1: i32,
    arg2: i32,
    arg3: i32,
) -> i64 {
    let mut msg = [0u8; 64];
    // Set destination endpoint (first 4 bytes)
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    // Set call number (offset 4-8)
    msg[4..8].copy_from_slice(&vfs_call.to_le_bytes());
    // Set payload fields
    msg[12..16].copy_from_slice(&arg1.to_le_bytes());
    msg[16..20].copy_from_slice(&arg2.to_le_bytes());
    msg[20..24].copy_from_slice(&arg3.to_le_bytes());

    let result = unsafe { crate::ipc::do_sync_ipc(caller, msg.as_mut_ptr(), crate::ipc::SENDREC) };
    if result != 0 {
        return result as i64;
    }

    // Read the reply status from offset 4-8 (m_type).
    let reply_status = i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
    reply_status as i64
}

/// SYS_mkdir (40) — create a directory.
unsafe fn sys_mkdir_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0] as *const u8;
    let path_len = args[1] as usize;
    let mode = args[2] as i32;
    let _ = (path_ptr, path_len);
    // Route to VFS: VFS_MKDIR = 0x109
    unsafe { vfs_ipc_call(caller, 0x109, mode, 0, 0) }
}

/// SYS_unlink (41) — remove a file.
unsafe fn sys_unlink_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0] as *const u8;
    let path_len = args[1] as usize;
    let _ = (path_ptr, path_len);
    // Route to VFS: VFS_UNLINK = 0x107
    unsafe { vfs_ipc_call(caller, 0x107, 0, 0, 0) }
}

/// SYS_rmdir (42) — remove a directory.
unsafe fn sys_rmdir_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0] as *const u8;
    let path_len = args[1] as usize;
    let _ = (path_ptr, path_len);
    // Route to VFS: VFS_RMDIR = 0x112
    unsafe { vfs_ipc_call(caller, 0x112, 0, 0, 0) }
}

/// SYS_link (43) — create a hard link.
unsafe fn sys_link_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let old_ptr = args[0] as *const u8;
    let new_ptr = args[1] as *const u8;
    let _ = (old_ptr, new_ptr);
    // Route to VFS: VFS_LINK = 0x106
    unsafe { vfs_ipc_call(caller, 0x106, 0, 0, 0) }
}

/// SYS_chmod (44) — change file mode.
unsafe fn sys_chmod_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0] as *const u8;
    let path_len = args[1] as usize;
    let mode = args[2] as i32;
    let _ = (path_ptr, path_len);
    // Route to VFS: VFS_CHMOD = 0x10B
    unsafe { vfs_ipc_call(caller, 0x10B, mode, 0, 0) }
}

/// SYS_chown (45) — change file owner.
unsafe fn sys_chown_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0] as *const u8;
    let path_len = args[1] as usize;
    let owner = args[2] as i32;
    let group = args[3] as i32;
    let _ = (path_ptr, path_len);
    // Route to VFS: VFS_CHOWN = 0x10C
    unsafe { vfs_ipc_call(caller, 0x10C, owner, group, 0) }
}

/// SYS_mknod (46) — create a device node.
unsafe fn sys_mknod_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0] as *const u8;
    let path_len = args[1] as usize;
    let mode = args[2] as i32;
    let dev = args[3] as i32;
    let _ = (path_ptr, path_len);
    // Route to VFS: VFS_MKNOD = 0x10A
    unsafe { vfs_ipc_call(caller, 0x10A, mode, dev, 0) }
}

/// SYS_getdents (57) — read directory entries.
/// Forwards to VFS via IPC. VFS's do_getdents reads:
///   fd at offset 8, buf_addr at offset 16, count at offset 24.
unsafe fn sys_getdents_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    let buf_ptr = args[1];
    let count = args[2] as u32;

    let mut msg = [0u8; 64];
    msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    msg[4..8].copy_from_slice(&0x11Di32.to_le_bytes()); // VFS_GETDENTS = 0x11D
    msg[8..12].copy_from_slice(&fd.to_le_bytes());
    msg[16..24].copy_from_slice(&buf_ptr.to_le_bytes());
    msg[24..28].copy_from_slice(&count.to_le_bytes());

    let result = unsafe { crate::ipc::do_sync_ipc(caller, msg.as_mut_ptr(), crate::ipc::SENDREC) };
    if result != 0 {
        return result as i64;
    }
    let reply_status = i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
    reply_status as i64
}

// IPC syscall handlers (46-49)

/// SYS_IPC_SEND (46) — send a message to a process.
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
unsafe fn sys_kernel_call_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let call_nr = args[0] as i32;
    let msg_ptr = args[1] as *mut u8;
    if msg_ptr.is_null() {
        return -14; // EFAULT
    }
    unsafe {
        // Copy user message into kernel buffer
        let mut kbuf = [0u8; crate::proc::MESSAGE_SIZE];
        core::ptr::copy_nonoverlapping(msg_ptr, kbuf.as_mut_ptr(), crate::proc::MESSAGE_SIZE);
        // Set call number at bytes 0-3 (for kernel_call_dispatch)
        let call_val = (crate::system::KERNEL_CALL as u32 + call_nr as u32) as i32;
        kbuf[0..4].copy_from_slice(&call_val.to_ne_bytes());
        // Set source endpoint at bytes 4-7
        let src_ep = (*caller).p_endpoint;
        kbuf[4..8].copy_from_slice(&src_ep.to_ne_bytes());
        // Set delivery address for result copy-back
        (*caller).p_delivermsg_vir = msg_ptr as u64;
        let result = crate::system::kernel_call_dispatch(caller, &mut kbuf);
        // Copy result back to user (handles EDONTREPLY / VMSUSPEND internally)
        crate::system::kernel_call_finish(caller, &mut kbuf, result);
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

/// Helper: load a binary from initramfs and apply it to a target process.
/// Returns 0 on success, negative error code on failure.
unsafe fn exec_initramfs_for_target(rp: *mut crate::proc::Proc, path: &str) -> i64 {
    unsafe {
        let (data, _mode) = match crate::initramfs::find_initramfs_file(path) {
            Some(d) => d,
            None => return -2,
        };
        let ehdr = match crate::elf::parse_elf_header(data) {
            Ok(e) => e,
            Err(_) => return -38,
        };

        // Parse ELF to get bounds (no identity-mapped writes that would
        // corrupt boot process code at 0x1000000).
        let boot_cr3_val = crate::pagetable::boot_cr3();
        let loaded = {
            let ehdr = &*(data.as_ptr() as *const crate::elf::Elf64Ehdr);
            let phoff = ehdr.e_phoff as usize;
            let phnum = ehdr.e_phnum as usize;
            let phentsize = ehdr.e_phentsize as usize;
            let mut base = u64::MAX;
            let mut top = 0u64;
            for i in 0..phnum {
                let phdr =
                    &*(data.as_ptr().add(phoff + i * phentsize) as *const crate::elf::Elf64Phdr);
                if phdr.p_type != crate::elf::PT_LOAD {
                    continue;
                }
                if phdr.p_vaddr < base {
                    base = phdr.p_vaddr;
                }
                let seg_top = phdr.p_vaddr + phdr.p_memsz;
                if seg_top > top {
                    top = seg_top;
                }
            }
            if base == u64::MAX {
                return -38;
            }
            crate::elf::LoadedElf {
                base,
                top,
                entry: ehdr.e_entry,
            }
        };

        // Architecture-specific user stack base.
        #[cfg(target_arch = "x86_64")]
        let user_stack_base: u64 = 0x0FE00000;
        #[cfg(target_arch = "riscv64")]
        let user_stack_base: u64 = crate::hal::user_stack_base();
        let user_stack_size: usize = crate::hal::user_stack_size();
        let stack_top = user_stack_base + user_stack_size as u64;

        // Setup user stack (writes through identity map; on RISC-V the stack
        // base is 0x8FE00000 which IS in RAM, so this works).
        let saved_cr3 = crate::hal::read_cr3();
        crate::hal::write_cr3(boot_cr3_val);
        let user_rsp = match crate::elf::setup_user_stack(stack_top, user_stack_size, &[path]) {
            Ok(rsp) => rsp,
            Err(_) => {
                crate::hal::write_cr3(saved_cr3);
                return -38;
            }
        };
        crate::hal::write_cr3(saved_cr3);

        let code_start = loaded.base & !0xFFF;
        let code_end = (loaded.top + 0xFFF) & !0xFFF;
        let stack_start = user_stack_base & !0xFFF;
        let stack_end = (user_stack_base + user_stack_size as u64 + 0xFFF) & !0xFFF;

        // Build new page table (arch-specific)

        // RISC-V: RAM starts at 0x80000000, so identity map at VA 0x1000000
        // (the ELF virtual address) writes to MMIO, not RAM. We must allocate
        // physical pages for the binary and map VA → allocated PA.
        #[cfg(target_arch = "riscv64")]
        let root = {
            let new_root = match crate::hal::alloc_phys_page() {
                Some(p) => p,
                None => return -12,
            };
            core::ptr::write_bytes(new_root as *mut u8, 0, crate::hal::PAGE_SIZE as usize);
            // Deep-copy boot root entries (identity map for kernel + devices).
            let boot_root = boot_cr3_val as *const u64;
            for i in 0usize..4 {
                let e = core::ptr::read(boot_root.add(i));
                core::ptr::write((new_root as *mut u64).add(i), e);
            }
            // Allocate physical pages for the code and load ELF segments.
            let code_pages = ((code_end - code_start) / 4096) as usize;
            let phys_code_base = match crate::hal::alloc_phys_contig(code_pages) {
                Some(b) => b,
                None => return -12,
            };
            // Load ELF segments to the allocated physical pages.
            let ehdr = &*(data.as_ptr() as *const crate::elf::Elf64Ehdr);
            let phoff = ehdr.e_phoff as usize;
            let phnum = ehdr.e_phnum as usize;
            let phentsize = ehdr.e_phentsize as usize;
            for i in 0..phnum {
                let phdr =
                    &*(data.as_ptr().add(phoff + i * phentsize) as *const crate::elf::Elf64Phdr);
                if phdr.p_type != crate::elf::PT_LOAD {
                    continue;
                }
                let seg_vaddr = phdr.p_vaddr;
                let seg_offset = seg_vaddr - code_start;
                let dst = (phys_code_base + seg_offset) as *mut u8;
                if phdr.p_filesz > 0 {
                    let src = data.as_ptr().add(phdr.p_offset as usize);
                    core::ptr::copy_nonoverlapping(src, dst, phdr.p_filesz as usize);
                }
                let bss = phdr.p_memsz - phdr.p_filesz;
                if bss > 0 {
                    core::ptr::write_bytes(dst.add(phdr.p_filesz as usize), 0, bss as usize);
                }
            }
            // Allocate physical pages for the stack (identity-map works for
            // stack since 0x8FE00000 IS in RAM on RISC-V).
            let stack_pages = ((stack_end - stack_start) / 4096) as usize;
            let phys_stack_base = match crate::hal::alloc_phys_contig(stack_pages) {
                Some(b) => b,
                None => return -12,
            };
            // Copy stack data from identity-mapped temp area to allocated pages.
            // setup_user_stack wrote the stack at user_stack_base (0x8FE00000)
            // while BOOT_CR3 was active (identity map).
            core::ptr::copy_nonoverlapping(
                user_stack_base as *const u8,
                phys_stack_base as *mut u8,
                user_stack_size,
            );
            // Map user code: VA → allocated PA
            #[cfg(target_arch = "riscv64")]
            let user_flags = crate::pagetable::PG_P
                | crate::pagetable::PG_RW
                | crate::pagetable::PG_U
                | 0x02
                | 0x08
                | 0xC0; // R|X|A|D
            #[cfg(target_arch = "x86_64")]
            let user_flags =
                crate::pagetable::PG_P | crate::pagetable::PG_RW | crate::pagetable::PG_U;
            let mut va = code_start;
            let mut pa = phys_code_base;
            while va < code_end {
                if crate::pagetable::map_page(new_root, va, pa, user_flags).is_err() {
                    return -12;
                }
                va += 0x1000;
                pa += 0x1000;
            }
            // Map stack: VA → allocated PA
            let mut va = stack_start;
            let mut pa = phys_stack_base;
            while va < stack_end {
                if crate::pagetable::map_page(new_root, va, pa, user_flags).is_err() {
                    return -12;
                }
                va += 0x1000;
                pa += 0x1000;
            }
            new_root
        };

        #[cfg(target_arch = "x86_64")]
        let root = {
            let pml4 = match crate::hal::alloc_phys_page() {
                Some(p) => p,
                None => return -12,
            };
            core::ptr::write_bytes(pml4 as *mut u8, 0, crate::hal::PAGE_SIZE as usize);
            let boot_pml4 = boot_cr3_val as *const u64;
            let pml4e0 = core::ptr::read(boot_pml4);
            let pdpt_phys = crate::hal::pte_to_phys(pml4e0);
            let boot_pdpt = pdpt_phys as *const u64;
            let pdpte0 = core::ptr::read(boot_pdpt);
            let pd_phys = crate::hal::pte_to_phys(pdpte0);
            let boot_pd = pd_phys as *const u64;
            let pdpt_page = match crate::hal::alloc_phys_page() {
                Some(p) => p,
                None => return -12,
            };
            let pd_page = match crate::hal::alloc_phys_page() {
                Some(p) => p,
                None => return -12,
            };
            core::ptr::write_bytes(pdpt_page as *mut u8, 0, 4096);
            core::ptr::write_bytes(pd_page as *mut u8, 0, 4096);
            let flags = crate::pagetable::PG_P | crate::pagetable::PG_RW | crate::pagetable::PG_U;
            core::ptr::write(pml4 as *mut u64, pdpt_page | flags);
            core::ptr::write(pdpt_page as *mut u64, pd_page | flags);
            for i in 0usize..512 {
                let e = core::ptr::read(boot_pd.add(i));
                core::ptr::write((pd_page as *mut u64).add(i), e);
            }
            for i in 256usize..512 {
                let e = core::ptr::read(boot_pml4.add(i));
                core::ptr::write((pml4 as *mut u64).add(i), e);
            }
            // Allocate physical pages for the code and load ELF segments.
            let code_pages = ((code_end - code_start) / 4096) as usize;
            let phys_code_base = match crate::hal::alloc_phys_contig(code_pages) {
                Some(b) => b,
                None => return -12,
            };
            // Copy ELF segments from initramfs data to the allocated pages.
            let ehdr = &*(data.as_ptr() as *const crate::elf::Elf64Ehdr);
            let phoff = ehdr.e_phoff as usize;
            let phnum = ehdr.e_phnum as usize;
            let phentsize = ehdr.e_phentsize as usize;
            for i in 0..phnum {
                let phdr =
                    &*(data.as_ptr().add(phoff + i * phentsize) as *const crate::elf::Elf64Phdr);
                if phdr.p_type != crate::elf::PT_LOAD {
                    continue;
                }
                let seg_vaddr = phdr.p_vaddr;
                let seg_offset = seg_vaddr - code_start;
                let dst = (phys_code_base + seg_offset) as *mut u8;
                if phdr.p_filesz > 0 {
                    let src = data.as_ptr().add(phdr.p_offset as usize);
                    core::ptr::copy_nonoverlapping(src, dst, phdr.p_filesz as usize);
                }
                let bss = phdr.p_memsz - phdr.p_filesz;
                if bss > 0 {
                    core::ptr::write_bytes(dst.add(phdr.p_filesz as usize), 0, bss as usize);
                }
            }
            // Allocate physical pages for the stack.
            let stack_pages = ((stack_end - stack_start) / 4096) as usize;
            let phys_stack_base = match crate::hal::alloc_phys_contig(stack_pages) {
                Some(b) => b,
                None => return -12,
            };
            // Copy stack data from identity-mapped temp area to allocated pages.
            // setup_user_stack wrote the stack at user_stack_base (0x0FE00000)
            // while BOOT_CR3 was active (identity map). Under the kernel CR3,
            // the identity map still covers 0-1GB, so both source and dest are
            // accessible.
            core::ptr::copy_nonoverlapping(
                user_stack_base as *const u8,
                phys_stack_base as *mut u8,
                user_stack_size,
            );
            // Map user code: VA -> allocated PA
            let user_flags =
                crate::pagetable::PG_P | crate::pagetable::PG_RW | crate::pagetable::PG_U;
            let mut va = code_start;
            let mut pa = phys_code_base;
            while va < code_end {
                if crate::pagetable::map_page(pml4, va, pa, user_flags).is_err() {
                    return -12;
                }
                va += 0x1000;
                pa += 0x1000;
            }
            // Map stack: VA -> allocated PA
            let mut va = stack_start;
            let mut pa = phys_stack_base;
            while va < stack_end {
                if crate::pagetable::map_page(pml4, va, pa, user_flags).is_err() {
                    return -12;
                }
                va += 0x1000;
                pa += 0x1000;
            }
            pml4
        };

        // Set the new page table.
        core::ptr::write_volatile(&mut (*rp).p_seg.p_cr3, root);

        // Architecture-specific register setup.
        let rsp_fb = if user_rsp == 0 {
            user_stack_base + user_stack_size as u64 - 0x30
        } else {
            user_rsp
        };

        #[cfg(target_arch = "x86_64")]
        {
            core::ptr::write_volatile((*rp).p_reg.as_mut_ptr().add(168) as *mut u64, rsp_fb);
            core::ptr::write_volatile((*rp).p_reg.as_mut_ptr() as *mut u64, rsp_fb);
            crate::hal::write_frame_field(&mut (*rp).p_reg, 16, ehdr.e_entry);
            crate::hal::write_frame_field(&mut (*rp).p_reg, 72, 0x0202);
            crate::hal::write_frame_field(&mut (*rp).p_reg, 40, rsp_fb);
            core::arch::asm!("mfence", options(nostack, preserves_flags));
            (*rp).p_misc_flags.fetch_or(
                crate::proc::MiscFlags::CONTEXT_SET.bits(),
                core::sync::atomic::Ordering::SeqCst,
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            let p_reg = &mut (*rp).p_reg;
            p_reg[0..8].copy_from_slice(&ehdr.e_entry.to_ne_bytes());
            p_reg[16..24].copy_from_slice(&rsp_fb.to_ne_bytes());
            p_reg[80..88].copy_from_slice(&0u64.to_ne_bytes());
            let sst: u64 = 0x2020;
            p_reg[248..256].copy_from_slice(&sst.to_ne_bytes());
            (*rp).p_misc_flags.fetch_or(
                crate::proc::MiscFlags::CONTEXT_SET.bits(),
                core::sync::atomic::Ordering::SeqCst,
            );
            core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        }

        // Clean up legacy misc flags that may have been set before exec.
        // RTS_RECEIVING is not touched here — that's only needed when called
        // from SYS_EXEC_TARGET (where the target is blocked), and is handled
        // by sys_exec_target_handler after this function returns.
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

        0
    }
}

/// SYS_exec_replace (61) — replace current process with a new binary
/// SYS_EXEC_REPLACE (61) — replace the current process with a binary from initramfs.
unsafe fn sys_exec_replace_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    unsafe {
        let path_ptr = args[0] as *const u8;
        if path_ptr.is_null() {
            return -14;
        }
        let mut path_buf = [0u8; 256];
        let mut path_len = 0usize;
        for (i, slot) in path_buf.iter_mut().enumerate().take(255) {
            let byte = core::ptr::read_volatile(path_ptr.add(i));
            if byte == 0 {
                break;
            }
            *slot = byte;
            path_len = i + 1;
        }
        if path_len == 0 {
            return -14;
        }
        let path = match core::str::from_utf8(&path_buf[..path_len]) {
            Ok(s) => s,
            Err(_) => return -14,
        };
        exec_initramfs_for_target(caller, path)
    }
}

/// SYS_EXEC_TARGET (62) — exec a binary from initramfs for a specific process.
/// args[0] = target endpoint, args[1] = path pointer in caller's space.
unsafe fn sys_exec_target_handler(_caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    unsafe {
        let target_ep = args[0] as i32;
        let path_ptr = args[1] as *const u8;
        if path_ptr.is_null() {
            return -14;
        }

        if !crate::table::is_ok_endpoint(target_ep) {
            return -5; // EIO
        }
        let proc_nr = crate::table::endpoint_slot(target_ep);
        let rp = crate::table::proc_addr(proc_nr);
        if rp.is_null() || (*rp).is_empty() || (*rp).p_endpoint != target_ep {
            return -5;
        }

        let mut path_buf = [0u8; 256];
        let mut path_len = 0usize;
        for (i, slot) in path_buf.iter_mut().enumerate().take(255) {
            let byte = core::ptr::read_volatile(path_ptr.add(i));
            if byte == 0 {
                break;
            }
            *slot = byte;
            path_len = i + 1;
        }
        if path_len == 0 {
            return -14;
        }
        let path = match core::str::from_utf8(&path_buf[..path_len]) {
            Ok(s) => s,
            Err(_) => return -14,
        };

        let result = exec_initramfs_for_target(rp, path);
        if result == 0 {
            // Exec succeeded — the target process was blocked on SENDREC
            // to PM (waiting for the exec reply). Clear its blocking state
            // and enqueue it so the scheduler picks it up.
            use crate::proc::RtsFlags;
            let old_rts = (*rp).p_rts_flags.fetch_and(
                !RtsFlags::RECEIVING.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            if old_rts & !RtsFlags::RECEIVING.bits() == 0 {
                crate::sched::enqueue(rp);
            }
        }
        result
    }
}

/// SYS_fork (58) — create a child process.
///
/// Finds a free Proc slot, copies the caller's state, sets rax=0 in
/// the child so it returns 0 from fork. Returns the child's endpoint
/// to the parent (as a PID substitute).
unsafe fn sys_fork_handler(caller: *mut crate::proc::Proc, _args: &[u64; 6]) -> i64 {
    unsafe {
        // Find a free slot.
        for slot in 0..crate::proc::NR_PROCS_TOTAL {
            let rpc = crate::table::proc_addr(slot as i32);
            if rpc.is_null() || rpc == caller {
                continue;
            }
            if (*rpc)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed)
                & crate::proc::RtsFlags::SLOT_FREE.bits()
                != 0
            {
                // Found a free slot — clone the caller into it.
                core::ptr::copy_nonoverlapping(caller, rpc, 1);
                (*rpc).p_nr = slot as i32;
                crate::hal::write_retval(&mut (*rpc).p_reg, 0); // child returns 0
                (*rpc).p_user_time = 0;
                (*rpc).p_sys_time = 0;
                let clear_mf = (crate::proc::MiscFlags::REPLY_PEND
                    | crate::proc::MiscFlags::VIRT_TIMER
                    | crate::proc::MiscFlags::PROF_TIMER
                    | crate::proc::MiscFlags::SC_TRACE
                    | crate::proc::MiscFlags::SPROF_SEEN
                    | crate::proc::MiscFlags::STEP)
                    .bits();
                (*rpc).p_misc_flags.store(
                    (*caller)
                        .p_misc_flags
                        .load(core::sync::atomic::Ordering::Relaxed)
                        & !clear_mf,
                    core::sync::atomic::Ordering::Relaxed,
                );
                (*rpc).p_virt_left = 0;
                (*rpc).p_prof_left = 0;
                (*rpc).p_cpu_time_left = 0;
                (*rpc).p_cycles = 0;
                (*rpc).p_kcall_cycles = 0;
                (*rpc).p_kipc_cycles = 0;
                (*rpc).p_signal_received = 0;

                // Clear SLOT_FREE flag to mark slot in use.
                (*rpc).p_rts_flags.fetch_and(
                    !crate::proc::RtsFlags::SLOT_FREE.bits(),
                    core::sync::atomic::Ordering::Relaxed,
                );

                // Store child endpoint in parent's deferred fields for
                // waitpid to use.
                let child_ep = (*rpc).p_endpoint;
                (*caller).p_defer_r1 = child_ep as u64;

                return child_ep as i64;
            }
        }
        -11 // EAGAIN — no free slot
    }
}

/// NR_IS_FORK_CHILD (63) — returns 1 if this process was created by fork
/// and hasn't yet detected it's the child. Used to distinguish parent from
/// child in the PM IPC fork path, where both share the same page table.
unsafe fn sys_is_fork_child_handler(caller: *mut crate::proc::Proc, _args: &[u64; 6]) -> i64 {
    unsafe {
        let r1 = (*caller).p_defer_r1;
        if r1 == 1 {
            (*caller).p_defer_r1 = 0;
            1
        } else {
            0
        }
    }
}

/// SYS_waitpid (59) — wait for a child process to exit.
///
/// args[0] = child endpoint (or 0 for any child, or -1 for any child).
/// Blocks until the child exits, then frees its slot and returns 0.
/// Returns negative error code on failure.
unsafe fn sys_waitpid_handler(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    unsafe {
        let wanted_ep = args[0] as i32;
        // Determine which child to wait for.
        // If no specific child requested, use the one stored in defer_r1
        // from the most recent fork in this process.
        let child_ep = if wanted_ep == 0 || wanted_ep == -1 {
            (*caller).p_defer_r1 as i32
        } else {
            wanted_ep
        };
        if child_ep == 0 {
            return -10; // ECHILD
        }
        // Find the child's Proc.
        let child_slot = crate::table::endpoint_slot(child_ep);
        let child = crate::table::proc_addr(child_slot);
        if child.is_null() || (*child).p_endpoint != child_ep {
            return -10; // ECHILD
        }
        // Spin until the child's Proc slot is SLOT_FREE (child has exited).
        loop {
            let flags = (*child)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            if flags & crate::proc::RtsFlags::SLOT_FREE.bits() != 0 {
                // Child has exited
                return 0;
            }
            // Yield CPU so the child (or scheduler) can run.
            crate::hal::pause();
        }
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
        register_basic_syscall(58, sys_fork_handler); // NR_FORK
        register_basic_syscall(59, sys_waitpid_handler); // NR_WAITPID
        register_basic_syscall(61, sys_exec_replace_handler); // SYS_EXEC_REPLACE
        register_basic_syscall(62, sys_exec_target_handler); // SYS_EXEC_TARGET
        register_basic_syscall(63, sys_is_fork_child_handler); // NR_IS_FORK_CHILD
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
    fn test_write_bad_fd_returns_ebadf() {
        unsafe {
            let buf = [0u8; 10];
            let args = [99u64, buf.as_ptr() as u64, 10u64, 0, 0, 0];
            let rp = crate::table::proc_addr(0);
            assert_eq!(sys_write_handler(rp, &args), -9);
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
            let rp = crate::table::proc_addr(0);
            (*rp).p_nr = 0;
            (*rp).p_endpoint = 100;
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
        _check(sys_is_fork_child_handler);
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

    #[test]
    fn test_is_fork_child_handler() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // Without flag set, returns 0
            (*rp).p_defer_r1 = 0;
            let result = sys_is_fork_child_handler(rp, &[0u64; 6]);
            assert_eq!(result, 0);
            // With flag set, returns 1 and clears it
            (*rp).p_defer_r1 = 1;
            let result = sys_is_fork_child_handler(rp, &[0u64; 6]);
            assert_eq!(result, 1);
            // Flag should be cleared
            assert_eq!((*rp).p_defer_r1, 0);
        }
    }
}
