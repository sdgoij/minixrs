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
//!
//! riscv64 has the same defect with a different shape: `PSL_USERSET` runs a process with
//! `sstatus.FS=Dirty` (the FP unit on, the *registers* authoritative), so when the scheduler hands
//! the CPU to another process the live `f0`-`f31` are the departing process's and the arriving one
//! reads them. Measured: two processes each holding a value in `fs0` across a `getpid()` lost it on
//! every round (`FPPROBE CORRUPT`), while one process alone kept it. There is no RISC-V `FS=Off`
//! trap arm to fall back on and the trap frame has no room for `f0`-`f31`, so riscv64 saves at the
//! switch point instead: the switch sites (`riscv_post_syscall`, `riscv_timer_callback`,
//! `switch_to_user` at boot — all in `crates/kernel-boot/src/riscv64.rs`) call `save` on the process
//! being left and `restore` on the one being entered. That is enough because the kernel executes no
//! floating point itself: nothing between a process's last user instruction and the switch can
//! disturb its registers. `fork` saves the parent before copying, since at that moment its
//! registers are still live.

use crate::proc::Proc;

/// Where a riscv64 image keeps `fcsr`, past the 256 bytes of `f0`-`f31` (`fcsr` holds the rounding
/// mode and the accrued exception flags, so a process that changed either must not be handed the
/// next process's value).
#[cfg(target_arch = "riscv64")]
const RISCV_FCSR_OFF: usize = 256;

