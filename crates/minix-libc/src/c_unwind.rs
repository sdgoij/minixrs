//! The Itanium unwinder entry points a C runtime normally supplies.
//!
//! minix has no unwinder: the port builds with `panic = "abort"` and loads no
//! unwind tables, so the backtrace printer in `libstd` has no frames to walk.
//! These symbols exist so that a `std` binary links; the walk reports the end
//! of the stack at once, which leaves the printed trace empty.

#[cfg(target_os = "minix")]
use core::ffi::{c_int, c_void};

/// `_URC_END_OF_STACK` from `unwind.h`.
#[cfg(target_os = "minix")]
const URC_END_OF_STACK: c_int = 5;

/// Walk the stack, calling `cb` once per frame. There are no frames.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn _Unwind_Backtrace(
    _cb: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int,
    _arg: *mut c_void,
) -> c_int {
    URC_END_OF_STACK
}

/// The instruction pointer in an unwind context. Unreachable while the walk
/// above yields no context to ask about.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Unwind_GetIP(_ctx: *mut c_void) -> usize {
    0
}
