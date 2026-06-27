//! Raw hardware operations for x86_64.
//!
//! Provides higher-level abstractions over the assembly primitives in
//! `asm.rs`: FPU, APIC, serial, TSC, atomics, and convenience wrappers.

use crate::asm;
use core::sync::atomic::{Ordering, fence};

pub use crate::asm::{
    hlt, inb, inl, intr_disable, intr_enable, invlpg, inw, lgdt, lidt, ltr, outb, outl, outw,
    read_cr0, read_cr2, read_cr3, read_cr4, tlb_flush, write_cr0, write_cr3, write_cr4,
};

// ── GDT/IDT/TSS read ─────────────────────────────────────

pub fn sgdt() -> (u16, u64) {
    let mut desc: [u8; 10] = [0u8; 10];
    unsafe {
        asm::sgdt(&mut desc);
    }
    let limit = u16::from_ne_bytes([desc[0], desc[1]]);
    let base = u64::from_ne_bytes([
        desc[2], desc[3], desc[4], desc[5], desc[6], desc[7], desc[8], desc[9],
    ]);
    (limit, base)
}

pub fn sidt() -> (u16, u64) {
    let mut desc: [u8; 10] = [0u8; 10];
    unsafe {
        asm::sidt(&mut desc);
    }
    let limit = u16::from_ne_bytes([desc[0], desc[1]]);
    let base = u64::from_ne_bytes([
        desc[2], desc[3], desc[4], desc[5], desc[6], desc[7], desc[8], desc[9],
    ]);
    (limit, base)
}

pub fn str() -> u16 {
    unsafe { asm::str_sel() }
}

// ── TLB ──────────────────────────────────────────────────

pub fn tlb_flush_current() {
    unsafe {
        write_cr3(read_cr3());
    }
}

pub fn tlb_flush_global() {
    unsafe {
        let cr4 = read_cr4();
        write_cr4(cr4 & !0x80);
        write_cr4(cr4 | 0x80);
    }
}

pub fn tlb_flush_page(addr: u64) {
    unsafe {
        invlpg(addr);
    }
}

// ── FPU ───────────────────────────────────────────────────

pub const FPU_SAVE_AREA_SIZE: usize = 512;

/// # Safety
/// `buf` must be a valid, 16-byte-aligned 512-byte region.
pub unsafe fn save_fpu(buf: &mut [u8; FPU_SAVE_AREA_SIZE]) {
    unsafe {
        asm::fxsave(buf);
    }
}

/// # Safety
/// `buf` must contain a valid FXSAVE image from `save_fpu`.
pub unsafe fn restore_fpu(buf: &[u8; FPU_SAVE_AREA_SIZE]) {
    unsafe {
        asm::fxrstor(buf);
    }
}

pub fn fpu_init() {
    unsafe {
        asm::fninit();
        asm::fnclex();
    }
}

// ── IDT gate builders ─────────────────────────────────────

/// 16-byte x86_64 IDT gate descriptor as (low_qword, high_qword).
pub const fn idt_gate_descriptor(
    offset: u64,
    selector: u16,
    ist: u8,
    typ: u8,
    dpl: u8,
    present: bool,
) -> (u64, u64) {
    let p = if present { 0x80u64 } else { 0x00u64 };
    let tdp = (typ as u64) | ((dpl as u64) << 5) | p;
    let low = (offset & 0xFFFF)
        | ((selector as u64) << 16)
        | ((ist as u64 & 0x07) << 32)
        | (tdp << 40)
        | ((offset >> 16) & 0xFFFF) << 48;
    let high = (offset >> 32) & 0xFFFFFFFF;
    (low, high)
}

pub const fn idt_int_gate_64(offset: u64, selector: u16, dpl: u8) -> (u64, u64) {
    idt_gate_descriptor(offset, selector, 0, 14, dpl, true)
}

pub const fn idt_trap_gate_64(offset: u64, selector: u16, dpl: u8) -> (u64, u64) {
    idt_gate_descriptor(offset, selector, 0, 15, dpl, true)
}

// ── APIC ──────────────────────────────────────────────────

const LAPIC_BASE: u64 = 0xFEE00000;

/// # Safety
/// The APIC must be initialized and mapped.
pub unsafe fn apic_read(reg: u32) -> u32 {
    let addr = (LAPIC_BASE + reg as u64) as *const u32;
    unsafe { addr.read_volatile() }
}

/// # Safety
/// The APIC must be initialized and mapped.
pub unsafe fn apic_write(reg: u32, value: u32) {
    let addr = (LAPIC_BASE + reg as u64) as *mut u32;
    unsafe {
        addr.write_volatile(value);
    }
}

/// # Safety
/// The APIC must be initialized.
pub unsafe fn apic_eoi() {
    unsafe {
        apic_write(crate::interrupt::LAPIC_EOI, 0);
    }
}

// ── PIC ───────────────────────────────────────────────────

use crate::interrupt as intr;

/// # Safety
/// The PIC must be initialized.
pub unsafe fn pic_read_irr() -> u16 {
    unsafe {
        outb(intr::PIC_MASTER_CMD, 0x0A);
        outb(intr::PIC_SLAVE_CMD, 0x0A);
        let m = inb(intr::PIC_MASTER_CMD) as u16;
        let s = inb(intr::PIC_SLAVE_CMD) as u16;
        (s << 8) | m
    }
}

/// # Safety
/// The PIC must be initialized.
pub unsafe fn pic_read_isr() -> u16 {
    unsafe {
        outb(intr::PIC_MASTER_CMD, 0x0B);
        outb(intr::PIC_SLAVE_CMD, 0x0B);
        let m = inb(intr::PIC_MASTER_CMD) as u16;
        let s = inb(intr::PIC_SLAVE_CMD) as u16;
        (s << 8) | m
    }
}

