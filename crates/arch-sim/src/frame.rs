//! Frame layout for the simulator.
//!
//! The kernel keeps a process's saved registers as a `[u8; 256]` blob and reads
//! them only through the `hal::read_frame_field` / `write_frame_field` pair, so
//! the layout is the HAL's business. Two constraints come from outside:
//!
//! - `debug.rs` reads the instruction pointer at offset 160 and the stack
//!   pointer at 168 directly, so those offsets are fixed.
//! - `write_retval` / `read_syscall_nr` use offset 0, matching the original
//!   `rax`.
//!
//! The remaining offsets are this port's choice; they follow the x86_64 port's
//! shape where that costs nothing, which is why `CS` sits at 184.

/// Return value, and the syscall number on entry.
pub const RAX: usize = 0;

/// First of six syscall argument slots.
pub const ARG0: usize = 8;

pub const RIP: usize = 160;
pub const RSP: usize = 168;
pub const RFLAGS: usize = 176;

/// Resume-mode marker. The simulator has no privilege levels, so this is inert
/// bookkeeping kept so the frame shape matches the other ports.
pub const CS: usize = 184;

/// The saved-register blob, as the kernel sees it.
pub type TrapFrame = [u8; 256];

/// Byte offset of syscall argument `i`.
pub const fn arg_offset(i: usize) -> usize {
    ARG0 + i * 8
}

/// A zeroed frame with a plausible initial flag state.
pub fn default_frame() -> [u8; 256] {
    let mut frame = [0u8; 256];
    frame[RFLAGS..RFLAGS + 8].copy_from_slice(&0x202u64.to_ne_bytes());
    frame[CS..CS + 8].copy_from_slice(&1u64.to_ne_bytes());
    frame
}

/// Read a 64-bit field from a raw frame.
///
/// # Safety
///
/// `offset + 8` must be within the 256-byte frame.
pub unsafe fn read_field(frame: &[u8; 256], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&frame[offset..offset + 8]);
    u64::from_ne_bytes(bytes)
}

/// Write a 64-bit field into a raw frame.
///
/// # Safety
///
/// `offset + 8` must be within the 256-byte frame.
pub unsafe fn write_field(frame: &mut [u8; 256], offset: usize, val: u64) {
    frame[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}
