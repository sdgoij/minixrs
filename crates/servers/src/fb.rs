//! Framebuffer character-device server — `/dev/fb`.
//!
//! Selects the backend at boot: bochs-display (PCI probe for `1234:1111`,
//! mode-set to 1024×768×32, BARs identity-mapped via `VM_MAP_PHYS`) on
//! x86, or virtio-gpu (device ID 16 on the riscv/aarch64 `virt` machines)
//! whose framebuffer is a server-owned RAM buffer attached as resource
//! backing. Fills a test pattern so the framebuffer is verifiable via QMP
//! `screendump` with no input, then serves the CDEV_* protocol for
//! `/dev/fb` (open/close, inline read/write, grant-based ioctls, and the
//! FBIOFLUSH push for explicit-flush devices like virtio-gpu).

use arch_common::com::{
    CDEV_CLOSE, CDEV_IOCTL, CDEV_MAP, CDEV_OPEN, CDEV_READ, CDEV_WRITE, is_cdev_rq,
};
use drivers::bus::virtio;
use drivers::video::fb::{
    FBIOFLUSH, FBIOGET_FSCREENINFO, FBIOGET_VSCREENINFO, FBIOPAN_DISPLAY, FBIOPUT_VSCREENINFO,
    FbArch, FbBackend, Framebuffer, VirtioGpuArch,
};

/// Global driver state — one backend + one driver instance (no heap).
/// The backend is selected at boot: bochs-display on x86, virtio-gpu on
/// riscv/aarch64 (bochs probe fails there, so we switch).
static mut FB_BACKEND: FbBackend = FbBackend::new_bochs();
static mut FB_DRIVER: Framebuffer = Framebuffer::new();

/// The virtio-gpu framebuffer is a guest-RAM buffer owned by this server
/// (page-aligned, 1024×768×32 = 3 MiB). Its guest-physical address is what
/// the K3 mmap path maps into consumers. Only the virtio-gpu backend uses
/// it; bochs uses device memory.
#[repr(align(4096))]
struct FbBufCell(core::cell::UnsafeCell<[u8; 3 * 1024 * 1024]>);
unsafe impl Sync for FbBufCell {}
impl FbBufCell {
    const fn new() -> Self {
        Self(core::cell::UnsafeCell::new([0u8; 3 * 1024 * 1024]))
    }
    fn get(&self) -> u64 {
        self.0.get() as u64
    }
}
static FB_BUF: FbBufCell = FbBufCell::new();

/// Scratch space for ioctl arg structs and inline write data.
static mut FB_SCRATCH: [u8; 128] = [0; 128];

/// Port-I/O hook: route every access through SYS_DEVIO (userland drivers
/// have no direct I/O port access). The request/port/value live at
/// payload[0..12]; the result comes back in payload[0..4].
///
/// # Safety
///
/// Called from the devio hook registry; safe as long as the request is a
/// valid SYS_DEVIO shape.
#[cfg(target_os = "minix")]
fn devio_hook(request: u32, port: u16, value: u32) -> u32 {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&request.to_ne_bytes());
    msg[12..16].copy_from_slice(&(port as u32).to_ne_bytes());
    msg[16..20].copy_from_slice(&value.to_ne_bytes());
    // SYS_DEVIO is kernel_call 21 (kernel_call adds KERNEL_CALL itself).
    minix_rt::kernel_call(21, &mut msg);
    u32::from_ne_bytes(msg[8..12].try_into().unwrap_or([0u8; 4]))
}

/// Physical-memory mapping hook: map `phys..phys+len` into this process
/// via `VM_MAP_PHYS` (identity-mapped, user-accessible) and return the VA.
///
/// # Safety
///
/// `phys`/`len` must describe a real device memory range.
#[cfg(target_os = "minix")]
fn physmap_hook(phys: u64, len: usize) -> u64 {
    let mut msg = [0u8; 64];
    msg[4..8].copy_from_slice(&(arch_common::com::VM_MAP_PHYS as i32).to_ne_bytes());
    msg[8..12].copy_from_slice(&(-1i32).to_ne_bytes()); // target = self
    msg[12..16].copy_from_slice(&(len as i32).to_ne_bytes());
    // The BAR physicals fit in 32 bits (bochs-display is a 32-bit PCI
    // device); do_map_phys reads the field back as u32.
    msg[16..20].copy_from_slice(&((phys as u32) as i32).to_ne_bytes());
    let r = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::VM_PROC_NR as u64,
            msg.as_mut_ptr() as u64,
        )
    };
    if r < 0 {
        return 0;
    }
    let mtype = i32::from_ne_bytes(msg[4..8].try_into().unwrap_or([0u8; 4]));
    if mtype != 0 {
        return 0;
    }
    (u32::from_ne_bytes(msg[8..12].try_into().unwrap_or([0u8; 4]))) as u64
}

