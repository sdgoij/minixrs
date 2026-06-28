//! VESA framebuffer character device driver.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/video/fb/`
//!
//! Provides `/dev/fb` with read/write to framebuffer memory and ioctls
//! for screen info queries and panning.  Hardware-specific operations
//! are delegated to the `FbArch` trait.

#![allow(clippy::new_without_default)]

use crate::DriverError;

/// Fixed screen information (immutable hardware characteristics).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FbFixScreeninfo {
    pub id: [u8; 16],
    pub xpanstep: u16,
    pub ypanstep: u16,
    pub ywrapstep: u16,
    pub line_length: u32,
    pub mmio_start: u64,
    pub mmio_len: usize,
    pub reserved: [u16; 15],
}

impl FbFixScreeninfo {
    pub const fn new() -> Self {
        Self {
            id: [0u8; 16],
            xpanstep: 0,
            ypanstep: 0,
            ywrapstep: 0,
            line_length: 0,
            mmio_start: 0,
            mmio_len: 0,
            reserved: [0u16; 15],
        }
    }
}

/// Bitfield description for a colour channel.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FbBitfield {
    pub offset: u32,
    pub length: u32,
    pub msb_right: u32,
}

impl FbBitfield {
    pub const fn new() -> Self {
        Self {
            offset: 0,
            length: 0,
            msb_right: 0,
        }
    }
}

/// Variable screen information (modifiable display parameters).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FbVarScreeninfo {
    pub xres: u32,
    pub yres: u32,
    pub xres_virtual: u32,
    pub yres_virtual: u32,
    pub xoffset: u32,
    pub yoffset: u32,
    pub bits_per_pixel: u32,
    pub red: FbBitfield,
    pub green: FbBitfield,
    pub blue: FbBitfield,
    pub transp: FbBitfield,
    pub reserved: [u16; 10],
}

impl FbVarScreeninfo {
    pub const fn new() -> Self {
        Self {
            xres: 0,
            yres: 0,
            xres_virtual: 0,
            yres_virtual: 0,
            xoffset: 0,
            yoffset: 0,
            bits_per_pixel: 32,
            red: FbBitfield::new(),
            green: FbBitfield::new(),
            blue: FbBitfield::new(),
            transp: FbBitfield::new(),
            reserved: [0u16; 10],
        }
    }
}

/// Framebuffer device descriptor.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FbDevice {
    pub base: u64,
    pub size: u64,
}

impl FbDevice {
    pub const fn new() -> Self {
        Self { base: 0, size: 0 }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// IOCTL constants (from `sys/ioc_fb.h`)
// ═══════════════════════════════════════════════════════════════════════════

pub const FBIOGET_VSCREENINFO: u32 = 0x4600;
pub const FBIOPUT_VSCREENINFO: u32 = 0x4601;
pub const FBIOGET_FSCREENINFO: u32 = 0x4602;
pub const FBIOPAN_DISPLAY: u32 = 0x4603;

// ═══════════════════════════════════════════════════════════════════════════
// Arch trait
// ═══════════════════════════════════════════════════════════════════════════

/// Architecture-specific framebuffer operations.
pub trait FbArch {
    fn init(&mut self, minor: usize) -> Result<(), DriverError>;
    fn device(&self, minor: usize) -> Result<FbDevice, DriverError>;
    fn var_screeninfo(&self, minor: usize) -> Result<FbVarScreeninfo, DriverError>;
    fn set_var_screeninfo(
        &mut self,
        minor: usize,
        info: &FbVarScreeninfo,
    ) -> Result<(), DriverError>;
    fn fix_screeninfo(&self, minor: usize) -> Result<FbFixScreeninfo, DriverError>;
    fn pan_display(&mut self, minor: usize, info: &FbVarScreeninfo) -> Result<(), DriverError>;
}

// ═══════════════════════════════════════════════════════════════════════════
// Driver
// ═══════════════════════════════════════════════════════════════════════════

pub struct Framebuffer {
    pub open_count: i32,
    pub initialized: bool,
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framebuffer {
    pub const fn new() -> Self {
        Self {
            open_count: 0,
            initialized: false,
        }
    }

    pub fn open(&mut self, _minor: usize) -> Result<(), DriverError> {
        if self.initialized {
            self.open_count += 1;
            return Ok(());
        }
        todo!("fb_open needs arch_fb_init; see PORTING_PLAN.md 12.22")
    }

    pub fn close(&mut self, _minor: usize) -> Result<(), DriverError> {
        if self.open_count > 0 {
            self.open_count -= 1;
        }
        Ok(())
    }

    pub fn read(&self, _minor: usize, _pos: u64, _buf: &mut [u8]) -> Result<usize, DriverError> {
        todo!("fb_read needs sys_safecopyto; see PORTING_PLAN.md 12.22")
    }

    pub fn write(&mut self, _minor: usize, _pos: u64, _buf: &[u8]) -> Result<usize, DriverError> {
        todo!("fb_write needs sys_safecopyfrom; see PORTING_PLAN.md 12.22")
    }

    pub fn ioctl(&mut self, _minor: usize, _request: u32) -> Result<(), DriverError> {
        todo!("fb_ioctl needs sys_safecopy; see PORTING_PLAN.md 12.22")
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_types_new() {
        let fix = FbFixScreeninfo::new();
        assert_eq!(fix.mmio_len, 0);
        let bf = FbBitfield::new();
        assert_eq!(bf.offset, 0);
        let var = FbVarScreeninfo::new();
        assert_eq!(var.bits_per_pixel, 32);
        let dev = FbDevice::new();
        assert_eq!(dev.base, 0);
    }

    #[test]
    fn test_ioctl_constants() {
        assert_eq!(FBIOGET_VSCREENINFO, 0x4600);
        assert_eq!(FBIOPUT_VSCREENINFO, 0x4601);
        assert_eq!(FBIOGET_FSCREENINFO, 0x4602);
        assert_eq!(FBIOPAN_DISPLAY, 0x4603);
    }

    #[test]
    fn test_open_close() {
        let mut fb = Framebuffer::new();
        fb.open_count = 1;
        assert!(fb.close(0).is_ok());
        assert_eq!(fb.open_count, 0);
    }

    #[test]
    fn test_type_sizes() {
        // Verify all types have reasonable sizes with repr(C) layout.
        // Exact sizes differ between platforms due to usize width.
        assert_eq!(size_of::<FbBitfield>(), 12);
        assert!(size_of::<FbVarScreeninfo>() >= 96);
        assert!(size_of::<FbVarScreeninfo>() <= 128);
        assert!(size_of::<FbFixScreeninfo>() >= 60);
        assert!(size_of::<FbFixScreeninfo>() <= 128);
    }

    #[test]
    fn test_fb_state_new() {
        let s = Framebuffer::new();
        assert_eq!(s.open_count, 0);
        assert!(!s.initialized);
    }

    #[test]
    fn test_var_screeninfo_reserved() {
        let var = FbVarScreeninfo::new();
        assert_eq!(var.reserved.len(), 10);
    }

    #[test]
    fn test_fix_screeninfo_reserved() {
        let fix = FbFixScreeninfo::new();
        assert_eq!(fix.reserved.len(), 15);
    }
}
