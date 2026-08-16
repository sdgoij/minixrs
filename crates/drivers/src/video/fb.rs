//! VESA framebuffer character device driver.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/video/fb/`
//!
//! Provides `/dev/fb` with read/write to framebuffer memory and ioctls
//! for screen info queries and panning.  Hardware-specific operations
//! are delegated to the `FbArch` trait.

#![allow(clippy::new_without_default)]

use core::sync::atomic::AtomicUsize;

use crate::DriverError;

pub use crate::video::virtio_gpu::VirtioGpuArch;

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
/// Push the framebuffer to the display (no-op on VGA-style devices whose
/// LFB is always scanned out; virtio-gpu needs an explicit RESOURCE_FLUSH).
pub const FBIOFLUSH: u32 = 0x4604;

// Arch trait

/// Architecture-specific framebuffer operations.
pub trait FbArch {
    fn init(&mut self, minor: usize) -> Result<(), DriverError>;
    fn device(&self, minor: usize) -> Result<FbDevice, DriverError>;
    /// Address the driver reads/writes framebuffer bytes through. For
    /// device-memory backends this is the identity-mapped device VA (the
    /// same as `device().base`); for RAM-backed backends (virtio-gpu) it
    /// is the buffer's image VA while `device().base` is the guest-physical
    /// address handed to VFS for mmap.
    fn mem(&self, minor: usize) -> Result<u64, DriverError> {
        self.device(minor).map(|d| d.base)
    }
    fn var_screeninfo(&self, minor: usize) -> Result<FbVarScreeninfo, DriverError>;
    fn set_var_screeninfo(
        &mut self,
        minor: usize,
        info: &FbVarScreeninfo,
    ) -> Result<(), DriverError>;
    fn fix_screeninfo(&self, minor: usize) -> Result<FbFixScreeninfo, DriverError>;
    fn pan_display(&mut self, minor: usize, info: &FbVarScreeninfo) -> Result<(), DriverError>;
    /// Push framebuffer contents to the display. Default: nothing to do
    /// (VGA-style LFBs are scanned out continuously).
    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
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

// Bochs VBE backend — QEMU bochs-display / std VGA

/// QEMU bochs-display PCI vendor/device IDs (`-device bochs-display`).
pub const BOCHS_PCI_VENDOR: u16 = 0x1234;
pub const BOCHS_PCI_DEVICE: u16 = 0x1111;

/// Default mode-set resolution.
pub const BOCHS_DEFAULT_XRES: u32 = 1024;
pub const BOCHS_DEFAULT_YRES: u32 = 768;

// VBE dispi register indices (Bochs VBE spec).
const VBE_DISPI_INDEX_XRES: u16 = 0x1;
const VBE_DISPI_INDEX_YRES: u16 = 0x2;
const VBE_DISPI_INDEX_BPP: u16 = 0x3;
const VBE_DISPI_INDEX_ENABLE: u16 = 0x4;

const VBE_DISPI_ENABLED: u16 = 0x01;
const VBE_DISPI_LFB_ENABLED: u16 = 0x40;

/// Legacy VBE register I/O ports (`-device VGA` fallback).
const VBE_DISPI_IOPORT_INDEX: u16 = 0x1CE;
const VBE_DISPI_IOPORT_DATA: u16 = 0x1CF;

/// bochs-display places the VBE register file at MMIO offset 0x500 of the
/// register BAR (QEMU `PCI_VGA_BOCHS_OFFSET`); offset 0 of the BAR holds
/// the EDID blob. Each register is 16-bit at `idx * 2` within that region.
const VBE_DISPI_MMIO_REG_OFFSET: u64 = 0x500;

/// PCI config space ports.
const PCI_ADDR_PORT: u16 = 0xCF8;
const PCI_DATA_PORT: u16 = 0xCFC;

/// Legacy VBE LFB physical address once the LFB is enabled over ports.
const VBE_LFB_PHYS: u64 = 0xE000_0000;
/// Legacy LFB size assumed when the VRAM-size register reads zero.
const VBE_LFB_DEFAULT_SIZE: u64 = 4 * 1024 * 1024;

/// Port-I/O hook (the fb server routes this through SYS_DEVIO on MINIX;
/// host tests install a fake register file).
pub type DevioFn = fn(request: u32, port: u16, value: u32) -> u32;

/// Physical-memory mapping hook: map `phys..phys+len` and return the VA
/// (the fb server routes this through VM_MAP_PHYS on MINIX, which maps
/// identity; host tests return a fake buffer).
pub type PhysMapFn = fn(phys: u64, len: usize) -> u64;

static DEVIO_FN: AtomicUsize = AtomicUsize::new(0);
static PHYSMAP_FN: AtomicUsize = AtomicUsize::new(0);

/// Install the port-I/O hook used by the PCI probe and the legacy VBE
/// register path.
pub fn fb_set_devio(hook: DevioFn) {
    DEVIO_FN.store(hook as usize, core::sync::atomic::Ordering::Relaxed);
}

/// Install the physical-memory mapping hook.
pub fn fb_set_physmap(hook: PhysMapFn) {
    PHYSMAP_FN.store(hook as usize, core::sync::atomic::Ordering::Relaxed);
}

/// Invoke the port-I/O hook; a missing hook returns 0.
unsafe fn devio(request: u32, port: u16, value: u32) -> u32 {
    let raw = DEVIO_FN.load(core::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        return 0;
    }
    let hook: DevioFn = unsafe { core::mem::transmute(raw) };
    hook(request, port, value)
}

/// Invoke the physmap hook; a missing hook returns 0 (failure).
unsafe fn physmap(phys: u64, len: usize) -> u64 {
    let raw = PHYSMAP_FN.load(core::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        return 0;
    }
    let hook: PhysMapFn = unsafe { core::mem::transmute(raw) };
    hook(phys, len)
}

/// QEMU Bochs VBE framebuffer backend.
///
/// Two access paths, chosen in `init`:
/// - PCI `1234:1111` (`-device bochs-display`): the framebuffer and the
///   VBE register file live in separate memory BARs, mapped via the
///   physmap hook; registers are a direct 16-bit array — register `idx`
///   at byte offset `idx * 2` of the register BAR.
/// - No such device (`-device VGA`): the classic VBE registers on I/O
///   ports `0x1CE`/`0x1CF`; the LFB appears at `0xE0000000` once enabled.
#[repr(C)]
pub struct BochsArch {
    /// Framebuffer VA + size.
    pub dev: FbDevice,
    /// MMIO register BAR VA (0 → legacy I/O port registers).
    pub regs: u64,
    pub var: FbVarScreeninfo,
    pub fix: FbFixScreeninfo,
}

impl BochsArch {
    pub const fn new() -> Self {
        Self {
            dev: FbDevice::new(),
            regs: 0,
            var: FbVarScreeninfo::new(),
            fix: FbFixScreeninfo::new(),
        }
    }

