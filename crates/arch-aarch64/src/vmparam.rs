//! AArch64 VM parameter constants — 4KB granule, 4-level paging.
//!
//! 4KB + 4 levels: L0→L1→L2→L3, 9 bits per level, 48-bit VA.
//! User addresses: bits 55-48 = 0x00 (0x0000000000000000 – 0x0000FFFFFFFFFFFF)

/// L1: PUD shift (bits 30-38) — 1GB blocks.
pub const L1_SHIFT: u32 = 30;
/// L2: PMD shift (bits 21-29) — 2MB blocks.
pub const L2_SHIFT: u32 = 21;
/// L3: PT shift (bits 12-20) — 4KB pages.
pub const L3_SHIFT: u32 = 12;

pub const NBPD_L1: u64 = 1u64 << L1_SHIFT;
pub const NBPD_L2: u64 = 1u64 << L2_SHIFT;
pub const NBPD_L3: u64 = 1u64 << L3_SHIFT;

pub const NENTRIES: u64 = 512;

pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
pub const PAGE_MASK: u64 = PAGE_SIZE - 1;

/// Top of user stack.
pub const USRSTACK: u64 = 0x0000_0FFF_FFFF_E000;

pub const VM_MIN_ADDRESS: u64 = 0;
pub const VM_MAXUSER_ADDRESS: u64 = 0x0000_0FFF_FFFF_FFFF;
pub const VM_MAX_ADDRESS: u64 = 0xFFFF_FFFF_FFFF_FFFF;

pub const VM_MIN_KERNEL_ADDRESS: u64 = 0xFFFF_0000_0000_0000;
pub const VM_MAX_KERNEL_ADDRESS: u64 = 0xFFFF_FFFF_FFFF_FFFF;

pub const MAXTSIZ: u64 = 256 * 1024 * 1024;
pub const DFLDSIZ: u64 = 512 * 1024 * 1024;
pub const MAXDSIZ: u64 = 128 * 1024 * 1024 * 1024;
pub const DFLSSIZ: u64 = 8 * 1024 * 1024;
pub const MAXSSIZ: u64 = 64 * 1024 * 1024;

// ---- Port user-VA layout (host-testable; hal.rs re-exports these) ----

/// Virtual address of the kernel's identity map (the RAM base). Also the
/// first non-user address: the kernel is loaded and identity-mapped here
/// inside TTBR0, and user space is only the low 1 GiB below it.
pub const fn kern_vaddr() -> u64 {
    0x4000_0000
}

/// Top of the user-accessible VA range.
///
/// The kernel is identity-mapped at the RAM base ([`kern_vaddr`]) inside
/// TTBR0, so aarch64 user space is only the low 1 GiB below it (exec image
/// @16 MiB, brk heap [`user_heap_base`]-[`user_heap_limit`], mmap
/// [`mmap_base`]+, stack [`user_stack_base`]). A ceiling that covered the
/// whole TTBR0 range (2^44 - 1) let a kernel-range VA pass the user-fault
/// gate, so the EL1 handler eret-retried it forever (KNOWN_ISSUES
/// [aarch64] #3); keeping the ceiling below the kernel window makes
/// kernel-range faults fatal instead.
pub const MAX_USER_ADDRESS: u64 = kern_vaddr();

/// User stack base virtual address, just below the RAM start so the stack
/// gets maximum space below it while staying in the low 1 GiB (PUD[0]).
pub const fn user_stack_base() -> u64 {
    0x3FC0_0000u64
}

/// User stack size: 1 MiB — server binaries allocate large stack frames
/// (e.g. pfs_main's inlined init uses ~340KB), which would underflow a
/// 64KB stack.
pub const fn user_stack_size() -> usize {
    0x100_000
}

/// Base of the anonymous-mmap search range. Must stay in the
/// user-accessible low 1 GiB (PUD[0]): everything at/above 0x40000000 is
/// the kernel's EL1-only identity map and cannot be mapped for user access.
pub const fn mmap_base() -> u64 {
    0x3000_0000
}

/// Base of the userland brk heap. AArch64 user space is only the low 1 GiB
/// (PUD[0]): the kernel's EL1-only identity map starts at 0x40000000, so a
/// heap at the top of the range (0x3FE00000, as on x86/riscv) would
/// collide with the kernel block after ~2 MiB of growth. The heap sits
/// below the anonymous-mmap base (0x30000000) so heap growth (up) and mmap
/// regions (up from the mmap base) cannot overlap.
pub const fn user_heap_base() -> u64 {
    0x2000_0000
}

/// Exclusive upper bound for brk growth — the anonymous-mmap base.
pub const fn user_heap_limit() -> u64 {
    0x3000_0000
}

/// Base of VM's temporary self-mapping range (kernel call 62 VM_PAGING_MAP
/// into VM's own address space). The generic "just below the arch user top"
/// spot used on x86/riscv would land on the mmap base here (0x30000000,
/// given the lowered [`MAX_USER_ADDRESS`]), so VM's scratch lives in the
/// free gap between the exec image and the brk heap instead.
pub const fn vm_scratch_base() -> u64 {
    0x1000_0000
}

pub const USRIOSIZE: u32 = 300;
pub const VM_PHYS_SIZE: u64 = USRIOSIZE as u64 * PAGE_SIZE;
pub const VM_MAX_KERNEL_BUF: u64 = 384 * 1024 * 1024;
pub const VM_PHYSSEG_MAX: u32 = 32;
pub const VM_NFREELIST: u32 = 2;
pub const VM_FREELIST_DEFAULT: u32 = 0;
pub const VM_FREELIST_FIRST16: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paging_constants() {
        assert_eq!(PAGE_SIZE, 4096);
        assert_eq!(NBPD_L3, 4096);
        assert_eq!(NBPD_L2, 2 * 1024 * 1024);
        assert_eq!(NBPD_L1, 1024 * 1024 * 1024);
        assert_eq!(NENTRIES, 512);
    }

    #[test]
    fn test_address_ranges() {
        assert_eq!(VM_MAXUSER_ADDRESS, 0x0000_0FFF_FFFF_FFFF);
        assert_eq!(VM_MIN_KERNEL_ADDRESS, 0xFFFF_0000_0000_0000);
    }

    #[test]
    fn test_user_va_ceiling_below_kernel_window() {
        // The kernel is identity-mapped at the RAM base; the user-VA
        // ceiling must sit at/below it so a kernel-range fault is fatal
        // instead of passing the user-fault gate and being eret-retried
        // (KNOWN_ISSUES [aarch64] #3). All values are const, so these are
        // compile-time pins.
        const _: () = assert!(kern_vaddr() == 0x4000_0000);
        const _: () = assert!(MAX_USER_ADDRESS == kern_vaddr());
        // Every user range lives below the ceiling.
        const _: () = assert!(0x0100_0000 < MAX_USER_ADDRESS, "exec image base");
        const _: () = assert!(user_heap_base() < MAX_USER_ADDRESS);
        const _: () = assert!(user_heap_limit() <= MAX_USER_ADDRESS);
        const _: () = assert!(mmap_base() < MAX_USER_ADDRESS);
        const _: () = assert!(user_stack_base() < MAX_USER_ADDRESS);
        const _: () = assert!(vm_scratch_base() < MAX_USER_ADDRESS);
        // The old value covered the whole TTBR0 range — must not return.
        const _: () = assert!(MAX_USER_ADDRESS < 0x0000_0FFF_FFFF_FFFF);
    }
}
