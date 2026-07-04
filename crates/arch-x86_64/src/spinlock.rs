//! Spinlock implementation — adapted from `minix/kernel/spinlock.h`
//!
//! Provides spinlock primitives for SMP synchronization. In single-CPU
//! mode (default), all lock/unlock operations are compile-time no-ops.
//!
//! **x86_64 differences from i386:**
//! - Uses `core::sync::atomic::AtomicU32` instead of C `atomic_t`
//! - `atomic_cas_32` from `hw` module wraps `compare_exchange`
//! - `core::hint::spin_loop()` for the PAUSE instruction in the spin loop

use core::sync::atomic::Ordering;

use crate::hw;

/// Whether SMP is compiled in.
pub const CONFIG_SMP: bool = false;

/// Maximum number of CPUs.
pub const CONFIG_MAX_CPUS: u32 = 1;

/// Whether real spinlock operations are needed.
const SPINLOCKS_ACTIVE: bool = CONFIG_SMP && CONFIG_MAX_CPUS > 1;

// Spinlock

/// A spinlock — busy-wait mutual exclusion primitive.
///
/// When `SPINLOCKS_ACTIVE` is false (single-CPU default), all operations
/// are no-ops and compile to no instructions.
pub struct Spinlock {
    lock: core::sync::atomic::AtomicU32,
}

impl Spinlock {
    /// Create a new unlocked spinlock.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lock: core::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Initialize the spinlock to unlocked.
    pub fn init(&mut self) {
        self.lock.store(0, Ordering::Relaxed);
    }

    /// Acquire the spinlock, spinning until it becomes available.
    ///
    /// # Safety
    ///
    /// The caller must ensure no deadlock (e.g., not holding the lock
    /// already).
    pub unsafe fn lock(&self) {
        if SPINLOCKS_ACTIVE {
            while hw::atomic_cas_32(&self.lock, 0, 1) != 0 {
                // Hint to the CPU that we're in a spin-wait loop (PAUSE).
                core::hint::spin_loop();
            }
        }
    }

    /// Try to acquire the spinlock without blocking.
    ///
    /// Returns `true` if the lock was acquired.
    ///
    /// # Safety
    ///
    /// The caller must ensure no deadlock.
    pub unsafe fn try_lock(&self) -> bool {
        if !SPINLOCKS_ACTIVE {
            true
        } else {
            hw::atomic_cas_32(&self.lock, 0, 1) == 0
        }
    }

    /// Release the spinlock.
    ///
    /// # Safety
    ///
    /// The caller must hold the lock.
    pub unsafe fn unlock(&self) {
        if SPINLOCKS_ACTIVE {
            self.lock.store(0, Ordering::Release);
        }
    }
}

impl Default for Spinlock {
    fn default() -> Self {
        Self::new()
    }
}

// Big Kernel Lock (BKL)

/// Big Kernel Lock — a global lock serializing kernel entry.
///
/// In single-CPU mode this is a no-op.
pub static BIG_KERNEL_LOCK: Spinlock = Spinlock::new();

/// Acquire the Big Kernel Lock.
///
/// # Safety
///
/// See `Spinlock::lock`.
pub unsafe fn bkl_lock() {
    unsafe {
        BIG_KERNEL_LOCK.lock();
    }
}

/// Release the Big Kernel Lock.
///
/// # Safety
///
/// See `Spinlock::unlock`.
pub unsafe fn bkl_unlock() {
    unsafe {
        BIG_KERNEL_LOCK.unlock();
    }
}

// Macros — matching Minix C `SPINLOCK_DEFINE`, `SPINLOCK_DECLARE`

/// Define a new Spinlock static.
///
/// In single-CPU mode this expands to nothing; the static is dead-code
/// eliminated by the optimizer.
#[macro_export]
macro_rules! spinlock_define {
    ($name:ident) => {
        static $name: $crate::spinlock::Spinlock = $crate::spinlock::Spinlock::new();
    };
}

/// Define a private Spinlock static.
#[macro_export]
macro_rules! private_spinlock_define {
    ($name:ident) => {
        static $name: $crate::spinlock::Spinlock = $crate::spinlock::Spinlock::new();
    };
}

