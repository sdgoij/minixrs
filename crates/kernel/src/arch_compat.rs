//! Architecture compatibility — re-exports types that differ per arch.
//! Used by kernel code that needs to reference arch-specific types
//! where the HAL abstraction doesn't apply (Mcontext, TrapFrame size).

#[cfg(target_arch = "riscv64")]
pub use arch_riscv64::mcontext::Mcontext;
/// Re-export the arch-specific Mcontext type.
#[cfg(target_arch = "x86_64")]
pub use arch_x86_64::mcontext::Mcontext;

/// Size of the p_reg array in Proc (used for bounds checking).
pub const P_REG_SIZE: u64 = 256;

/// Re-export the arch-specific TrapFrame type.
#[cfg(target_arch = "x86_64")]
pub use arch_x86_64::frame::TrapFrame;
#[cfg(not(target_arch = "x86_64"))]
/// On non-x86_64, TrapFrame is just a byte array (compatible type).
pub type TrapFrame = [u8; 256];