/// Query this process's VA→PA image translation offset (SYS_GETINFO
/// GET_PHYS_DELTA) and hand it to the virtio transport so queue and
/// descriptor addresses are programmed as guest-physical addresses (same
/// pattern as the virtio-blk/net servers).
#[cfg(target_os = "minix")]
fn init_phys_delta() {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&arch_common::com::GET_PHYS_DELTA.to_ne_bytes());
    minix_rt::kernel_call(26, &mut msg); // SYS_GETINFO
    let delta = i64::from_ne_bytes(msg[0..8].try_into().unwrap_or([0u8; 8]));
    virtio::virtio_set_phys_delta(delta);
}

#[cfg(not(target_os = "minix"))]
fn init_phys_delta() {}

/// Write a boot-time status line to the console (stdout).
fn slog(msg: &[u8]) {
    #[cfg(target_os = "minix")]
    unsafe {
        minix_rt::write(1, msg.as_ptr(), msg.len());
    }
    #[cfg(not(target_os = "minix"))]
    let _ = msg;
}

/// Select and initialize the framebuffer backend: bochs-display first
/// (x86), then virtio-gpu (riscv/aarch64, where the bochs probe finds no
/// PCI VGA). The virtio-gpu backend is pointed at the server-owned RAM
/// buffer before its init so the device can attach it as backing.
fn backend_init() -> bool {
    let backend = unsafe { &mut *core::ptr::addr_of_mut!(FB_BACKEND) };
    match backend {
        FbBackend::Bochs(arch) => {
            if arch.init(0).is_ok() {
                slog(b"fb: backend bochs-display\n");
                return true;
            }
            slog(b"fb: no bochs-display, trying virtio-gpu\n");
            *backend = FbBackend::VirtioGpu(VirtioGpuArch::new());
            match backend {
                FbBackend::VirtioGpu(v) => {
                    v.set_fb_va(FB_BUF.get());
                    match v.init(0) {
                        Ok(()) => {
                            slog(b"fb: backend virtio-gpu\n");
                            true
                        }
                        Err(e) => {
                            let code = match e {
                                drivers::DriverError::NotFound => 6,         // ENXIO
                                drivers::DriverError::Busy => 16,            // EBUSY
                                drivers::DriverError::InvalidArgument => 22, // EINVAL
                                drivers::DriverError::Unsupported => 95,     // EOPNOTSUPP
                                drivers::DriverError::Io | drivers::DriverError::Unknown => 5, // EIO
                            };
                            let mut m = [0u8; 64];
                            let msg = b"fb: virtio-gpu init failed err=";
                            m[..msg.len()].copy_from_slice(msg);
                            let mut i = msg.len();
                            let mut v = code as u32;
                            let mut tmp = [0u8; 10];
                            let mut j = 10;
                            loop {
                                j -= 1;
                                tmp[j] = b'0' + (v % 10) as u8;
                                v /= 10;
                                if v == 0 {
                                    break;
                                }
                            }
                            while j < 10 {
                                m[i] = tmp[j];
                                i += 1;
                                j += 1;
                            }
                            m[i] = b'\n';
                            i += 1;
                            slog(&m[..i]);
                            false
                        }
                    }
                }
                FbBackend::Bochs(_) => false,
            }
        }
        FbBackend::VirtioGpu(_) => false,
    }
}

/// Fill the framebuffer with a distinctive test pattern: the left third
/// red, the middle third green, the right third blue. Screendump pixel
/// asserts key off these colors, so the mode-set + BAR mapping are proven
/// end-to-end.
fn fill_test_pattern(arch: &mut dyn FbArch) {
    let (xres, yres) = (
        drivers::video::fb::BOCHS_DEFAULT_XRES,
        drivers::video::fb::BOCHS_DEFAULT_YRES,
    );
    let pitch = xres * 4;
    let mut row = [0u8; 4096];
    for x in 0..xres {
        let px = if x < xres / 3 {
            [0u8, 0, 0xFF, 0] // red (XRGB8888 LE → B,G,R,0)
        } else if x < 2 * xres / 3 {
            [0u8, 0xFF, 0, 0] // green
        } else {
            [0xFFu8, 0, 0, 0] // blue
        };
        row[(x * 4) as usize..(x * 4 + 4) as usize].copy_from_slice(&px);
    }
    for y in 0..yres {
        let off = (y * pitch) as u64;
        let n = pitch as usize;
        let _ = write_row(arch, off, &row[..n]);
    }
}

/// Write one framebuffer row via the driver's volatile write path.
fn write_row(arch: &dyn FbArch, pos: u64, data: &[u8]) -> usize {
    // The Framebuffer driver's write takes the arch by reference; a whole
    // row is far larger than the scratch, so write in chunks through the
    // driver's own path.
    let driver = unsafe { &mut *core::ptr::addr_of_mut!(FB_DRIVER) };
    driver.write(0, pos, data, arch).unwrap_or(0)
}

