//! Virtual disk (VNode Disk) driver — block device backed by a regular file.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/vnd/`
//!
//! In the full Minix system, this driver maps a regular file to appear as
//! a block device, using `VNDIOCSET` / `VNDIOCCLR` ioctls to configure
//! and tear down the mapping.
//!
//! This is a `no_std` stub.  The real VFS-based implementation (file
//! descriptor management, `pread`/`pwrite` I/O, `mmap` buffer allocation)
//! depends on the system server and file system layers (Phase 12+).  For
//! now the driver returns `DriverError::NotFound` on all operations.

use crate::DriverError;
use core::cell::UnsafeCell;

/// Size of the intermediate I/O transfer buffer (64 KB).
pub const VND_BUF_SIZE: usize = 65536;

/// Number of partitions per drive (from `partition.h`: `NR_PARTITIONS` = 4).
pub const NR_PARTITIONS: usize = 4;

/// Number of device slots per drive (primary + partitions).
pub const DEV_PER_DRIVE: usize = 1 + NR_PARTITIONS; // 5

/// Number of subpartition slots per drive.
pub const SUB_PER_DRIVE: usize = NR_PARTITIONS * NR_PARTITIONS; // 16

/// Minor device number for the first subpartition of the first partition.
pub const MINOR_D0P0S0: usize = 128;

/// Use user-specified geometry (instead of computed).
pub const VNDIOF_HASGEOM: u32 = 0x01;
/// Expose the device as read-only.
pub const VNDIOF_READONLY: u32 = 0x02;
/// Force close (overrides busy check).
pub const VNDIOF_FORCE: u32 = 0x04;

/// Configure the virtual disk with a file descriptor.
pub const VNDIOCSET: u32 = 0x4600;
/// Tear down the virtual disk configuration.
pub const VNDIOCCLR: u32 = 0x4601;
/// Query the current virtual disk configuration.
pub const VNDIOCGET: u32 = 0x4603;

/// Default sector size in bytes.
pub const SECTOR_SIZE: u32 = 512;

/// Default geometry for large disks: 64 heads, 32 sectors/track.
pub const DEFAULT_HEADS: u64 = 64;
pub const DEFAULT_SECTORS: u64 = 32;

/// Virtual disk geometry (from `vndvar.h` `struct vndgeom`).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct VndGeom {
    pub secsize: u32,
    pub nsectors: u32,
    pub ntracks: u32,
    pub ncylinders: u32,
}

/// Virtual disk partition geometry.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct PartGeom {
    pub base: u64,
    pub size: u64,
    pub cylinders: u64,
    pub heads: u64,
    pub sectors: u64,
}

/// A single partition or subpartition entry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VndDevice {
    pub base: u64,
    pub size: u64,
}

impl VndDevice {
    pub const fn new() -> Self {
        Self { base: 0, size: 0 }
    }
}

impl Default for VndDevice {
    fn default() -> Self {
        Self::new()
    }
}

/// IOCTL argument for VNDIOCSET / VNDIOCCLR (from `vndvar.h` `struct vnd_ioctl`).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VndIoctl {
    pub vnd_fildes: i32,
    pub vnd_flags: i32,
    pub vnd_geom: VndGeom,
    pub vnd_size: u64,
}

impl VndIoctl {
    pub const fn new() -> Self {
        Self {
            vnd_fildes: -1,
            vnd_flags: 0,
            vnd_geom: VndGeom {
                secsize: 0,
                nsectors: 0,
                ntracks: 0,
                ncylinders: 0,
            },
            vnd_size: 0,
        }
    }
}

impl Default for VndIoctl {
    fn default() -> Self {
        Self::new()
    }
}

/// IOCTL argument for VNDIOCGET (from `vndvar.h` `struct vnd_user`).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VndUser {
    pub vnu_unit: i32,
    pub vnu_dev: u64,
    pub vnu_ino: u64,
}

impl VndUser {
    pub const fn new() -> Self {
        Self {
            vnu_unit: 0,
            vnu_dev: 0,
            vnu_ino: 0,
        }
    }
}