/// Declare an extern Spinlock static.
#[macro_export]
macro_rules! spinlock_declare {
    ($name:ident) => {
        extern "C" {
            static $name: $crate::spinlock::Spinlock;
        }
    };
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinlock_new_is_unlocked() {
        let lock = Spinlock::new();
        // try_lock should succeed on a fresh lock
        unsafe {
            assert!(lock.try_lock());
        }
    }

    #[test]
    fn test_spinlock_lock_unlock() {
        unsafe {
            let lock = Spinlock::new();
            lock.lock();
            // When SPINLOCKS_ACTIVE, the lock is held and try_lock fails.
            // In single-CPU mode, lock() is a no-op so try_lock still succeeds.
            if SPINLOCKS_ACTIVE {
                assert!(!lock.try_lock());
            }
            lock.unlock();
            // After unlocking, try_lock should succeed
            assert!(lock.try_lock());
        }
    }

    #[test]
    fn test_spinlock_trylock() {
        unsafe {
            let lock = Spinlock::new();
            assert!(lock.try_lock());
            // Second try should fail (already held) — only when spinlocks active
            if SPINLOCKS_ACTIVE {
                assert!(!lock.try_lock());
                lock.unlock();
            }
            assert!(lock.try_lock());
        }
    }

    #[test]
    fn test_spinlock_init() {
        unsafe {
            let mut lock = Spinlock::new();
            lock.lock();
            if SPINLOCKS_ACTIVE {
                assert!(!lock.try_lock());
            }
            lock.init();
            // After init, should be unlocked
            assert!(lock.try_lock());
        }
    }

    #[test]
    fn test_bkl_lock_unlock() {
        unsafe {
            bkl_lock();
            // BKL held — try_lock on BIG_KERNEL_LOCK should fail
            if SPINLOCKS_ACTIVE {
                assert!(!BIG_KERNEL_LOCK.try_lock());
            }
            bkl_unlock();
        }
    }

    #[test]
    fn test_config_constants() {
        const _: () = assert!(!CONFIG_SMP);
        const _: () = assert!(CONFIG_MAX_CPUS == 1);
        const _: () = assert!(!SPINLOCKS_ACTIVE);
    }

    #[test]
    fn test_spinlock_default() {
        let lock = Spinlock::default();
        unsafe {
            assert!(lock.try_lock());
        }
    }

    #[test]
    fn test_spinlock_double_unlock() {
        // Unlocking an already unlocked spinlock must not panic.
        unsafe {
            let lock = Spinlock::new();
            lock.unlock();
            lock.unlock();
            // After double unlock, lock should still be acquirable.
            assert!(lock.try_lock());
        }
    }

    #[test]
    fn test_spinlock_const_new() {
        // Verify Spinlock::new() can be used in const context.
        #[allow(clippy::declare_interior_mutable_const)]
        const _LOCK: Spinlock = Spinlock::new();
    }

    #[test]
    fn test_spinlock_define_macro() {
        // Verify the spinlock_define! macro compiles.
        spinlock_define!(TEST_LOCK);
        unsafe {
            assert!(TEST_LOCK.try_lock());
        }
    }

    #[test]
    fn test_private_spinlock_define_macro() {
        private_spinlock_define!(_PRIVATE_LOCK);
        unsafe {
            assert!(_PRIVATE_LOCK.try_lock());
        }
    }

    #[test]
    fn test_bkl_roundtrip() {
        // BKL lock/unlock roundtrip should not panic.
        unsafe {
            bkl_lock();
            bkl_unlock();
        }
    }

    #[test]
    fn test_spinlock_trylock_after_lock() {
        unsafe {
            let lock = Spinlock::new();
            lock.lock();
            // After lock(), unlock(), the lock should be free.
            lock.unlock();
            assert!(lock.try_lock());
        }
    }

    #[test]
    fn test_spinlock_init_unlocks() {
        unsafe {
            let mut lock = Spinlock::new();
            lock.lock();
            // Re-init resets to unlocked.
            lock.init();
            assert!(lock.try_lock());
        }
    }

    #[test]
    fn test_big_kernel_lock_const() {
        // BIG_KERNEL_LOCK is a static, verify it's constructible as const.
        const _CHECK: () = {
            let _: Spinlock = Spinlock::new();
        };
    }
}