/// Bytes a riscv64 image occupies: `f0`-`f31` plus `fcsr`. One page is allocated and only this
/// prefix is used; `FPU_STATE_SIZE` stays 256 because that is what the user-visible `mc_fpstate`
/// holds (registers only), as the mcontext ABI defines it.
#[cfg(target_arch = "riscv64")]
const RISCV_IMAGE_SIZE: usize = RISCV_FCSR_OFF + 8;

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
    #[cfg(target_arch = "riscv64")]
    unsafe {
        if rp.is_null() {
            return;
        }
        let area = riscv_area(rp);
        if area.is_null() {
            return;
        }
        // `fsd` is the 64-bit store: F and D are both in the target's feature set, and the ABI is
        // Lp64d, so every `f` register is FLEN=64 even where a value is only a `float`.
        core::arch::asm!(
            "fsd f0,   0({p})", "fsd f1,   8({p})", "fsd f2,  16({p})", "fsd f3,  24({p})",
            "fsd f4,  32({p})", "fsd f5,  40({p})", "fsd f6,  48({p})", "fsd f7,  56({p})",
            "fsd f8,  64({p})", "fsd f9,  72({p})", "fsd f10, 80({p})", "fsd f11, 88({p})",
            "fsd f12, 96({p})", "fsd f13,104({p})", "fsd f14,112({p})", "fsd f15,120({p})",
            "fsd f16,128({p})", "fsd f17,136({p})", "fsd f18,144({p})", "fsd f19,152({p})",
            "fsd f20,160({p})", "fsd f21,168({p})", "fsd f22,176({p})", "fsd f23,184({p})",
            "fsd f24,192({p})", "fsd f25,200({p})", "fsd f26,208({p})", "fsd f27,216({p})",
            "fsd f28,224({p})", "fsd f29,232({p})", "fsd f30,240({p})", "fsd f31,248({p})",
            "frcsr {fcsr}",
            "sd    {fcsr}, 256({p})",
            p = in(reg) area,
            fcsr = out(reg) _,
            options(nostack, preserves_flags),
        );
        let old = (*rp)
            .p_misc_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        (*rp).p_misc_flags.store(
            old | crate::proc::MiscFlags::FPU_INITIALIZED.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64")))]
    {
        let _ = rp;
    }
}

/// Load `rp`'s FP image into the live registers just before it runs.
///
/// x86_64 reloads inside its `restore()` asm (`FXRSTOR` from `FPU_STATE_OFF`), so this is a no-op
/// there; riscv64's trap return is generic asm with no per-arch offset to hook, so its switch sites
/// call this in Rust instead.
///
/// # Safety
///
/// `rp` must be a valid `Proc`, and this must run on the last Rust before the user's registers are
/// read — after this the caller may not execute floating point, or it overwrites what it just
/// loaded.
pub unsafe fn restore(rp: *mut Proc) {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        if rp.is_null() {
            return;
        }
        // `sstatus.FS` has to be non-Off or the first `fld` is an illegal instruction. A trap from
        // user mode already carries `Dirty` (`PSL_USERSET`), but the boot path has never been in
        // user mode and does not, so set it here rather than assume it.
        core::arch::asm!(
            "csrs sstatus, {fs}",
            fs = in(reg) arch_riscv64::psl::sstatus::FS_DIRTY,
            options(nostack, preserves_flags),
        );
        let area = (*rp).p_seg.fpu_state;
        if area.is_null() {
            // No image: a process that has not run yet must start with zeroed registers, not with
            // whatever the last one left. (`save` allocates on first use, so a process that has run
            // and used FP always has one; fork's child gets a copy.)
            core::arch::asm!(
                "fscsr zero",
                "fmv.d.x f0, zero",
                "fmv.d.x f1, zero",
                "fmv.d.x f2, zero",
                "fmv.d.x f3, zero",
                "fmv.d.x f4, zero",
                "fmv.d.x f5, zero",
                "fmv.d.x f6, zero",
                "fmv.d.x f7, zero",
                "fmv.d.x f8, zero",
                "fmv.d.x f9, zero",
                "fmv.d.x f10, zero",
                "fmv.d.x f11, zero",
                "fmv.d.x f12, zero",
                "fmv.d.x f13, zero",
                "fmv.d.x f14, zero",
                "fmv.d.x f15, zero",
                "fmv.d.x f16, zero",
                "fmv.d.x f17, zero",
                "fmv.d.x f18, zero",
                "fmv.d.x f19, zero",
                "fmv.d.x f20, zero",
                "fmv.d.x f21, zero",
                "fmv.d.x f22, zero",
                "fmv.d.x f23, zero",
                "fmv.d.x f24, zero",
                "fmv.d.x f25, zero",
                "fmv.d.x f26, zero",
                "fmv.d.x f27, zero",
                "fmv.d.x f28, zero",
                "fmv.d.x f29, zero",
                "fmv.d.x f30, zero",
                "fmv.d.x f31, zero",
                options(nostack, preserves_flags),
            );
            return;
        }
        core::arch::asm!(
            "fld  f0,   0({p})", "fld  f1,   8({p})", "fld  f2,  16({p})", "fld  f3,  24({p})",
            "fld  f4,  32({p})", "fld  f5,  40({p})", "fld  f6,  48({p})", "fld  f7,  56({p})",
            "fld  f8,  64({p})", "fld  f9,  72({p})", "fld  f10, 80({p})", "fld  f11, 88({p})",
            "fld  f12, 96({p})", "fld  f13,104({p})", "fld  f14,112({p})", "fld  f15,120({p})",
            "fld  f16,128({p})", "fld  f17,136({p})", "fld  f18,144({p})", "fld  f19,152({p})",
            "fld  f20,160({p})", "fld  f21,168({p})", "fld  f22,176({p})", "fld  f23,184({p})",
            "fld  f24,192({p})", "fld  f25,200({p})", "fld  f26,208({p})", "fld  f27,216({p})",
            "fld  f28,224({p})", "fld  f29,232({p})", "fld  f30,240({p})", "fld  f31,248({p})",
            "ld   {fcsr}, 256({p})",
            "fscsr {fcsr}",
            p = in(reg) area,
            fcsr = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        let _ = rp;
    }
}

/// The riscv64 image area for `rp`, allocating and zeroing a page on first use.
///
/// # Safety
///
/// `rp` must be a valid `Proc`.
#[cfg(target_arch = "riscv64")]
unsafe fn riscv_area(rp: *mut Proc) -> *mut u8 {
    let mut area = unsafe { (*rp).p_seg.fpu_state };
    if area.is_null() {
        let Some(page) = crate::hal::alloc_phys_contig(1) else {
            return core::ptr::null_mut();
        };
        area = page as *mut u8;
        // Zero the whole page: the image is only `RISCV_IMAGE_SIZE` of it, but an uninitialised
        // tail is a value a later width could read as state.
        unsafe { core::ptr::write_bytes(area, 0, 1 << 12) };
        unsafe { (*rp).p_seg.fpu_state = area };
    }
    area
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
    #[cfg(target_arch = "riscv64")]
    unsafe {
        if parent.is_null() || child.is_null() {
            return;
        }
        // Unlike x86_64 there is no eager entry save to have captured the parent's registers: this
        // runs inside the fork syscall, and the registers are still live. Capture them first, or
        // the child copies a stale image (or null).
        save(parent);
        (*child).p_seg.fpu_state = core::ptr::null_mut();
        let src = (*parent).p_seg.fpu_state;
        if src.is_null() {
            return;
        }
        let Some(page) = crate::hal::alloc_phys_contig(1) else {
            return;
        };
        let dst = page as *mut u8;
        core::ptr::copy_nonoverlapping(src, dst, RISCV_IMAGE_SIZE);
        (*child).p_seg.fpu_state = dst;
    }
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
    #[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64")))]
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
    #[cfg(target_arch = "riscv64")]
    unsafe {
        if rp.is_null() {
            return;
        }
        let area = (*rp).p_seg.fpu_state;
        if !area.is_null() {
            core::ptr::write_bytes(area, 0, RISCV_IMAGE_SIZE);
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64")))]
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