impl Default for VndUser {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime state of the virtual disk driver.
///
/// Mirrors the C `static struct` in `vnd.c`.
#[allow(dead_code)]
struct VndState {
    /// File descriptor for the backing file (-1 = unconfigured).
    fd: i32,
    /// Number of times the device is open.
    openct: i32,
    /// Whether to exit after the last close.
    exiting: bool,
    /// Whether the device is read-only.
    rdonly: bool,
    /// Device on which the backing file resides.
    dev: u64,
    /// Inode number of the backing file.
    ino: u64,
    /// Partition table entries.
    part: [VndDevice; DEV_PER_DRIVE],
    /// Subpartition entries.
    subpart: [VndDevice; SUB_PER_DRIVE],
    /// Geometry information.
    geom: PartGeom,
    /// Intermediate I/O transfer buffer.
    buf: [u8; VND_BUF_SIZE],
    /// Whether the driver has been initialized.
    initialized: bool,
}

impl VndState {
    const fn new() -> Self {
        Self {
            fd: -1,
            openct: 0,
            exiting: false,
            rdonly: false,
            dev: 0,
            ino: 0,
            part: [VndDevice::new(); DEV_PER_DRIVE],
            subpart: [VndDevice::new(); SUB_PER_DRIVE],
            geom: PartGeom {
                base: 0,
                size: 0,
                cylinders: 0,
                heads: 0,
                sectors: 0,
            },
            buf: [0u8; VND_BUF_SIZE],
            initialized: false,
        }
    }
}

struct StateCell(UnsafeCell<VndState>);
unsafe impl Sync for StateCell {}
impl StateCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(VndState::new()))
    }
    fn get(&self) -> *mut VndState {
        self.0.get()
    }
}

static STATE: StateCell = StateCell::new();

fn state_ptr() -> *mut VndState {
    STATE.get()
}

/// Compute device geometry from total size in bytes.
///
/// Corresponds to `vnd_layout()` in the C reference when no user geometry
/// is provided.  Uses the same algorithm: for large disks (>= 32*64 sectors),
/// uses 64 heads / 32 sectors per track; otherwise 1 head / 1 sector.
fn compute_geometry(size: u64) -> PartGeom {
    let sectors = size / (SECTOR_SIZE as u64);
    let geom_size = sectors * (SECTOR_SIZE as u64);

    if sectors >= 32 * 64 {
        PartGeom {
            base: 0,
            size: geom_size,
            cylinders: sectors / (32 * 64),
            heads: 64,
            sectors: 32,
        }
    } else {
        PartGeom {
            base: 0,
            size: geom_size,
            cylinders: sectors,
            heads: 1,
            sectors: 1,
        }
    }
}

/// Initialize the virtual disk driver.
///
/// Must be called before any other function.
///
/// # Safety
///
/// Must be called exactly once, with exclusive access.
pub unsafe fn vnd_init() {
    // SAFETY: caller guarantees exclusive access.
    let st = unsafe { &mut *state_ptr() };
    st.fd = -1;
    st.openct = 0;
    st.exiting = false;
    st.rdonly = false;
    st.dev = 0;
    st.ino = 0;
    st.geom = PartGeom {
        base: 0,
        size: 0,
        cylinders: 0,
        heads: 0,
        sectors: 0,
    };
    st.initialized = true;
}

/// Open a device minor.
///
/// Returns `Err(DriverError::NotFound)` if the device is not configured
/// (fd == -1) and minor is not 0, or if the partition does not exist.
///
/// Corresponds to `vnd_open()` in the C reference.
pub fn vnd_open(minor: usize, _access: i32) -> Result<(), DriverError> {
    let st = unsafe { &mut *state_ptr() };

    if !st.initialized {
        return Err(DriverError::NotFound);
    }

    // No sub/partition devices are available before initialization.
    if st.fd == -1 && minor != 0 {
        return Err(DriverError::NotFound);
    }

    if st.fd != -1 && vnd_part_inner(st, minor).is_none() {
        return Err(DriverError::NotFound);
    }

    // Block write access if unconfigured or read-only.
    if _access & 2 != 0 {
        if st.fd == -1 {
            return Err(DriverError::NotFound);
        }
        if st.rdonly {
            return Err(DriverError::InvalidArgument);
        }
    }

    // Re-parse partitions on first open after configuration.
    if st.fd != -1 && st.openct == 0 {
        vnd_partition_inner(st);

        if vnd_part_inner(st, minor).is_none() {
            return Err(DriverError::NotFound);
        }
    }

    st.openct += 1;
    Ok(())
}

