//! Minimal libc for FFI — C ABI wrappers over `minix-std` primitives.
//!
//! Provides `extern "C"` functions that wrap the Rust-native `minix-std` and
//! `minix-rt` APIs so that any remaining C code can link against them.
//!
//! All functions follow the POSIX convention: return -1 on error (or
//! `MAP_FAILED`/`SIG_ERR` where POSIX says so) and set `errno` (via
//! `__errno_location`). `errno` is a `#[thread_local]` slot, so each C
//! thread sees its own value.

#![no_std]
#![allow(dead_code)]
// `#[thread_local]` on statics is feature-gated in this toolchain line; the
// std crate declares the same feature for its TLS statics. Only minix builds
// compile TLS code (the per-thread errno slot); host builds use a plain
// static and need no gate, so the crate compiles on a stable host toolchain.
#![cfg_attr(target_os = "minix", feature(thread_local))]

#[cfg(target_os = "minix")]
mod pthread;

// The C-library helper modules (stdio/time/wchar/string/stdlib/setjmp)
// that the old `tools/c-libc.c` used to provide. Exported symbols are
// `#[cfg(target_os = "minix")]`-gated inside each module; the pure
// helpers compile everywhere so the host test suite can exercise them.
mod c_locale;
mod c_net;
mod c_setjmp;
mod c_stdio;
mod c_stdlib;
mod c_string;
mod c_sys;
mod c_termios;
mod c_time;
mod c_wchar;

/// The `panic` lang item for the shared-object build (`libc.so`).
///
/// The rlib that the port's binaries link against leaves this to the program
/// that links it, but a `cdylib` is a final artifact and nothing downstream can
/// supply it. The target's panic strategy is `abort`, and a shared library has
/// no unwinder, so a panic stops the program.
#[cfg(feature = "so")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    minix_rt::exit(101)
}

#[cfg(target_os = "minix")]
use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};

// ---- errno ----

/// Per-thread errno in native TLS on minix. `crt0` (and the pthread
/// trampoline) sets up the thread pointer (FS base) before `main`, so this is
/// genuinely per-thread: each C thread sees its own `errno`. Host builds
/// (single-threaded test scaffolding) use a shared atomic slot instead, which
/// avoids needing the `thread_local` feature on a stable host toolchain.
#[cfg(target_os = "minix")]
#[thread_local]
static ERRNO: core::cell::UnsafeCell<i32> = core::cell::UnsafeCell::new(0);

#[cfg(not(target_os = "minix"))]
static ERRNO: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

/// POSIX `errno` accessor: returns this thread's TLS errno slot.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn __errno_location() -> *mut c_int {
    // SAFETY: the pointer is only used by C `errno` reads and stays valid for
    // the thread's lifetime.
    ERRNO.get().cast::<c_int>()
}

#[inline]
pub(crate) fn set_errno(e: i32) {
    #[cfg(target_os = "minix")]
    {
        // SAFETY: only this thread touches its own TLS errno slot.
        unsafe { *ERRNO.get() = e };
    }
    #[cfg(not(target_os = "minix"))]
    ERRNO.store(e, core::sync::atomic::Ordering::Relaxed);
}

// ---- per-thread TLS runtime ----

// The TLS block image bounds, defined by `tools/minix-user.ld`: `.tdata`
// holds the initialized image, `.tbss` follows it zeroed. The runtime copies
// the image into a per-thread block and sets the thread pointer at the block
// (FS base on x86_64, tpidr_el0 on AArch64, tp on RISC-V).
#[cfg(target_os = "minix")]
unsafe extern "C" {
    static __tls_start: u8;
    static __tdata_end: u8;
    static __tls_end: u8;
}

/// Allocate and initialize a TLS block for a new thread, returning the
/// thread pointer to install via `minix_rt::thread_set_tls` (0 on failure).
///
/// Runs on the main thread — the C heap is single-threaded and worker
/// threads must not call `sbrk` — so `pthread_create` prepares the block
/// here and the trampoline only issues the `thread_set_tls` syscall.
#[cfg(target_os = "minix")]
pub(crate) fn tls_block_alloc() -> usize {
    unsafe {
        let start = core::ptr::addr_of!(__tls_start).addr();
        let tdata_end = core::ptr::addr_of!(__tdata_end).addr();
        let end = core::ptr::addr_of!(__tls_end).addr();
        let size = end - start;
        if size == 0 {
            return 0;
        }
        // Allocate size + 32 so the block can be 16-aligned, and so there is
        // room for the x86_64 TCB self-pointer just past the TLS image (the
        // thread pointer may sit up to 15 bytes above `block + size`).
        let alloc = minix_rt::sbrk(size as isize + 32);
        if alloc < 0 {
            return 0;
        }
        let block = ((alloc as usize) + 15) & !15;
        // Copy the `.tdata` init image, zero the `.tbss` tail.
        core::ptr::copy_nonoverlapping(
            core::ptr::with_exposed_provenance::<u8>(start),
            core::ptr::with_exposed_provenance_mut::<u8>(block),
            tdata_end - start,
        );
        core::ptr::write_bytes(
            core::ptr::with_exposed_provenance_mut::<u8>(block + (tdata_end - start)),
            0,
            end - tdata_end,
        );
        // Thread-pointer convention per arch: x86_64 uses the negative-offset
        // TLS layout (TP past the end of the image, 16-aligned, self-pointer
        // at [TP]); aarch64/riscv64 point at the block start.
        #[cfg(target_arch = "x86_64")]
        let tp = {
            let tp = (block + size + 15) & !15;
            core::ptr::write(core::ptr::with_exposed_provenance_mut::<u64>(tp), tp as u64);
            tp
        };
        #[cfg(not(target_arch = "x86_64"))]
        let tp = block;
        tp
    }
}

/// Initialize the calling thread's TLS block and thread pointer. Called by
/// `crt0` before `main` and by the pthread trampoline for new threads, so
/// any `#[thread_local]` access (errno, the pthread handle) works.
///
/// A statically linked program's block bounds come from the linker script, which
/// is why this can build one. A dynamically linked program has a `PT_INTERP`
/// loader instead, and the loader builds the same block from this object's
/// `PT_TLS` and installs the pointer before it runs any initialiser
/// (`crates/ldso/src/rtld.rs`), so the shared-object build has nothing to add —
/// and a second block would move the thread pointer away from the storage the
/// loader's `__tls_get_addr` hands out.
///
/// Best effort: on heap failure the thread pointer is left unset and TLS
/// accesses will fault.
///
/// # Safety
///
/// Must be called exactly once per thread, before any TLS access on it.
#[cfg(all(target_os = "minix", not(feature = "so")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn minix_libc_tls_init() {
    let tp = tls_block_alloc();
    if tp != 0 {
        minix_rt::thread_set_tls(tp);
    }
}

/// The shared-object build's `minix_libc_tls_init`, which the loader has already
/// done: it laid the thread's block out from this object's `PT_TLS` and installed
/// the thread pointer before any initialiser could run.
///
/// `tls_block_alloc` still builds a block for each thread
/// `pthread_create` starts, and that is sound because the loader rounds the
/// segment's `p_memsz` the same way — so a worker's own thread pointer is at the
/// same distance from its storage as the block `__tls_get_addr` expects.
///
/// # Safety
///
/// Kept for `crt0`'s contract; it changes nothing.
#[cfg(all(target_os = "minix", feature = "so"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn minix_libc_tls_init() {}