    /// Write VBE register `idx` = `val`.
    ///
    /// Ports: the classic index/data pair at 0x1CE/0x1CF. MMIO
    /// (bochs-display): the register file is a direct array — register
    /// `idx` at byte offset `idx * 2`, 16-bit.
    unsafe fn write_reg(&self, idx: u16, val: u16) {
        if self.regs != 0 {
            unsafe {
                core::ptr::write_volatile((self.regs + (idx as u64) * 2) as *mut u16, val);
            }
        } else {
            unsafe {
                devio(
                    arch_common::com::DIO_OUTPUT_WORD,
                    VBE_DISPI_IOPORT_INDEX,
                    idx as u32,
                );
                devio(
                    arch_common::com::DIO_OUTPUT_WORD,
                    VBE_DISPI_IOPORT_DATA,
                    val as u32,
                );
            }
        }
    }

    /// Read VBE register `idx`.
    unsafe fn read_reg(&self, idx: u16) -> u16 {
        if self.regs != 0 {
            unsafe { core::ptr::read_volatile((self.regs + (idx as u64) * 2) as *const u16) }
        } else {
            unsafe {
                devio(
                    arch_common::com::DIO_OUTPUT_WORD,
                    VBE_DISPI_IOPORT_INDEX,
                    idx as u32,
                );
                devio(arch_common::com::DIO_INPUT_WORD, VBE_DISPI_IOPORT_DATA, 0) as u16
            }
        }
    }

