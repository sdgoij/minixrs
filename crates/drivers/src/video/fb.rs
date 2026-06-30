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

// IOCTL constants (from `sys/ioc_fb.h`)

pub const FBIOGET_VSCREENINFO: u32 = 0x4600;
pub const FBIOPUT_VSCREENINFO: u32 = 0x4601;
pub const FBIOGET_FSCREENINFO: u32 = 0x4602;
pub const FBIOPAN_DISPLAY: u32 = 0x4603;

// Arch trait

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

// NullArch — test backend

/// No-op architecture backend for testing.
///
/// Uses an internal 4 KB buffer as fake framebuffer memory so that
/// read/write tests can verify data round-trips through the driver.
pub struct NullArch {
    pub dev: FbDevice,
    pub var: FbVarScreeninfo,
    pub fix: FbFixScreeninfo,
    pub mem: [u8; 4096],
}

impl NullArch {
    pub fn new() -> Self {
        Self {
            dev: FbDevice {
                base: 0,
                size: 4096,
            },
            var: FbVarScreeninfo::new(),
            fix: FbFixScreeninfo::new(),
            mem: [0u8; 4096],
        }
    }
}

impl Default for NullArch {
    fn default() -> Self {
        Self::new()
    }
}

impl FbArch for NullArch {
    fn init(&mut self, _minor: usize) -> Result<(), DriverError> {
        Ok(())
    }

    fn device(&self, _minor: usize) -> Result<FbDevice, DriverError> {
        Ok(FbDevice {
            base: self.mem.as_ptr() as u64,
            size: 4096,
        })
    }

    fn var_screeninfo(&self, _minor: usize) -> Result<FbVarScreeninfo, DriverError> {
        Ok(self.var)
    }

    fn set_var_screeninfo(
        &mut self,
        _minor: usize,
        info: &FbVarScreeninfo,
    ) -> Result<(), DriverError> {
        self.var = *info;
        Ok(())
    }

    fn fix_screeninfo(&self, _minor: usize) -> Result<FbFixScreeninfo, DriverError> {
        Ok(self.fix)
    }

    fn pan_display(&mut self, _minor: usize, info: &FbVarScreeninfo) -> Result<(), DriverError> {
        self.var.xoffset = info.xoffset;
        self.var.yoffset = info.yoffset;
        Ok(())
    }
}

// Driver

