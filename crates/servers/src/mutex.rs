//! Spin-lock based mutex for `no_std` environments.
//!
//! Provides a simple spinlock + `UnsafeCell` wrapper that avoids
//! `static mut` entirely. Intended for single-threaded or carefully
//! synchronized contexts (e.g., test setup vs. function call boundaries).

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// A simple spinlock mutex for `no_std` use.
///
/// # Safety
///
/// This is NOT suitable for interrupt handlers or real concurrency —
/// it does not disable interrupts. It is designed to provide safe
/// `&mut` access to global state without `static mut`, primarily to
/// work around Rust 2024's `deny(static_mut_refs)`.
pub struct Mutex<T> {
    /// Whether the lock is held.
    locked: AtomicBool,
    /// The protected data.
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
        }
    }

    /// Acquire the lock, spinning until it becomes available.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        MutexGuard { mutex: self }
    }

    /// Try to acquire the lock without spinning.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(MutexGuard { mutex: self })
        } else {
            None
        }
    }
}

/// A guard that holds the `Mutex` lock.
///
/// Provides `Deref` and `DerefMut` to access the inner data.
/// Releases the lock when dropped.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        self.mutex.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_unlock() {
        let m = Mutex::new(42u32);
        {
            let mut guard = m.lock();
            assert_eq!(*guard, 42);
            *guard = 100;
        }
        let guard = m.lock();
        assert_eq!(*guard, 100);
    }

    #[test]
    fn test_try_lock() {
        let m = Mutex::new(0u32);
        let g1 = m.try_lock();
        assert!(g1.is_some());
        let g2 = m.try_lock();
        assert!(g2.is_none());
        drop(g1);
        let g3 = m.try_lock();
        assert!(g3.is_some());
    }

    #[test]
    fn test_lock_guard_deref_mut() {
        let m = Mutex::new([0u8; 4]);
        m.lock().copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(*m.lock(), [1, 2, 3, 4]);
    }

    #[test]
    fn test_lock_with_struct() {
        #[derive(Debug, PartialEq)]
        struct Point {
            x: i32,
            y: i32,
        }
        let m = Mutex::new(Point { x: 0, y: 0 });
        {
            let mut p = m.lock();
            p.x = 10;
            p.y = 20;
        }
        let p = m.lock();
        assert_eq!(*p, Point { x: 10, y: 20 });
    }

    #[test]
    fn test_lock_with_array() {
        let m = Mutex::new([0u16; 128]);
        {
            let mut arr = m.lock();
            arr[64] = 42;
            arr[127] = 99;
        }
        let arr = m.lock();
        assert_eq!(arr[64], 42);
        assert_eq!(arr[127], 99);
    }

    #[test]
    fn test_try_lock_reacquire_after_drop() {
        let m = Mutex::new(0u32);
        assert!(m.try_lock().is_some());
        // guard dropped
        assert!(m.try_lock().is_some());
    }

    #[test]
    fn test_guard_drop_releases_lock() {
        let m = Mutex::new(0u32);
        {
            let _g = m.lock();
            assert!(m.try_lock().is_none());
        }
        assert!(m.try_lock().is_some());
    }

    #[test]
    fn test_mutex_is_send_sync() {
        fn check_send<T: Send>(_: &T) {}
        fn check_sync<T: Sync>(_: &T) {}
        let m = Mutex::new(0u32);
        check_send(&m);
        check_sync(&m);
    }

    #[test]
    fn test_static_mutex_with_struct() {
        static LOCK: Mutex<[u8; 8]> = Mutex::new([0u8; 8]);
        {
            let mut buf = LOCK.lock();
            buf.copy_from_slice(b"testdata");
        }
        let buf = LOCK.lock();
        assert_eq!(&buf[..], b"testdata");
    }

    #[test]
    fn test_sequential_locks_preserve_data() {
        let m = Mutex::new([0u32; 10]);
        for i in 0..10 {
            m.lock()[i] = i as u32;
        }
        let v = m.lock();
        assert_eq!(v[5], 5);
        assert_eq!(v[9], 9);
    }

    #[test]
    fn test_double_lock_blocks_try_lock() {
        let m = Mutex::new(0u32);
        let _g1 = m.lock();
        assert!(m.try_lock().is_none());
        drop(_g1);
        assert!(m.try_lock().is_some());
    }
}
