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

#[cfg(all(not(feature = "sim"), target_arch = "wasm32"))]
pub use arch_wasm32::hal::*;

#[cfg(all(not(feature = "sim"), target_arch = "riscv64"))]
pub use arch_riscv64::hal::*;

#[cfg(all(not(feature = "sim"), target_arch = "aarch64"))]
pub use arch_aarch64::hal::*;

/// How to copy between two processes' address spaces when this arch's page
/// tables cannot do it, or `None` when they can.
///
/// `None` is the answer for every arch that is not wasm32, and it means "take
/// the CR3 path": the three hardware arches really do join two address spaces
/// that way, and `arch-sim` — whose `boot_cr3()` is 0 — has the limitation M0
/// already recorded rather than a second way to make the copy.
///
/// `arch-wasm32` supplies the `Some` side, because there the two address spaces
/// are separate linear memories and only the host owns both. The cfg keeps this
/// definition out of the way of that one: an explicit item shadows a glob import,
/// so an unconditional `None` here would hide `arch-wasm32`'s `Some`.
#[cfg(not(target_arch = "wasm32"))]
pub static CROSS_ADDRESS_SPACE_COPY: Option<arch_common::safecopies::CrossAddressSpaceCopy> = None;
