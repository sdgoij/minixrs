//! RISC-V64 PLIC (Platform-Level Interrupt Controller) driver.
//!
//! The PLIC connects external interrupt sources (UART, virtio, etc.)
//! to S-mode external interrupts (scause 9).
//!
//! QEMU virt PLIC memory map:
//!   0x0C000000: Priority registers (1 per IRQ, 4 bytes each, IRQs 1-53)
//!   0x0C002000: Enable bits for hart 0 (2 × 4 bytes for 53 IRQs)
//!   0x0C200000: Threshold for hart 0 (4 bytes)
//!   0x0C200004: Claim/Complete for hart 0 (4 bytes)

#![cfg(target_arch = "riscv64")]

use core::ptr::{read_volatile, write_volatile};

/// PLIC base address on QEMU virt.
pub const PLIC_BASE: u64 = 0x0C000000;

/// Enable bit offset for hart 0 (S-mode).
pub const PLIC_ENABLE_HART0: u64 = PLIC_BASE + 0x2000 + 0x80; // S-mode context
/// Claim/Complete register for hart 0 (S-mode).
pub const PLIC_CLAIM_HART0: u64 = PLIC_BASE + 0x200000 + 0x1F0000 + 0x04; // S-mode claim
/// Threshold register for hart 0 (S-mode).
pub const PLIC_THRESHOLD_HART0: u64 = PLIC_BASE + 0x200000 + 0x1F0000; // S-mode threshold

/// UART IRQ number on QEMU virt.
pub const UART_IRQ: u32 = 10;

/// Read a 32-bit MMIO register at the given offset from PLIC base.
unsafe fn plic_read(offset: u64) -> u32 {
    unsafe { read_volatile((PLIC_BASE + offset) as *const u32) }
}

/// Write a 32-bit MMIO register at the given offset from PLIC base.
unsafe fn plic_write(offset: u64, val: u32) {
    unsafe { write_volatile((PLIC_BASE + offset) as *mut u32, val) }
}

/// Initialize the PLIC for single-hart (hart 0, S-mode).
///
/// Sets the threshold to 0 (accept all IRQs) and disables all.
///
/// # Safety
///
/// Must be called once during boot, before enabling external interrupts.
pub unsafe fn init_plic() {
    // Set threshold to 0 (accept all priorities)
    plic_write(PLIC_THRESHOLD_HART0 - PLIC_BASE, 0);

    // Disable all IRQs initially (write 0 to both enable words)
    plic_write(PLIC_ENABLE_HART0 - PLIC_BASE, 0);
    plic_write(PLIC_ENABLE_HART0 - PLIC_BASE + 4, 0);
}

/// Enable a specific IRQ for hart 0 (S-mode).
///
/// Also sets the priority to 1 (lowest non-zero priority).
///
/// # Safety
///
/// Must be called after `init_plic()`.
pub unsafe fn enable_irq(irq: u32) {
    if irq == 0 || irq > 53 {
        return;
    }

    // Set priority to 1 (non-zero = enabled; 0 = disabled)
    plic_write((irq as u64) * 4, 1);

    // Set enable bit
    let enable_reg = if irq < 32 { 0u64 } else { 4u64 };
    let bit = (irq % 32) as u32;
    let old = plic_read(PLIC_ENABLE_HART0 - PLIC_BASE + enable_reg);
    plic_write(
        PLIC_ENABLE_HART0 - PLIC_BASE + enable_reg,
        old | (1u32 << bit),
    );
}

/// Disable a specific IRQ for hart 0 (S-mode).
///
/// # Safety
///
/// Must be called after `init_plic()`.
pub unsafe fn disable_irq(irq: u32) {
    if irq == 0 || irq > 53 {
        return;
    }
    let enable_reg = if irq < 32 { 0u64 } else { 4u64 };
    let bit = (irq % 32) as u32;
    let old = plic_read(PLIC_ENABLE_HART0 - PLIC_BASE + enable_reg);
    plic_write(
        PLIC_ENABLE_HART0 - PLIC_BASE + enable_reg,
        old & !(1u32 << bit),
    );
}

/// Claim the highest-priority pending IRQ.
///
/// Returns the IRQ number, or 0 if none pending.
///
/// # Safety
///
/// Must be called from the external interrupt handler.
pub unsafe fn claim_irq() -> u32 {
    plic_read(PLIC_CLAIM_HART0 - PLIC_BASE)
}

/// Complete (acknowledge) an IRQ after handling.
///
/// # Safety
///
/// Must be called after handling the IRQ returned by `claim_irq()`.
pub unsafe fn complete_irq(irq: u32) {
    plic_write(PLIC_CLAIM_HART0 - PLIC_BASE, irq);
}

/// Check if an IRQ is pending (debug/status).
///
/// # Safety
///
/// MMIO access must be safe.
pub unsafe fn irq_pending(irq: u32) -> bool {
    if irq == 0 || irq > 53 {
        return false;
    }
    // Pending bits are at PLIC_BASE + 0x1000 (first pending word)
    let pending_reg = if irq < 32 { 0u64 } else { 4u64 };
    let bit = (irq % 32) as u32;
    let pending = plic_read(0x1000 + pending_reg);
    (pending & (1u32 << bit)) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plic_constants() {
        assert_eq!(PLIC_BASE, 0x0C000000);
        assert_eq!(UART_IRQ, 10);
    }

    #[test]
    fn test_irq_ranges() {
        // IRQ 0 is invalid (no interrupt 0)
        // IRQs 1-53 are valid on QEMU virt
        assert!(UART_IRQ >= 1 && UART_IRQ <= 53);
    }
}