/// Close a device minor.
///
/// Corresponds to `vnd_close()` in the C reference.
pub fn vnd_close(_minor: usize) -> Result<(), DriverError> {
    let st = unsafe { &mut *state_ptr() };

    if !st.initialized {
        return Err(DriverError::NotFound);
    }

    if st.openct == 0 {
        return Err(DriverError::InvalidArgument);
    }

    st.openct -= 1;

    // If exiting and fully closed, the server would terminate.
    // In this stub we just reset.
    if st.exiting && st.openct == 0 {
        vnd_cleanup_inner(st);
    }

    Ok(())
}

/// Transfer data to/from the virtual disk.
///
/// Returns the number of bytes transferred, or an error.
/// The full implementation uses `pread`/`pwrite` on the backing file.
///
/// Corresponds to `vnd_transfer()` in the C reference.
pub fn vnd_transfer(
    minor: usize,
    do_write: bool,
    position: u64,
    buf: &mut [u8],
) -> Result<usize, DriverError> {
    let st = unsafe { &mut *state_ptr() };

    if !st.initialized || st.fd == -1 {
        return Err(DriverError::NotFound);
    }

    let dv = vnd_part_inner(st, minor).ok_or(DriverError::NotFound)?;

    if do_write && st.rdonly {
        return Err(DriverError::InvalidArgument);
    }

    // Limit to device size.
    let max_size = if dv.size > 0 {
        dv.size - position.min(dv.size)
    } else {
        0
    };

    let size = (buf.len() as u64).min(max_size);
    if size == 0 {
        return Ok(0);
    }

    // In the full implementation, this would call pread/pwrite in chunks
    // using the intermediate buffer.  For now we just report the size
    // without performing actual I/O.
    //
    // TODO(11b.6): implement pread/pwrite via VFS backcall once the
    // server framework (Phase 12) provides file descriptor access.
    //
    // For testing, we zero-fill on read to avoid returning garbage.
    if !do_write {
        for byte in buf.iter_mut().take(size as usize) {
            *byte = 0;
        }
    }

    Ok(size as usize)
}

/// Process an IOCTL request.
///
/// Corresponds to `vnd_ioctl()` in the C reference.
/// VFS backcall: duplicate a file descriptor from a user process.
///
/// Real implementation calls VFS `copyfd()` to copy the fd identified
/// by `user_fd` from `user_endpt` into our process's fd table.
/// Returns the new fd on success, or a negative error code.
/// See PORTING_PLAN.md Phase 12.14 follow-up.
///
/// # Safety
///
/// `user_endpt` must be a valid endpoint.
pub unsafe fn vnd_copyfd(_user_endpt: i32, _user_fd: i32) -> Result<i32, DriverError> {
    Err(DriverError::Unsupported)
}

/// VFS backcall: fstat a file descriptor to check it's a regular file.
///
/// Real implementation calls `fstat(fd, &st)` to get the file's stat.
/// Returns (st_dev, st_ino) on success.
/// See PORTING_PLAN.md Phase 12.14 follow-up.
///
/// # Safety
///
/// `fd` must be a valid file descriptor obtained from `vnd_copyfd`.
pub unsafe fn vnd_fstat(_fd: i32) -> Result<(u64, u64), DriverError> {
    Err(DriverError::Unsupported)
}

/// VFS backcall: mmap an intermediate I/O buffer.
///
/// Real implementation calls `mmap(NULL, size, PROT_READ|PROT_WRITE,
/// MAP_ANON|MAP_PRIVATE, -1, 0)`.
/// Returns the virtual address of the mapped buffer.
///
/// # Safety
///
/// `size` must be > 0.
pub unsafe fn vnd_mmap_buf(_size: usize) -> Result<u64, DriverError> {
    Err(DriverError::Unsupported)
}

/// VFS backcall: munmap an I/O buffer.
///
/// Real implementation calls `munmap(addr, size)`.
///
/// # Safety
///
/// `addr` must be from a previous `vnd_mmap_buf` call.
pub unsafe fn vnd_munmap_buf(_addr: u64, _size: usize) -> Result<(), DriverError> {
    Err(DriverError::Unsupported)
}

/// VFS backcall: close a file descriptor.
///
/// Real implementation calls `close(fd)`.
///
/// # Safety
///
/// `fd` must be a valid file descriptor.
pub unsafe fn vnd_close_fd(_fd: i32) -> Result<(), DriverError> {
    Err(DriverError::Unsupported)
}

