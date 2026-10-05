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

use core::ffi::{c_char, c_int, c_long, c_uint, c_void};
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use super::set_errno;

/// Default per-thread stack size (same as the std thread PAL).
const PTHREAD_STACK_SIZE: usize = 256 * 1024;

const EINVAL: i32 = 22;
const ENOMEM: i32 = 12;
const ESRCH: i32 = 3;

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

/// POSIX `pthread_kill(thread, sig)`.
///
/// The tid is the whole of it: threads are `Proc`s here, so PM hands the tid to
/// the kernel (`SIGCALLS_TID_OFF`), which resolves it with `find_thread_by_tid`
/// and builds the handler frame on that thread's own saved registers.
/// `pthread_self()` returns null on this port's main thread - tid 0 - so a null
/// handle names the process itself rather than being invalid.
///
/// What this is not: a signal that has to *pend* (blocked in the mask) loses the
/// target, because masks are per process, and PM re-delivers it to the process
/// when it unblocks. The unblocked case, which is what `pthread_kill` is for, is
/// exact.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_kill(thread: usize, sig: c_int) -> c_int {
    let tid = if thread == 0 {
        0
    } else {
        // SAFETY: `thread` is a `pthread_t` this layer handed out.
        let tid = unsafe { (*(thread as *const Pthread)).tid };
        if tid <= 0 {
            return fail(ESRCH);
        }
        tid
    };
    match minix_std::time::thread_kill(tid, sig) {
        Ok(()) => 0,
        Err(e) => fail(e.0),
    }
}

// ---- mutex attributes and typed mutexes ----

const EBUSY: i32 = 16;
const EAGAIN: i32 = 11;
const ETIMEDOUT: i32 = 110;

const MUTEX_NORMAL: i32 = 0;
const MUTEX_RECURSIVE: i32 = 1;

/// `pthread_mutex_t`, matching `tools/c-include/pthread.h`: the futex word at
/// offset 0 (the lock), the kind, and — for a recursive mutex — the owning tid
/// and the recursion depth.
#[repr(C)]
pub struct PthreadMutex {
    state: u32,
    kind: i32,
    owner: i32,
    count: i32,
}

#[repr(C)]
pub struct PthreadMutexAttr {
    kind: i32,
}

