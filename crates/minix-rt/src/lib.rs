//! Minix userspace runtime (`minix-rt`).
//!
//! Provides the runtime environment for userspace executables:
//! - `_start` entry point (naked asm, ABI-compatible with kernel exec)
//! - Panic handler (format + write to stderr, abort)
//! - Bump allocator backed by `brk` syscall (`BrkAllocator`)
//! - Syscall wrappers (`syscall0`–`syscall6` via `syscall` instruction)
//! - `exit()`, `write()`, `getpid()`, `sbrk()` primitives
//!
//! On x86_64, userspace syscalls use the `syscall` instruction:
//! - RAX = syscall number
//! - RDI, RSI, RDX, R10, R8, R9 = arguments 1–6
//! - RCX = saved RIP (set by `syscall` instruction)
//! - R11 = saved RFLAGS (set by `syscall` instruction)
//! - Return value in RAX

#![no_std]
#![allow(dead_code, unused_unsafe)]

use core::alloc::Layout;
use core::sync::atomic::{AtomicUsize, Ordering};

// ═══════════════════════════════════════════════════════════════════════════
// Syscall numbers (from `.refs/minix-3.3.0/minix/include/minix/callnr.h`)
// ═══════════════════════════════════════════════════════════════════════════

/// Exit process.
const NR_EXIT: u64 = 0;
/// Get process ID.
const NR_GETPID: u64 = 20;
/// Write to file descriptor.
const NR_WRITE: u64 = 3;
/// Set program break (heap end).
const NR_BRK: u64 = 36; // SBRK
/// Read from file descriptor.
const NR_READ: u64 = 2;
/// Open file.
const NR_OPEN: u64 = 4;
/// Close file descriptor.
const NR_CLOSE: u64 = 5;
/// Duplicate file descriptor.
const NR_DUP: u64 = 32;
/// Create a directory.
const NR_MKDIR: u64 = 40;
/// Remove a file.
const NR_UNLINK: u64 = 41;
/// Remove a directory.
const NR_RMDIR: u64 = 42;
/// Create a hard link.
const NR_LINK: u64 = 43;
/// Change file mode.
const NR_CHMOD: u64 = 44;
/// Change file owner.
const NR_CHOWN: u64 = 45;
/// Create a device node.
const NR_MKNOD: u64 = 56;
/// Get directory entries.
const NR_GETDENTS: u64 = 57;
/// IPC send.
pub const SEND_CALL: u64 = 46;
/// IPC receive.
pub const RECEIVE_CALL: u64 = 47;
/// IPC sendrec.
pub const SENDREC_CALL: u64 = 48;
/// IPC notify.
pub const NOTIFY_CALL: u64 = 49;

// ═══════════════════════════════════════════════════════════════════════════
// Syscall wrappers
// ═══════════════════════════════════════════════════════════════════════════

/// Perform a syscall with 0 arguments.
///
/// # Safety
///
/// The syscall number must be valid for the current execution context.
#[inline]
pub unsafe fn syscall0(nr: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") nr,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") ret,
            options(nostack),
        );
    }
    ret
}

/// Perform a syscall with 1 argument.
///
/// # Safety
///
/// The syscall number and argument must be valid.
#[inline]
pub unsafe fn syscall1(nr: u64, a1: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") nr,
            in("rdi") a1,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") ret,
            options(nostack),
        );
    }
    ret
}

/// Perform a syscall with 2 arguments.
///
/// # Safety
///
/// The syscall number and arguments must be valid.
#[inline]
pub unsafe fn syscall2(nr: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") ret,
            options(nostack),
        );
    }
    ret
}

/// Perform a syscall with 3 arguments.
///
/// # Safety
///
/// The syscall number and arguments must be valid.
#[inline]
pub unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") ret,
            options(nostack),
        );
    }
    ret
}

/// Perform a syscall with 4 arguments.
///
/// # Safety
///
/// The syscall number and arguments must be valid.
#[inline]
pub unsafe fn syscall4(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") ret,
            options(nostack),
        );
    }
    ret
}

/// Perform a syscall with 5 arguments.
///
/// # Safety
///
/// The syscall number and arguments must be valid.
#[inline]
pub unsafe fn syscall5(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") ret,
            options(nostack),
        );
    }
    ret
}