/// Standard POSIX error return: record `errno` and return -1.
#[inline]
pub(crate) fn fail(e: i32) -> i32 {
    set_errno(e);
    -1
}

// ---- C heap (malloc family) over the program break ----

// First-fit allocator over `sbrk`. Blocks are contiguous from `HEAP_START` to
// the current break; each block carries a 16-byte header ([size, flags]) and
// the user pointer is 16-byte aligned. `free` coalesces with the neighbours.

const HDR: usize = 16; // [size: usize][flags: usize]
const ALIGN: usize = 16;
const USED: usize = 1;

/// The page the target's `sbrk` works in. `sbrk` returns a page-aligned
/// address and leaves the break page-aligned as well, so a heap block that
/// does not fill its pages ends before the break and the *next* extension's
/// block lands past the untouched gap (or past a thread stack sbrk'd in
/// between). The free-list walk stops at the first zeroed header — that is
/// what keeps it out of a stack — so a block above such a gap is unreachable:
/// memory that can never be handed out or coalesced again, one page per
/// extension, until `sbrk` fails. Rounding each extension request to a whole
/// page is what makes consecutive blocks meet, and it is how the walk stays
/// able to see the whole heap.
const HEAP_PAGE: usize = 4096;

/// The `sbrk` request for a block of `need` bytes, and the size to record for
/// it: `(ask, size)`. The last partial page is left as a free block when it is
/// big enough to hold one, so the slack stays reusable; when it is not, the
/// block absorbs it (the caller never sees the difference, the size in the
/// header is the port's own).
#[inline]
fn extend_request(need: usize) -> (usize, usize) {
    let ask = align_up(need, HEAP_PAGE);
    let pad = ask - need;
    if pad >= HDR + ALIGN {
        (ask, need)
    } else {
        (ask, ask)
    }
}

static mut HEAP_START: usize = 0;
/// Exact end of the heap (past the last block). The program break may be
/// higher — the VM server rounds it to pages, and thread stacks are sbrk'd
/// beyond it — so the free-list walk must not use `current_break()`: it
/// would cross into the zeroed/rounded gap or the thread-stack region and
/// read a size-0 header (infinite loop).
static mut HEAP_END: usize = 0;

#[inline]
unsafe fn hdr_size(p: *mut u8) -> usize {
    // SAFETY: `p` is a block start within the heap.
    unsafe { *(p as *mut usize) }
}

#[inline]
unsafe fn hdr_flags(p: *mut u8) -> usize {
    // SAFETY: `p` is a block start within the heap.
    unsafe { *((p as *mut usize).add(1)) }
}

#[inline]
unsafe fn set_hdr(p: *mut u8, size: usize, flags: usize) {
    // SAFETY: `p` is a block start within the heap.
    unsafe {
        *(p as *mut usize) = size;
        *((p as *mut usize).add(1)) = flags;
    }
}

#[inline]
fn align_up(n: usize, a: usize) -> usize {
    (n + a - 1) & !(a - 1)
}

/// `malloc(size)`: allocate `size` bytes, 16-byte aligned.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    let need = align_up(size.max(1), ALIGN) + HDR;
    unsafe {
        let heap = HEAP_START;
        if heap == 0 {
            // First allocation: extend the break and start the heap there.
            let (ask, size) = extend_request(need);
            let base = minix_rt::sbrk(ask as isize);
            if base < 0 {
                set_errno(-base as i32);
                return core::ptr::null_mut();
            }
            HEAP_START = base as usize;
            HEAP_END = HEAP_START + ask;
            set_hdr(base as *mut u8, size, USED);
            let pad = ask - size;
            if pad >= HDR + ALIGN {
                set_hdr((HEAP_START + size) as *mut u8, pad, 0);
            }
            return (HEAP_START + HDR) as *mut c_void;
        }

        // First-fit over the existing blocks.
        let end = HEAP_END;
        let mut p = heap;
        while p < end {
            let sz = hdr_size(p as *mut u8);
            if hdr_flags(p as *mut u8) == 0 && sz >= need {
                if sz >= need + HDR + ALIGN {
                    // Split off a free remainder.
                    set_hdr(p as *mut u8, need, USED);
                    set_hdr((p + need) as *mut u8, sz - need, 0);
                } else {
                    set_hdr(p as *mut u8, sz, USED);
                }
                return (p + HDR) as *mut c_void;
            }
            // A zeroed or out-of-range header means the walk left the real
            // heap (thread stacks sit in the same break region); stop here.
            if sz == 0 || p + sz > end {
                break;
            }
            p += sz;
        }

        // Nothing fit: extend the break and place a fresh block there (it may
        // not be contiguous with the old heap — a thread stack sbrk'd in
        // between — so track the new end explicitly; see `extend_request` for
        // why the request is a whole number of pages).
        let (ask, size) = extend_request(need);
        let r = minix_rt::sbrk(ask as isize);
        if r < 0 {
            set_errno(-r as i32);
            return core::ptr::null_mut();
        }
        let b = r as usize;
        set_hdr(b as *mut u8, size, USED);
        let pad = ask - size;
        if pad >= HDR + ALIGN {
            set_hdr((b + size) as *mut u8, pad, 0);
        }
        HEAP_END = b + ask;
        (b + HDR) as *mut c_void
    }
}

/// `free(ptr)`: mark the block free and coalesce with adjacent free blocks.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let block = ptr as usize - HDR;
        let mut sz = hdr_size(block as *mut u8);
        set_hdr(block as *mut u8, sz, 0);
        // Coalesce with the next block.
        let next = block + sz;
        if next < HEAP_END && hdr_flags(next as *mut u8) == 0 {
            sz += hdr_size(next as *mut u8);
            set_hdr(block as *mut u8, sz, 0);
        }
        // Coalesce with the previous block (walk from the heap start).
        let mut prev = HEAP_START;
        while prev < block {
            let psz = hdr_size(prev as *mut u8);
            if prev + psz == block && hdr_flags(prev as *mut u8) == 0 {
                set_hdr(prev as *mut u8, psz + sz, 0);
                break;
            }
            if psz == 0 || prev + psz > HEAP_END {
                break;
            }
            prev += psz;
        }
    }
}

/// `calloc(nmemb, size)`: zero-initialized allocation.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(nmemb: usize, size: usize) -> *mut c_void {
    let total = nmemb.saturating_mul(size);
    let p = unsafe { malloc(total) };
    if !p.is_null() {
        // SAFETY: `p` holds `total` bytes from `malloc`.
        unsafe { core::ptr::write_bytes(p as *mut u8, 0, total) };
    }
    p
}