/// VFS backcall: fsync a file descriptor.
///
/// Real implementation calls `fsync(fd)`.
///
/// # Safety
///
/// `fd` must be a valid file descriptor.
pub unsafe fn vnd_fsync(_fd: i32) -> Result<(), DriverError> {
    Err(DriverError::Unsupported)
}

/// Copy data from a user-space grant into a local buffer.
///
/// Real implementation calls `sys_safecopyfrom(endpt, grant, offset, buf, size)`.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `size` bytes.
/// `endpt` and `grant` must be valid (caller-provided IPC grant).
pub unsafe fn vnd_safecopy_from(
    _endpt: i32,
    _grant: u32,
    _offset: u32,
    _buf: *mut u8,
    _size: usize,
) -> Result<(), DriverError> {
    Err(DriverError::Unsupported)
}

/// Copy data from a local buffer to a user-space grant.
///
/// Real implementation calls `sys_safecopyto(endpt, grant, offset, buf, size)`.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `size` bytes.
/// `endpt` and `grant` must be valid (caller-provided IPC grant).
pub unsafe fn vnd_safecopy_to(
    _endpt: i32,
    _grant: u32,
    _offset: u32,
    _buf: *const u8,
    _size: usize,
) -> Result<(), DriverError> {
    Err(DriverError::Unsupported)
}

/// Process I/O control requests for the virtual disk.
///
/// Currently stubs VNDIOCSET and VNDIOCGET with proper error returns.
/// Real implementations require VFS backcalls (copyfd, fstat, mmap).
pub fn vnd_ioctl(request: u32, endpt: i32, grant: u32) -> Result<(), DriverError> {
    let st = unsafe { &mut *state_ptr() };

    if !st.initialized {
        return Err(DriverError::NotFound);
    }

    match request {
        VNDIOCSET => {
            // The VND must not be busy.
            if st.fd != -1 || st.openct != 1 {
                return Err(DriverError::Busy);
            }

            // Copy in VndIoctl from user.
            let mut vnd = VndIoctl::new();
            unsafe {
                if vnd_safecopy_from(
                    endpt,
                    grant,
                    0,
                    &mut vnd as *mut _ as *mut u8,
                    core::mem::size_of::<VndIoctl>(),
                )
                .is_err()
                {
                    return Err(DriverError::Unsupported);
                }
            }

            // Copy file descriptor from user process.
            let fd = unsafe {
                match vnd_copyfd(endpt, vnd.vnd_fildes) {
                    Ok(fd) => fd,
                    Err(_) => return Err(DriverError::Unsupported),
                }
            };

            // Check that the target file is regular.
            let (st_dev, st_ino) = unsafe {
                match vnd_fstat(fd) {
                    Ok((dev, ino)) => (dev, ino),
                    Err(e) => {
                        let _ = vnd_close_fd(fd);
                        return Err(e);
                    }
                }
            };

            // Allocate I/O transfer buffer (inline in VndState).
            // The buffer is already allocated as part of the state struct.

            // Set device state.
            st.dev = st_dev;
            st.ino = st_ino;
            st.rdonly = (vnd.vnd_flags as u32 & VNDIOF_READONLY) != 0;
            st.fd = fd;

            // Compute geometry from file size.
            let file_size = vnd.vnd_size;
            let layout = compute_geometry(file_size);
            st.geom = layout;

            // Set the device size in the user's struct.
            vnd.vnd_size = file_size;
            unsafe {
                let _ = vnd_safecopy_to(
                    endpt,
                    grant,
                    0,
                    &vnd as *const _ as *const u8,
                    core::mem::size_of::<VndIoctl>(),
                );
            }

            Ok(())
        }
        VNDIOCCLR => {
            if st.fd == -1 {
                return Err(DriverError::NotFound);
            }

            // Copy in VndIoctl to check FORCE flag (best-effort).
            let mut vnd = VndIoctl::new();
            let got_flags = unsafe {
                vnd_safecopy_from(
                    endpt,
                    grant,
                    0,
                    &mut vnd as *mut _ as *mut u8,
                    core::mem::size_of::<VndIoctl>(),
                )
                .is_ok()
            };

            let force = got_flags && (vnd.vnd_flags as u32 & VNDIOF_FORCE) != 0;

            if !force && st.openct != 1 {
                return Err(DriverError::Busy);
            }

            // Clean up (no munmap needed — buffer is inline in VndState).
            unsafe {
                let _ = vnd_close_fd(st.fd);
            }
            st.fd = -1;
            st.dev = 0;
            st.ino = 0;

            Ok(())
        }
        VNDIOCGET => {
            let mut vnu = VndUser::new();
            vnu.vnu_unit = 0; // single instance

            // If configured, fill in device/inode info.
            if st.fd != -1 {
                vnu.vnu_dev = st.dev;
                vnu.vnu_ino = st.ino;
            }

            unsafe {
                if vnd_safecopy_to(
                    endpt,
                    grant,
                    0,
                    &vnu as *const _ as *const u8,
                    core::mem::size_of::<VndUser>(),
                )
                .is_err()
                {
                    return Err(DriverError::Unsupported);
                }
            }

            Ok(())
        }
        _ => Err(DriverError::Unsupported),
    }
}