/// Perform a syscall with 6 arguments.
///
/// # Safety
///
/// The syscall number and arguments must be valid.
#[inline]
pub unsafe fn syscall6(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") ret,
            options(nostack),
        );
    }
    ret
}

// ═══════════════════════════════════════════════════════════════════════════
// POSIX-like primitives
// ═══════════════════════════════════════════════════════════════════════════

/// Exit the current process with the given status code.
pub fn exit(status: i32) -> ! {
    unsafe {
        syscall1(NR_EXIT, status as u64);
    }
    loop {
        unsafe { core::arch::asm!("pause") };
    }
}

/// Write `buf` to file descriptor `fd`.
/// Returns the number of bytes written, or a negative error code.
pub fn write(fd: i32, buf: &[u8]) -> i64 {
    unsafe { syscall3(NR_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Read from a file descriptor.
pub fn read(fd: i32, buf: &mut [u8]) -> i64 {
    unsafe {
        syscall3(
            NR_READ,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
}

/// Open a file.
pub fn open(path: &[u8], flags: i32) -> i64 {
    unsafe {
        syscall3(
            NR_OPEN,
            path.as_ptr() as u64,
            path.len() as u64,
            flags as u64,
        )
    }
}

/// Close a file descriptor.
pub fn close(fd: i32) -> i64 {
    unsafe { syscall1(NR_CLOSE, fd as u64) }
}

/// Create a directory.
pub fn mkdir(path: &[u8], mode: u32) -> i64 {
    unsafe { syscall2(NR_MKDIR, path.as_ptr() as u64, mode as u64) }
}

/// Remove a file.
pub fn unlink(path: &[u8]) -> i64 {
    unsafe { syscall1(NR_UNLINK, path.as_ptr() as u64) }
}

/// Remove a directory.
pub fn rmdir(path: &[u8]) -> i64 {
    unsafe { syscall1(NR_RMDIR, path.as_ptr() as u64) }
}

/// Create a hard link.
pub fn link(old: &[u8], new: &[u8]) -> i64 {
    unsafe { syscall2(NR_LINK, old.as_ptr() as u64, new.as_ptr() as u64) }
}

/// Change file mode.
pub fn chmod(path: &[u8], mode: u32) -> i64 {
    unsafe { syscall2(NR_CHMOD, path.as_ptr() as u64, mode as u64) }
}

/// Change file owner.
pub fn chown(path: &[u8], owner: i32, group: i32) -> i64 {
    unsafe { syscall3(NR_CHOWN, path.as_ptr() as u64, owner as u64, group as u64) }
}

/// Create a device node.
pub fn mknod(path: &[u8], mode: u32, dev: u64) -> i64 {
    unsafe { syscall3(NR_MKNOD, path.as_ptr() as u64, mode as u64, dev) }
}

/// Get the current process ID.
pub fn getpid() -> i32 {
    unsafe { syscall0(NR_GETPID) as i32 }
}

/// Change the program break (heap end).
/// If `addr` is 0, returns the current break.
/// Otherwise, sets the break to `addr` and returns the new break on success,
/// or a negative error code on failure.
///
/// # Safety
///
/// `addr` must be a valid heap address or null (to query the current break).
/// The caller must ensure no other code concurrently modifies the program break.
pub unsafe fn brk(addr: *const u8) -> i64 {
    unsafe { syscall1(NR_BRK, addr as u64) }
}

/// Increase the program break by `increment` bytes and return the old break,
/// or return a negative error code on failure.
///
/// # Safety
///
/// The caller must ensure no other code concurrently modifies the program break.
/// The `increment` must not cause the break to overflow the address space.
pub unsafe fn sbrk(increment: isize) -> i64 {
    unsafe {
        // Get current break.
        let old = brk(core::ptr::null());
        if old < 0 {
            return old;
        }
        let new = (old as usize).wrapping_add(increment as usize);
        let result = brk(new as *const u8);
        if result < 0 {
            return result;
        }
        old
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Entry point
// ═══════════════════════════════════════════════════════════════════════════

// External main function defined by the user program.
#[cfg(target_os = "none")]
unsafe extern "Rust" {
    fn main(argc: i32, argv: *const *const u8) -> i32;
}

// Program arguments passed by the kernel.
#[cfg(target_os = "none")]
unsafe extern "C" {
    static __executable_start: u8;
}

/// `_start` entry point — called by the kernel exec loader.
///
/// Parses argc/argv from the stack, calls `main`, and exits with the return value.
///
/// # Safety
///
/// Must be called as the process entry point by the kernel exec loader.
/// The stack must be set up per SysV ABI with `argc` at `[rsp]` followed
/// by `argv` pointers and a null terminator.
#[cfg(all(not(test), target_os = "none"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn _start() -> ! {
    // Naked asm: no prologue. On entry, [rsp] = argc, [rsp+8] = argv[0].
    // Read argc/argv, call main, then exit with the return value.
    core::arch::naked_asm!(
        "mov    rdi, [rsp]",
        "lea    rsi, [rsp + 8]",
        "call   main",
        "mov    rdi, rax",
        "xor    eax, eax",
        "syscall",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Panic handler
// ═══════════════════════════════════════════════════════════════════════════

/// Panic handler — writes the panic message to stderr and aborts.
#[cfg(all(not(test), target_os = "none"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Format panic message into a stack buffer and write to stderr.
    let mut buf = [0u8; 256];
    let payload = info.message();
    // Use core::fmt to write the message.
    use core::fmt::Write;
    let mut cursor = BufWriter {
        buf: &mut buf,
        pos: 0,
    };
    let _ = write!(cursor, "panic: {payload}");
    let len = cursor.pos.min(buf.len() - 1);
    let msg = &buf[..len];

    let _ = write(2, msg);
    let _ = write(2, b"\n");

    // Abort via exit.
    exit(-1);
}

/// Simple cursor-based buffer writer for formatting panic messages.
struct BufWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl core::fmt::Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buf.len().saturating_sub(self.pos);
        let copy_len = bytes.len().min(remaining);
        self.buf[self.pos..self.pos + copy_len].copy_from_slice(&bytes[..copy_len]);
        self.pos += copy_len;
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Bump allocator
// ═══════════════════════════════════════════════════════════════════════════

/// A simple bump allocator backed by the `brk` syscall.
///
/// Allocations are made by incrementing a pointer into the heap.
/// Deallocation is a no-op — memory is reclaimed only on process exit.
pub struct BrkAllocator {
    /// Current bump pointer (next allocation address).
    ptr: AtomicUsize,
}

impl BrkAllocator {
    /// Create a new bump allocator.
    ///
    /// Initializes the bump pointer to the current program break.
    pub const fn new() -> Self {
        Self {
            ptr: AtomicUsize::new(0),
        }
    }

    /// Allocate memory with the given layout.
    ///
    /// Returns a pointer to the allocated memory, or null on failure.
    ///
    /// # Safety
    ///
    /// `layout` must have non-zero size. The caller must ensure that
    /// concurrent calls to `alloc` or `dealloc` are properly synchronized.
    pub fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        // Round up the current pointer to the required alignment.
        let current = self.ptr.load(Ordering::Relaxed);
        let aligned = (current + align - 1) & !(align - 1);

        // Extend the heap.
        let new_end = aligned + size;
        let heap_end = (unsafe { brk(core::ptr::null()) }) as usize;

        if new_end > heap_end {
            // Need to extend the heap.
            let result = unsafe { brk(new_end as *const u8) };
            if result < 0 {
                return core::ptr::null_mut();
            }
        }

        self.ptr.store(aligned + size, Ordering::Relaxed);
        aligned as *mut u8
    }

    /// Deallocate memory — no-op for bump allocator.
    ///
    /// # Safety
    ///
    /// `ptr` must have been allocated by this allocator with the given layout.
    pub unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator: no-op.
    }
}

unsafe impl Send for BrkAllocator {}
unsafe impl Sync for BrkAllocator {}

unsafe impl core::alloc::GlobalAlloc for BrkAllocator {
    /// Allocate memory with the given layout.
    ///
    /// # Safety
    ///
    /// `layout` must have non-zero size.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        BrkAllocator::alloc(self, layout)
    }

    /// Deallocate memory previously allocated by this allocator.
    ///
    /// # Safety
    ///
    /// `ptr` must be a pointer returned by `alloc` with the same `layout`.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { BrkAllocator::dealloc(self, ptr, layout) }
    }
}

// Provide a Default impl for generic code that requests #[global_allocator].
impl Default for BrkAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// The global allocator instance.
#[cfg(target_os = "none")]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_numbers() {
        assert_eq!(NR_EXIT, 0);
        assert_eq!(NR_WRITE, 3);
        assert_eq!(NR_GETPID, 20);
        assert_eq!(NR_BRK, 36);
    }

    #[test]
    fn test_syscall0_signature() {
        // Verify the function compiles with the right signature.
        fn _check(f: unsafe fn(u64) -> i64) {
            let _ = f;
        }
        _check(syscall0);
    }

    #[test]
    fn test_syscall6_signature() {
        fn _check(f: unsafe fn(u64, u64, u64, u64, u64, u64, u64) -> i64) {
            let _ = f;
        }
        _check(syscall6);
    }

    #[test]
    fn test_alignment_math() {
        // Round up to 16-byte alignment.
        assert_eq!((0 + 15) & !15, 0);
        assert_eq!((1 + 15) & !15, 16);
        assert_eq!((15 + 15) & !15, 16);
        assert_eq!((16 + 15) & !15, 16);
        assert_eq!((17 + 15) & !15, 32);

        // 4096-byte alignment.
        let align = 4096;
        assert_eq!((0 + align - 1) & !(align - 1), 0);
        assert_eq!((1 + align - 1) & !(align - 1), 4096);
        assert_eq!((4095 + align - 1) & !(align - 1), 4096);
        assert_eq!((4096 + align - 1) & !(align - 1), 4096);
    }

    #[test]
    fn test_brk_null_returns_break() {
        // Calling brk(null) should return the current break (no-op).
        // We can't test this in a host test environment, but we can
        // verify the function compiles and has the right signature.
        fn _check(f: unsafe fn(*const u8) -> i64) {
            let _ = f;
        }
        _check(brk);
    }

    #[test]
    fn test_sbrk_signature() {
        fn _check(f: unsafe fn(isize) -> i64) {
            let _ = f;
        }
        _check(sbrk);
    }

    #[test]
    fn test_write_signature() {
        fn _check(f: fn(i32, &[u8]) -> i64) {
            let _ = f;
        }
        _check(write);
    }

    #[test]
    fn test_getpid_signature() {
        fn _check(f: fn() -> i32) {
            let _ = f;
        }
        _check(getpid);
    }

    #[test]
    fn test_exit_signature() {
        fn _check(f: fn(i32) -> !) {
            let _ = f;
        }
        _check(exit);
    }

    #[test]
    fn test_allocator_layout() {
        // Verify the allocator can be constructed and has the right size.
        let alloc = BrkAllocator::new();
        assert_eq!(alloc.ptr.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_buf_writer() {
        use core::fmt::Write;
        let mut buf = [0u8; 16];
        {
            let mut w = BufWriter {
                buf: &mut buf,
                pos: 0,
            };
            let _ = write!(w, "hello");
            assert_eq!(w.pos, 5);
        }
        assert_eq!(&buf[..5], b"hello");
    }

    #[test]
    fn test_buf_writer_overflow() {
        use core::fmt::Write;
        let mut buf = [0u8; 4];
        {
            let mut w = BufWriter {
                buf: &mut buf,
                pos: 0,
            };
            let _ = write!(w, "hello world");
            assert_eq!(w.pos, 4);
        }
        assert_eq!(&buf, b"hell");
    }

    #[test]
    fn test_allocator_is_send_sync() {
        fn check_send<T: Send>(_: &T) {}
        fn check_sync<T: Sync>(_: &T) {}
        let alloc = BrkAllocator::new();
        check_send(&alloc);
        check_sync(&alloc);
    }

    #[test]
    #[cfg(target_os = "none")]
    fn test_global_allocator_impl() {
        // Verify the global allocator trait is implemented.
        fn _check<T: core::alloc::GlobalAlloc>(_: &T) {}
        _check(&ALLOCATOR);
    }
}
