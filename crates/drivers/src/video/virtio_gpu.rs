//! Virtio-GPU 2D framebuffer backend (virtio device ID 16).
//!
//! The virtio-gpu is the display device on QEMU riscv64/aarch64 `virt`
//! machines (they have no bochs-display / PCI VGA; L1 verified the device
//! slots into the port's existing device-ID virtio scan). This backend
//! implements the 2D command ring — RESOURCE_CREATE_2D →
//! RESOURCE_ATTACH_BACKING → SET_SCANOUT → RESOURCE_FLUSH — with the
//! framebuffer being a contiguous guest-RAM buffer (a server-owned static,
//! page-aligned) whose guest-physical address is exposed through the K3
//! char-device mmap path (`FbDevice.base`).
//!
//! Drawing is plain writes to that buffer; RESOURCE_FLUSH pushes it to the
//! display (explicit-flush semantics, unlike VGA's always-visible LFB), so
//! the server flushes after its boot pattern and consumers issue an
//! FBIOFLUSH after drawing.

use crate::DriverError;
use crate::bus::virtio::{self, VirtioDevice, VirtioPhysBuf};
use crate::video::fb::{
    BOCHS_DEFAULT_XRES, BOCHS_DEFAULT_YRES, FbArch, FbBitfield, FbDevice, FbFixScreeninfo,
    FbVarScreeninfo,
};

/// Virtio device ID for the GPU (`.refs/minix-3.3.0` virtio spec).
pub const VIRTIO_GPU_DEVICE_ID: u16 = 0x0010;

/// 2D pixel format: B8G8R8X8_UNORM. QEMU maps the virtio-gpu formats to
/// the PIXMAN_BE_* (big-endian) variants, whose memory order on an LE host
/// is the reversed component order: B8G8R8X8 -> BE_b8g8r8x8 reads memory
/// as [B,G,R,X] — exactly the port's XRGB8888 byte order. X8R8G8B8 would
/// map to BE_x8r8g8b8 (memory [X,R,G,B]) and swap R/G on the display.
const VIRTIO_GPU_FORMAT_B8G8R8X8: u32 = 2;

/// Command types (`linux/virtio_gpu.h`).
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;

/// Response types.
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
const RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
const RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;

/// Resource ID of the single scanout resource this backend manages.
const RESOURCE_ID: u32 = 1;

/// Framebuffer geometry (matches `BOCHS_DEFAULT_XRES`/`YRES`, which the
/// wserver/fbterm hardcode).
const XRES: u32 = BOCHS_DEFAULT_XRES;
const YRES: u32 = BOCHS_DEFAULT_YRES;
const FB_SIZE: u64 = (XRES as u64) * (YRES as u64) * 4;

/// `struct virtio_gpu_ctrl_hdr` — 24 bytes: type, flags, fence_id, ctx_id,
/// ring_idx, padding[3]. There is NO resource_id in the header; every
/// command carries its resource reference in its own body fields.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CtrlHdr {
    type_: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    ring_idx: u8,
    padding: [u8; 3],
}

impl CtrlHdr {
    const fn new(type_: u32) -> Self {
        Self {
            type_,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            ring_idx: 0,
            padding: [0; 3],
        }
    }
}

/// `struct virtio_gpu_resource_create_2d` — 44 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ResourceCreate2D {
    hdr: CtrlHdr,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

/// `struct virtio_gpu_mem_entry` — 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MemEntry {
    addr: u64,
    length: u32,
    padding: u32,
}

/// `struct virtio_gpu_resource_attach_backing` with one entry — 48 bytes:
/// hdr, resource_id, nr_entries, then the entries (QEMU reads them from
/// offset 32).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct AttachBacking {
    hdr: CtrlHdr,
    resource_id: u32,
    nr_entries: u32,
    entries: [MemEntry; 1],
}

/// `struct virtio_gpu_rect` — 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// `struct virtio_gpu_set_scanout` — 52 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct SetScanout {
    hdr: CtrlHdr,
    r: Rect,
    scanout_id: u32,
    resource_id: u32,
}

/// `struct virtio_gpu_transfer_to_host_2d` — 56 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct TransferToHost2D {
    hdr: CtrlHdr,
    r: Rect,
    offset: u64,
    resource_id: u32,
    padding: u32,
}

/// `struct virtio_gpu_resource_flush` — 48 bytes: hdr, rect, resource_id,
/// padding.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ResourceFlush {
    hdr: CtrlHdr,
    r: Rect,
    resource_id: u32,
    padding: u32,
}

