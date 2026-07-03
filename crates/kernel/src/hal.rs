//! Architecture HAL (Hardware Abstraction Layer).
//!
//! This is THE ONLY file in `kernel/src/` that uses `#[cfg(target_arch)]`.
//! It re-exports the correct arch-specific HAL implementation.
//! Everything else in the kernel calls `hal::*()` unconditionally.

#[cfg(target_arch = "x86_64")]
pub use arch_x86_64::hal::*;

#[cfg(target_arch = "riscv64")]
pub use arch_riscv64::hal::*;