/// `struct timespec`, matching `tools/c-include/time.h` (`long`/`long`).
#[repr(C)]
pub struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_init(attr: *mut PthreadMutexAttr) -> c_int {
    if attr.is_null() {
        return fail(EINVAL);
    }
    unsafe { (*attr).kind = MUTEX_NORMAL };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_destroy(_attr: *mut PthreadMutexAttr) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_settype(
    attr: *mut PthreadMutexAttr,
    kind: c_int,
) -> c_int {
    if attr.is_null() {
        return fail(EINVAL);
    }
    unsafe { (*attr).kind = kind };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_gettype(
    attr: *const PthreadMutexAttr,
    kind: *mut c_int,
) -> c_int {
    if attr.is_null() || kind.is_null() {
        return fail(EINVAL);
    }
    unsafe { *kind = (*attr).kind };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_init(m: *mut PthreadMutex, attr: *const c_void) -> c_int {
    if m.is_null() {
        return fail(EINVAL);
    }
    let kind = if attr.is_null() {
        MUTEX_NORMAL
    } else {
        unsafe { (*(attr as *const PthreadMutexAttr)).kind }
    };
    unsafe {
        (*m).state = 0;
        (*m).kind = kind;
        (*m).owner = 0;
        (*m).count = 0;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_destroy(_m: *mut PthreadMutex) -> c_int {
    0
}

/// Acquire `m`, honouring a recursive mutex: a thread that already owns it
/// deepens the count instead of deadlocking on its own lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_lock(m: *mut PthreadMutex) -> c_int {
    if m.is_null() {
        return fail(EINVAL);
    }
    let me = minix_rt::thread_self();
    if unsafe { (*m).kind } == MUTEX_RECURSIVE
        && unsafe { (*m).state } != 0
        && unsafe { (*m).owner } == me
    {
        unsafe { (*m).count += 1 };
        return 0;
    }
    let state = unsafe { core::ptr::addr_of_mut!((*m).state) } as *mut AtomicU32;
    while unsafe { (*state).swap(1, Ordering::Acquire) } != 0 {
        // SAFETY: the mutex's futex word lives as long as the mutex.
        unsafe { minix_rt::futex_wait(core::ptr::addr_of!((*m).state), 1) };
    }
    unsafe {
        (*m).owner = me;
        (*m).count = 1;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_trylock(m: *mut PthreadMutex) -> c_int {
    if m.is_null() {
        return fail(EINVAL);
    }
    let me = minix_rt::thread_self();
    if unsafe { (*m).kind } == MUTEX_RECURSIVE
        && unsafe { (*m).state } != 0
        && unsafe { (*m).owner } == me
    {
        unsafe { (*m).count += 1 };
        return 0;
    }
    let state = unsafe { core::ptr::addr_of_mut!((*m).state) } as *mut AtomicU32;
    if unsafe { (*state).swap(1, Ordering::Acquire) } == 0 {
        unsafe {
            (*m).owner = me;
            (*m).count = 1;
        }
        0
    } else {
        fail(EBUSY)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_timedlock(
    m: *mut PthreadMutex,
    abstime: *const Timespec,
) -> c_int {
    if m.is_null() {
        return fail(EINVAL);
    }
    // No futex timeout exists yet, so a timed lock polls to its deadline rather
    // than being woken exactly at it.
    loop {
        if unsafe { pthread_mutex_trylock(m) } == 0 {
            return 0;
        }
        if !abstime.is_null() && now_micros() >= abstime_micros(abstime) {
            return fail(ETIMEDOUT);
        }
        sleep_micros(2_000);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_unlock(m: *mut PthreadMutex) -> c_int {
    if m.is_null() {
        return fail(EINVAL);
    }
    if unsafe { (*m).kind } == MUTEX_RECURSIVE {
        unsafe { (*m).count -= 1 };
        if unsafe { (*m).count } > 0 {
            return 0;
        }
    }
    unsafe {
        (*m).owner = 0;
        (*m).count = 0;
        (*(core::ptr::addr_of_mut!((*m).state) as *mut AtomicU32)).store(0, Ordering::Release);
    }
    minix_rt::futex_wake(unsafe { core::ptr::addr_of!((*m).state) }, 1);
    0
}

// ---- condition variables over the same futex ----

/// `pthread_cond_t`: a sequence counter. A waiter records the count, unlocks,
/// and futex-waits for it to change; a signal bumps it and wakes, so a wake that
/// races the wait is caught by the waiter's `futex_wait` comparing the value.
#[repr(C)]
pub struct PthreadCond {
    seq: u32,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_init(c: *mut PthreadCond, _attr: *const c_void) -> c_int {
    if c.is_null() {
        return fail(EINVAL);
    }
    unsafe { (*c).seq = 0 };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_destroy(_c: *mut PthreadCond) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_wait(c: *mut PthreadCond, m: *mut PthreadMutex) -> c_int {
    if c.is_null() || m.is_null() {
        return fail(EINVAL);
    }
    let seq = unsafe { core::ptr::addr_of!((*c).seq) };
    let seen = unsafe { (*(seq as *mut AtomicU32)).load(Ordering::SeqCst) };
    unsafe { pthread_mutex_unlock(m) };
    unsafe { minix_rt::futex_wait(seq, seen) };
    unsafe { pthread_mutex_lock(m) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_timedwait(
    c: *mut PthreadCond,
    m: *mut PthreadMutex,
    abstime: *const Timespec,
) -> c_int {
    if c.is_null() || m.is_null() {
        return fail(EINVAL);
    }
    let seq = unsafe { core::ptr::addr_of_mut!((*c).seq) };
    let seen = unsafe { (*(seq as *mut AtomicU32)).load(Ordering::SeqCst) };
    unsafe { pthread_mutex_unlock(m) };
    // No futex timeout exists yet, so a timed wait polls the sequence.
    let r = loop {
        if unsafe { (*(seq as *mut AtomicU32)).load(Ordering::SeqCst) } != seen {
            break 0;
        }
        if !abstime.is_null() && now_micros() >= abstime_micros(abstime) {
            break fail(ETIMEDOUT);
        }
        sleep_micros(2_000);
    };
    unsafe { pthread_mutex_lock(m) };
    r
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_signal(c: *mut PthreadCond) -> c_int {
    if c.is_null() {
        return fail(EINVAL);
    }
    let seq = unsafe { core::ptr::addr_of_mut!((*c).seq) };
    unsafe { (*(seq as *mut AtomicU32)).fetch_add(1, Ordering::SeqCst) };
    minix_rt::futex_wake(seq as *const u32, 1);
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_broadcast(c: *mut PthreadCond) -> c_int {
    if c.is_null() {
        return fail(EINVAL);
    }
    let seq = unsafe { core::ptr::addr_of_mut!((*c).seq) };
    unsafe { (*(seq as *mut AtomicU32)).fetch_add(1, Ordering::SeqCst) };
    minix_rt::futex_wake(seq as *const u32, u32::MAX);
    0
}

/// `pthread_condattr_t`: only the clock id is stored, matching
/// `tools/c-include/pthread.h`. The clock choice is advisory here — the timed
/// wait compares against the monotonic clock regardless.
#[repr(C)]
pub struct PthreadCondAttr {
    clock: c_int,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_init(attr: *mut PthreadCondAttr) -> c_int {
    if attr.is_null() {
        return fail(EINVAL);
    }
    unsafe { (*attr).clock = 0 };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_destroy(_attr: *mut PthreadCondAttr) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_setclock(
    attr: *mut PthreadCondAttr,
    clock_id: c_long,
) -> c_int {
    if attr.is_null() {
        return fail(EINVAL);
    }
    unsafe { (*attr).clock = clock_id as c_int };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_getclock(
    attr: *const PthreadCondAttr,
    clock_id: *mut c_long,
) -> c_int {
    if attr.is_null() || clock_id.is_null() {
        return fail(EINVAL);
    }
    unsafe { *clock_id = (*attr).clock as c_long };
    0
}

/// `pthread_barrier_t`, matching `tools/c-include/pthread.h`: the arrival count,
/// the number that have arrived, and a generation that trip-counting waiters
/// watch to know when to leave.
#[repr(C)]
pub struct PthreadBarrier {
    mutex: PthreadMutex,
    cond: PthreadCond,
    count: c_uint,
    waiting: c_uint,
    generation: c_uint,
}

/// `PTHREAD_BARRIER_SERIAL_THREAD`: which single waiter the barrier releases
/// first.
const PTHREAD_BARRIER_SERIAL_THREAD: c_int = -1;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_barrier_init(
    barrier: *mut PthreadBarrier,
    _attr: *const c_void,
    count: c_uint,
) -> c_int {
    if barrier.is_null() || count == 0 {
        return fail(EINVAL);
    }
    unsafe {
        pthread_mutex_init(&mut (*barrier).mutex, core::ptr::null());
        pthread_cond_init(&mut (*barrier).cond, core::ptr::null());
        (*barrier).count = count;
        (*barrier).waiting = 0;
        (*barrier).generation = 0;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_barrier_destroy(barrier: *mut PthreadBarrier) -> c_int {
    if barrier.is_null() {
        return fail(EINVAL);
    }
    unsafe {
        pthread_mutex_destroy(&mut (*barrier).mutex);
        pthread_cond_destroy(&mut (*barrier).cond);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_barrier_wait(barrier: *mut PthreadBarrier) -> c_int {
    if barrier.is_null() {
        return fail(EINVAL);
    }
    unsafe { pthread_mutex_lock(&mut (*barrier).mutex) };
    let generation = unsafe { (*barrier).generation };
    unsafe { (*barrier).waiting += 1 };
    if unsafe { (*barrier).waiting } == unsafe { (*barrier).count } {
        unsafe {
            (*barrier).waiting = 0;
            (*barrier).generation = generation.wrapping_add(1);
        }
        unsafe { pthread_cond_broadcast(&mut (*barrier).cond) };
        unsafe { pthread_mutex_unlock(&mut (*barrier).mutex) };
        PTHREAD_BARRIER_SERIAL_THREAD
    } else {
        while unsafe { (*barrier).generation } == generation {
            unsafe { pthread_cond_wait(&mut (*barrier).cond, &mut (*barrier).mutex) };
        }
        unsafe { pthread_mutex_unlock(&mut (*barrier).mutex) };
        0
    }
}

/// `pthread_rwlock_t`, matching `tools/c-include/pthread.h`: one mutex and
/// condition guard the reader count and the writer flag.
#[repr(C)]
pub struct PthreadRwlock {
    mutex: PthreadMutex,
    cond: PthreadCond,
    readers: c_int,
    writer: c_int,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_rwlock_init(
    rwlock: *mut PthreadRwlock,
    _attr: *const c_void,
) -> c_int {
    if rwlock.is_null() {
        return fail(EINVAL);
    }
    unsafe {
        pthread_mutex_init(&mut (*rwlock).mutex, core::ptr::null());
        pthread_cond_init(&mut (*rwlock).cond, core::ptr::null());
        (*rwlock).readers = 0;
        (*rwlock).writer = 0;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_rwlock_destroy(rwlock: *mut PthreadRwlock) -> c_int {
    if rwlock.is_null() {
        return fail(EINVAL);
    }
    unsafe {
        pthread_mutex_destroy(&mut (*rwlock).mutex);
        pthread_cond_destroy(&mut (*rwlock).cond);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_rwlock_rdlock(rwlock: *mut PthreadRwlock) -> c_int {
    if rwlock.is_null() {
        return fail(EINVAL);
    }
    unsafe { pthread_mutex_lock(&mut (*rwlock).mutex) };
    while unsafe { (*rwlock).writer } != 0 {
        unsafe { pthread_cond_wait(&mut (*rwlock).cond, &mut (*rwlock).mutex) };
    }
    unsafe { (*rwlock).readers += 1 };
    unsafe { pthread_mutex_unlock(&mut (*rwlock).mutex) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_rwlock_tryrdlock(rwlock: *mut PthreadRwlock) -> c_int {
    if rwlock.is_null() {
        return fail(EINVAL);
    }
    unsafe { pthread_mutex_lock(&mut (*rwlock).mutex) };
    if unsafe { (*rwlock).writer } != 0 {
        unsafe { pthread_mutex_unlock(&mut (*rwlock).mutex) };
        return fail(EBUSY);
    }
    unsafe {
        (*rwlock).readers += 1;
        pthread_mutex_unlock(&mut (*rwlock).mutex);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_rwlock_wrlock(rwlock: *mut PthreadRwlock) -> c_int {
    if rwlock.is_null() {
        return fail(EINVAL);
    }
    unsafe { pthread_mutex_lock(&mut (*rwlock).mutex) };
    while unsafe { (*rwlock).writer } != 0 || unsafe { (*rwlock).readers } != 0 {
        unsafe { pthread_cond_wait(&mut (*rwlock).cond, &mut (*rwlock).mutex) };
    }
    unsafe { (*rwlock).writer = 1 };
    unsafe { pthread_mutex_unlock(&mut (*rwlock).mutex) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_rwlock_trywrlock(rwlock: *mut PthreadRwlock) -> c_int {
    if rwlock.is_null() {
        return fail(EINVAL);
    }
    unsafe { pthread_mutex_lock(&mut (*rwlock).mutex) };
    if unsafe { (*rwlock).writer } != 0 || unsafe { (*rwlock).readers } != 0 {
        unsafe { pthread_mutex_unlock(&mut (*rwlock).mutex) };
        return fail(EBUSY);
    }
    unsafe {
        (*rwlock).writer = 1;
        pthread_mutex_unlock(&mut (*rwlock).mutex);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_rwlock_unlock(rwlock: *mut PthreadRwlock) -> c_int {
    if rwlock.is_null() {
        return fail(EINVAL);
    }
    unsafe {
        pthread_mutex_lock(&mut (*rwlock).mutex);
        if (*rwlock).writer != 0 {
            (*rwlock).writer = 0;
        } else if (*rwlock).readers > 0 {
            (*rwlock).readers -= 1;
        }
        pthread_cond_broadcast(&mut (*rwlock).cond);
        pthread_mutex_unlock(&mut (*rwlock).mutex);
    }
    0
}

/// POSIX `sched_yield()`: hand the CPU to another runnable thread. The rest of
/// the `sched.h` interface (priorities, policies, `sched_setscheduler`) is not
/// supported.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sched_yield() -> c_int {
    minix_rt::thread_yield();
    0
}

/// Advisory thread naming. PM has no way to name a thread, so the request is
/// accepted and dropped; Mesa ignores the result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_setname_np(_thread: usize, _name: *const c_char) -> c_int {
    0
}

// ---- once ----

const ONCE_DONE: u32 = 2;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_once(once: *mut u32, init: unsafe extern "C" fn()) -> c_int {
    if once.is_null() {
        return fail(EINVAL);
    }
    let p = once as *mut AtomicU32;
    if unsafe { (*p).load(Ordering::Acquire) } == ONCE_DONE {
        return 0;
    }
    if unsafe { (*p).compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) }.is_ok() {
        unsafe { init() };
        unsafe { (*p).store(ONCE_DONE, Ordering::Release) };
        minix_rt::futex_wake(once as *const u32, u32::MAX);
    } else {
        loop {
            let v = unsafe { (*p).load(Ordering::Acquire) };
            if v == ONCE_DONE {
                break;
            }
            unsafe { minix_rt::futex_wait(once as *const u32, v) };
        }
    }
    0
}

// ---- thread-specific data ----

/// The most keys a process may hold. Mesa's `tss_t` use is a handful; this is
/// generous without making each thread's value block large.
const MAX_KEYS: usize = 128;

/// Destructors, indexed by key, plus a per-thread array of values — which is
/// what makes `getspecific` per-thread without a lock.
static KEY_DTORS: [AtomicUsize; MAX_KEYS] = [const { AtomicUsize::new(0) }; MAX_KEYS];
static NEXT_KEY: AtomicU32 = AtomicU32::new(0);
#[thread_local]
static KEY_VALUES: [core::cell::Cell<*mut c_void>; MAX_KEYS] =
    [const { core::cell::Cell::new(core::ptr::null_mut()) }; MAX_KEYS];

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_key_create(key: *mut u32, dtor: usize) -> c_int {
    if key.is_null() {
        return fail(EINVAL);
    }
    let id = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    if id as usize >= MAX_KEYS {
        return fail(EAGAIN);
    }
    KEY_DTORS[id as usize].store(dtor, Ordering::Release);
    unsafe { *key = id };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_key_delete(key: u32) -> c_int {
    if key as usize >= MAX_KEYS {
        return fail(EINVAL);
    }
    KEY_DTORS[key as usize].store(0, Ordering::Release);
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_setspecific(key: u32, value: *const c_void) -> c_int {
    if key as usize >= MAX_KEYS {
        return fail(EINVAL);
    }
    KEY_VALUES[key as usize].set(value as *mut c_void);
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_getspecific(key: u32) -> *mut c_void {
    if key as usize >= MAX_KEYS {
        return core::ptr::null_mut();
    }
    KEY_VALUES[key as usize].get()
}

/// `minix_std`'s monotonic clock in microseconds since boot (0 if unavailable).
fn now_micros() -> u128 {
    match minix_std::time::clock_gettime(1) {
        Ok(t) => (t.tv_sec.max(0) as u128) * 1_000_000 + (t.tv_nsec.max(0) as u128) / 1000,
        Err(_) => 0,
    }
}

fn abstime_micros(ts: *const Timespec) -> u128 {
    let sec = unsafe { (*ts).tv_sec }.max(0) as u128;
    let nsec = unsafe { (*ts).tv_nsec }.max(0) as u128;
    sec * 1_000_000 + nsec / 1000
}

/// Sleep, or spin on a host build (which never runs the timed paths).
fn sleep_micros(us: u32) {
    #[cfg(target_os = "minix")]
    unsafe {
        crate::c_sys::usleep(us);
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = us;
        core::hint::spin_loop();
    }
}

/// Record `errno` and return -1 (POSIX error convention).
fn fail(e: i32) -> i32 {
    set_errno(e);
    -1
}