/// Main loop: receive CDEV requests, dispatch to the Framebuffer driver,
/// reply with SEND (a SENDREC would consume the caller's next request).
pub fn fb_server_main() {
    #[cfg(target_os = "minix")]
    {
        const ANY: i32 = 0x0000_ffff;

        drivers::video::fb::fb_set_devio(devio_hook);
        drivers::video::fb::fb_set_physmap(physmap_hook);
        virtio::virtio_set_devio(devio_hook);
        init_phys_delta();

        if backend_init() {
            let arch = unsafe { &mut *core::ptr::addr_of_mut!(FB_BACKEND) };
            let arch: &mut dyn FbArch = arch;
            fill_test_pattern(arch);
            // virtio-gpu is explicit-flush: push the pattern to the
            // display (no-op for bochs).
            let _ = arch.flush();
        }

        loop {
            let mut msg = arch_common::ipc::Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };
            let src = unsafe {
                minix_rt::syscall2(
                    minix_rt::RECEIVE_CALL,
                    ANY as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                )
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;
            let call_type = msg.m_type as u32;
            let result = if is_cdev_rq(call_type) {
                unsafe { handle_cdev_request(&mut msg, src_ep, call_type) }
            } else {
                -38 // ENOSYS
            };
            msg.m_type = result;
            unsafe {
                minix_rt::syscall2(
                    minix_rt::SEND_CALL,
                    src_ep as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                );
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // Host stub — the server loop cannot run outside the MINIX target.
    }
}

/// Dispatch a CDEV request to the Framebuffer driver.
///
/// # Safety
///
/// `msg` must point to a valid received message.
unsafe fn handle_cdev_request(
    msg: &mut arch_common::ipc::Message,
    who_e: i32,
    call_type: u32,
) -> i32 {
    // Standard CDEV message layout (m2 fields):
    //   m2_i1 = minor  (payload +0), m2_i2 = flags (+4), m2_i3 = grant (+8)
    //   m2_l1 = position (+16), m2_l2 = count (+24), m2_l3 = inline data (+32)
    let minor = unsafe { msg.m_payload.m2.m2i1 as u32 };
    let arch = unsafe { &mut *core::ptr::addr_of_mut!(FB_BACKEND) };
    let arch: &mut dyn FbArch = arch;
    let driver = unsafe { &mut *core::ptr::addr_of_mut!(FB_DRIVER) };

    match call_type {
        CDEV_OPEN => {
            let access = unsafe { msg.m_payload.m2.m2i2 };
            match driver.open(minor as usize, arch) {
                Ok(()) => {
                    // A framebuffer open succeeds without an access-mode
                    // distinction; mirror the access flags back like other
                    // char drivers so VFS records them.
                    access
                }
                Err(_) => -6, // ENXIO
            }
        }
        CDEV_CLOSE => {
            let _ = driver.close(minor as usize);
            0
        }
        CDEV_READ => {
            let position = unsafe { msg.m_payload.m2.m2l1 as u64 };
            let count = unsafe { msg.m_payload.m2.m2l2 as usize };
            let n = count.min(48);
            let dst = unsafe { &mut *core::ptr::addr_of_mut!(FB_SCRATCH) };
            match driver.read(minor as usize, position, &mut dst[..n], arch) {
                Ok(0) => 0,
                Ok(got) => {
                    unsafe {
                        msg.m_payload.raw[..got].copy_from_slice(&dst[..got]);
                    }
                    got as i32
                }
                Err(_) => -5, // EIO
            }
        }
        CDEV_WRITE => {
            let position = unsafe { msg.m_payload.m2.m2l1 as u64 };
            let count = unsafe { msg.m_payload.m2.m2l2 as usize };
            // Inline write data in m2_l3 (the last 8 payload bytes).
            let n = count.min(8);
            let src = unsafe { &mut *core::ptr::addr_of_mut!(FB_SCRATCH) };
            unsafe {
                src[..n].copy_from_slice(&msg.m_payload.raw[32..32 + n]);
            }
            match driver.write(minor as usize, position, &src[..n], arch) {
                Ok(got) => got as i32,
                Err(_) => -5, // EIO
            }
        }
        CDEV_IOCTL => {
            let request = unsafe { msg.m_payload.m2.m2i2 as u32 };
            let grant = unsafe { msg.m_payload.m2.m2i3 as u32 };
            let user = unsafe { msg.m_payload.m2.m2l1 } as i32;
            do_ioctl(minor, request, who_e, grant, user)
        }
        CDEV_MAP => {
            // Device-memory mmap: reply with the framebuffer's physical
            // range (phys u64 @ payload 0, len u64 @ payload 8). For bochs
            // the arch's `dev.base` is the identity-mapped device VA
            // (equals the phys); for virtio-gpu it is the backing buffer's
            // guest-physical address directly.
            match arch.device(minor as usize) {
                Ok(dev) => {
                    unsafe {
                        msg.m_payload.raw[0..8].copy_from_slice(&dev.base.to_le_bytes());
                        msg.m_payload.raw[8..16].copy_from_slice(&dev.size.to_le_bytes());
                    }
                    0
                }
                Err(_) => -6, // ENXIO
            }
        }
        _ => -38, // ENOSYS
    }
}

/// Grant-based fb ioctl: read the arg struct from the caller's buffer via
/// the VFS-created grant, dispatch, write results back.
///
/// # Safety
///
/// `grant` must be a valid VFS magic grant over the caller's buffer.
fn do_ioctl(minor: u32, request: u32, who_e: i32, grant: u32, user: i32) -> i32 {
    let arch = unsafe { &mut *core::ptr::addr_of_mut!(FB_BACKEND) };
    let arch: &mut dyn FbArch = arch;
    let driver = unsafe { &mut *core::ptr::addr_of_mut!(FB_DRIVER) };
    let scratch = unsafe { &mut *core::ptr::addr_of_mut!(FB_SCRATCH) };

    let (arg_size, is_out) = match request {
        FBIOGET_VSCREENINFO => (
            core::mem::size_of::<drivers::video::fb::FbVarScreeninfo>(),
            true,
        ),
        FBIOPUT_VSCREENINFO => (
            core::mem::size_of::<drivers::video::fb::FbVarScreeninfo>(),
            false,
        ),
        FBIOGET_FSCREENINFO => (
            core::mem::size_of::<drivers::video::fb::FbFixScreeninfo>(),
            true,
        ),
        FBIOPAN_DISPLAY => (
            core::mem::size_of::<drivers::video::fb::FbVarScreeninfo>(),
            false,
        ),
        FBIOFLUSH => (0, false),
        _ => return -25, // ENOTTY
    };
    if arg_size > scratch.len() {
        return -7; // E2BIG
    }

    // Fetch the arg struct from the caller through the grant. No-arg
    // ioctls (FBIOFLUSH) come with GRANT_INVALID; skip the copy.
    if !is_out && arg_size > 0 {
        if safecopy_from(who_e, grant, scratch, arg_size) != 0 {
            return -14; // EFAULT
        }
    }

    let data = scratch;
    let result = driver.ioctl(minor as usize, request, &mut data[..arg_size], arch);
    match result {
        Ok(()) => {
            // Write results back (GET-family ioctls).
            if is_out && safecopy_to(user, grant, &data[..arg_size]) != 0 {
                return -14; // EFAULT
            }
            0
        }
        Err(_) => -25, // ENOTTY
    }
}

/// Copy `count` bytes from the granter's granted buffer via SYS_SAFECOPYFROM.
#[cfg(target_os = "minix")]
fn safecopy_from(granter: i32, grant: u32, data: &mut [u8], count: usize) -> i32 {
    let mut kmsg = [0u8; 64];
    kmsg[8..12].copy_from_slice(&granter.to_ne_bytes());
    kmsg[12..16].copy_from_slice(&(grant as i32).to_ne_bytes());
    kmsg[16..24].copy_from_slice(&0u64.to_ne_bytes()); // offset
    kmsg[24..32].copy_from_slice(&(data.as_ptr() as u64).to_ne_bytes());
    kmsg[32..40].copy_from_slice(&(count as u64).to_ne_bytes());
    minix_rt::kernel_call(31, &mut kmsg) // SYS_SAFECOPYFROM
}

/// Copy `count` bytes to the grantee's granted buffer via SYS_SAFECOPYTO.
#[cfg(target_os = "minix")]
fn safecopy_to(grantee: i32, grant: u32, data: &[u8]) -> i32 {
    let mut kmsg = [0u8; 64];
    kmsg[8..12].copy_from_slice(&grantee.to_ne_bytes());
    kmsg[12..16].copy_from_slice(&(grant as i32).to_ne_bytes());
    kmsg[16..24].copy_from_slice(&0u64.to_ne_bytes()); // offset
    kmsg[24..32].copy_from_slice(&(data.as_ptr() as u64).to_ne_bytes());
    kmsg[32..40].copy_from_slice(&(data.len() as u64).to_ne_bytes());
    minix_rt::kernel_call(32, &mut kmsg) // SYS_SAFECOPYTO
}

#[cfg(not(target_os = "minix"))]
fn safecopy_from(_granter: i32, _grant: u32, _data: &mut [u8], _count: usize) -> i32 {
    -14 // EFAULT
}

#[cfg(not(target_os = "minix"))]
fn safecopy_to(_grantee: i32, _grant: u32, _data: &[u8]) -> i32 {
    -14 // EFAULT
}