/// Bytes of a `repr(C)` struct for the command ring.
fn struct_bytes<T>(v: &T) -> &[u8] {
    // Safety: repr(C) structs are plain data; converting to bytes for the
    // virtqueue descriptor is the standard driver pattern (host reads the
    // same layout).
    unsafe { core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>()) }
}

/// Virtio-GPU framebuffer backend.
///
/// The framebuffer memory is owned by the server (a page-aligned static);
/// the backend is told its image VA via [`VirtioGpuArch::set_fb_va`] before
/// `init`, and derives the guest-physical address from the transport's
/// VA→PA delta.
pub struct VirtioGpuArch {
    /// Framebuffer descriptor for the K3 mmap path — `base` is the
    /// guest-physical address of the buffer (what VFS/VM map).
    pub dev: FbDevice,
    /// Image VA of the server-owned framebuffer buffer (what this process
    /// reads/writes through `FbArch::mem`).
    pub fb_va: u64,
    pub var: FbVarScreeninfo,
    pub fix: FbFixScreeninfo,
    initialized: bool,
    vdev: Option<VirtioDevice>,
    /// Command ring scratch: one command + one response slot.
    cmd: [u8; 64],
    resp: [u8; 32],
}

impl VirtioGpuArch {
    pub const fn new() -> Self {
        Self {
            dev: FbDevice::new(),
            fb_va: 0,
            var: FbVarScreeninfo::new(),
            fix: FbFixScreeninfo::new(),
            initialized: false,
            vdev: None,
            cmd: [0u8; 64],
            resp: [0u8; 32],
        }
    }

    /// Set the image VA of the server-owned framebuffer buffer.
    pub fn set_fb_va(&mut self, va: u64) {
        self.fb_va = va;
    }

    /// Submit one command: `cmd` is copied into the command slot, the
    /// response slot is supplied writable, then we spin for the used-ring
    /// completion and check the response type.
    fn send_cmd(&mut self, cmd: &[u8]) -> Result<(), DriverError> {
        if cmd.len() > self.cmd.len() {
            return Err(DriverError::InvalidArgument);
        }
        self.cmd[..cmd.len()].copy_from_slice(cmd);
        let cmd_addr = self.cmd.as_ptr() as u64;
        let resp_addr = self.resp.as_ptr() as u64;
        let dev = self.vdev.as_mut().ok_or(DriverError::NotFound)?;
        let bufs = [
            VirtioPhysBuf {
                addr: cmd_addr,
                size: cmd.len() as u32,
            },
            VirtioPhysBuf {
                addr: resp_addr | 1, // writable
                size: self.resp.len() as u32,
            },
        ];
        virtio::virtio_to_queue(dev, 0, &bufs, 0).map_err(|_| DriverError::Io)?;
        let mut spins = 0u32;
        loop {
            if virtio::virtio_from_queue(dev, 0).is_some() {
                break;
            }
            spins += 1;
            if spins >= 50_000_000 {
                return Err(DriverError::Busy);
            }
            core::hint::spin_loop();
        }
        let rtype = u32::from_le_bytes(self.resp[0..4].try_into().unwrap_or([0u8; 4]));
        if rtype != RESP_OK_NODATA {
            return Err(match rtype {
                RESP_ERR_OUT_OF_MEMORY => DriverError::Busy,
                RESP_ERR_INVALID_SCANOUT_ID | RESP_ERR_INVALID_RESOURCE_ID => {
                    DriverError::InvalidArgument
                }
                _ => DriverError::Io,
            });
        }
        Ok(())
    }

    /// Push the guest framebuffer to the display: TRANSFER_TO_HOST_2D copies
    /// the attached guest buffer into the host scanout image, then
    /// RESOURCE_FLUSH makes the change visible.
    fn flush_inner(&mut self) -> Result<(), DriverError> {
        let rect = Rect {
            x: 0,
            y: 0,
            width: XRES,
            height: YRES,
        };
        let transfer = TransferToHost2D {
            hdr: CtrlHdr::new(CMD_TRANSFER_TO_HOST_2D),
            r: rect,
            offset: 0,
            resource_id: RESOURCE_ID,
            padding: 0,
        };
        self.send_cmd(struct_bytes(&transfer))?;
        let cmd = ResourceFlush {
            hdr: CtrlHdr::new(CMD_RESOURCE_FLUSH),
            r: rect,
            resource_id: RESOURCE_ID,
            padding: 0,
        };
        self.send_cmd(struct_bytes(&cmd))
    }

    fn screen_info(&self) -> FbVarScreeninfo {
        FbVarScreeninfo {
            xres: XRES,
            yres: YRES,
            xres_virtual: XRES,
            yres_virtual: YRES,
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
        }
    }
}