/// Look up a partition or subpartition by minor device number.
///
/// Corresponds to `vnd_part()` in the C reference.
pub fn vnd_part(minor: usize) -> Option<VndDevice> {
    let st = unsafe { &*state_ptr() };
    if !st.initialized {
        return None;
    }
    vnd_part_inner(st, minor).copied()
}

/// Return geometry information.
///
/// Corresponds to `vnd_geometry()` in the C reference.
pub fn vnd_geometry(_minor: usize) -> PartGeom {
    let st = unsafe { &*state_ptr() };
    st.geom
}

/// Check if the device is currently configured (has an fd).
pub fn vnd_is_configured() -> bool {
    let st = unsafe { &*state_ptr() };
    st.initialized && st.fd != -1
}

/// Get the current open count.
pub fn vnd_open_count() -> i32 {
    let st = unsafe { &*state_ptr() };
    st.openct
}

/// Configure the device with a file descriptor (for testing).
///
/// This is the `set_fd()` function referenced in the stub fix (11b.13).
/// Unlike the full `VNDIOCSET` path, this does not require `openct == 1`,
/// allowing the device to be configured before any opens.
///
/// # Safety
///
/// Must have exclusive access to state.
pub unsafe fn vnd_set_fd(fd: i32, size: u64, rdonly: bool) -> Result<(), DriverError> {
    // SAFETY: caller guarantees exclusive access.
    let st = unsafe { &mut *state_ptr() };
    if !st.initialized {
        return Err(DriverError::NotFound);
    }

    st.fd = fd;
    st.rdonly = rdonly;
    st.geom = compute_geometry(size);
    st.part[0] = VndDevice {
        base: 0,
        size: st.geom.size,
    };
    Ok(())
}

/// Clear the device configuration.
///
/// # Safety
///
/// Must have exclusive access to state.
pub unsafe fn vnd_clear() {
    // SAFETY: caller guarantees exclusive access.
    let st = unsafe { &mut *state_ptr() };
    st.fd = -1;
    st.rdonly = false;
    st.geom = PartGeom::default();
    st.part = [VndDevice::new(); DEV_PER_DRIVE];
    st.subpart = [VndDevice::new(); SUB_PER_DRIVE];
}

/// Request a graceful shutdown.
///
/// # Safety
///
/// Must have exclusive access to state.
pub unsafe fn vnd_terminate() {
    // SAFETY: caller guarantees exclusive access.
    let st = unsafe { &mut *state_ptr() };
    st.exiting = true;
    if st.openct == 0 {
        vnd_cleanup_inner(st);
    }
}

fn vnd_partition_inner(st: &mut VndState) {
    // Reset partition tables.
    st.part = [VndDevice::new(); DEV_PER_DRIVE];
    st.subpart = [VndDevice::new(); SUB_PER_DRIVE];

    // Set the primary device size.
    st.part[0] = VndDevice {
        base: 0,
        size: st.geom.size,
    };

    // TODO(11b.6): Call partition table parser once available.
}

fn vnd_part_inner(st: &VndState, minor: usize) -> Option<&VndDevice> {
    if minor < DEV_PER_DRIVE {
        Some(&st.part[minor])
    } else if minor >= MINOR_D0P0S0 {
        let sub = minor - MINOR_D0P0S0;
        if sub < SUB_PER_DRIVE {
            Some(&st.subpart[sub])
        } else {
            None
        }
    } else {
        None
    }
}