/// # Safety
/// The PIC must be initialized.
pub unsafe fn pic_eoi(irq: u8) {
    if irq >= 8 {
        unsafe {
            outb(intr::PIC_SLAVE_CMD, intr::PIC_EOI);
        }
    }
    unsafe {
        outb(intr::PIC_MASTER_CMD, intr::PIC_EOI);
    }
}

// ── Serial ────────────────────────────────────────────────

pub const COM1: u16 = 0x3F8;
pub const COM2: u16 = 0x2F8;

/// # Safety
/// `port` must be a valid serial port base.
pub unsafe fn arch_ser_init(port: u16) {
    unsafe {
        outb(port + 1, 0x00);
        outb(port + 3, 0x80);
        outb(port, 0x01);
        outb(port + 1, 0x00);
        outb(port + 3, 0x03);
        outb(port + 2, 0xC7);
        outb(port + 4, 0x0B);
    }
}

/// # Safety
/// `port` must be a valid serial port base.
pub unsafe fn ser_putc(port: u16, c: u8) {
    unsafe {
        while inb(port + 5) & 0x20 == 0 {}
        outb(port, c);
    }
}

/// # Safety
/// `port` must be a valid serial port base.
pub unsafe fn ser_getc(port: u16) -> Option<u8> {
    unsafe {
        if inb(port + 5) & 1 == 0 {
            None
        } else {
            Some(inb(port))
        }
    }
}

/// # Safety
/// `port` must be a valid serial port base.
pub unsafe fn ser_puts(port: u16, s: &[u8]) {
    for &c in s {
        unsafe {
            ser_putc(port, c);
        }
    }
}

// ── TSC ───────────────────────────────────────────────────

pub fn read_tsc() -> u64 {
    unsafe { asm::rdtsc() }
}

pub fn read_tsc_serialized() -> u64 {
    unsafe {
        let _ = asm::cpuid(0);
        asm::rdtsc()
    }
}

#[inline]
pub fn read_apic_tsc() -> u64 {
    read_tsc()
}

// ── Atomics ───────────────────────────────────────────────

pub use core::sync::atomic::AtomicU32;
pub use core::sync::atomic::AtomicU64;

pub fn atomic_fence() {
    fence(Ordering::SeqCst);
}
pub fn atomic_load_acquire(src: &AtomicU64) -> u64 {
    src.load(Ordering::Acquire)
}
pub fn atomic_store_release(dst: &AtomicU64, value: u64) {
    dst.store(value, Ordering::Release);
}

pub fn atomic_cas_64(dst: &AtomicU64, expected: u64, desired: u64) -> u64 {
    dst.compare_exchange(expected, desired, Ordering::SeqCst, Ordering::SeqCst)
        .unwrap_or_else(|x| x)
}
pub fn atomic_cas_32(dst: &AtomicU32, expected: u32, desired: u32) -> u32 {
    dst.compare_exchange(expected, desired, Ordering::SeqCst, Ordering::SeqCst)
        .unwrap_or_else(|x| x)
}
pub fn atomic_exchange_64(dst: &AtomicU64, value: u64) -> u64 {
    dst.swap(value, Ordering::SeqCst)
}
pub fn atomic_exchange_32(dst: &AtomicU32, value: u32) -> u32 {
    dst.swap(value, Ordering::SeqCst)
}
pub fn atomic_add_64(dst: &AtomicU64, value: u64) -> u64 {
    dst.fetch_add(value, Ordering::SeqCst)
}
pub fn atomic_add_32(dst: &AtomicU32, value: u32) -> u32 {
    dst.fetch_add(value, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atomic_cas_64() {
        let x = AtomicU64::new(42);
        assert_eq!(atomic_cas_64(&x, 42, 100), 42);
        assert_eq!(x.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn test_atomic_cas_32() {
        let x = AtomicU32::new(10);
        assert_eq!(atomic_cas_32(&x, 10, 20), 10);
    }

    #[test]
    fn test_atomic_exchange() {
        let x = AtomicU64::new(1);
        assert_eq!(atomic_exchange_64(&x, 2), 1);
    }

    #[test]
    fn test_atomic_add() {
        let x = AtomicU64::new(5);
        assert_eq!(atomic_add_64(&x, 3), 5);
    }

    #[test]
    fn test_idt_gate() {
        let (lo, hi) = idt_int_gate_64(0xFFFF800010002000, 0x08, 0);
        assert_eq!(lo & 0xFFFF, 0x2000);
        assert_eq!((lo >> 16) & 0xFFFF, 0x08);
        assert_eq!((lo >> 40) & 0xFF, 0x8E);
        assert_eq!(hi & 0xFFFFFFFF, 0xFFFF8000);
    }

    #[test]
    fn test_idt_trap_gate() {
        let (lo, hi) = idt_trap_gate_64(0x1000, 0x08, 3);
        assert_eq!((lo >> 40) & 0xFF, 0xEF);
        assert_eq!(hi, 0, "high qword must be 0 for 48-bit offset");
    }

    #[test]
    fn test_fpu_constants() {
        assert_eq!(FPU_SAVE_AREA_SIZE, 512);
    }
    #[test]
    fn test_serial_constants() {
        assert_eq!(COM1, 0x3F8);
        assert_eq!(COM2, 0x2F8);
    }
    #[test]
    fn test_tsc_compiles() {
        let _ = read_tsc();
    }
}