    /// Read a 32-bit PCI config register via the 0xCF8/0xCFC ports.
    unsafe fn pci_cfg_read32(&self, bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
        let addr = 0x8000_0000u32
            | ((bus as u32) << 16)
            | ((dev as u32) << 11)
            | ((func as u32) << 8)
            | ((reg as u32) & 0xFC);
        unsafe {
            devio(arch_common::com::DIO_OUTPUT_LONG, PCI_ADDR_PORT, addr);
            devio(arch_common::com::DIO_INPUT_LONG, PCI_DATA_PORT, 0)
        }
    }

    unsafe fn pci_cfg_read16(&self, bus: u8, dev: u8, func: u8, reg: u8) -> u16 {
        let raw = unsafe { self.pci_cfg_read32(bus, dev, func, reg) };
        ((raw >> (((reg as u32) & 0x02) * 8)) & 0xFFFF) as u16
    }

    /// Run the mode-set dance and refresh `var`/`fix` from the device.
    unsafe fn mode_set(&mut self) -> Result<(), DriverError> {
        unsafe {
            self.write_reg(VBE_DISPI_INDEX_ENABLE, 0);
            self.write_reg(VBE_DISPI_INDEX_XRES, BOCHS_DEFAULT_XRES as u16);
            self.write_reg(VBE_DISPI_INDEX_YRES, BOCHS_DEFAULT_YRES as u16);
            self.write_reg(VBE_DISPI_INDEX_BPP, 32);
            self.write_reg(
                VBE_DISPI_INDEX_ENABLE,
                VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED,
            );
            // The XRES readback confirms the register path is live (the
            // MMIO layout of the register BAR is QEMU-version-dependent).
            if self.read_reg(VBE_DISPI_INDEX_XRES) != BOCHS_DEFAULT_XRES as u16 {
                return Err(DriverError::NotFound);
            }
        }
        Ok(())
    }
}

impl Default for BochsArch {
    fn default() -> Self {
        Self::new()
    }
}

impl FbArch for BochsArch {
    fn init(&mut self, _minor: usize) -> Result<(), DriverError> {
        // Probe bus 0 for the bochs-display device.
        let mut found = false;
        'probe: for dev in 0..32u8 {
            for func in 0..8u8 {
                let vendor = unsafe { self.pci_cfg_read16(0, dev, func, 0x00) };
                if vendor == 0xFFFF || vendor == 0 {
                    if func == 0 {
                        let hdr = unsafe { self.pci_cfg_read16(0, dev, 0, 0x0E) };
                        if hdr & 0x80 == 0 {
                            break;
                        }
                    }
                    continue;
                }
                let device = unsafe { self.pci_cfg_read16(0, dev, func, 0x02) };
                if vendor == BOCHS_PCI_VENDOR && device == BOCHS_PCI_DEVICE {
                    found = true;
                    // Standard bochs-display BAR layout: BAR0 = framebuffer
                    // (16 MiB default vgamem), BAR2 = MMIO register file
                    // (4 KiB). Read the phys addresses from config space;
                    // sizing via the write-0xFFFFFFFF trick is skipped
                    // (QEMU's emulated BARs report stable addresses).
                    let fb_raw = unsafe { self.pci_cfg_read32(0, dev, func, 0x10) };
                    let reg_raw = unsafe { self.pci_cfg_read32(0, dev, func, 0x18) };
                    if fb_raw == 0 {
                        return Err(DriverError::NotFound);
                    }
                    let fb_phys = (fb_raw & !0xF) as u64;
                    let fb_len = 16 * 1024 * 1024;
                    let fb_va = unsafe { physmap(fb_phys, fb_len) };
                    if fb_va == 0 {
                        return Err(DriverError::NotFound);
                    }
                    self.dev = FbDevice {
                        base: fb_va,
                        size: fb_len as u64,
                    };
                    self.regs = 0;
                    if reg_raw != 0 {
                        let r_phys = (reg_raw & !0xF) as u64;
                        let r_va = unsafe { physmap(r_phys, 4096) };
                        if r_va != 0 {
                            // The register file sits at offset 0x500 of the
                            // BAR, not at the BAR base.
                            self.regs = r_va + VBE_DISPI_MMIO_REG_OFFSET;
                            self.fix.mmio_start = r_phys;
                            self.fix.mmio_len = 4096;
                        }
                    }
                    break 'probe;
                }
            }
        }

