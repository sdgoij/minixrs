//! x86_64 interrupt controller constants — adapted from `apicvar.h`,
//! `pic.h`, and `intr.h`
//!
//! **x86_64 differences from i386:**
//! - APIC is mandatory on x86_64 (always present)
//! - PIC (8259A) may still exist but is masked when APIC is active
//! - I/O APIC handles device interrupt routing
//! - IRQ vectors: 0x20-0x2F for PIC, 0x30-0xEF for I/O APIC

// ── Legacy 8259A PIC ────────────────────────────────────────────────────

pub const PIC_MASTER_CMD: u16 = 0x20;
pub const PIC_MASTER_DATA: u16 = 0x21;
pub const PIC_SLAVE_CMD: u16 = 0xA0;
pub const PIC_SLAVE_DATA: u16 = 0xA1;

pub const PIC_EOI: u8 = 0x20;
pub const PIC_ICW1: u8 = 0x11;
pub const PIC_ICW4: u8 = 0x01;
pub const PIC_CASCADE_ID: u8 = 0x04;
pub const PIC_MASTER_BASE: u8 = 0x20;
pub const PIC_SLAVE_BASE: u8 = 0x28;

// ── Local APIC base ─────────────────────────────────────────────────────

pub const LAPIC_DEFAULT_BASE: u64 = 0xFEE00000;

pub const LAPIC_ID: u32 = 0x020;
pub const LAPIC_VERSION: u32 = 0x030;
pub const LAPIC_TPR: u32 = 0x080;
pub const LAPIC_EOI: u32 = 0x0B0;
pub const LAPIC_SVR: u32 = 0x0F0;
pub const LAPIC_SVR_ENABLE: u32 = 0x100;

pub const LAPIC_ISR: u32 = 0x100;
pub const LAPIC_TMR: u32 = 0x180;
pub const LAPIC_IRR: u32 = 0x200;
pub const LAPIC_ESR: u32 = 0x280;
pub const LAPIC_ICR_LOW: u32 = 0x300;
pub const LAPIC_ICR_HIGH: u32 = 0x310;

pub const LAPIC_LVT_TIMER: u32 = 0x320;
pub const LAPIC_LVT_THERMAL: u32 = 0x330;
pub const LAPIC_LVT_PERFMON: u32 = 0x340;
pub const LAPIC_LVT_LINT0: u32 = 0x350;
pub const LAPIC_LVT_LINT1: u32 = 0x360;
pub const LAPIC_LVT_ERROR: u32 = 0x370;

pub const LAPIC_TIMER_ICR: u32 = 0x380;
pub const LAPIC_TIMER_CCR: u32 = 0x390;
pub const LAPIC_TIMER_DCR: u32 = 0x3E0;

// ── LVT entry bit definitions ───────────────────────────────────────────

pub const LVT_MASKED: u32 = 0x00010000;
pub const LVT_TRIGGER_LEVEL: u32 = 0x00008000;
pub const LVT_STATUS_PENDING: u32 = 0x00001000;
pub const LVT_POLARITY_LOW: u32 = 0x00002000;

pub const LVT_DELIVERY_FIXED: u32 = 0x00000000;
pub const LVT_DELIVERY_NMI: u32 = 0x00000400;
pub const LVT_DELIVERY_SMI: u32 = 0x00000200;
pub const LVT_DELIVERY_EXTINT: u32 = 0x00000700;

// ── ICR (IPI) bit definitions ───────────────────────────────────────────

pub const ICR_DELIVERY_FIXED: u64 = 0x00000000;
pub const ICR_DELIVERY_NMI: u64 = 0x00000400;
pub const ICR_DELIVERY_INIT: u64 = 0x00000500;
pub const ICR_DELIVERY_STARTUP: u64 = 0x00000600;

pub const ICR_DEST_SELF: u64 = 0x00040000;
pub const ICR_DEST_ALL: u64 = 0x00080000;
pub const ICR_DEST_ALLBUT: u64 = 0x000C0000;

// ── I/O APIC ────────────────────────────────────────────────────────────

pub const IOAPIC_DEFAULT_BASE: u64 = 0xFEC00000;
pub const IOAPIC_ID: u32 = 0x00;
pub const IOAPIC_VERSION: u32 = 0x01;
pub const IOAPIC_REDIR_TABLE: u32 = 0x10;

// ── IRQ vector layout ───────────────────────────────────────────────────

pub const VECTOR_TIMER: u8 = 0x20;
pub const VECTOR_SPURIOUS: u8 = 0xFF;

// ── IRQ hook structure ──────────────────────────────────────────────────

pub const NR_IRQ_HOOKS: u32 = 64;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct IrqHook {
    pub irq: u32,
    pub vector: u32,
    pub id: u32,
    pub policy: u32,
    pub plug: u32,
}


// ── Helper ──────────────────────────────────────────────────────────────

pub const fn irq_vector(irq: u32) -> u8 {
    (VECTOR_TIMER as u32 + irq) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pic_constants() {
        assert_eq!(PIC_MASTER_CMD, 0x20);
        assert_eq!(PIC_SLAVE_CMD, 0xA0);
    }

    #[test]
    fn test_lapic_constants() {
        assert_eq!(LAPIC_DEFAULT_BASE, 0xFEE00000);
        assert_eq!(LAPIC_SVR, 0x0F0);
        assert_eq!(LAPIC_LVT_LINT0, 0x350);
    }

    #[test]
    fn test_ioapic_constants() {
        assert_eq!(IOAPIC_DEFAULT_BASE, 0xFEC00000);
        assert_eq!(IOAPIC_VERSION, 0x01);
    }

    #[test]
    fn test_irq_vector() {
        assert_eq!(irq_vector(0), 0x20);
        assert_eq!(irq_vector(15), 0x2F);
    }
}
