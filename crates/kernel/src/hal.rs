//! Architecture HAL (Hardware Abstraction Layer).
//!
//! This is THE ONLY file in `kernel/src/` that uses `#[cfg(target_arch)]`.
//! It re-exports the correct arch-specific HAL implementation.
//! Everything else in the kernel calls `hal::*()` unconditionally.
//!
//! The `sim` arm comes first because it is a *host* build of an existing target
//! arch: without it, a `--features sim` build on x86_64 would silently pick the
//! real x86_64 HAL and try to execute privileged instructions.

#[cfg(feature = "sim")]
pub use arch_sim::hal::*;

#[cfg(all(not(feature = "sim"), target_arch = "x86_64"))]
pub use arch_x86_64::hal::*;

#[cfg(all(not(feature = "sim"), target_arch = "riscv64"))]
pub use arch_riscv64::hal::*;

#[cfg(all(not(feature = "sim"), target_arch = "aarch64"))]
pub use arch_aarch64::hal::*;