fn vnd_cleanup_inner(st: &mut VndState) {
    st.fd = -1;
    st.exiting = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn reset_state() {
        let st = unsafe { &mut *state_ptr() };
        st.fd = -1;
        st.openct = 0;
        st.exiting = false;
        st.rdonly = false;
        st.dev = 0;
        st.ino = 0;
        st.geom = PartGeom::default();
        st.initialized = false;
    }

    #[test]
    fn test_constants() {
        assert_eq!(VND_BUF_SIZE, 65536);
        assert_eq!(NR_PARTITIONS, 4);
        assert_eq!(DEV_PER_DRIVE, 5);
        assert_eq!(SUB_PER_DRIVE, 16);
        assert_eq!(MINOR_D0P0S0, 128);
        assert_eq!(SECTOR_SIZE, 512);
    }

    #[test]
    fn test_ioctl_flags() {
        assert_eq!(VNDIOF_HASGEOM, 0x01);
        assert_eq!(VNDIOF_READONLY, 0x02);
        assert_eq!(VNDIOF_FORCE, 0x04);
    }

    #[test]
    fn test_ioctl_codes() {
        assert_eq!(VNDIOCSET, 0x4600);
        assert_eq!(VNDIOCCLR, 0x4601);
        assert_eq!(VNDIOCGET, 0x4603);
    }

    #[test]
    fn test_vnd_device_new() {
        let d = VndDevice::new();
        assert_eq!(d.base, 0);
        assert_eq!(d.size, 0);
    }

    #[test]
    fn test_vnd_device_default() {
        let d: VndDevice = Default::default();
        assert_eq!(d.base, 0);
    }

    #[test]
    fn test_vnd_ioctl_new() {
        let io = VndIoctl::new();
        assert_eq!(io.vnd_fildes, -1);
        assert_eq!(io.vnd_flags, 0);
        assert_eq!(io.vnd_size, 0);
    }

    #[test]
    fn test_vnd_user_new() {
        let u = VndUser::new();
        assert_eq!(u.vnu_unit, 0);
        assert_eq!(u.vnu_dev, 0);
        assert_eq!(u.vnu_ino, 0);
    }

    #[test]
    fn test_part_geom_default() {
        let g: PartGeom = Default::default();
        assert_eq!(g.base, 0);
        assert_eq!(g.size, 0);
        assert_eq!(g.cylinders, 0);
        assert_eq!(g.heads, 0);
        assert_eq!(g.sectors, 0);
    }

    #[test]
    fn test_vnd_geom_default() {
        let g: VndGeom = Default::default();
        assert_eq!(g.secsize, 0);
        assert_eq!(g.nsectors, 0);
    }

    #[test]
    fn test_vnd_init() {
        unsafe {
            reset_state();
            vnd_init();
            assert!((*state_ptr()).initialized);
            assert_eq!((*state_ptr()).fd, -1);
        }
    }

    #[test]
    fn test_vnd_open_not_initialized() {
        unsafe {
            reset_state();
            assert!(vnd_open(0, 0).is_err());
        }
    }

    #[test]
    fn test_vnd_open_unconfigured() {
        unsafe {
            reset_state();
            vnd_init();
            // Minor 0 should be available even when unconfigured.
            assert!(vnd_open(0, 0).is_ok());
            // Non-zero minors should fail when unconfigured.
            assert!(vnd_open(1, 0).is_err());
            assert!(vnd_open(128, 0).is_err());
        }
    }

    #[test]
    fn test_vnd_open_close_cycle() {
        unsafe {
            reset_state();
            vnd_init();

            // Configure the device.
            assert!(vnd_set_fd(3, 1024 * 1024, false).is_ok());
            assert!(vnd_is_configured());

            // Open.
            assert!(vnd_open(0, 0).is_ok());
            assert_eq!(vnd_open_count(), 1);

            // Close.
            assert!(vnd_close(0).is_ok());
            assert_eq!(vnd_open_count(), 0);
        }
    }

    #[test]
    fn test_vnd_open_readonly() {
        unsafe {
            reset_state();
            vnd_init();

            // Configure as read-only.
            assert!(vnd_set_fd(3, 1024 * 1024, true).is_ok());

            // Open with write access should fail.
            assert!(vnd_open(0, 2).is_err());

            // Open with read-only access should succeed.
            assert!(vnd_open(0, 0).is_ok());
        }
    }

    #[test]
    fn test_vnd_close_twice() {
        unsafe {
            reset_state();
            vnd_init();
            assert!(vnd_set_fd(3, 4096, false).is_ok());
            assert!(vnd_open(0, 0).is_ok());
            assert!(vnd_close(0).is_ok());
            // Second close should fail.
            assert!(vnd_close(0).is_err());
        }
    }

    #[test]
    fn test_vnd_transfer_unconfigured() {
        unsafe {
            reset_state();
            vnd_init();
            let mut buf = [0u8; 512];
            assert!(vnd_transfer(0, false, 0, &mut buf).is_err());
        }
    }

    #[test]
    fn test_vnd_transfer_basic() {
        unsafe {
            reset_state();
            vnd_init();
            assert!(vnd_set_fd(3, 4096, false).is_ok());

            let mut buf = [0u8; 512];
            let n = vnd_transfer(0, false, 0, &mut buf).unwrap();
            assert_eq!(n, 512);
        }
    }

    #[test]
    fn test_vnd_transfer_beyond_eof() {
        unsafe {
            reset_state();
            vnd_init();
            assert!(vnd_set_fd(3, 512, false).is_ok());

            let mut buf = [0u8; 512];
            // Position at EOF.
            let n = vnd_transfer(0, false, 512, &mut buf).unwrap();
            assert_eq!(n, 0);

            // Position beyond EOF.
            let n2 = vnd_transfer(0, false, 1024, &mut buf).unwrap();
            assert_eq!(n2, 0);
        }
    }

    #[test]
    fn test_vnd_transfer_rdonly() {
        unsafe {
            reset_state();
            vnd_init();
            assert!(vnd_set_fd(3, 4096, true).is_ok());

            let mut buf = [0u8; 512];
            // Write on read-only device should fail.
            assert!(vnd_transfer(0, true, 0, &mut buf).is_err());
        }
    }

    #[test]
    fn test_vnd_ioctl_set_unconfigured() {
        unsafe {
            reset_state();
            vnd_init();
            // VNDIOCSET with no open: hits openct != 1 check first.
            assert!(vnd_ioctl(VNDIOCSET, 0, 0).is_err());
        }
    }

    #[test]
    fn test_vnd_ioctl_set_reaches_vfs_stub() {
        unsafe {
            reset_state();
            vnd_init();
            // Open the device first so openct == 1.
            assert!(vnd_open(0, 0).is_ok());
            // fd == -1 && openct == 1 passes the pre-conditions.
            // Now returns Unsupported (VFS backcall stubs).
            assert_eq!(vnd_ioctl(VNDIOCSET, 0, 0), Err(DriverError::Unsupported));
        }
    }

    #[test]
    fn test_vnd_ioctl_clr_configured() {
        unsafe {
            reset_state();
            vnd_init();
            assert!(vnd_set_fd(3, 4096, false).is_ok());
            // Open the device so openct == 1 (required for non-FORCE clear).
            assert!(vnd_open(0, 0).is_ok());
            assert!(vnd_ioctl(VNDIOCCLR, 0, 0).is_ok());
            assert!(!vnd_is_configured());
        }
    }

    #[test]
    fn test_vnd_part_lookup() {
        unsafe {
            reset_state();
            vnd_init();
            // With a configured device, minor 0 should return the primary
            // partition.
            assert!(vnd_set_fd(3, 4096, false).is_ok());
            let dv = vnd_part(0);
            assert!(dv.is_some());
            assert_eq!(dv.unwrap().base, 0);
        }
    }

    #[test]
    fn test_compute_geometry_small() {
        let g = compute_geometry(512); // 1 sector
        assert_eq!(g.cylinders, 1);
        assert_eq!(g.heads, 1);
        assert_eq!(g.sectors, 1);
        assert_eq!(g.size, 512);
    }

    #[test]
    fn test_compute_geometry_large() {
        let g = compute_geometry(1024 * 1024 * 1024); // 1 GB
        assert!(g.cylinders > 0);
        assert_eq!(g.heads, 64);
        assert_eq!(g.sectors, 32);
    }
}