impl Default for VirtioGpuArch {
    fn default() -> Self {
        Self::new()
    }
}

impl FbArch for VirtioGpuArch {
    fn init(&mut self, _minor: usize) -> Result<(), DriverError> {
        if self.initialized {
            return Ok(());
        }
        if self.fb_va == 0 {
            return Err(DriverError::InvalidArgument);
        }

        let mut dev = virtio::virtio_probe(VIRTIO_GPU_DEVICE_ID, "virtio-gpu", &[], 0)
            .map_err(|_| DriverError::NotFound)?;
        virtio::virtio_alloc_queue(&mut dev, 0).map_err(|_| DriverError::Io)?;
        virtio::virtio_device_ready(&mut dev);
        self.vdev = Some(dev);

        let fb_pa = self.fb_va.wrapping_add(virtio::virtio_phys_delta() as u64);

        // RESOURCE_CREATE_2D: resource 1, B8G8R8X8, 1024×768.
        let cmd = ResourceCreate2D {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_2D),
            resource_id: RESOURCE_ID,
            format: VIRTIO_GPU_FORMAT_B8G8R8X8,
            width: XRES,
            height: YRES,
        };
        self.send_cmd(struct_bytes(&cmd))?;

        // RESOURCE_ATTACH_BACKING: one entry covering the whole buffer
        // (page-aligned, page-multiple length).
        let cmd = AttachBacking {
            hdr: CtrlHdr::new(CMD_RESOURCE_ATTACH_BACKING),
            resource_id: RESOURCE_ID,
            nr_entries: 1,
            entries: [MemEntry {
                addr: fb_pa,
                length: FB_SIZE as u32,
                padding: 0,
            }],
        };
        self.send_cmd(struct_bytes(&cmd))?;

        // SET_SCANOUT: scanout 0 shows resource 1 at full size.
        let cmd = SetScanout {
            hdr: CtrlHdr::new(CMD_SET_SCANOUT),
            r: Rect {
                x: 0,
                y: 0,
                width: XRES,
                height: YRES,
            },
            scanout_id: 0,
            resource_id: RESOURCE_ID,
        };
        self.send_cmd(struct_bytes(&cmd))?;

        // Initial FLUSH so the (all-zero) buffer is displayed.
        self.flush_inner()?;