/// Framebuffer character device driver.
///
/// Reads and writes from/to framebuffer memory using volatile pointer
/// access through the arch backend's `device()` descriptor.
/// Ioctls are dispatched to the corresponding `FbArch` methods.
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

    pub fn open(&mut self, minor: usize, arch: &mut dyn FbArch) -> Result<(), DriverError> {
        if self.initialized {
            self.open_count += 1;
            return Ok(());
        }
        arch.init(minor)?;
        self.initialized = true;
        self.open_count = 1;
        Ok(())
    }

    pub fn close(&mut self, _minor: usize) -> Result<(), DriverError> {
        if self.open_count > 0 {
            self.open_count -= 1;
        }
        Ok(())
    }

    /// Read bytes from framebuffer memory starting at `pos`.
    ///
    /// Uses volatile reads at the device's base address.  The actual
    /// comparison is done on the host side, so on real hardware this
    /// performs the MMIO read correctly.
    pub fn read(
        &self,
        minor: usize,
        pos: u64,
        buf: &mut [u8],
        arch: &dyn FbArch,
    ) -> Result<usize, DriverError> {
        let dev = arch.device(minor)?;
        if pos >= dev.size {
            return Ok(0);
        }
        let avail = (dev.size - pos) as usize;
        let n = buf.len().min(avail);
        let src = (dev.base + pos) as *const u8;
        for (i, dst) in buf.iter_mut().enumerate().take(n) {
            // Safety: we trust the arch backend to provide a valid
            // framebuffer address.  The address range is within dev.size.
            *dst = unsafe { core::ptr::read_volatile(src.add(i)) };
        }
        Ok(n)
    }

    /// Write bytes to framebuffer memory starting at `pos`.
    ///
    /// Uses volatile writes at the device's base address.
    pub fn write(
        &mut self,
        minor: usize,
        pos: u64,
        buf: &[u8],
        arch: &dyn FbArch,
    ) -> Result<usize, DriverError> {
        let dev = arch.device(minor)?;
        if pos >= dev.size {
            return Ok(0);
        }
        let avail = (dev.size - pos) as usize;
        let n = buf.len().min(avail);
        let dst = (dev.base + pos) as *mut u8;
        for (i, &val) in buf.iter().enumerate().take(n) {
            // Safety: same as read — address validated via arch.device().
            unsafe { core::ptr::write_volatile(dst.add(i), val) };
        }
        Ok(n)
    }

    /// Perform a framebuffer ioctl.
    ///
    /// `data` is a byte buffer that may hold a struct (GET fills it,
    /// PUT reads from it).  Returns an error if the buffer is too
    /// small for the requested struct.
    pub fn ioctl(
        &mut self,
        minor: usize,
        request: u32,
        data: &mut [u8],
        arch: &mut dyn FbArch,
    ) -> Result<(), DriverError> {
        match request {
            FBIOGET_VSCREENINFO => {
                let var = arch.var_screeninfo(minor)?;
                let bytes = unsafe {
                    core::slice::from_raw_parts(
                        &var as *const FbVarScreeninfo as *const u8,
                        core::mem::size_of::<FbVarScreeninfo>(),
                    )
                };
                if data.len() < bytes.len() {
                    return Err(DriverError::Io);
                }
                data[..bytes.len()].copy_from_slice(bytes);
                Ok(())
            }
            FBIOPUT_VSCREENINFO => {
                let size = core::mem::size_of::<FbVarScreeninfo>();
                if data.len() < size {
                    return Err(DriverError::Io);
                }
                let info: FbVarScreeninfo =
                    unsafe { core::ptr::read(data.as_ptr() as *const FbVarScreeninfo) };
                arch.set_var_screeninfo(minor, &info)
            }
            FBIOGET_FSCREENINFO => {
                let fix = arch.fix_screeninfo(minor)?;
                let bytes = unsafe {
                    core::slice::from_raw_parts(
                        &fix as *const FbFixScreeninfo as *const u8,
                        core::mem::size_of::<FbFixScreeninfo>(),
                    )
                };
                if data.len() < bytes.len() {
                    return Err(DriverError::Io);
                }
                data[..bytes.len()].copy_from_slice(bytes);
                Ok(())
            }
            FBIOPAN_DISPLAY => {
                let size = core::mem::size_of::<FbVarScreeninfo>();
                if data.len() < size {
                    return Err(DriverError::Io);
                }
                let info: FbVarScreeninfo =
                    unsafe { core::ptr::read(data.as_ptr() as *const FbVarScreeninfo) };
                arch.pan_display(minor, &info)
            }
            _ => Err(DriverError::InvalidArgument),
        }
    }
}