        if !found {
            // Legacy VBE over ports: registers on 0x1CE/0x1CF, LFB at the
            // fixed VBE address once enabled.
            self.regs = 0;
            self.dev = FbDevice {
                base: VBE_LFB_PHYS,
                size: VBE_LFB_DEFAULT_SIZE,
            };
        }

        if unsafe { self.mode_set() }.is_err() {
            return Err(DriverError::Io);
        }

        // Refresh the screen info from the device.
        let xres = BOCHS_DEFAULT_XRES;
        let yres = BOCHS_DEFAULT_YRES;
        self.var = FbVarScreeninfo {
            xres,
            yres,
            xres_virtual: xres,
            yres_virtual: yres,
            xoffset: 0,
            yoffset: 0,
            bits_per_pixel: 32,
            red: FbBitfield {
                offset: 16,
                length: 8,
                msb_right: 0,
            },
            green: FbBitfield {
                offset: 8,
                length: 8,
                msb_right: 0,
            },
            blue: FbBitfield {
                offset: 0,
                length: 8,
                msb_right: 0,
            },
            transp: FbBitfield {
                offset: 24,
                length: 8,
                msb_right: 0,
            },
            reserved: [0; 10],
        };
        self.fix.line_length = xres * 4;
        Ok(())
    }

    fn device(&self, _minor: usize) -> Result<FbDevice, DriverError> {
        if self.dev.size == 0 {
            return Err(DriverError::NotFound);
        }
        Ok(self.dev)
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

/// Runtime-selected framebuffer backend. x86 QEMU has bochs-display
/// (PCI VGA); riscv64/aarch64 `virt` machines have virtio-gpu, so the
/// server probes bochs first and falls back to virtio-gpu (one binary on
/// all arches).
#[allow(clippy::large_enum_variant)] // both variants live in BSS as a static
pub enum FbBackend {
    Bochs(BochsArch),
    VirtioGpu(VirtioGpuArch),
}

impl FbBackend {
    pub const fn new_bochs() -> Self {
        FbBackend::Bochs(BochsArch::new())
    }
}

impl FbArch for FbBackend {
    fn init(&mut self, minor: usize) -> Result<(), DriverError> {
        match self {
            FbBackend::Bochs(a) => a.init(minor),
            FbBackend::VirtioGpu(a) => a.init(minor),
        }
    }

    fn device(&self, minor: usize) -> Result<FbDevice, DriverError> {
        match self {
            FbBackend::Bochs(a) => a.device(minor),
            FbBackend::VirtioGpu(a) => a.device(minor),
        }
    }

    fn mem(&self, minor: usize) -> Result<u64, DriverError> {
        match self {
            FbBackend::Bochs(a) => a.mem(minor),
            FbBackend::VirtioGpu(a) => a.mem(minor),
        }
    }

    fn var_screeninfo(&self, minor: usize) -> Result<FbVarScreeninfo, DriverError> {
        match self {
            FbBackend::Bochs(a) => a.var_screeninfo(minor),
            FbBackend::VirtioGpu(a) => a.var_screeninfo(minor),
        }
    }

    fn set_var_screeninfo(
        &mut self,
        minor: usize,
        info: &FbVarScreeninfo,
    ) -> Result<(), DriverError> {
        match self {
            FbBackend::Bochs(a) => a.set_var_screeninfo(minor, info),
            FbBackend::VirtioGpu(a) => a.set_var_screeninfo(minor, info),
        }
    }

    fn fix_screeninfo(&self, minor: usize) -> Result<FbFixScreeninfo, DriverError> {
        match self {
            FbBackend::Bochs(a) => a.fix_screeninfo(minor),
            FbBackend::VirtioGpu(a) => a.fix_screeninfo(minor),
        }
    }

    fn pan_display(&mut self, minor: usize, info: &FbVarScreeninfo) -> Result<(), DriverError> {
        match self {
            FbBackend::Bochs(a) => a.pan_display(minor, info),
            FbBackend::VirtioGpu(a) => a.pan_display(minor, info),
        }
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        match self {
            FbBackend::Bochs(a) => a.flush(),
            FbBackend::VirtioGpu(a) => a.flush(),
        }
    }
}

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
        let base = arch.mem(minor)?;
        let src = (base + pos) as *const u8;
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
        let base = arch.mem(minor)?;
        let dst = (base + pos) as *mut u8;
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
            FBIOFLUSH => arch.flush(),
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
    fn test_bochs_arch_layout() {
        // repr(C) pins the field offsets so gdb/FB_ARCH reads and any
        // future ioctl marshalling are deterministic.
        assert_eq!(size_of::<BochsArch>(), 200);
        assert_eq!(core::mem::offset_of!(BochsArch, dev), 0);
        assert_eq!(core::mem::offset_of!(BochsArch, regs), 16);
        assert_eq!(core::mem::offset_of!(BochsArch, var), 24);
        assert_eq!(core::mem::offset_of!(BochsArch, fix), 120);
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

    // Bochs VBE fake environment: a fake PCI config space (one
    // bochs-display device at dev 2), fake VBE register ports, a fake
    // register BAR and a fake framebuffer. The devio hook routes 0xCF8/
    // 0xCFC to the config space and 0x1CE/0x1CF to the register pair;
    // the physmap hook maps the BAR physicals to the fake buffers.

    const FAKE_FB_SIZE: usize = 4 * 1024 * 1024;
    const FAKE_BAR0_PHYS: u64 = 0xFD00_0000;
    const FAKE_BAR2_PHYS: u64 = 0xFEB0_0000;

    static mut FAKE_PCI_ADDR: u32 = 0;
    static mut FAKE_PCI: [u32; 64] = [0; 64];
    static mut FAKE_VBE_INDEX: u16 = 0;
    static mut FAKE_VBE_REGS: [u16; 16] = [0; 16];
    static mut FAKE_FB: [u8; FAKE_FB_SIZE] = [0; FAKE_FB_SIZE];
    static mut FAKE_REGS: [u8; 4096] = [0; 4096];

    fn pci_ptr() -> *mut u32 {
        core::ptr::addr_of_mut!(FAKE_PCI) as *mut u32
    }

    fn fb_ptr() -> *mut u8 {
        core::ptr::addr_of_mut!(FAKE_FB) as *mut u8
    }

    fn regs_ptr() -> *mut u8 {
        core::ptr::addr_of_mut!(FAKE_REGS) as *mut u8
    }

    unsafe fn vbe_regs(idx: u16) -> u16 {
        unsafe {
            core::ptr::read_volatile(
                core::ptr::addr_of!(FAKE_VBE_REGS)
                    .cast::<u16>()
                    .add(idx as usize),
            )
        }
    }

    unsafe fn pci_slot() -> usize {
        // Register offset within the single fake device's config space.
        unsafe { (((FAKE_PCI_ADDR & 0xFC) >> 2) & 0x3F) as usize }
    }

    fn fake_devio(request: u32, port: u16, value: u32) -> u32 {
        let is_out = request & arch_common::com::DIO_DIRMASK == arch_common::com::DIO_OUTPUT;
        unsafe {
            match port {
                0xCF8 => {
                    if is_out {
                        FAKE_PCI_ADDR = value;
                    }
                    0
                }
                0xCFC => {
                    let slot = pci_slot();
                    if is_out {
                        if value == 0xFFFF_FFFF && (4..10).contains(&slot) {
                            // BAR sizing: return the size mask (BAR0 = 16 MiB,
                            // BAR2 = 4 KiB).
                            core::ptr::write_volatile(
                                pci_ptr().add(slot),
                                if slot == 4 { 0xFF00_0000 } else { 0xFFFF_F000 },
                            );
                        } else {
                            core::ptr::write_volatile(pci_ptr().add(slot), value);
                        }
                        0
                    } else {
                        core::ptr::read_volatile(pci_ptr().add(slot))
                    }
                }
                0x1CE => {
                    if is_out {
                        FAKE_VBE_INDEX = value as u16;
                    }
                    0
                }
                0x1CF => {
                    if is_out {
                        core::ptr::write_volatile(
                            core::ptr::addr_of_mut!(FAKE_VBE_REGS)
                                .cast::<u16>()
                                .add(FAKE_VBE_INDEX as usize),
                            value as u16,
                        );
                    }
                    core::ptr::read_volatile(
                        core::ptr::addr_of!(FAKE_VBE_REGS)
                            .cast::<u16>()
                            .add(FAKE_VBE_INDEX as usize),
                    ) as u32
                }
                _ => 0,
            }
        }
    }

    fn fake_physmap(phys: u64, _len: usize) -> u64 {
        match phys {
            FAKE_BAR0_PHYS => fb_ptr() as u64,
            FAKE_BAR2_PHYS => regs_ptr() as u64,
            _ => 0,
        }
    }

    /// Configure the fake PCI space: a bochs-display at the single fake
    /// device (if `present`) with the given BAR set (bit i set = BAR i
    /// present).
    fn setup_fake_pci(present: bool, bars: u8) {
        unsafe {
            for slot in 0..64 {
                core::ptr::write_volatile(pci_ptr().add(slot), 0);
            }
            if present {
                // Vendor/device in one dword: vendor low 16, device high 16.
                core::ptr::write_volatile(pci_ptr().add(0), 0x1234 | (0x1111 << 16));
            }
            if bars & 1 != 0 {
                core::ptr::write_volatile(pci_ptr().add(4), (FAKE_BAR0_PHYS as u32) | 0x8);
            }
            if bars & 4 != 0 {
                core::ptr::write_volatile(pci_ptr().add(6), FAKE_BAR2_PHYS as u32);
            }
            FAKE_VBE_INDEX = 0;
            for i in 0..16 {
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!(FAKE_VBE_REGS).cast::<u16>().add(i),
                    0,
                );
            }
            for i in 0..4096 {
                core::ptr::write_volatile(regs_ptr().add(i), 0);
            }
        }
    }

    #[test]
    fn test_bochs_port_mode_sets_registers_through_fake_ports() {
        unsafe {
            setup_fake_pci(true, 0b001); // BAR0 only → port register path
            fb_set_devio(fake_devio);
            fb_set_physmap(fake_physmap);

            let mut arch = BochsArch::new();
            assert!(arch.init(0).is_ok(), "mode-set must succeed");

            // The port path wrote the full mode-set dance: ENABLE=0 first,
            // then XRES/YRES/BPP, then ENABLE=0x41 into the register file
            // (the readback leaves the index register at XRES, so assert on
            // the file itself).
            assert_eq!(
                vbe_regs(VBE_DISPI_INDEX_ENABLE),
                VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED
            );
            assert_eq!(vbe_regs(VBE_DISPI_INDEX_XRES), BOCHS_DEFAULT_XRES as u16);
            assert_eq!(arch.dev.base, fb_ptr() as u64);
            assert_eq!(arch.dev.size, 16 * 1024 * 1024);
            assert_eq!(arch.var.xres, BOCHS_DEFAULT_XRES);
            assert_eq!(arch.fix.line_length, BOCHS_DEFAULT_XRES * 4);
        }
    }

    #[test]
    fn test_bochs_mmio_mode_sets_registers_through_fake_bar() {
        unsafe {
            setup_fake_pci(true, 0b101); // BAR0 + BAR2 → MMIO register path
            fb_set_devio(fake_devio);
            fb_set_physmap(fake_physmap);

            let mut arch = BochsArch::new();
            assert!(arch.init(0).is_ok());

            // The MMIO path writes each register at byte offset 0x500 +
            // idx*2 (the VBE file inside the register BAR); the mode-set
            // leaves ENABLE=0x41 at 0x500+4*2 and XRES at 0x500+1*2.
            let xres = u16::from_le_bytes([
                core::ptr::read_volatile(regs_ptr().add(0x500 + 2)),
                core::ptr::read_volatile(regs_ptr().add(0x500 + 3)),
            ]);
            let enable = u16::from_le_bytes([
                core::ptr::read_volatile(regs_ptr().add(0x500 + 8)),
                core::ptr::read_volatile(regs_ptr().add(0x500 + 9)),
            ]);
            assert_eq!(xres, BOCHS_DEFAULT_XRES as u16);
            assert_eq!(enable, VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED);
            assert_eq!(arch.regs, regs_ptr() as u64 + VBE_DISPI_MMIO_REG_OFFSET);
            assert_eq!(arch.dev.size, 16 * 1024 * 1024);
            assert_eq!(arch.fix.mmio_start, FAKE_BAR2_PHYS);
            assert_eq!(arch.fix.mmio_len, 4096);
        }
    }

    #[test]
    fn test_bochs_no_device_falls_back_to_legacy_lfb() {
        unsafe {
            setup_fake_pci(false, 0); // no bochs-display → legacy VBE over ports
            fb_set_devio(fake_devio);
            fb_set_physmap(fake_physmap);

            let mut arch = BochsArch::new();
            assert!(arch.init(0).is_ok());
            assert_eq!(arch.regs, 0);
            assert_eq!(arch.dev.base, VBE_LFB_PHYS);
            assert_eq!(arch.dev.size, VBE_LFB_DEFAULT_SIZE);
            // The port register dance still ran.
            assert_eq!(
                vbe_regs(VBE_DISPI_INDEX_ENABLE),
                VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED
            );
        }
    }

    #[test]
    fn test_bochs_writes_through_framebuffer() {
        unsafe {
            setup_fake_pci(true, 0b001);
            fb_set_devio(fake_devio);
            fb_set_physmap(fake_physmap);

            let mut arch = BochsArch::new();
            assert!(arch.init(0).is_ok());
            let mut fb = Framebuffer::new();
            let n = fb.write(0, 0, &[1, 2, 3, 4], &arch).unwrap();
            assert_eq!(n, 4);
            for (i, &expect) in [1u8, 2, 3, 4].iter().enumerate() {
                assert_eq!(core::ptr::read_volatile(fb_ptr().add(i)), expect);
            }
            let mut buf = [0u8; 4];
            let n = fb.read(0, 0, &mut buf, &arch).unwrap();
            assert_eq!(n, 4);
            assert_eq!(buf, [1, 2, 3, 4]);
        }
    }
}