        self.dev = FbDevice {
            base: fb_pa,
            size: FB_SIZE,
        };
        self.var = self.screen_info();
        self.fix.line_length = XRES * 4;
        self.initialized = true;
        Ok(())
    }

    fn device(&self, _minor: usize) -> Result<FbDevice, DriverError> {
        if self.dev.size == 0 {
            return Err(DriverError::NotFound);
        }
        Ok(self.dev)
    }

    fn mem(&self, _minor: usize) -> Result<u64, DriverError> {
        if self.fb_va == 0 {
            return Err(DriverError::NotFound);
        }
        Ok(self.fb_va)
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

    fn flush(&mut self) -> Result<(), DriverError> {
        self.flush_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_struct_sizes_match_spec() {
        // Sizes follow `linux/virtio_gpu.h`: the 24-byte ctrl header has no
        // resource_id (ring_idx + padding[3] instead), so every command's
        // body fields sit 8 bytes earlier than a 32-byte header would put
        // them.
        assert_eq!(core::mem::size_of::<CtrlHdr>(), 24);
        assert_eq!(core::mem::size_of::<ResourceCreate2D>(), 40);
        assert_eq!(core::mem::size_of::<MemEntry>(), 16);
        assert_eq!(core::mem::size_of::<AttachBacking>(), 48);
        assert_eq!(core::mem::size_of::<Rect>(), 16);
        assert_eq!(core::mem::size_of::<SetScanout>(), 48);
        assert_eq!(core::mem::size_of::<TransferToHost2D>(), 56);
        assert_eq!(core::mem::size_of::<ResourceFlush>(), 48);
    }

    #[test]
    fn protocol_constants() {
        assert_eq!(VIRTIO_GPU_DEVICE_ID, 0x0010);
        assert_eq!(VIRTIO_GPU_FORMAT_B8G8R8X8, 2);
        assert_eq!(CMD_RESOURCE_CREATE_2D, 0x0101);
        assert_eq!(CMD_RESOURCE_ATTACH_BACKING, 0x0106);
        assert_eq!(CMD_SET_SCANOUT, 0x0103);
        assert_eq!(CMD_RESOURCE_FLUSH, 0x0104);
        assert_eq!(CMD_TRANSFER_TO_HOST_2D, 0x0105);
        assert_eq!(RESP_OK_NODATA, 0x1100);
        assert_eq!(FB_SIZE, 1024 * 768 * 4);
    }

    #[test]
    fn create_2d_command_bytes() {
        let cmd = ResourceCreate2D {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_2D),
            resource_id: 1,
            format: VIRTIO_GPU_FORMAT_B8G8R8X8,
            width: 1024,
            height: 768,
        };
        let bytes = struct_bytes(&cmd);
        // hdr (24 bytes), then resource_id @24, format @28, width @32,
        // height @36.
        assert_eq!(
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            CMD_RESOURCE_CREATE_2D
        );
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 0); // hdr ring_idx+padding
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            VIRTIO_GPU_FORMAT_B8G8R8X8
        );
        assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 1024);
        assert_eq!(u32::from_le_bytes(bytes[36..40].try_into().unwrap()), 768);
    }

    #[test]
    fn attach_backing_carries_resource_in_body_and_translates_pa() {
        let cmd = AttachBacking {
            hdr: CtrlHdr::new(CMD_RESOURCE_ATTACH_BACKING),
            resource_id: 7,
            nr_entries: 1,
            entries: [MemEntry {
                addr: 0x1234_5000,
                length: 0x300000,
                padding: 0,
            }],
        };
        let bytes = struct_bytes(&cmd);
        // hdr @0..24 (no resource ref), body resource_id @24, nr_entries
        // @28, entry addr @32, length @40.
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), 1);
        assert_eq!(
            u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            0x1234_5000
        );
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            0x0030_0000
        );
    }

    #[test]
    fn set_scanout_and_flush_carry_resource_in_body() {
        let ss = SetScanout {
            hdr: CtrlHdr::new(CMD_SET_SCANOUT),
            r: Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            },
            scanout_id: 0,
            resource_id: 1,
        };
        let b = struct_bytes(&ss);
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_SET_SCANOUT
        );
        assert_eq!(u32::from_le_bytes(b[20..24].try_into().unwrap()), 0); // hdr ring_idx+padding
        // rect @24..40, scanout_id @40, resource_id @44.
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 1);

        let fl = ResourceFlush {
            hdr: CtrlHdr::new(CMD_RESOURCE_FLUSH),
            r: Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            },
            resource_id: 1,
            padding: 0,
        };
        let b = struct_bytes(&fl);
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_RESOURCE_FLUSH
        );
        // rect @24..40, resource_id @40, padding @44.
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 0); // r.x
        assert_eq!(u32::from_le_bytes(b[32..36].try_into().unwrap()), 1024); // r.width
        assert_eq!(u32::from_le_bytes(b[36..40].try_into().unwrap()), 768); // r.height
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 1); // resource_id
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 0);
    }

    #[test]
    fn transfer_to_host_2d_layout() {
        let t = TransferToHost2D {
            hdr: CtrlHdr::new(CMD_TRANSFER_TO_HOST_2D),
            r: Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            },
            offset: 0,
            resource_id: 1,
            padding: 0,
        };
        let b = struct_bytes(&t);
        // rect @24..40, offset @40..48, resource_id @48, padding @52.
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_TRANSFER_TO_HOST_2D
        );
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(b[32..36].try_into().unwrap()), 1024);
        assert_eq!(u32::from_le_bytes(b[36..40].try_into().unwrap()), 768);
        assert_eq!(u64::from_le_bytes(b[40..48].try_into().unwrap()), 0); // offset
        assert_eq!(u32::from_le_bytes(b[48..52].try_into().unwrap()), 1); // resource_id
        assert_eq!(u32::from_le_bytes(b[52..56].try_into().unwrap()), 0);
    }

    #[test]
    fn screen_info_is_1024x768x32() {
        let arch = VirtioGpuArch::new();
        let var = arch.screen_info();
        assert_eq!(var.xres, 1024);
        assert_eq!(var.yres, 768);
        assert_eq!(var.bits_per_pixel, 32);
        assert_eq!(var.red.offset, 16);
        assert_eq!(var.blue.offset, 0);
    }

    #[test]
    fn device_and_mem_require_init_state() {
        let mut arch = VirtioGpuArch::new();
        assert!(arch.device(0).is_err()); // dev.size == 0
        assert!(arch.mem(0).is_err()); // fb_va == 0
        // init refuses to probe without a framebuffer VA (checked before
        // the virtio probe, so this is safe on the host).
        assert!(arch.init(0).is_err());
        arch.set_fb_va(0x1000);
        assert_eq!(arch.mem(0).unwrap(), 0x1000);
        // The probe itself needs real virtio hardware — target-tested.
    }
}
