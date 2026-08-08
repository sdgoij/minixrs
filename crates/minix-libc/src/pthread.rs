//! C-ABI pthreads over the kernel's 1:1 thread syscalls (THREADS.md Slice 3).
//!
//! `pthread_t` is a pointer to a per-thread [`Pthread`] bookkeeping struct
//! (kernel tid + join retval); the kernel tid itself is not exposed to the C
//! caller. Thread stacks come from the C heap (single-threaded, and
//! `pthread_create` runs on the main thread) so they cannot collide with
//! later `malloc` growth; every thread runs the [`pthread_trampoline`], which
//! sets up the thread's TLS block (per-thread errno) before calling the user
//! routine.
//!
//! Not thread-safe by design: the C heap (`malloc`) is a single-threaded
//! first-fit allocator; concurrent allocation from several threads needs the
//! Stage-A allocator work (see THREADS.md "Related work").

use core::ffi::{c_int, c_void};
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use super::set_errno;

/// Default per-thread stack size (same as the std thread PAL).
const PTHREAD_STACK_SIZE: usize = 256 * 1024;

const EINVAL: i32 = 22;
const ENOMEM: i32 = 12;

/// Per-thread bookkeeping; `pthread_t` points at this.
#[repr(C)]
struct Pthread {
    tid: i32,
    retval: *mut c_void,
    detached: i32,
}

/// Start routine + argument + handle + precomputed thread pointer, packed
/// for the trampoline (the kernel passes a single argument register). The
/// TLS block is prepared by `pthread_create` on the main thread so the
/// trampoline never touches the heap.
#[repr(C)]
struct PthreadStart {
    start: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
    pth: *mut Pthread,
    tp: usize,
}

/// The current thread's handle, per thread. Null on the main thread (which
/// has no `Pthread`); `pthread_self` returns it for comparison.
#[thread_local]
static CURRENT_PTHREAD: core::cell::Cell<*mut Pthread> =
    core::cell::Cell::new(core::ptr::null_mut());

/// The spawned-thread entry: install the precomputed thread pointer (the
/// TLS block was allocated by `pthread_create` on the main thread), run the
/// user routine, store its return value for `pthread_join`, and exit the
/// thread.
///
/// The C heap is single-threaded and owned by the main thread: worker
/// threads must not `malloc`/`sbrk`/`free` here. The [`PthreadStart`]
/// handle (56 bytes) and the detached [`Pthread`] leak per thread —
/// reclaimed by the Stage-A allocator work (THREADS.md).
unsafe extern "C" fn pthread_trampoline(data: usize) -> ! {
    unsafe {
        let start = &mut *(data as *mut PthreadStart);
        if start.tp != 0 {
            minix_rt::thread_set_tls(start.tp);
        }
        CURRENT_PTHREAD.set(start.pth);
        let ret = (start.start)(start.arg);
        (*start.pth).retval = ret;
        minix_rt::thread_exit(0);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_create(
    thread: *mut usize,
    _attr: *const c_void,
    start_routine: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
) -> c_int {
    unsafe {
        if thread.is_null() || start_routine as usize == 0 {
            return fail(EINVAL);
        }
        let pth = super::malloc(size_of::<Pthread>()) as *mut Pthread;
        if pth.is_null() {
            return fail(ENOMEM);
        }
        (*pth).tid = 0;
        (*pth).retval = core::ptr::null_mut();
        (*pth).detached = 0;

        let start = super::malloc(size_of::<PthreadStart>()) as *mut PthreadStart;
        if start.is_null() {
            super::free(pth as *mut c_void);
            return fail(ENOMEM);
        }
        (*start).start = start_routine;
        (*start).arg = arg;
        (*start).pth = pth;
        // Prepare the thread's TLS block on the main thread (the C heap is
        // single-threaded; the trampoline only installs the thread pointer).
        (*start).tp = super::tls_block_alloc();

        // Thread stacks come from the C heap (main-thread-only, so the
        // single-threaded allocator is safe). An sbrk'd stack would sit
        // inside the heap's break range, and a later `malloc` grow would
        // allocate into it — clobbering the stack and any handles stored
        // there. The stack block is intentionally never freed.
        let stack = super::malloc(PTHREAD_STACK_SIZE);
        if stack.is_null() {
            super::free(start as *mut c_void);
            super::free(pth as *mut c_void);
            return fail(ENOMEM);
        }
        let base = stack as usize;
        #[cfg(target_arch = "x86_64")]
        let stack_top = ((base + PTHREAD_STACK_SIZE) & !0xF) - 8;
        #[cfg(not(target_arch = "x86_64"))]
        let stack_top = (base + PTHREAD_STACK_SIZE) & !0xF;

        let entry: usize = pthread_trampoline as unsafe extern "C" fn(usize) -> ! as usize;
        let tid = minix_rt::thread_create(entry, stack_top, start as usize);
        if tid <= 0 {
            super::free(start as *mut c_void);
            super::free(pth as *mut c_void);
            return fail(-tid as i32);
        }
        (*pth).tid = tid;
        core::ptr::write(thread, pth as usize);
        0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_join(thread: usize, retval: *mut *mut c_void) -> c_int {
    unsafe {
        let pth = thread as *mut Pthread;
        if pth.is_null() || (*pth).tid <= 0 {
            return fail(EINVAL);
        }
        let r = minix_rt::thread_join((*pth).tid);
        if r < 0 {
            return fail(-r as i32);
        }
        if !retval.is_null() {
            core::ptr::write(retval, (*pth).retval);
        }
        super::free(pth as *mut c_void);
        0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_exit(retval: *mut c_void) -> ! {
    unsafe {
        let pth = CURRENT_PTHREAD.get();
        if !pth.is_null() {
            (*pth).retval = retval;
        }
        minix_rt::thread_exit(0);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_self() -> usize {
    CURRENT_PTHREAD.get() as usize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_equal(a: usize, b: usize) -> c_int {
    (a == b) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_detach(thread: usize) -> c_int {
    unsafe {
        let pth = thread as *mut Pthread;
        if pth.is_null() || (*pth).tid <= 0 {
            return fail(EINVAL);
        }
        (*pth).detached = 1;
        0
    }
}

// ---- mutexes over the futex syscalls ----

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_init(m: *mut u32, _attr: *const c_void) -> c_int {
    if m.is_null() {
        return fail(EINVAL);
    }
    unsafe { core::ptr::write_volatile(m, 0) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_destroy(_m: *mut u32) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_lock(m: *mut u32) -> c_int {
    if m.is_null() {
        return fail(EINVAL);
    }
    let state = m as *mut AtomicU32;
    while unsafe { (*state).swap(1, Ordering::Acquire) } != 0 {
        // SAFETY: `m` points to a readable `u32` for the mutex's lifetime.
        unsafe { minix_rt::futex_wait(m, 1) };
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_unlock(m: *mut u32) -> c_int {
    if m.is_null() {
        return fail(EINVAL);
    }
    let state = m as *mut AtomicU32;
    unsafe { (*state).store(0, Ordering::Release) };
    minix_rt::futex_wake(m, 1);
    0
}

/// Record `errno` and return -1 (POSIX error convention).
fn fail(e: i32) -> i32 {
    set_errno(e);
    -1
}
