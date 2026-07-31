//! AArch64-specific kernel code.
//!
//! ARMv8-A 64-bit: 4KB granule, 4-level paging (PGD→PUD→PMD→PT),
//! exception levels EL1 (kernel) and EL0 (user), GICv2 interrupt
//! controller, PL011 UART, Generic Timer.

#![no_std]

#[cfg(target_arch = "aarch64")]
use core::sync::atomic::AtomicU64;

/// Boot page table root physical address.
#[cfg(target_arch = "aarch64")]
pub static BOOT_CR3: AtomicU64 = AtomicU64::new(0);

pub mod alloc;
#[cfg(target_arch = "aarch64")]
pub mod cpulocals;
#[cfg(target_arch = "aarch64")]
pub mod exception;
pub mod fork;
pub mod frame;
#[cfg(target_arch = "aarch64")]
pub mod hal;
pub mod mcontext;
pub mod param;
pub mod psl;
pub mod pte;
#[cfg(target_arch = "aarch64")]
pub mod switch;
#[cfg(target_arch = "aarch64")]
pub mod timer;
pub mod vmparam;

/// Initialize AArch64 architecture subsystem.
#[cfg(target_arch = "aarch64")]
pub fn init() {
    // Enable FP/SIMD at EL1 and EL0 (disabled by default at reset).
    unsafe {
        core::arch::asm!("msr cpacr_el1, {val}", val = in(reg) (3u64 << 20), options(nomem, nostack));
    }

    // Set up exception vector table (VBAR_EL1).
    let vbar = crate::exception::vector_table_addr();
    unsafe {
        core::arch::asm!("msr vbar_el1, {vbar}", vbar = in(reg) vbar, options(nomem, nostack));
    }

    // Configure MAIR_EL1 for memory attributes.
    // Attr0 = 0xFF (Normal memory, Inner/Outer WB/WA)
    // Attr1 = 0x44 (Device memory, nGnRE)
    let mair: u64 = 0xFF | (0x44 << 8);
    unsafe {
        core::arch::asm!("msr mair_el1, {mair}", mair = in(reg) mair, options(nomem, nostack));
    }

    // Configure TCR_EL1 for 4KB granule, 48-bit VA, both TTBR0 and TTBR1.
    let tcr: u64 = 16u64           // T0SZ = 16 (48-bit VA)
        | (16u64 << 16)            // T1SZ = 16
        | (1u64 << 23)             // EPD1 = 1 (disable TTBR1 walks)
        | (0b10u64 << 30)          // TG1 = 2 (4KB for TTBR1)
        | (0b11u64 << 12)          // SH0 = Inner Shareable
        | (0b11u64 << 28)          // SH1 = Inner Shareable
        | (0b01u64 << 10)          // ORGN0 = Normal WB/WA (walker uses cache)
        | (0b01u64 << 8)           // IRGN0 = Normal WB/WA (walker uses cache)
        | (0b01u64 << 26)          // ORGN1 = Normal WB/WA
        | (0b01u64 << 24)          // IRGN1 = Normal WB/WA
        | (4u64 << 32); // IPS = 40-bit physical address
    unsafe {
        core::arch::asm!("msr tcr_el1, {tcr}", tcr = in(reg) tcr, options(nomem, nostack));
    }

    // Configure SCTLR_EL1: enable caches, disable MMU for now.
    unsafe {
        let mut sctlr: u64;
        core::arch::asm!("mrs {sctlr}, sctlr_el1", sctlr = out(reg) sctlr);
        sctlr |= (1 << 2) | (1 << 12) | (1 << 3); // C, I, SA
        sctlr &= !((1 << 25) | (1 << 19) | (1 << 1) | (1 << 0)); // Clear EE, WXN, A, M
        sctlr |= 1 << 14; // DZE
        core::arch::asm!("msr sctlr_el1, {sctlr}", sctlr = in(reg) sctlr, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }

    // Initialize PL011 UART.
    hal::uart_init();
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let _ = 0;
    }
}
