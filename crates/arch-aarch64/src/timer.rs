//! AArch64 Generic Timer (CNTP) interface.
//!
//! Uses the CNTP_CVAL_EL0 and CNTP_CTL_EL0 system registers
//! to configure periodic timer interrupts. Timer acknowledgement
//! is via system registers — no GIC MMIO access needed.

/// Timer frequency in Hz (QEMU virt default).
const TIMER_FREQ_HZ: u64 = 62_500_000;

/// Scheduler tick rate.
const TICK_HZ: u64 = 100;

/// Number of timer cycles between ticks.
const CYCLES_PER_TICK: u64 = TIMER_FREQ_HZ / TICK_HZ;

/// Initialize the generic timer for periodic interrupts.
///
/// # Safety
///
/// Must be called before enabling timer interrupts.
pub unsafe fn init_timer() {
    unsafe {
        // Disable and mask.
        core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) 0u64);

        let current: u64;
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) current);
        core::arch::asm!("msr cntp_cval_el0, {}", in(reg) current + CYCLES_PER_TICK);

        // Enable, unmask.
        core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) 1u64);
    }
}

/// Acknowledge the timer IRQ and reprogram for the next tick.
/// Uses system registers only — no GIC access, so it works
/// even when device MMIO isn't mapped in the current page table.
pub unsafe fn timer_irq_ack() {
    unsafe {
        core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) 0u64);

        let current: u64;
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) current);
        core::arch::asm!("msr cntp_cval_el0, {}", in(reg) current + CYCLES_PER_TICK);

        core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) 1u64);
    }
}
