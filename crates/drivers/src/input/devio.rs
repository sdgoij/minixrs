//! Devio-backed I/O backend for the PS/2 controller.
//!
//! Userland drivers have no direct port access; the server routes every
//! inb/outb through SYS_DEVIO (the same hook pattern as the framebuffer
//! driver's `fb_set_devio`). Host tests install a fake hook.

use core::sync::atomic::AtomicUsize;

use super::controller::IoBackend;

/// Port-I/O hook: `fn(request, port, value) -> u32` (the read value for
/// input requests). A missing hook returns 0.
pub type DevioFn = fn(u32, u16, u32) -> u32;

static DEVIO_FN: AtomicUsize = AtomicUsize::new(0);

/// Install the port-I/O hook used by the PS/2 controller on the MINIX
/// target (routed through SYS_DEVIO).
pub fn input_set_devio(hook: DevioFn) {
    DEVIO_FN.store(hook as usize, core::sync::atomic::Ordering::Relaxed);
}

/// Invoke the devio hook; a missing hook returns 0.
fn devio(request: u32, port: u16, value: u32) -> u32 {
    let raw = DEVIO_FN.load(core::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        return 0;
    }
    let hook: DevioFn = unsafe { core::mem::transmute(raw) };
    hook(request, port, value)
}

/// [`IoBackend`] that routes port I/O through the devio hook.
pub struct DevioIo;

impl IoBackend for DevioIo {
    unsafe fn inb(port: u16) -> u8 {
        devio(arch_common::com::DIO_INPUT_BYTE, port, 0) as u8
    }

    unsafe fn outb(port: u16, value: u8) {
        devio(arch_common::com::DIO_OUTPUT_BYTE, port, value as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU16;

    static TEST_PORT: AtomicU16 = AtomicU16::new(0);
    static TEST_VALUE: AtomicU16 = AtomicU16::new(0);

    fn fake_devio(request: u32, port: u16, value: u32) -> u32 {
        TEST_PORT.store(port, core::sync::atomic::Ordering::Relaxed);
        TEST_VALUE.store(value as u16, core::sync::atomic::Ordering::Relaxed);
        if request == arch_common::com::DIO_INPUT_BYTE {
            0xAB
        } else {
            0
        }
    }

    #[test]
    fn test_devio_hook_inb_outb() {
        input_set_devio(fake_devio);
        unsafe {
            assert_eq!(DevioIo::inb(0x60), 0xAB);
            assert_eq!(TEST_PORT.load(core::sync::atomic::Ordering::Relaxed), 0x60);
            DevioIo::outb(0x64, 0x20);
            assert_eq!(TEST_PORT.load(core::sync::atomic::Ordering::Relaxed), 0x64);
            assert_eq!(TEST_VALUE.load(core::sync::atomic::Ordering::Relaxed), 0x20);
        }
    }
}