// Tests

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
        let mut arch = NullArch::new();
        assert!(fb.open(0, &mut arch).is_ok());
        assert_eq!(fb.open_count, 1);
        assert!(fb.initialized);
        assert!(fb.close(0).is_ok());
        assert_eq!(fb.open_count, 0);
    }

    #[test]
    fn test_open_calls_arch_init() {
        let mut fb = Framebuffer::new();
        let mut arch = NullArch::new();
        assert!(fb.open(0, &mut arch).is_ok());
        // Second open should not re-init
        assert!(fb.open(0, &mut arch).is_ok());
        assert_eq!(fb.open_count, 2);
    }

    #[test]
    fn test_read_past_end_returns_zero() {
        let fb = Framebuffer::new();
        let arch = NullArch::new();
        let mut buf = [0u8; 4];
        let n = fb.read(0, 5000, &mut buf, &arch).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_write_then_read_roundtrip() {
        let mut fb = Framebuffer::new();
        let arch = NullArch::new();
        let write_data = [0xAA, 0xBB, 0xCC, 0xDD];
        let n = fb.write(0, 0, &write_data, &arch).unwrap();
        assert_eq!(n, 4);
        // Verify via the arch's internal buffer
        assert_eq!(&arch.mem[..4], &write_data);
        // Read back via the driver
        let mut read_buf = [0u8; 4];
        let n = fb.read(0, 0, &mut read_buf, &arch).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&read_buf, &write_data);
    }

    #[test]
    fn test_write_clamps_to_device_size() {
        let mut fb = Framebuffer::new();
        let arch = NullArch::new();
        // Write past end of NullArch's 4096-byte buffer
        let write_data = [0xFFu8; 100];
        let n = fb.write(0, 4000, &write_data, &arch).unwrap();
        assert_eq!(n, 96); // only 96 bytes fit (4096 - 4000)
    }

    #[test]
    fn test_read_clamps_to_device_size() {
        let fb = Framebuffer::new();
        let arch = NullArch::new();
        let mut buf = [0u8; 200];
        let n = fb.read(0, 4000, &mut buf, &arch).unwrap();
        assert_eq!(n, 96);
    }

    #[test]
    fn test_ioctl_get_var_screeninfo() {
        let mut fb = Framebuffer::new();
        let mut arch = NullArch::new();
        arch.var.xres = 1024;
        arch.var.yres = 768;
        let mut data = [0u8; 128];
        assert!(
            fb.ioctl(
                0,
                FBIOGET_VSCREENINFO,
                &mut data[..size_of::<FbVarScreeninfo>()],
                &mut arch
            )
            .is_ok()
        );
        let info: FbVarScreeninfo =
            unsafe { core::ptr::read(data.as_ptr() as *const FbVarScreeninfo) };
        assert_eq!(info.xres, 1024);
        assert_eq!(info.yres, 768);
    }

    #[test]
    fn test_ioctl_put_var_screeninfo() {
        let mut fb = Framebuffer::new();
        let mut arch = NullArch::new();
        let mut info = FbVarScreeninfo::new();
        info.xres = 800;
        info.yres = 600;
        let info_bytes = unsafe {
            core::slice::from_raw_parts(
                &info as *const FbVarScreeninfo as *const u8,
                size_of::<FbVarScreeninfo>(),
            )
        };
        let mut data = [0u8; 128];
        data[..info_bytes.len()].copy_from_slice(info_bytes);
        assert!(
            fb.ioctl(
                0,
                FBIOPUT_VSCREENINFO,
                &mut data[..size_of::<FbVarScreeninfo>()],
                &mut arch
            )
            .is_ok()
        );
        assert_eq!(arch.var.xres, 800);
        assert_eq!(arch.var.yres, 600);
    }

    #[test]
    fn test_ioctl_get_fix_screeninfo() {
        let mut fb = Framebuffer::new();
        let mut arch = NullArch::new();
        arch.fix.line_length = 640;
        let mut data = [0u8; 128];
        assert!(
            fb.ioctl(
                0,
                FBIOGET_FSCREENINFO,
                &mut data[..size_of::<FbFixScreeninfo>()],
                &mut arch
            )
            .is_ok()
        );
        let fix: FbFixScreeninfo =
            unsafe { core::ptr::read(data.as_ptr() as *const FbFixScreeninfo) };
        assert_eq!(fix.line_length, 640);
    }

    #[test]
    fn test_ioctl_pan_display() {
        let mut fb = Framebuffer::new();
        let mut arch = NullArch::new();
        let mut info = FbVarScreeninfo::new();
        info.xoffset = 10;
        info.yoffset = 20;
        let info_bytes = unsafe {
            core::slice::from_raw_parts(
                &info as *const FbVarScreeninfo as *const u8,
                size_of::<FbVarScreeninfo>(),
            )
        };
        let mut data = [0u8; 128];
        data[..info_bytes.len()].copy_from_slice(info_bytes);
        assert!(
            fb.ioctl(
                0,
                FBIOPAN_DISPLAY,
                &mut data[..size_of::<FbVarScreeninfo>()],
                &mut arch
            )
            .is_ok()
        );
        assert_eq!(arch.var.xoffset, 10);
        assert_eq!(arch.var.yoffset, 20);
    }

    #[test]
    fn test_ioctl_unknown_request_returns_error() {
        let mut fb = Framebuffer::new();
        let mut arch = NullArch::new();
        let mut data = [0u8; 4];
        assert!(fb.ioctl(0, 0x9999, &mut data, &mut arch).is_err());
    }

    #[test]
    fn test_ioctl_buffer_too_small_returns_error() {
        let mut fb = Framebuffer::new();
        let mut arch = NullArch::new();
        let mut data = [0u8; 4];
        assert!(
            fb.ioctl(0, FBIOGET_VSCREENINFO, &mut data, &mut arch)
                .is_err()
        );
    }

    #[test]
    fn test_type_sizes() {
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

    #[test]
    fn test_null_arch_new() {
        let arch = NullArch::new();
        assert_eq!(arch.dev.size, 4096);
    }

    #[test]
    fn test_null_arch_device() {
        let arch = NullArch::new();
        let dev = arch.device(0).unwrap();
        assert_eq!(dev.base, arch.mem.as_ptr() as u64);
        assert_eq!(dev.size, 4096);
    }

    #[test]
    fn test_null_arch_init() {
        let mut arch = NullArch::new();
        assert!(arch.init(0).is_ok());
    }

    #[test]
    fn test_write_zero_length() {
        let mut fb = Framebuffer::new();
        let arch = NullArch::new();
        let n = fb.write(0, 0, &[], &arch).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_read_zero_length() {
        let fb = Framebuffer::new();
        let arch = NullArch::new();
        let n = fb.read(0, 0, &mut [], &arch).unwrap();
        assert_eq!(n, 0);
    }
}
