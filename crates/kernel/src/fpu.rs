//! Keeping a process's live FPU/SIMD registers across kernel entry.
//!
//! x86_64 code built by rustc uses SSE by default, and nothing here saved a user process's
//! vector registers before the kernel ran. A value the compiler keeps live in `%xmm0` across a
//! page fault therefore came back as whatever the kernel's own last 16-byte copy left in that
//! register. On this port that is not theoretical: `uu_seq`'s `movups %xmm0,0x18(%rsi)` stores
//! the first 16 bytes of a `SYS_VMCTL` kernel-call message into `ArgMatches`, which is what made
//! every allocation-heavy coreutils tool die inside `clap` (KNOWN_ISSUES 12).
//!
//! The live state is saved into `p_seg.fpu_state` on the way in (`save`) and reloaded from it by
//! `restore` on the way out, so the kernel may use the FPU freely in between and a context switch
//! in the middle cannot lose the state either.
//!
//! Every user→kernel path must call `save` as its *first* Rust. The hooks today are
//! `save_fault_context` (`#PF`), `syscall_handler_c`, and the timer, serial, keyboard and mouse ISR
//! callbacks — all in `crates/kernel-boot/src/main.rs`. `#GP`/`#UD`/`#DF`/`#DB` never return, so
//! they need none, and the profiling clock's ISR (`arch_x86_64::hal::init_profile_clock`) is not
//! installed anywhere today; if it is ever used it needs the same treatment. Calling `save` twice
//! for one entry would write a clobbered state over a good one, so add new hooks at the earliest
//! point of a new path, not part-way through an existing one.

use crate::proc::Proc;

/// Save the live FPU/SIMD state into `rp`'s area, allocating the area on first use.
///
/// # Safety
///
/// `rp` must be a valid `Proc`, and this must run on kernel entry *before* any kernel code that
/// could use the FPU. It must be called exactly once per entry: a second call for the same entry
/// would save an already-clobbered state over the good one.
pub unsafe fn save(rp: *mut Proc) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if rp.is_null() {
            return;
        }
        let mut area = (*rp).p_seg.fpu_state;
        if area.is_null() {
            // One page: the FXSAVE area is 512 bytes and needs 16-byte alignment, which the page
            // allocator gives for free. The kernel reaches it through the identity map.
            let Some(page) = crate::hal::alloc_phys_contig(1) else {
                return;
            };
            area = page as *mut u8;
            core::ptr::write_bytes(area, 0, 1 << 12);
            (*rp).p_seg.fpu_state = area;
        }
        arch_x86_64::hw::save_fpu(&mut *(area as *mut [u8; arch_x86_64::hw::FPU_SAVE_AREA_SIZE]));
        let old = (*rp)
            .p_misc_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        (*rp).p_misc_flags.store(
            old | crate::proc::MiscFlags::FPU_INITIALIZED.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = rp;
    }
}

/// Give `child` its own copy of `parent`'s FPU/SIMD state.
///
/// C MINIX keeps the FXSAVE area *inside* `Proc`, so `*rpc = *rpp` copies the state by value and a
/// fork inherits it. Here it is a pointer, so the struct copy would leave parent and child sharing
/// one area and overwriting each other's state. `parent`'s live state has already been saved —
/// fork runs inside a syscall, and every syscall entry saves it.
///
/// # Safety
///
/// `parent` and `child` must both be valid `Proc`s, and `child` must not own an area yet.
pub unsafe fn fork_inherit(parent: *mut Proc, child: *mut Proc) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if parent.is_null() || child.is_null() {
            return;
        }
        (*child).p_seg.fpu_state = core::ptr::null_mut();
        let src = (*parent).p_seg.fpu_state;
        if src.is_null() {
            return;
        }
        let Some(page) = crate::hal::alloc_phys_contig(1) else {
            return;
        };
        let dst = page as *mut u8;
        core::ptr::copy_nonoverlapping(src, dst, crate::hal::FPU_STATE_SIZE);
        (*child).p_seg.fpu_state = dst;
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (parent, child);
    }
}

/// Forget `rp`'s saved FPU/SIMD state, so the image that replaces it does not start with the old
/// image's registers (the same clearing C does at exec).
///
/// # Safety
///
/// `rp` must be a valid `Proc`.
pub unsafe fn reset(rp: *mut Proc) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if rp.is_null() {
            return;
        }
        let area = (*rp).p_seg.fpu_state;
        if !area.is_null() {
            core::ptr::write_bytes(area, 0, crate::hal::FPU_STATE_SIZE);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = rp;
    }
}

/// Byte offset of `Proc.p_seg.fpu_state`, for `restore`'s `FXRSTOR`.
///
/// `restore` is arch code and cannot name a kernel type, the same problem `TLS_FS_BASE_OFF`
/// solves for the thread pointer. Returns 0 where the field is not used, which disables the load.
pub fn state_offset() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        core::mem::offset_of!(Proc, p_seg) + core::mem::offset_of!(crate::proc::SegFrame, fpu_state)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}