/// `realloc(ptr, size)`: grow/shrink an allocation. Shrinks keep the block
/// (no split); grows allocate fresh and copy.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    if ptr.is_null() {
        return unsafe { malloc(size) };
    }
    unsafe {
        // The size is in the block header, HDR bytes below the pointer handed
        // out — not at the pointer itself, which is the caller's first byte.
        // Reading it there gave `realloc` a garbage "old size": a grow whose
        // garbage read high returned the same, too-small block, and the caller
        // then overran it (a long-running C loop of growing `realloc`s died of
        // one, which is what a `bash` arithmetic loop was doing), while a grow
        // whose garbage read low copied from the wrong length.
        let old = hdr_size((ptr as usize - HDR) as *mut u8) - HDR;
        if size <= old {
            return ptr;
        }
        let np = malloc(size);
        if !np.is_null() {
            core::ptr::copy_nonoverlapping(ptr as *const u8, np as *mut u8, old);
            free(ptr);
        }
        np
    }
}

// File I/O

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: c_int) -> c_int {
    if path.is_null() {
        return fail(22); // EINVAL
    }
    let path_str = unsafe { core::ffi::CStr::from_ptr(path) };
    // C paths are byte strings, not necessarily UTF-8.
    let path_bytes = path_str.to_bytes();
    match unsafe { minix_std::fs::open(path_bytes, flags, mode as u32) } {
        Ok(fd) => fd,
        Err(e) => fail(e.0),
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    if buf.is_null() {
        return fail(22) as isize; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, count) };
    match unsafe { minix_std::fs::read(fd, slice) } {
        Ok(n) => n as isize,
        Err(e) => fail(e.0) as isize,
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t {
    if buf.is_null() {
        return fail(22) as isize; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
    match unsafe { minix_std::fs::write(fd, slice) } {
        Ok(n) => n as isize,
        Err(e) => fail(e.0) as isize,
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn close(fd: c_int) -> c_int {
    match minix_std::fs::close(fd) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t {
    match minix_std::fs::lseek(fd, offset, whence) {
        Ok(pos) => pos,
        Err(e) => fail(e.0) as i64,
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int {
    match minix_std::fs::fcntl(fd, cmd, arg) {
        Ok(r) => r,
        Err(e) => fail(e.0),
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn dup(fd: c_int) -> c_int {
    // POSIX `dup(fd)` == `fcntl(fd, F_DUPFD, 0)`.
    match minix_std::fs::fcntl(fd, 0, 0) {
        Ok(r) => r,
        Err(e) => fail(e.0),
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn dup2(fd: c_int, newfd: c_int) -> c_int {
    match minix_std::fs::dup2(fd, newfd) {
        Ok(r) => r,
        Err(e) => fail(e.0),
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe(fds: *mut [c_int; 2]) -> c_int {
    if fds.is_null() {
        return fail(22); // EINVAL
    }
    match minix_std::fs::pipe() {
        Ok((r, w)) => {
            unsafe {
                (*fds)[0] = r;
                (*fds)[1] = w;
            }
            0
        }
        Err(e) => fail(e.0),
    }
}

// Process lifecycle

/// C `struct rusage` (`tools/c-include/sys/resource.h`), zero-filled by
/// `wait4` (PM does not report resource usage yet).
#[cfg(target_os = "minix")]
#[repr(C)]
struct Rusage {
    ru_utime: crate::c_time::TimeVal,
    ru_stime: crate::c_time::TimeVal,
    ru_maxrss: c_long,
    ru_ixrss: c_long,
    ru_idrss: c_long,
    ru_isrss: c_long,
    ru_minflt: c_long,
    ru_majflt: c_long,
    ru_nswap: c_long,
    ru_inblock: c_long,
    ru_oublock: c_long,
    ru_msgsnd: c_long,
    ru_msgrcv: c_long,
    ru_nsignals: c_long,
    ru_nvcsw: c_long,
    ru_nivcsw: c_long,
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fork() -> c_int {
    match unsafe { minix_std::process::fork() } {
        Ok(pid) => pid,
        Err(e) => fail(e.0),
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
/// # Safety
///
/// `status` must be null or point to a writable `c_int`.
pub unsafe extern "C" fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int {
    match minix_std::process::waitpid(pid, options) {
        Ok((child, s)) => {
            if !status.is_null() {
                unsafe { *status = s };
            }
            child
        }
        Err(e) => fail(e.0),
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
/// # Safety
///
/// Same obligation as `waitpid`: `status` must be null or writable.
pub unsafe extern "C" fn wait(status: *mut c_int) -> c_int {
    unsafe { waitpid(-1, status, 0) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wait4(
    pid: c_int,
    status: *mut c_int,
    options: c_int,
    usage: *mut c_void,
) -> c_int {
    // PM does not return resource usage yet; zero the struct so LLVM's
    // ProcessStatistics never sees garbage.
    if !usage.is_null() {
        unsafe { core::ptr::write_bytes(usage as *mut u8, 0, size_of::<Rusage>()) };
    }
    unsafe { waitpid(pid, status, options) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn exit(status: c_int) -> ! {
    // Run the C++/atexit destructors before the process actually exits.
    unsafe { __cxa_finalize(core::ptr::null_mut()) };
    minix_std::process::exit(status);
}

/// Itanium ABI `__cxa_atexit` registry entry.
#[cfg(target_os = "minix")]
#[derive(Clone, Copy)]
#[repr(C)]
struct AtExitEntry {
    func: Option<extern "C" fn(*mut c_void)>,
    arg: *mut c_void,
    dso: *mut c_void,
}

/// Fixed-size registry — no dynamic allocation before libc is up. The C
/// heap is not thread-safe yet and the runtime is effectively
/// single-threaded, so no lock.
#[cfg(target_os = "minix")]
const ATEXIT_MAX: usize = 64;
#[cfg(target_os = "minix")]
static ATEXIT_N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(target_os = "minix")]
static mut ATEXIT: [AtExitEntry; ATEXIT_MAX] = [AtExitEntry {
    func: None,
    arg: core::ptr::null_mut(),
    dso: core::ptr::null_mut(),
}; ATEXIT_MAX];

/// Itanium ABI `__cxa_atexit`: register `func(arg)` to run at exit (or
/// when `__cxa_finalize(dso)` is called). Returns 0 on success.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn __cxa_atexit(
    func: extern "C" fn(*mut c_void),
    arg: *mut c_void,
    dso: *mut c_void,
) -> c_int {
    unsafe {
        let n = ATEXIT_N.load(core::sync::atomic::Ordering::Relaxed);
        if n >= ATEXIT_MAX {
            return -1; // registry full
        }
        ATEXIT[n] = AtExitEntry {
            func: Some(func),
            arg,
            dso,
        };
        ATEXIT_N.store(n + 1, core::sync::atomic::Ordering::Relaxed);
    }
    0
}

/// Itanium ABI `__cxa_finalize`: run and deregister the handlers for
/// `dso` (null = all), in reverse registration order.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_finalize(dso: *mut c_void) {
    unsafe {
        let mut n = ATEXIT_N.load(core::sync::atomic::Ordering::Relaxed);
        while n > 0 {
            n -= 1;
            let e = &mut ATEXIT[n];
            if dso.is_null() || e.dso == dso {
                if let Some(f) = e.func.take() {
                    f(e.arg);
                }
            }
        }
        if dso.is_null() {
            ATEXIT_N.store(0, core::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// C `atexit` over the Itanium registry. The function pointer is cast to
/// the one-arg form; the Itanium ABI calls atexit handlers with a dummy
/// argument, which the zero-arg callee ignores.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn atexit(func: extern "C" fn()) -> c_int {
    let f: extern "C" fn(*mut c_void) = unsafe { core::mem::transmute(func) };
    __cxa_atexit(f, core::ptr::null_mut(), core::ptr::null_mut())
}

/// The dummy `__dso_handle` for statically-linked objects: its address is
/// the handle value passed as `dso` to `__cxa_atexit`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub static __dso_handle: u8 = 0;

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn abort() -> ! {
    // SIGABRT exit status (128 + 6), matching libc conventions.
    minix_std::process::exit(134);
}

/// POSIX `system()`: run `cmd` through `/bin/sh -c` and return the shell's
/// exit status, or -1 when the child could not be started.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn system(cmd: *const c_char) -> c_int {
    const SH: &[u8] = b"/bin/sh\0";
    const DASH_C: &[u8] = b"-c\0";
    if cmd.is_null() {
        return 1;
    }
    let pid = unsafe { fork() };
    if pid < 0 {
        return -1;
    }
    if pid == 0 {
        let argv: [*const c_char; 4] = [
            SH.as_ptr() as *const c_char,
            DASH_C.as_ptr() as *const c_char,
            cmd,
            core::ptr::null(),
        ];
        unsafe { crate::c_sys::execv(SH.as_ptr() as *const c_char, argv.as_ptr()) };
        exit(127)
    }
    let mut status: c_int = 0;
    if unsafe { waitpid(pid, &mut status, 0) } < 0 {
        return -1;
    }
    // The wait status carries the exit code in its second byte.
    (status >> 8) & 0xff
}

/// glibc-shaped `__assert_fail()`: the out-of-line half of the `assert` macro.
/// Reports the failure on stderr and aborts. `line` is an `unsigned int`, as
/// the C declaration has it.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __assert_fail(
    assertion: *const c_char,
    file: *const c_char,
    line: u32,
    function: *const c_char,
) -> ! {
    let fmt = b"%s:%u: %s: assertion \"%s\" failed\n\0";
    let mut buf = [0u8; 512];
    let n = unsafe {
        crate::c_stdio::snprintf(
            buf.as_mut_ptr() as *mut c_char,
            buf.len(),
            fmt.as_ptr() as *const c_char,
            file,
            line,
            function,
            assertion,
        )
    };
    if n > 0 {
        let len = if (n as usize) < buf.len() {
            n as usize
        } else {
            buf.len() - 1
        };
        let _ = unsafe { write(2, buf.as_ptr() as *const c_void, len) };
    }
    abort()
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn getppid() -> c_int {
    match minix_std::process::getpid() {
        Ok((_pid, ppid)) => ppid,
        Err(e) => {
            set_errno(e.0);
            -1
        }
    }
}

/// C-facing scalar aliases. Signatures are written with these so the headers
/// generated from this crate carry the POSIX names; cbindgen maps the bare
/// Rust primitives to `uintptr_t`/`intptr_t`/`int64_t` instead. These are C's
/// names, so they keep C's spelling.
#[allow(non_camel_case_types)]
pub type size_t = usize;
#[allow(non_camel_case_types)]
pub type ssize_t = isize;
#[allow(non_camel_case_types)]
pub type off_t = i64;
#[allow(non_camel_case_types)]
pub type time_t = i64;

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn getpid() -> c_int {
    match minix_std::process::getpid() {
        Ok((pid, _ppid)) => pid,
        Err(_) => -1,
    }
}

/// `gethostname(2)`: copy the machine name into `name`, NUL-terminated.
/// A single name is enough — lock-file host IDs only need to distinguish
/// machines, and there is one (the QEMU guest).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gethostname(name: *mut c_char, len: usize) -> c_int {
    if name.is_null() || len == 0 {
        return fail(22); // EINVAL
    }
    let host = b"minix";
    let n = host.len().min(len - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(host.as_ptr() as *const c_char, name, n);
        *name.add(n) = 0;
    }
    0
}

/// `stat(2)`: file metadata for `path` (follows symlinks).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stat(path: *const c_char, buf: *mut c_void) -> c_int {
    unsafe { stat_path_impl(minix_std::fs::stat, path, buf) }
}

/// `lstat(2)`: file metadata for `path` (does not follow symlinks).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lstat(path: *const c_char, buf: *mut c_void) -> c_int {
    unsafe { stat_path_impl(minix_std::fs::lstat, path, buf) }
}

/// `fstat(2)`: file metadata for an open descriptor.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstat(fd: c_int, buf: *mut c_void) -> c_int {
    if buf.is_null() {
        return fail(22); // EINVAL
    }
    match minix_std::fs::fstat(fd) {
        Ok(st) => {
            unsafe { fill_c_stat(buf, &st) };
            0
        }
        Err(e) => fail(e.0),
    }
}

/// Shared path→`struct stat` glue for `stat`/`lstat`.
///
/// # Safety
///
/// `path` must be a NUL-terminated C string and `buf` must point to at
/// least `size_of::<Stat>()` writable bytes.
#[cfg(target_os = "minix")]
unsafe fn stat_path_impl(
    f: fn(&str) -> Result<minix_std::fs::Stat, minix_std::MinixErr>,
    path: *const c_char,
    buf: *mut c_void,
) -> c_int {
    if path.is_null() || buf.is_null() {
        return fail(22); // EINVAL
    }
    let s = unsafe { core::ffi::CStr::from_ptr(path) };
    // VFS stat takes `&str` paths (C paths are byte strings, so non-UTF-8
    // names are rejected for now).
    let path_str = match core::str::from_utf8(s.to_bytes()) {
        Ok(p) => p,
        Err(_) => return fail(22), // EINVAL
    };
    match f(path_str) {
        Ok(st) => {
            unsafe { fill_c_stat(buf, &st) };
            0
        }
        Err(e) => fail(e.0),
    }
}

/// Copy a `minix_std::fs::Stat` (repr(C), 88 bytes) into a C `struct stat`.
/// The layouts are field-for-field identical (offsets 0, 8, 16, 20, 24, 28,
/// 32, 40, 48, 56, 64, 72, 80), so one byte copy suffices.
///
/// # Safety
///
/// `buf` must point to at least `size_of::<Stat>()` writable bytes.
#[cfg(target_os = "minix")]
unsafe fn fill_c_stat(buf: *mut c_void, st: &minix_std::fs::Stat) {
    unsafe {
        core::ptr::copy_nonoverlapping(
            st as *const minix_std::fs::Stat as *const u8,
            buf as *mut u8,
            core::mem::size_of::<minix_std::fs::Stat>(),
        );
    }
}

// Memory management

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mmap(
    addr: *mut c_void,
    length: usize,
    prot: c_int,
    flags: c_int,
    fd: c_int,
    offset: i64,
) -> *mut c_void {
    unsafe {
        let r =
            minix_std::vmem::mmap(addr as *mut u8, length, prot, flags, fd, offset) as *mut c_void;
        if (r as usize) == usize::MAX {
            // MAP_FAILED; the vmem layer discards the errno, so use a generic.
            set_errno(12); // ENOMEM
        }
        r
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn munmap(addr: *mut c_void, length: usize) -> c_int {
    let r = unsafe { minix_std::vmem::munmap(addr as *mut u8, length) };
    if r < 0 {
        set_errno(12); // ENOMEM (vmem discards the errno)
    }
    r
}

// Time

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
/// # Safety
///
/// `tp` must be null or point to a writable `TimeSpec`.
pub unsafe extern "C" fn clock_gettime(
    clock_id: c_int,
    tp: *mut minix_std::time::TimeSpec,
) -> c_int {
    if tp.is_null() {
        return fail(22); // EINVAL
    }
    match minix_std::time::clock_gettime(clock_id) {
        Ok(ts) => {
            unsafe { *tp = ts };
            0
        }
        Err(e) => fail(e.0),
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn alarm(seconds: c_uint) -> c_uint {
    minix_std::time::alarm(seconds)
}

// Signals

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn kill(pid: c_int, sig: c_int) -> c_int {
    match minix_std::time::kill(pid, sig) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

/// POSIX `sigprocmask()`: examine or change the signal mask. `set` and
/// `oldset` point at 16-byte `sigset_t` buffers in the caller; pass NULL to
/// skip either.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigprocmask(how: c_int, set: *const SigSet, oldset: *mut SigSet) -> c_int {
    match minix_std::time::sigprocmask(how, set as u64, oldset as u64) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

/// POSIX `sigsuspend()`: atomically install `mask` and wait for a signal.
///
/// POSIX defines no success return: the call ends as -1/EINTR once a handler
/// has run, which is what PM's release of a suspended call produces.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigsuspend(mask: *const SigSet) -> c_int {
    match minix_std::time::sigsuspend(mask as u64) {
        Ok(()) => fail(4), // EINTR
        Err(e) => fail(e.0),
    }
}

/// POSIX `sigpending()`: write the set of pending signals into `set`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigpending(set: *mut SigSet) -> c_int {
    match minix_std::time::sigpending(set as u64) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

/// POSIX `signal()`: set the disposition of `signum` to `handler`
/// (SIG_DFL=0, SIG_IGN=1, or a handler address).
///
/// Returns the previous disposition on success (not fetched — the
/// registration path only sets), or SIG_ERR (all-ones) on error.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn signal(signum: c_int, handler: u64) -> u64 {
    match minix_std::time::signal(signum, handler) {
        Ok(()) => 0, // SIG_DFL
        Err(e) => {
            set_errno(e.0);
            !0 // SIG_ERR
        }
    }
}

/// POSIX `sigaction()`: set a signal action from a C `struct sigaction`
/// (handler u64@0, mask 16 bytes@8, flags i32@24 — 28 bytes).
///
/// `oldact` is accepted but not filled (the old action is not fetched).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigaction(
    signum: c_int,
    act: *const c_void,
    _oldact: *mut c_void,
) -> c_int {
    if act.is_null() {
        return 0; // query-only; oldact not fetched
    }
    let bytes = unsafe { core::slice::from_raw_parts(act as *const u8, 28) };
    let act_arr: [u8; 28] = bytes.try_into().unwrap();
    let (handler, mask, flags) = minix_std::time::decode_action(&act_arr);
    match minix_std::time::sigaction(signum, handler, mask, flags) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

// ---- signal.h helper functions ----

/// C `sigset_t` — two unsigned longs (the header declares `unsigned long
/// sigset_t[2]`).
#[cfg(target_os = "minix")]
type SigSet = [c_ulong; 2];

/// Bits carried by one `SigSet` word. The set spans 64 signal numbers, so on a
/// 32-bit target the two words split them 32/32 rather than 64/0 — the index
/// arithmetic has to follow the word width instead of a fixed 64.
#[cfg(target_os = "minix")]
const SIGSET_WORD_BITS: usize = c_ulong::BITS as usize;

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigemptyset(set: *mut SigSet) -> c_int {
    if set.is_null() {
        return fail(22); // EINVAL
    }
    unsafe {
        (*set)[0] = 0;
        (*set)[1] = 0;
    }
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigfillset(set: *mut SigSet) -> c_int {
    if set.is_null() {
        return fail(22); // EINVAL
    }
    unsafe {
        (*set)[0] = !0;
        (*set)[1] = !0;
    }
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigaddset(set: *mut SigSet, signum: c_int) -> c_int {
    if set.is_null() || signum <= 0 || signum > 64 {
        return fail(22); // EINVAL
    }
    let i = (signum - 1) as usize;
    unsafe {
        (*set)[i / SIGSET_WORD_BITS] |= 1 << (i % SIGSET_WORD_BITS);
    }
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigdelset(set: *mut SigSet, signum: c_int) -> c_int {
    if set.is_null() || signum <= 0 || signum > 64 {
        return fail(22); // EINVAL
    }
    let i = (signum - 1) as usize;
    unsafe {
        (*set)[i / SIGSET_WORD_BITS] &= !(1 << (i % SIGSET_WORD_BITS));
    }
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigismember(set: *const SigSet, signum: c_int) -> c_int {
    if set.is_null() || signum <= 0 || signum > 64 {
        return 0;
    }
    let i = (signum - 1) as usize;
    let bit = unsafe { ((*set)[i / SIGSET_WORD_BITS] >> (i % SIGSET_WORD_BITS)) & 1 };
    bit as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn raise(sig: c_int) -> c_int {
    let pid = match minix_std::process::getpid() {
        Ok((pid, _ppid)) => pid,
        Err(e) => return fail(e.0),
    };
    match minix_std::time::kill(pid, sig) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

static mut STRSIGNAL_BUF: [u8; 32] = [0; 32];

const SIGNAL_NAMES: [&[u8]; 32] = [
    b"",
    b"Hangup",
    b"Interrupt",
    b"Quit",
    b"Illegal instruction",
    b"Trace/breakpoint trap",
    b"Aborted",
    b"Bus error",
    b"Floating point exception",
    b"Killed",
    b"User defined signal 1",
    b"Segmentation fault",
    b"User defined signal 2",
    b"Broken pipe",
    b"Alarm clock",
    b"Terminated",
    b"",
    b"",
    b"",
    b"",
    b"Stopped (signal)",
    b"",
    b"",
    b"",
    b"",
    b"",
    b"",
    b"",
    b"Window changed",
    b"",
    b"",
    b"System call",
];

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strsignal(sig: c_int) -> *mut c_char {
    let name: &[u8] = if sig >= 0 && (sig as usize) < SIGNAL_NAMES.len() {
        let n = SIGNAL_NAMES[sig as usize];
        if n.is_empty() { b"Unknown signal" } else { n }
    } else {
        b"Unknown signal"
    };
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(STRSIGNAL_BUF) };
    let copy = name.len().min(buf.len() - 1);
    buf[..copy].copy_from_slice(&name[..copy]);
    buf[copy] = 0;
    buf.as_mut_ptr() as *mut c_char
}

// Sockets

/// C `struct sockaddr_in` (reference `net/gen/socket.h`): length, family,
/// network-order port and address, then padding. 16 bytes. The port/address
/// fields are byte arrays so the network-order layout is endianness-explicit
/// (layout-identical to the C `u16`/`u32` fields).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SockAddrIn {
    sin_len: u8,
    sin_family: u8,
    sin_port: [u8; 2], // network byte order
    sin_addr: [u8; 4], // network byte order
    sin_zero: [u8; 8],
}

/// Decode a C `struct sockaddr_in` into the address bytes + host-order port
/// the `minix-std` net layer expects. Errors are negated errnos
/// (EAFNOSUPPORT for a non-AF_INET family or a too-short buffer).
///
/// # Safety
///
/// `addr` must point to at least `addrlen` bytes of a `sockaddr_in`.
unsafe fn decode_sockaddr_in(addr: *const u8, addrlen: u32) -> Result<([u8; 4], u16), i32> {
    if addr.is_null() || addrlen < 16 {
        return Err(-97); // EAFNOSUPPORT
    }
    let s = unsafe { &*(addr as *const SockAddrIn) };
    if s.sin_family != 2 {
        // AF_INET
        return Err(-97); // EAFNOSUPPORT
    }
    Ok((s.sin_addr, u16::from_be_bytes(s.sin_port)))
}

/// Fill a C `struct sockaddr_in` (reference getpeername semantics: family
/// AF_INET, network-order port/address, sin_len = 16; copy at most
/// `*addrlen` bytes and update it).
///
/// # Safety
///
/// `addr`/`addrlen` must be valid for the sizes the C caller declared.
unsafe fn encode_sockaddr_in(addr: *mut u8, addrlen: *mut u32, ip: [u8; 4], port: u16) {
    if addr.is_null() || addrlen.is_null() {
        return;
    }
    let len = unsafe { *addrlen }.min(16);
    let s = SockAddrIn {
        sin_len: 16,
        sin_family: 2, // AF_INET
        sin_port: port.to_be_bytes(),
        sin_addr: ip,
        sin_zero: [0; 8],
    };
    unsafe {
        core::ptr::copy_nonoverlapping(&s as *const SockAddrIn as *const u8, addr, len as usize);
        *addrlen = len;
    }
}

/// `socket(2)`: create an endpoint — AF_INET/SOCK_STREAM → `/dev/tcp`,
/// AF_INET/SOCK_DGRAM → `/dev/udp`, AF_INET/SOCK_RAW+ICMP → `/dev/ip`.
/// Returns the file descriptor.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn socket(domain: c_int, type_: c_int, protocol: c_int) -> c_int {
    match minix_std::net::socket(domain, type_, protocol) {
        Ok(fd) => fd,
        Err(e) => fail(e.0),
    }
}

/// `bind(2)`: bind a socket to a `struct sockaddr_in` (AF_INET).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
/// # Safety
///
/// `address` must be a readable `struct sockaddr_in` of at least `address_len`
/// bytes.
pub unsafe extern "C" fn bind(sock: c_int, address: *const c_void, address_len: u32) -> c_int {
    let (ip, port) = match unsafe { decode_sockaddr_in(address as *const u8, address_len) } {
        Ok(a) => a,
        Err(e) => return fail(-e),
    };
    match minix_std::net::bind(sock, ip, port) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

/// `connect(2)`: run the TCP three-way handshake, or set the UDP default
/// destination and receive filter.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
/// # Safety
///
/// `address` must be a readable `struct sockaddr_in` of at least `address_len`
/// bytes.
pub unsafe extern "C" fn connect(sock: c_int, address: *const c_void, address_len: u32) -> c_int {
    let (ip, port) = match unsafe { decode_sockaddr_in(address as *const u8, address_len) } {
        Ok(a) => a,
        Err(e) => return fail(-e),
    };
    match minix_std::net::connect(sock, ip, port) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

/// `sendto(2)`: send one datagram, optionally to an explicit destination
/// (a `struct sockaddr_in`). Only `flags == 0` is supported; on a
/// connected socket `dest_addr` is ignored.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sendto(
    sock: c_int,
    buf: *const c_void,
    len: usize,
    flags: c_int,
    dest_addr: *const c_void,
    dest_len: u32,
) -> isize {
    if flags != 0 {
        return fail(95) as isize; // EOPNOTSUPP
    }
    if buf.is_null() {
        return fail(22) as isize; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, len) };
    let dest = if dest_addr.is_null() {
        None
    } else {
        match unsafe { decode_sockaddr_in(dest_addr as *const u8, dest_len) } {
            Ok((ip, port)) => Some(minix_std::net::SocketAddr { ip, port }),
            Err(e) => return fail(-e) as isize,
        }
    };
    match unsafe { minix_std::net::sendto(sock, slice, dest) } {
        Ok(n) => n as isize,
        Err(e) => fail(e.0) as isize,
    }
}

/// `recvfrom(2)`: receive one datagram and, when `src_addr`/`src_len` are
/// given, the sender's `struct sockaddr_in`. Only `flags == 0` is
/// supported.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn recvfrom(
    sock: c_int,
    buf: *mut c_void,
    len: usize,
    flags: c_int,
    src_addr: *mut c_void,
    src_len: *mut u32,
) -> isize {
    if flags != 0 {
        return fail(95) as isize; // EOPNOTSUPP
    }
    if buf.is_null() {
        return fail(22) as isize; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len) };
    match unsafe { minix_std::net::recvfrom(sock, slice) } {
        Ok((n, addr)) => {
            if !src_addr.is_null() && !src_len.is_null() && n > 0 {
                unsafe { encode_sockaddr_in(src_addr as *mut u8, src_len, addr.ip, addr.port) };
            }
            n as isize
        }
        Err(e) => fail(e.0) as isize,
    }
}

/// `shutdown(2)`: SHUT_WR/SHUT_RDWR send our FIN and keep reading;
/// SHUT_RD is ENOSYS (the net server cannot close the read half).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn shutdown(sock: c_int, how: c_int) -> c_int {
    match minix_std::net::shutdown(sock, how) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

/// `send(2)`: write the byte stream (TCP) or one datagram (UDP). Only
/// `flags == 0` is supported.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn send(sock: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize {
    if flags != 0 {
        return fail(95) as isize; // EOPNOTSUPP
    }
    if buf.is_null() {
        return fail(22) as isize; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, len) };
    match unsafe { minix_std::net::send(sock, slice) } {
        Ok(n) => n as isize,
        Err(e) => fail(e.0) as isize,
    }
}

/// `recv(2)`: read the byte stream (TCP) or one datagram (UDP). Only
/// `flags == 0` is supported; 0 means EOF for a stream.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn recv(sock: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize {
    if flags != 0 {
        return fail(95) as isize; // EOPNOTSUPP
    }
    if buf.is_null() {
        return fail(22) as isize; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len) };
    match unsafe { minix_std::net::recv(sock, slice) } {
        Ok(n) => n as isize,
        Err(e) => fail(e.0) as isize,
    }
}

/// `listen(2)`: mark a bound socket as a listener with the given backlog.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn listen(sock: c_int, backlog: c_int) -> c_int {
    match minix_std::net::listen(sock, backlog) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

/// `accept(2)`: accept the next pending connection. Fills `address` (a
/// `struct sockaddr_in`) when non-NULL, like the reference accept(2).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
/// # Safety
///
/// `address`/`address_len` must be null or valid for the sizes the C caller
/// declared.
pub unsafe extern "C" fn accept(sock: c_int, address: *mut c_void, address_len: *mut u32) -> c_int {
    let newfd = match minix_std::net::accept(sock) {
        Ok(fd) => fd,
        Err(e) => return fail(e.0),
    };
    if !address.is_null() && !address_len.is_null() {
        if let Ok((ip, port)) = minix_std::net::getpeername(newfd) {
            unsafe { encode_sockaddr_in(address as *mut u8, address_len, ip, port) };
        }
    }
    newfd
}

/// `getpeername(2)`: fill `address` with the connected peer.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
/// # Safety
///
/// `address`/`address_len` must be valid for the sizes the C caller declared.
pub unsafe extern "C" fn getpeername(
    sock: c_int,
    address: *mut c_void,
    address_len: *mut u32,
) -> c_int {
    if address.is_null() || address_len.is_null() {
        return fail(22); // EINVAL
    }
    match minix_std::net::getpeername(sock) {
        Ok((ip, port)) => {
            unsafe { encode_sockaddr_in(address as *mut u8, address_len, ip, port) };
            0
        }
        Err(e) => fail(e.0),
    }
}

/// `getsockname(2)`: fill `address` with the local bound address.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
/// # Safety
///
/// `address`/`address_len` must be valid for the sizes the C caller declared.
pub unsafe extern "C" fn getsockname(
    sock: c_int,
    address: *mut c_void,
    address_len: *mut u32,
) -> c_int {
    if address.is_null() || address_len.is_null() {
        return fail(22); // EINVAL
    }
    match minix_std::net::getsockname(sock) {
        Ok((ip, port)) => {
            unsafe { encode_sockaddr_in(address as *mut u8, address_len, ip, port) };
            0
        }
        Err(e) => fail(e.0),
    }
}

/// `poll(2)`: check fd readiness. There is no readiness notification in
/// the net server yet, so only the serial fds (0-2) are ever ready — a
/// socket poll returns 0 events (the caller retries or times out).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poll(fds: *mut c_void, nfds: u64, _timeout: c_int) -> c_int {
    if fds.is_null() {
        return fail(22); // EINVAL
    }
    let fds = fds as *mut PollFd;
    let mut ready = 0;
    for i in 0..nfds {
        let pfd = unsafe { &mut *fds.add(i as usize) };
        pfd.revents = 0;
        if pfd.fd >= 0 && pfd.fd <= 2 {
            // Serial console: readable and writable at all times.
            pfd.revents = (pfd.events as i16) & (POLLIN | POLLOUT);
            if pfd.revents != 0 {
                ready += 1;
            }
        }
    }
    ready
}

/// C `struct pollfd` (`tools/c-include/poll.h`).
#[cfg(target_os = "minix")]
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x001;
const POLLOUT: i16 = 0x004;

// ---- dlfcn.h ----
//
// A *dynamic* program loads a shared object through the loader: the functions below call
// it, and it supplies the four `__rtld_*` names the way it supplies the TLS bounds above —
// they are in no object's tables, so `loader_defined` in `crates/ldso/src/rtld.rs` answers
// them with the loader's own `dlopen` and friends. A *static* program has no loader in its
// address space at all (no `PT_INTERP`, nothing to ask), and says so.

/// The loader's `dlopen` family. Nothing defines these: a `libc.so` is linked with them
/// undefined, exactly as it is with `__tls_get_addr`.
#[cfg(all(target_os = "minix", feature = "so"))]
unsafe extern "C" {
    fn __rtld_dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn __rtld_dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn __rtld_dlclose(handle: *mut c_void) -> c_int;
    fn __rtld_dlerror() -> *mut c_char;
}

/// `dlopen(3)`: load a shared object now, and return a handle for `dlsym`.
///
/// `NULL` for `filename` is the caller's own image, i.e. the global scope.
#[cfg(all(target_os = "minix", feature = "so"))]
#[unsafe(no_mangle)]
pub extern "C" fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void {
    unsafe { __rtld_dlopen(filename, flags) }
}

/// `dlsym(3)`: the address `symbol` has in the scope `handle` names, or null.
#[cfg(all(target_os = "minix", feature = "so"))]
#[unsafe(no_mangle)]
pub extern "C" fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void {
    unsafe { __rtld_dlsym(handle, symbol) }
}

/// `dlerror(3)`: the last failure's message, once, or null when there is none.
#[cfg(all(target_os = "minix", feature = "so"))]
#[unsafe(no_mangle)]
pub extern "C" fn dlerror() -> *mut c_char {
    unsafe { __rtld_dlerror() }
}

/// `dlclose(3)`: accepted, and nothing is unloaded — the object stays mapped and in the
/// loader's table, so a second `dlopen` of it hands back the same handle.
#[cfg(all(target_os = "minix", feature = "so"))]
#[unsafe(no_mangle)]
pub extern "C" fn dlclose(handle: *mut c_void) -> c_int {
    unsafe { __rtld_dlclose(handle) }
}

/// `dlopen(3)`: unsupported — a statically linked image has no loader to load one with.
#[cfg(all(target_os = "minix", not(feature = "so")))]
#[unsafe(no_mangle)]
pub extern "C" fn dlopen(_filename: *const c_char, _flags: c_int) -> *mut c_void {
    core::ptr::null_mut()
}

/// `dlsym(3)`: unsupported (see `dlopen`).
#[cfg(all(target_os = "minix", not(feature = "so")))]
#[unsafe(no_mangle)]
pub extern "C" fn dlsym(_handle: *mut c_void, _symbol: *const c_char) -> *mut c_void {
    core::ptr::null_mut()
}

/// `dlclose(3)`: unsupported (see `dlopen`).
#[cfg(all(target_os = "minix", not(feature = "so")))]
#[unsafe(no_mangle)]
pub extern "C" fn dlclose(_handle: *mut c_void) -> c_int {
    -1
}

/// `dlerror(3)`: why the three above do nothing here.
#[cfg(all(target_os = "minix", not(feature = "so")))]
#[unsafe(no_mangle)]
pub extern "C" fn dlerror() -> *mut c_char {
    b"dlopen: this program is statically linked, so it has no loader\0".as_ptr() as *const c_char
        as *mut c_char
}

// Utility

/// Simple strlen for C strings.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    let mut len = 0;
    while unsafe { *s.add(len) } != 0 {
        len += 1;
    }
    len
}

/// Simple memset.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(s: *mut c_void, c: c_int, n: usize) -> *mut c_void {
    if s.is_null() {
        return s;
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(s as *mut u8, n) };
    for byte in slice.iter_mut() {
        *byte = c as u8;
    }
    s
}

/// Simple memcpy.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    if dest.is_null() || src.is_null() {
        return dest;
    }
    // Byte-by-byte volatile copy: `copy_nonoverlapping` lowers to a call to
    // `memcpy` itself on this target (LLVM emits a libcall for the memcpy
    // intrinsic, which tail-call-optimizes into an infinite loop). Volatile
    // accesses cannot be turned into a libcall.
    let d = dest as *mut u8;
    let s = src as *const u8;
    for i in 0..n {
        unsafe {
            core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i)));
        }
    }
    dest
}

/// Simple memmove (handles overlap).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    if dest.is_null() || src.is_null() {
        return dest;
    }
    // Same volatile-byte-loop rationale as `memcpy`; copy backward when the
    // destination overlaps the source from above so the copy stays correct.
    let d = dest as *mut u8;
    let s = src as *const u8;
    unsafe {
        if (dest as usize) <= (src as usize) {
            for i in 0..n {
                core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i)));
            }
        } else {
            for i in (0..n).rev() {
                core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i)));
            }
        }
    }
    dest
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    // Host-runnable: the sockaddr_in byte math (network-order port/addr).
    #[test]
    fn sockaddr_in_decode_matches_c_network_order_layout() {
        // sin_len=16, sin_family=AF_INET(2), sin_port=18080 (BE),
        // sin_addr=10.0.2.2 (BE bytes), sin_zero.
        let bytes: [u8; 16] = [16, 2, 0x46, 0xA0, 10, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0];
        let (ip, port) = unsafe { decode_sockaddr_in(bytes.as_ptr(), 16) }.unwrap();
        assert_eq!(ip, [10, 0, 2, 2]);
        assert_eq!(port, 18080);
    }

    #[test]
    fn sockaddr_in_encode_writes_c_network_order() {
        let mut bytes = [0xFFu8; 16];
        let mut len: u32 = 16;
        unsafe {
            encode_sockaddr_in(bytes.as_mut_ptr(), &mut len, [10, 0, 2, 2], 20000);
        }
        // 20000 = 0x4E20.
        assert_eq!(
            bytes,
            [16, 2, 0x4E, 0x20, 10, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(len, 16);
    }

    #[test]
    fn sockaddr_in_decode_rejects_bad_family_and_short_len() {
        let bad_family: [u8; 16] = [16, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            unsafe { decode_sockaddr_in(bad_family.as_ptr(), 16) },
            Err(-97)
        );
        assert_eq!(
            unsafe { decode_sockaddr_in(bad_family.as_ptr(), 8) },
            Err(-97)
        );
    }

    #[test]
    fn sockaddr_in_encode_respects_caller_buffer_size() {
        let mut bytes = [0xFFu8; 16];
        let mut len: u32 = 8;
        unsafe {
            encode_sockaddr_in(bytes.as_mut_ptr(), &mut len, [10, 0, 2, 2], 20000);
        }
        assert_eq!(len, 8); // truncated, like the reference getpeername
        assert_eq!(&bytes[..4], &[16, 2, 0x4E, 0x20]);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_strlen() {
        unsafe {
            let s = b"hello\0";
            assert_eq!(strlen(s.as_ptr() as *const c_char), 5);
            assert_eq!(strlen(core::ptr::null()), 0);
            let empty = b"\0";
            assert_eq!(strlen(empty.as_ptr() as *const c_char), 0);
        }
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_memset() {
        let mut buf = [0xFFu8; 10];
        unsafe {
            memset(buf.as_mut_ptr() as *mut c_void, 0, 10);
        }
        assert_eq!(buf, [0; 10]);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_memcpy() {
        let src = [1u8, 2, 3, 4, 5];
        let mut dst = [0u8; 5];
        unsafe {
            memcpy(
                dst.as_mut_ptr() as *mut c_void,
                src.as_ptr() as *const c_void,
                5,
            );
        }
        assert_eq!(dst, [1, 2, 3, 4, 5]);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_memmove() {
        let mut buf = [1u8, 2, 3, 4, 5];
        // Overlapping: move bytes 0..3 to bytes 2..5
        unsafe {
            memmove(
                buf.as_mut_ptr().add(2) as *mut c_void,
                buf.as_ptr() as *const c_void,
                3,
            );
        }
        assert_eq!(buf, [1, 2, 1, 2, 3]);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_malloc_signatures() {
        fn _malloc(f: unsafe extern "C" fn(usize) -> *mut c_void) {
            let _ = f;
        }
        fn _free(f: unsafe extern "C" fn(*mut c_void)) {
            let _ = f;
        }
        fn _calloc(f: unsafe extern "C" fn(usize, usize) -> *mut c_void) {
            let _ = f;
        }
        fn _realloc(f: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void) {
            let _ = f;
        }
        fn _errno(f: extern "C" fn() -> *mut c_int) {
            let _ = f;
        }
        _malloc(malloc);
        _free(free);
        _calloc(calloc);
        _realloc(realloc);
        _errno(__errno_location);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_open_signature() {
        fn _check(f: unsafe extern "C" fn(*const c_char, c_int, c_int) -> c_int) {
            let _ = f;
        }
        _check(open);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_read_signature() {
        fn _check(f: unsafe extern "C" fn(c_int, *mut c_void, usize) -> isize) {
            let _ = f;
        }
        _check(read);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_write_signature() {
        fn _check(f: unsafe extern "C" fn(c_int, *const c_void, usize) -> isize) {
            let _ = f;
        }
        _check(write);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_close_signature() {
        fn _check(f: extern "C" fn(c_int) -> c_int) {
            let _ = f;
        }
        _check(close);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_fork_signature() {
        fn _check(f: unsafe extern "C" fn() -> c_int) {
            let _ = f;
        }
        _check(fork);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_exit_signature() {
        fn _check(f: extern "C" fn(c_int) -> !) {
            let _ = f;
        }
        _check(exit);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_mmap_signature() {
        fn _check(
            f: unsafe extern "C" fn(*mut c_void, usize, c_int, c_int, c_int, i64) -> *mut c_void,
        ) {
            let _ = f;
        }
        _check(mmap);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_kill_signature() {
        fn _check(f: extern "C" fn(c_int, c_int) -> c_int) {
            let _ = f;
        }
        _check(kill);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn test_socket_signatures() {
        fn _socket(f: extern "C" fn(c_int, c_int, c_int) -> c_int) {
            let _ = f;
        }
        fn _addr_in(f: unsafe extern "C" fn(c_int, *const c_void, u32) -> c_int) {
            let _ = f;
        }
        fn _addr_out(f: unsafe extern "C" fn(c_int, *mut c_void, *mut u32) -> c_int) {
            let _ = f;
        }
        fn _io(f: unsafe extern "C" fn(c_int, *const c_void, usize, c_int) -> isize) {
            let _ = f;
        }
        fn _iomut(f: unsafe extern "C" fn(c_int, *mut c_void, usize, c_int) -> isize) {
            let _ = f;
        }
        fn _sendto(
            f: unsafe extern "C" fn(
                c_int,
                *const c_void,
                usize,
                c_int,
                *const c_void,
                u32,
            ) -> isize,
        ) {
            let _ = f;
        }
        fn _recvfrom(
            f: unsafe extern "C" fn(
                c_int,
                *mut c_void,
                usize,
                c_int,
                *mut c_void,
                *mut u32,
            ) -> isize,
        ) {
            let _ = f;
        }
        fn _shutdown(f: extern "C" fn(c_int, c_int) -> c_int) {
            let _ = f;
        }
        fn _listen(f: extern "C" fn(c_int, c_int) -> c_int) {
            let _ = f;
        }
        _socket(socket);
        _addr_in(bind);
        _addr_in(connect);
        _sendto(sendto);
        _recvfrom(recvfrom);
        _shutdown(shutdown);
        _io(send);
        _iomut(recv);
        _listen(listen);
        _addr_out(accept);
        _addr_out(getpeername);
        _addr_out(getsockname);
    }
}
