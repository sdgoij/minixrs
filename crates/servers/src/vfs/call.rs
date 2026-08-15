//! VFS call handler functions â€” adapted from the following C sources:
//!
//! | Category         | Source file     | Functions                                 |
//! |------------------|-----------------|-------------------------------------------|
//! | File operations  | `open.c`        | `do_open`, `do_creat`, `do_close`, `do_lseek`, `do_mknod`, `do_mkdir` |
//! | File operations  | `read.c`        | `do_read`, `do_getdents`                  |
//! | File operations  | `write.c`       | `do_write`                                |
//! | File operations  | `pipe.c`        | `do_pipe2`                                |
//! | File operations  | `link.c`        | `do_link`, `do_unlink`, `do_rename`, `do_truncate`, `do_ftruncate`, `do_rdlink` |
//! | File operations  | `select.c`      | `do_select`                               |
//! | Directory ops    | `stadir.c`      | `do_chdir`, `do_fchdir`, `do_chroot`, `do_stat`, `do_fstat`, `do_lstat`, `do_statvfs`, `do_fstatvfs`, `do_getvfsstat` |
//! | Permission ops   | `protect.c`     | `do_access`, `do_chmod`, `do_chown`, `do_umask` |
//! | Mount ops        | `mount.c`       | `do_mount`, `do_umount`                   |
//! | Mount ops        | `dmap.c`        | `do_mapdriver`                            |
//! | Time ops         | `time.c`        | `do_utimens`                              |
//! | Misc ops         | `misc.c`        | `do_fcntl`, `do_sync`, `do_fsync`, `do_svrctl`, `do_getsysinfo`, `do_vm_call`, `do_getrusage` |
//! | Misc ops         | `gcov.c`        | `do_gcov_flush`                           |
//! | Lock ops         | `lock.c`        | `lock_op`                                 |

extern crate alloc;

use crate::vfs::consts::*;
use crate::vfs::filedes;
use crate::vfs::glo::vfs_global;
use crate::vfs::mount;
use crate::vfs::path;
use crate::vfs::path::PATH_RET_SYMLINK;
use crate::vfs::stadir::close_fd;
use crate::vfs::types::*;
use minix_std::fs::Stat;

/// Common: fd field offset in payload.
const FD_OFF: usize = 8;
/// lseek: offset (u64).
const LSEEK_OFF_OFF: usize = 12;

/// SELF endpoint constant (from kernel::system::SELF).
pub(crate) const SELF: i32 = 31742;
/// SYS_VIRCOPY kernel call number.
#[cfg_attr(not(target_os = "minix"), allow(dead_code))]
const SYS_VIRCOPY: i32 = 15;
/// O_EXCL open flag (from `minix/include/fcntl.h`).
const O_EXCL: u32 = 0o200;
/// O_TRUNC open flag (from `minix/include/fcntl.h`).
const O_TRUNC: u32 = 0o1000;
/// CP_FLAG_TRY: direct copy without VM fallback.
const CP_FLAG_TRY: i32 = 0x01;
/// SYS_VIRCOPY message field offsets (matches kernel/src/system.rs)
// NOTE: offset 0-7 is reserved for the kernel_call header
// (internal call number at 0-3, source endpoint at 4-7).
// All COPY_* fields must start at offset >= 8.
const COPY_SRC_ADDR_OFF: usize = 8;
const COPY_DST_ENDPT_OFF: usize = 16;
const COPY_DST_ADDR_OFF: usize = 24;
const COPY_NR_BYTES_OFF: usize = 32;
const COPY_FLAGS_OFF: usize = 40;
const COPY_SRC_ENDPT_OFF: usize = 48;

/// Perform a SYS_VIRCOPY kernel call to copy data between address spaces.
/// Runs the copy in ring 0 via the kernel call dispatch mechanism.
/// Safety: see `kernel::vm::virtual_copy`.
pub(crate) unsafe fn sys_vircopy(
    src_endpt: i32,
    src_addr: u64,
    dst_endpt: i32,
    dst_addr: u64,
    bytes: usize,
) -> i32 {
    let mut msg = [0u8; 64];
    msg[COPY_SRC_ENDPT_OFF..COPY_SRC_ENDPT_OFF + 4].copy_from_slice(&src_endpt.to_ne_bytes());
    msg[COPY_SRC_ADDR_OFF..COPY_SRC_ADDR_OFF + 8].copy_from_slice(&src_addr.to_ne_bytes());
    msg[COPY_DST_ENDPT_OFF..COPY_DST_ENDPT_OFF + 4].copy_from_slice(&dst_endpt.to_ne_bytes());
    msg[COPY_DST_ADDR_OFF..COPY_DST_ADDR_OFF + 8].copy_from_slice(&dst_addr.to_ne_bytes());
    msg[COPY_NR_BYTES_OFF..COPY_NR_BYTES_OFF + 8].copy_from_slice(&(bytes as u64).to_ne_bytes());
    msg[COPY_FLAGS_OFF..COPY_FLAGS_OFF + 4].copy_from_slice(&CP_FLAG_TRY.to_ne_bytes());
    #[cfg(target_os = "minix")]
    {
        minix_rt::kernel_call(SYS_VIRCOPY, &mut msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = &msg;
        -38 // ENOSYS
    }
}
/// lseek: whence (i32).
const LSEEK_WHENCE_OFF: usize = 20;
/// fcntl: cmd (i32).
const FCNTL_CMD_OFF: usize = 12;
/// fcntl: arg (i32).
const FCNTL_ARG_OFF: usize = 16;
/// copyfd: newfd (i32).
const COPYFD_NEWFD_OFF: usize = 12;
/// umask: mode (i32).
const UMASK_MODE_OFF: usize = 12;

fn r_i32(buf: &[u8; 64], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn r_u32(buf: &[u8; 64], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn r_u64(buf: &[u8; 64], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}

/// Get the current Fproc pointer. Returns None if null.
fn current_fp() -> Option<&'static mut Fproc> {
    unsafe { (*vfs_global()).fp.as_mut() }
}

// File operations

/// Perform the `open(name, flags)` system call (O_CREAT *not* set).
///
/// C source: `minix/servers/vfs/open.c` â€” `do_open()` (line 39)
/// Perform the `open(name, flags)` system call (O_CREAT *not* set).
///
/// C source: `minix/servers/vfs/open.c` â€” `do_open()` (line 39)
pub fn do_open() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };

    // Message layout: flags (offset 8), path_addr (offset 16), path_len (offset 24)
    let flags = r_i32(&glob.fs_m_in, 8) as u32;
    let path_addr = r_u64(&glob.fs_m_in, 16);
    let path_len = r_u32(&glob.fs_m_in, 24) as usize;

    // Reject O_CREAT (use do_creat instead).
    if flags & (1 << 12) != 0 {
        return EINVAL;
    }

    // Copy path from userspace.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    if path_addr == 0 || copy_len == 0 {
        return ENOENT;
    }
    let copy_r = unsafe {
        sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        )
    };
    if copy_r != 0 {
        return ENOENT;
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    if actual_len == 0 {
        return ENOENT;
    }

    // Get a free fd slot.
    let mut fd = 0i32;
    let r = unsafe { filedes::get_fd(fp, 0, &mut fd) };
    if r != OK {
        return r;
    }

    // Resolve the path via the FS request layer (req_lookup inside eat_path).
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        fp.fp_filp[fd as usize] = -1;
        return ENOENT;
    }

    // Compute access mode bits (R_BIT/W_BIT — the same bits forbidden()
    // takes) and check the caller's permission before opening.
    let mode_bits = match flags & 0o3 {
        0 => crate::vfs::protect::R_BIT, // O_RDONLY
        1 => crate::vfs::protect::W_BIT, // O_WRONLY
        2 => crate::vfs::protect::R_BIT | crate::vfs::protect::W_BIT, // O_RDWR
        _ => {
            unsafe { mount::put_vnode(vp) };
            fp.fp_filp[fd as usize] = -1;
            return EINVAL;
        }
    };
    let r = crate::vfs::protect::forbidden(fp, unsafe { &*vp }, mode_bits, false);
    if r != OK {
        unsafe { mount::put_vnode(vp) };
        fp.fp_filp[fd as usize] = -1;
        return r;
    }

    // Character devices: route the open to the registered driver before
    // wiring a filp. The filp still references the device vnode so that
    // close/dup/fcntl behave normally; reads, writes and close dispatch on
    // the vnode mode. Socket drivers reply CDEV_CLONED with a fresh minor —
    // the filp records the resulting device number and datagram-ness.
    let mut dev = unsafe { (*vp).v_dev };
    let mut open_flags = 0u32;
    if unsafe { (*vp).v_mode & S_IFMT } == S_IFCHR {
        let r =
            unsafe { crate::vfs::device::cdev_open((*vp).v_dev, flags as i32, &mut open_flags) };
        if r < 0 {
            unsafe { mount::put_vnode(vp) };
            fp.fp_filp[fd as usize] = -1;
            return r;
        }
        dev = r as u32;
    }

    // Allocate a filp entry.
    let filp_idx = unsafe { filedes::alloc_filp() };
    if filp_idx < 0 {
        unsafe { mount::put_vnode(vp) };
        fp.fp_filp[fd as usize] = -1;
        return filp_idx;
    }

    // Set up the filp and fd.
    unsafe {
        let glob = vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        (*filp_arr.add(filp_idx as usize)).filp_count = 1;
        (*filp_arr.add(filp_idx as usize)).filp_vno = vp;
        (*filp_arr.add(filp_idx as usize)).filp_flags = flags;
        (*filp_arr.add(filp_idx as usize)).filp_mode = mode_bits;
        (*filp_arr.add(filp_idx as usize)).filp_dev = dev;
        // The open reply flags the datagram channel with CDEV_DGRAM_OPEN;
        // convert it to the request flag cdev_io checks (CDEV_DGRAM).
        (*filp_arr.add(filp_idx as usize)).filp_dgram = if open_flags & CDEV_DGRAM_OPEN != 0 {
            CDEV_DGRAM
        } else {
            0
        };
    }

    // Release the vnode reference (the filp now holds it).
    unsafe { mount::put_vnode(vp) };

    fd
}

/// Perform the `creat(name, mode)` system call.
///
/// C source: `minix/servers/vfs/open.c` â€” `do_creat()` (line 59)
pub fn do_creat() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let open_flags = r_u32(&glob.fs_m_in, 24);
    let create_mode = r_u32(&glob.fs_m_in, 28);
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    // Null-terminate for easy string handling.
    path_buf[actual_len] = 0;
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let dirp = unsafe { path::last_dir(&resolve, fp) };
    if dirp.is_null() {
        return ENOENT;
    }
    // Find the filename (last component after the last '/' in path_buf).
    let file_name_ptr =
        if let Some(slash_pos) = path_buf[..actual_len].iter().rposition(|&b| b == b'/') {
            unsafe { path_buf.as_ptr().add(slash_pos + 1) }
        } else {
            path_buf.as_ptr()
        };
    let (r, _nd) = unsafe {
        crate::vfs::request::req_create(
            (*dirp).v_fs_e,
            (*dirp).v_inode_nr,
            create_mode as i32,
            fp.fp_effuid,
            fp.fp_effgid,
            file_name_ptr,
        )
    };
    unsafe { mount::put_vnode(dirp) };

    // C's `common_open`: a successful create means the file is new; EEXIST
    // from the FS means it already existed (an error only with O_EXCL). FS
    // servers report positive errno codes; VFS replies use negative errnos.
    let created;
    if r == OK {
        created = true;
    } else if r == -EEXIST {
        if open_flags & O_EXCL != 0 {
            return EEXIST;
        }
        created = false;
    } else if r > 0 {
        return -r;
    } else {
        return r;
    }

    // Get a free file descriptor before resolving the vnode.
    let mut fd = 0i32;
    let r2 = unsafe { filedes::get_fd(fp, 0, &mut fd) };
    if r2 != OK {
        return r2;
    }

    // Resolve the file (newly created, or pre-existing) to obtain a vnode.
    let mut resolve2 = Lookup::default();
    resolve2.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve2.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve2, fp) };
    if vp.is_null() {
        fp.fp_filp[fd as usize] = -1;
        return ENOENT;
    }

    // For an already-existing file, run the open-existing checks: verify
    // write permission, and truncate regular files when O_TRUNC is set.
    if !created {
        let r =
            crate::vfs::protect::forbidden(fp, unsafe { &*vp }, crate::vfs::protect::W_BIT, false);
        if r != OK {
            fp.fp_filp[fd as usize] = -1;
            unsafe { mount::put_vnode(vp) };
            return r;
        }
        let mode = unsafe { (*vp).v_mode };
        if mode & S_IFMT == S_IFREG && open_flags & O_TRUNC != 0 {
            let fs_e = unsafe { (*vp).v_fs_e };
            let inode_nr = unsafe { (*vp).v_inode_nr };
            let r = unsafe { crate::vfs::request::req_ftrunc(fs_e, inode_nr, 0, 0) };
            if r != OK {
                fp.fp_filp[fd as usize] = -1;
                unsafe { mount::put_vnode(vp) };
                return if r > 0 { -r } else { r };
            }
            unsafe { (*vp).v_size = 0 };
        } else if mode & S_IFMT == S_IFDIR {
            fp.fp_filp[fd as usize] = -1;
            unsafe { mount::put_vnode(vp) };
            return EISDIR;
        }
    }

    // Allocate a filp entry and wire up fd → filp → vnode.
    let filp_idx = unsafe { filedes::alloc_filp() };
    if filp_idx < 0 {
        fp.fp_filp[fd as usize] = -1;
        unsafe { mount::put_vnode(vp) };
        return filp_idx;
    }
    unsafe {
        let glob = vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        (*filp_arr.add(filp_idx as usize)).filp_count = 1;
        (*filp_arr.add(filp_idx as usize)).filp_vno = vp;
        (*filp_arr.add(filp_idx as usize)).filp_flags = open_flags;
        (*filp_arr.add(filp_idx as usize)).filp_mode = crate::vfs::protect::W_BIT;
    }
    fp.fp_filp[fd as usize] = filp_idx;
    unsafe { mount::put_vnode(vp) };
    fd
}

/// Perform the `close(fd)` system call.
///
/// C source: `minix/servers/vfs/open.c` â€” `do_close()` (line 664)
pub fn do_close() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let fd = r_i32(unsafe { &(*vfs_global()).fs_m_in }, FD_OFF);
    close_fd(fp, fd)
}

/// Perform the `lseek(fd, offset, whence)` system call.
///
/// C source: `minix/servers/vfs/open.c` â€” `do_lseek()` (line 143)
pub fn do_lseek() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let fd = r_i32(unsafe { &(*vfs_global()).fs_m_in }, FD_OFF);
    let _offset = r_u64(unsafe { &(*vfs_global()).fs_m_in }, LSEEK_OFF_OFF);
    let _whence = r_i32(unsafe { &(*vfs_global()).fs_m_in }, LSEEK_WHENCE_OFF);

    // Validate fd.
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }

    // Update the filp position.
    let filp_ptr = unsafe {
        let glob = &mut *vfs_global();
        let p = core::ptr::addr_of_mut!(glob.filp) as *mut Filp;
        p.add(filp_idx as usize)
    };
    unsafe {
        match _whence {
            0 => (*filp_ptr).filp_pos = _offset as i64,
            1 => (*filp_ptr).filp_pos += _offset as i64,
            2 => {
                let vp = (*filp_ptr).filp_vno;
                if vp.is_null() {
                    return EBADF;
                }
                let fsize = (*vp).v_size;
                (*filp_ptr).filp_pos = fsize + _offset as i64;
            }
            _ => return EINVAL,
        }
        (*filp_ptr).filp_pos as i32
    }
}

/// Perform the `read(fd, buf, count)` system call.
///
/// C source: `minix/servers/vfs/read.c` â€” `do_read()` (line 31)
pub fn do_read() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let buf_addr = r_u64(&glob.fs_m_in, 16);
    let count = r_u32(&glob.fs_m_in, 24) as usize;

    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }

    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        let filp = &mut *filp_arr.add(filp_idx as usize);
        if (filp.filp_mode & crate::vfs::protect::R_BIT) == 0 {
            return EBADF;
        }
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        // Pipe reads come from the in-VFS ring buffer.
        if crate::vfs::pipe::is_pipe_filp(filp.filp_pipe_ino) {
            let pipe_idx = crate::vfs::pipe::pipe_index_from_filp(filp.filp_pipe_ino);
            return crate::vfs::pipe::pipe_read_user(pipe_idx, fp.fp_endpoint, buf_addr, count);
        }
        // Character devices: route through the registered driver.
        if ((*vp).v_mode & S_IFMT) == S_IFCHR {
            return crate::vfs::device::cdev_io(
                crate::vfs::consts::CDEV_READ,
                filp.filp_dev,
                fp.fp_endpoint,
                buf_addr,
                filp.filp_pos,
                count as u64,
                filp.filp_dgram as i32,
            );
        }
        // Call the FS request layer to perform the read.
        let (r, new_pos) = crate::vfs::request::req_read(
            (*vp).v_fs_e,
            (*vp).v_inode_nr,
            buf_addr as *mut u8,
            filp.filp_pos,
            count as u32,
            fp.fp_endpoint,
            0,
        );
        if r >= 0 {
            filp.filp_pos = new_pos;
        }
        r
    }
}

/// Perform the `write(fd, buf, count)` system call.
///
/// C source: `minix/servers/vfs/read.c` â€” `read_write()` (line 132)
pub fn do_write() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let buf_addr = r_u64(&glob.fs_m_in, 16);
    let count = r_u32(&glob.fs_m_in, 24) as usize;

    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }

    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        let filp = &mut *filp_arr.add(filp_idx as usize);
        if (filp.filp_mode & crate::vfs::protect::W_BIT) == 0 {
            return EBADF;
        }
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        // Pipe writes go into the in-VFS ring buffer.
        if crate::vfs::pipe::is_pipe_filp(filp.filp_pipe_ino) {
            let pipe_idx = crate::vfs::pipe::pipe_index_from_filp(filp.filp_pipe_ino);
            return crate::vfs::pipe::pipe_write_user(pipe_idx, fp.fp_endpoint, buf_addr, count);
        }
        // Character devices: route through the registered driver.
        if ((*vp).v_mode & S_IFMT) == S_IFCHR {
            return crate::vfs::device::cdev_io(
                crate::vfs::consts::CDEV_WRITE,
                filp.filp_dev,
                fp.fp_endpoint,
                buf_addr,
                filp.filp_pos,
                count as u64,
                filp.filp_dgram as i32,
            );
        }
        let (r, new_pos) = crate::vfs::request::req_write(
            (*vp).v_fs_e,
            (*vp).v_inode_nr,
            buf_addr as *const u8,
            filp.filp_pos,
            count as u32,
            fp.fp_endpoint,
            0,
        );
        if r >= 0 {
            filp.filp_pos = new_pos;
        }
        r
    }
}

/// Perform the `getdents(fd, buf, count)` system call.
///
/// C source: `minix/servers/vfs/read.c` — `do_getdents()` (line 269)
pub fn do_getdents() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let buf_addr = r_u64(&glob.fs_m_in, 16);
    let count = r_u32(&glob.fs_m_in, 24) as usize;

    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }

    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        let filp = &mut *filp_arr.add(filp_idx as usize);
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        let (r, new_pos) = crate::vfs::request::req_getdents(
            (*vp).v_fs_e,
            (*vp).v_inode_nr,
            filp.filp_pos,
            buf_addr as *mut u8,
            count,
            0,
            fp.fp_endpoint,
        );
        if r >= 0 {
            filp.filp_pos = new_pos;
        }
        r
    }
}

/// Perform the `pipe2(flags)` system call.
pub fn do_pipe2() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let flags = r_i32(&glob.fs_m_in, 8);

    // Lock the PFS mount point.
    let vmp = mount::find_vmnt(PFS_PROC_NR);
    if vmp.is_null() {
        return ENOSYS;
    }

    // Allocate a vnode for the pipe.
    let vp = mount::get_free_vnode();
    if vp.is_null() {
        return ENFILE;
    }
    mount::lock_vnode(vp, VNODE_OPCL);

    // Acquire two file descriptors.
    let mut fd0 = 0i32;
    let r0 = unsafe { filedes::get_fd(fp, 0, &mut fd0) };
    if r0 != OK {
        mount::unlock_vnode(vp);
        return r0;
    }
    let mut fd1 = 0i32;
    let r1 = unsafe { filedes::get_fd(fp, 0, &mut fd1) };
    if r1 != OK {
        fp.fp_filp[fd0 as usize] = -1;
        mount::unlock_vnode(vp);
        return r1;
    }

    // Allocate filps and assign fds.
    let filp0 = unsafe { filedes::alloc_filp() };
    let filp1 = unsafe { filedes::alloc_filp() };
    if filp0 < 0 || filp1 < 0 {
        fp.fp_filp[fd0 as usize] = -1;
        fp.fp_filp[fd1 as usize] = -1;
        mount::unlock_vnode(vp);
        return ENFILE;
    }
    fp.fp_filp[fd0 as usize] = filp0;
    fp.fp_filp[fd1 as usize] = filp1;

    // Allocate a local pipe buffer (in-VFS, no separate PFS server).
    // Deviation from C: the original creates a named-pipe inode on PFS via
    // req_newnode; here the pipe data lives in VFS's own ring buffer, so no
    // PFS inode is needed and the vnode stays a local pipe vnode.
    let pipe_idx = match crate::vfs::pipe::alloc_pipe() {
        Some(idx) => idx,
        None => {
            fp.fp_filp[fd0 as usize] = -1;
            fp.fp_filp[fd1 as usize] = -1;
            mount::unlock_vnode(vp);
            return ENFILE;
        }
    };

    // Mark the vnode as a pipe and link filps to the pipe buffer.
    unsafe {
        (*vp).v_pipe = 1;
    }
    let pipe_ino = crate::vfs::pipe::pipe_index_for_filp(pipe_idx);
    let filp_arr = unsafe { core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp };
    // Read end (fd0)
    unsafe {
        (*filp_arr.add(filp0 as usize)).filp_pipe_ino = pipe_ino;
        (*filp_arr.add(filp0 as usize)).filp_mode = crate::vfs::protect::R_BIT;
        (*filp_arr.add(filp0 as usize)).filp_vno = vp;
    }
    // Write end (fd1)
    unsafe {
        (*filp_arr.add(filp1 as usize)).filp_pipe_ino = pipe_ino;
        (*filp_arr.add(filp1 as usize)).filp_mode = crate::vfs::protect::W_BIT;
        (*filp_arr.add(filp1 as usize)).filp_vno = vp;
    }

    // Apply flags to the pipe ends.
    let extra_flags = flags as u32 & !0o3;
    unsafe {
        (*filp_arr.add(filp0 as usize)).filp_flags = extra_flags;
        (*filp_arr.add(filp1 as usize)).filp_flags = extra_flags;
    }

    if (flags as u32) & 0x00400000 != 0 {
        fp.fp_cloexec |= 1u64 << fd0;
        fp.fp_cloexec |= 1u64 << fd1;
    }

    // Set pipe fds in fs_m_out for reply.
    unsafe {
        let glob = &mut *vfs_global();
        glob.fs_m_out[8..12].copy_from_slice(&fd0.to_le_bytes());
        glob.fs_m_out[12..16].copy_from_slice(&fd1.to_le_bytes());
    }

    OK
}

/// Perform the `ioctl(fd, request, arg)` system call.
///
/// C source: `minix/servers/vfs/device.c` â€” `do_ioctl()` (line 45)
pub fn do_ioctl() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let request = r_u32(&glob.fs_m_in, 12);
    let buf = r_u64(&glob.fs_m_in, 16);
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }
    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        let filp = &*filp_arr.add(filp_idx as usize);
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        let dev = filp.filp_dev;
        crate::vfs::device::cdev_io(
            CDEV_IOCTL,
            dev,
            fp.fp_endpoint,
            buf,
            0,
            request as u64,
            filp.filp_flags as i32,
        )
    }
}

/// Perform the `fcntl(fd, cmd, arg)` system call.
///
/// C source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` â€” `do_fcntl()` (line 110)
pub fn do_fcntl() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let fd = r_i32(unsafe { &(*vfs_global()).fs_m_in }, FD_OFF);
    let cmd = r_i32(unsafe { &(*vfs_global()).fs_m_in }, FCNTL_CMD_OFF);
    let _arg = r_i32(unsafe { &(*vfs_global()).fs_m_in }, FCNTL_ARG_OFF);

    match cmd {
        F_DUPFD => {
            // Duplicate fd â€” allocate the lowest free fd >= arg.
            let mut new_fd: i32 = 0;
            unsafe {
                let r = filedes::get_fd(fp, _arg.max(0), &mut new_fd);
                if r != OK {
                    return r;
                }
                let filp_idx = fp.fp_filp[fd as usize];
                if filp_idx < 0 {
                    return EBADF;
                }
                fp.fp_filp[new_fd as usize] = filp_idx;
                let glob = vfs_global();
                let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
                (*filp_arr.add(filp_idx as usize)).filp_count += 1;
            }
            new_fd
        }
        F_GETFD => {
            if fd < 0 || (fd as usize) >= OPEN_MAX || fp.fp_filp[fd as usize] < 0 {
                return EBADF;
            }
            if (fp.fp_cloexec >> fd) & 1 != 0 { 1 } else { 0 }
        }
        F_SETFD => {
            if fd < 0 || (fd as usize) >= OPEN_MAX || fp.fp_filp[fd as usize] < 0 {
                return EBADF;
            }
            if _arg & 1 != 0 {
                fp.fp_cloexec |= 1u64 << fd;
            } else {
                fp.fp_cloexec &= !(1u64 << fd);
            }
            OK
        }
        // File locking commands — delegate to lock_op.
        c if matches!(c, F_SETLK | F_SETLKW | F_GETLK) || c == F_UNLCK as i32 => unsafe {
            crate::vfs::lock::lock_op()
        },
        _ => ENOSYS,
    }
}

/// Perform the `copyfd(fd, newfd, flags)` â€” duplicate a file descriptor.
///
/// C source: `minix/servers/vfs/filedes.c` â€” `do_copyfd()` (line 82)
pub fn do_copyfd() -> i32 {
    let glob = unsafe { &*vfs_global() };
    let fp = match unsafe { glob.fp.as_mut() } {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let _newfd = r_i32(&glob.fs_m_in, COPYFD_NEWFD_OFF);

    // Validate source fd.
    if fd < 0 || (fd as usize) >= OPEN_MAX || fp.fp_filp[fd as usize] < 0 {
        return EBADF;
    }

    // Find a free fd slot starting from _newfd (or 0 if newfd < 0).
    let start = _newfd.max(0);
    let mut k: i32 = 0;
    unsafe {
        let r = filedes::get_fd(fp, start, &mut k);
        if r != OK {
            return r;
        }
        let filp_idx = fp.fp_filp[fd as usize];
        fp.fp_filp[k as usize] = filp_idx;
        let glob = vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        (*filp_arr.add(filp_idx as usize)).filp_count += 1;
    }
    k
}

/// Perform the `truncate(path, length)` system call.
///
/// C source: `minix/servers/vfs/link.c` â€” `do_truncate()` (line 91)
pub fn do_truncate() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let length = r_u64(&glob.fs_m_in, 24) as i64;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    let r = unsafe { crate::vfs::request::req_ftrunc((*vp).v_fs_e, (*vp).v_inode_nr, 0, length) };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `ftruncate(fd, length)` system call.
///
/// C source: `minix/servers/vfs/link.c` â€” `do_ftruncate()` (line 92)
pub fn do_ftruncate() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob_mut = unsafe { &mut *vfs_global() };
    let fd = r_i32(&glob_mut.fs_m_in, FD_OFF);
    let length = r_u64(&glob_mut.fs_m_in, 12) as i64;
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }
    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        let filp = &*filp_arr.add(filp_idx as usize);
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        crate::vfs::request::req_ftrunc((*vp).v_fs_e, (*vp).v_inode_nr, 0, length)
    }
}

/// Perform the `dup2(fd, newfd)` system call — make `newfd` a copy of `fd`,
/// closing `newfd` first if it is open.
///
/// C libc `dup2` is `close(fd2)` + `fcntl(F_DUPFD, fd2)`; that only yields
/// `fd2` when fds below it are occupied, which is not the case here (0..2
/// have no VFS mapping by default), so the exact-fd form is handled directly.
pub fn do_dup2() -> i32 {
    let glob = unsafe { &*vfs_global() };
    let fp = match unsafe { glob.fp.as_mut() } {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let newfd = r_i32(&glob.fs_m_in, COPYFD_NEWFD_OFF);

    if fd < 0 || (fd as usize) >= OPEN_MAX || fp.fp_filp[fd as usize] < 0 {
        return EBADF;
    }
    if newfd < 0 || (newfd as usize) >= OPEN_MAX {
        return EBADF;
    }
    if fd == newfd {
        return newfd;
    }
    // Close newfd if it is already open (POSIX dup2 semantics).
    if fp.fp_filp[newfd as usize] >= 0 {
        let r = close_fd(fp, newfd);
        if r != OK {
            return r;
        }
    }
    // Copy the fd table entry and bump the shared filp's refcount.
    let filp_idx = fp.fp_filp[fd as usize];
    fp.fp_filp[newfd as usize] = filp_idx;
    fp.fp_cloexec &= !(1u64 << newfd); // dup2 clears close-on-exec on newfd
    unsafe {
        let glob = vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        (*filp_arr.add(filp_idx as usize)).filp_count += 1;
    }
    newfd
}

/// Perform the `sync()` system call â€” flush all filesystem buffers.
///
/// Iterates all mounted filesystems and calls `req_sync` on each.
///
/// C source: `minix/servers/vfs/misc.c` â€” `do_sync()` (line 116)
pub fn do_sync() -> i32 {
    unsafe {
        let vmnt_arr = core::ptr::addr_of!((*vfs_global()).vmnt) as *const Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &*vmnt_arr.add(i);
            // Mounted check mirrors the C: m_fs_e != NONE and m_dev != NO_DEV.
            // The root's device is 0, so m_dev != 0 would skip it.
            if vmp.m_fs_e >= 0 && vmp.m_dev != u32::MAX {
                let _ = crate::vfs::request::req_sync(vmp.m_fs_e);
            }
        }
    }
    OK
}

/// Perform the `fsync(fd)` system call â€” flush a single file descriptor.
///
/// Validates the fd, gets the vnode from the filp, and calls `req_sync`
/// on the filesystem that owns the file.
///
/// C source: `minix/servers/vfs/misc.c` â€” `do_fsync()` (line 117)
pub fn do_fsync() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }
    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        let filp = &*filp_arr.add(filp_idx as usize);
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        crate::vfs::request::req_sync((*vp).v_fs_e)
    }
}

/// Perform the `select(nfds, readfds, writefds, errorfds, timeout)` call.
///
/// C source: `minix/servers/vfs/select.c` â€” `do_select()` (line 30)
pub fn do_select() -> i32 {
    unsafe { crate::vfs::select::do_select() }
}

/// Perform the `chdir(name)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_chdir()` (line 50)
pub fn do_chdir() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };

    // Parse path from message.
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;

    // Copy path from userspace.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        let r = sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        );
        if r != 0 {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);

    // Resolve the path.
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }

    // Check it's a directory.
    let mode = unsafe { (*vp).v_mode };
    if (mode & 0o170000) != 0o040000 {
        // S_IFDIR
        unsafe { mount::put_vnode(vp) };
        return ENOTDIR;
    }

    // Update fp_cdir.
    unsafe {
        // Release old cwd.
        if !fp.fp_cdir.is_null() {
            mount::put_vnode(fp.fp_cdir);
        }
        // Dup the new cwd.
        fp.fp_cdir = vp;
        mount::dup_vnode(vp);
    }

    unsafe { mount::put_vnode(vp) };
    OK
}

/// Perform the `fchdir(fd)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_fchdir()` (line 32)
pub fn do_fchdir() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);

    // Validate fd and get filp.
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }

    // Get the vnode from the filp.
    unsafe {
        let glob = vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        let filp = &*filp_arr.add(filp_idx as usize);
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }

        // Check it's a directory.
        let mode = (*vp).v_mode;
        if (mode & 0o170000) != 0o040000 {
            // S_IFDIR
            return ENOTDIR;
        }

        // Release old cwd.
        if !fp.fp_cdir.is_null() {
            mount::put_vnode(fp.fp_cdir);
        }
        // Dup the new cwd.
        fp.fp_cdir = vp;
        mount::dup_vnode(vp);
    }

    OK
}

/// Perform the `chroot(name)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_chroot()` (line 83)
pub fn do_chroot() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    // Only superuser may chroot.
    if fp.fp_effuid != SU_UID {
        return EPERM;
    }

    let glob = unsafe { &*vfs_global() };

    // Parse path from message.
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;

    // Copy path from userspace.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        let r = sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        );
        if r != 0 {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);

    // Resolve the path.
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }

    // Check it's a directory.
    let mode = unsafe { (*vp).v_mode };
    if (mode & 0o170000) != 0o040000 {
        // S_IFDIR
        unsafe { mount::put_vnode(vp) };
        return ENOTDIR;
    }

    // Update fp_rdir.
    unsafe {
        // Release old rdir.
        if !fp.fp_rdir.is_null() {
            mount::put_vnode(fp.fp_rdir);
        }
        // Dup the new rdir.
        fp.fp_rdir = vp;
        mount::dup_vnode(vp);
    }

    unsafe { mount::put_vnode(vp) };
    OK
}

/// Perform the `stat(path, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_stat()` (line 130)
pub fn do_stat() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };

    // Parse path from message.
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let buf_addr = r_u64(&glob.fs_m_in, 24);

    // Copy path from userspace.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        let r = sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        );
        if r != 0 {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);

    // Resolve the path.
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    let fs_e = unsafe { (*vp).v_fs_e };
    let inode_nr = unsafe { (*vp).v_inode_nr };
    let r = unsafe {
        crate::vfs::request::req_stat(
            fs_e,
            inode_nr,
            fp.fp_endpoint,
            buf_addr as *mut u8,
            core::mem::size_of::<Stat>(),
        )
    };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `fstat(fd, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_fstat()` (line 155)
pub fn do_fstat() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let buf_addr = r_u64(&glob.fs_m_in, 12);

    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }

    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        let filp = &*filp_arr.add(filp_idx as usize);
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        crate::vfs::request::req_stat(
            (*vp).v_fs_e,
            (*vp).v_inode_nr,
            fp.fp_endpoint,
            buf_addr as *mut u8,
            core::mem::size_of::<Stat>(),
        )
    }
}

/// Perform the `lstat(path, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_lstat()` (line 180)
pub fn do_lstat() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };

    // Parse path from message.
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let buf_addr = r_u64(&glob.fs_m_in, 24);

    // Copy path from userspace.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        let r = sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        );
        if r != 0 {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);

    // Resolve the path (lstat doesn't follow symlinks).
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    let fs_e = unsafe { (*vp).v_fs_e };
    let inode_nr = unsafe { (*vp).v_inode_nr };
    let r = unsafe {
        crate::vfs::request::req_stat(
            fs_e,
            inode_nr,
            fp.fp_endpoint,
            buf_addr as *mut u8,
            core::mem::size_of::<Stat>(),
        )
    };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `statvfs(path, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_statvfs()` (line 256)
pub fn do_statvfs() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let buf_addr = r_u64(&glob.fs_m_in, 24);
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    let fs_e = unsafe { (*vp).v_fs_e };
    let r = unsafe {
        crate::vfs::request::req_statvfs(
            fs_e,
            fp.fp_endpoint,
            buf_addr as *mut u8,
            core::mem::size_of::<Statvfs>(),
        )
    };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `fstatvfs(fd, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_fstatvfs()` (line 257)
pub fn do_fstatvfs() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let buf_addr = r_u64(&glob.fs_m_in, 16);
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }
    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        let filp = &*filp_arr.add(filp_idx as usize);
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        crate::vfs::request::req_statvfs(
            (*vp).v_fs_e,
            fp.fp_endpoint,
            buf_addr as *mut u8,
            core::mem::size_of::<Statvfs>(),
        )
    }
}

/// Perform the `getvfsstat(buf, bufsize, flags)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` â€” `do_getvfsstat()` (line 258)
pub fn do_getvfsstat() -> i32 {
    // Get VFS-wide filesystem statistics.
    let glob = unsafe { &*vfs_global() };
    let _buf_addr = r_u64(&glob.fs_m_in, 8);
    let _size = r_u64(&glob.fs_m_in, 16);
    // Would iterate vmnt table and copy the full statvfs array to user.
    // Count mounted filesystems, refreshing each one's stats via the FS.
    let mut stat_count = 0;
    unsafe {
        let vmnt_arr = core::ptr::addr_of!((*vfs_global()).vmnt) as *const Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &*vmnt_arr.add(i);
            if vmp.m_fs_e >= 0 && vmp.m_dev != 0 {
                let mut buf = Statvfs::default();
                crate::vfs::request::req_statvfs(
                    vmp.m_fs_e,
                    arch_common::com::VFS_PROC_NR,
                    &mut buf as *mut Statvfs as *mut u8,
                    core::mem::size_of::<Statvfs>(),
                );
                stat_count += 1;
            }
        }
    }
    if stat_count > 0 { stat_count } else { ENOSYS }
}

/// Perform the `readlink(path, buf, bufsize)` system call.
///
/// C source: `minix/servers/vfs/link.c` â€” `do_rdlink()` (line 94)
pub fn do_rdlink() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let _buf_addr = r_u64(&glob.fs_m_in, 24);
    let buf_size = r_u32(&glob.fs_m_in, 32) as usize;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    resolve.l_flags = PATH_RET_SYMLINK;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    let r = unsafe {
        crate::vfs::request::req_rdlink(
            (*vp).v_fs_e,
            (*vp).v_inode_nr,
            -1,
            core::ptr::null_mut(),
            buf_size,
            0,
        )
    };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `link(oldpath, newpath)` system call.
///
/// C source: `minix/servers/vfs/link.c` â€” `do_link()` (line 30)
pub fn do_link() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let name1_addr = r_u64(&glob.fs_m_in, 8);
    let name1_len = r_u32(&glob.fs_m_in, 16) as usize;
    let name2_addr = r_u64(&glob.fs_m_in, 24);
    let name2_len = r_u32(&glob.fs_m_in, 32) as usize;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = name1_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            name1_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    let src_fs_e = unsafe { (*vp).v_fs_e };
    let src_ino = unsafe { (*vp).v_inode_nr };
    // Copy name2 and resolve via last_dir.
    let mut name2_buf = [0u8; PATH_MAX];
    let copy2 = name2_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            name2_addr,
            SELF,
            name2_buf.as_mut_ptr() as u64,
            copy2,
        ) != 0
        {
            mount::put_vnode(vp);
            return EBADF;
        }
    }
    let actual2 = name2_buf[..copy2]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy2);
    let mut resolve2 = Lookup::default();
    resolve2.l_path[..actual2].copy_from_slice(&name2_buf[..actual2]);
    resolve2.l_path_len = actual2;
    let dirp = unsafe { path::last_dir(&resolve2, fp) };
    if dirp.is_null() {
        unsafe { mount::put_vnode(vp) };
        return ENOENT;
    }
    let dir_ino = unsafe { (*dirp).v_inode_nr };
    let r = unsafe { crate::vfs::request::req_link(src_fs_e, dir_ino, core::ptr::null(), src_ino) };
    unsafe { mount::put_vnode(dirp) };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `unlink(path)` system call (also used for `rmdir` in C).
///
/// C source: `minix/servers/vfs/link.c` â€” `do_unlink()` (line 88)
pub fn do_unlink() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    resolve.l_flags = PATH_RET_SYMLINK;
    let dirp = unsafe { path::last_dir(&resolve, fp) };
    if dirp.is_null() {
        return ENOENT;
    }
    let fs_e = unsafe { (*dirp).v_fs_e };
    let dir_ino = unsafe { (*dirp).v_inode_nr };
    let r = unsafe { crate::vfs::request::req_unlink(fs_e, dir_ino, core::ptr::null()) };
    unsafe { mount::put_vnode(dirp) };
    r
}

/// Perform the `rename(oldpath, newpath)` system call.
///
/// C source: `minix/servers/vfs/link.c` â€” `do_rename()` (line 89)
pub fn do_rename() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let name1_addr = r_u64(&glob.fs_m_in, 8);
    let name1_len = r_u32(&glob.fs_m_in, 16) as usize;
    let name2_addr = r_u64(&glob.fs_m_in, 24);
    let name2_len = r_u32(&glob.fs_m_in, 32) as usize;
    let mut buf = [0u8; PATH_MAX];
    let copy = name1_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            name1_addr,
            SELF,
            buf.as_mut_ptr() as u64,
            copy,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual = buf[..copy].iter().position(|&b| b == 0).unwrap_or(copy);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual].copy_from_slice(&buf[..actual]);
    resolve.l_path_len = actual;
    let dirp = unsafe { path::last_dir(&resolve, fp) };
    if dirp.is_null() {
        return ENOENT;
    }
    let old_parent_fs = unsafe { (*dirp).v_fs_e };
    let old_parent_ino = unsafe { (*dirp).v_inode_nr };

    // Resolve new path.
    let mut buf2 = [0u8; PATH_MAX];
    let copy2 = name2_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            name2_addr,
            SELF,
            buf2.as_mut_ptr() as u64,
            copy2,
        ) != 0
        {
            mount::put_vnode(dirp);
            return EBADF;
        }
    }
    let actual2 = buf2[..copy2].iter().position(|&b| b == 0).unwrap_or(copy2);
    let mut resolve2 = Lookup::default();
    resolve2.l_path[..actual2].copy_from_slice(&buf2[..actual2]);
    resolve2.l_path_len = actual2;
    let dirp2 = unsafe { path::last_dir(&resolve2, fp) };
    if dirp2.is_null() {
        unsafe { mount::put_vnode(dirp) };
        return ENOENT;
    }
    let new_parent_ino = unsafe { (*dirp2).v_inode_nr };

    let r = unsafe {
        crate::vfs::request::req_rename(
            old_parent_fs,
            old_parent_ino,
            core::ptr::null(),
            new_parent_ino,
            core::ptr::null(),
        )
    };
    unsafe { mount::put_vnode(dirp2) };
    unsafe { mount::put_vnode(dirp) };
    r
}

/// Perform the `mkdir(path, mode)` system call.
///
/// C source: `minix/servers/vfs/open.c` â€” `do_mkdir()` (line 145)
pub fn do_mkdir() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let mode = r_u32(&glob.fs_m_in, 24);
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let dirp = unsafe { path::last_dir(&resolve, fp) };
    if dirp.is_null() {
        return ENOENT;
    }
    // Find the filename (last component after the last '/' in path_buf).
    let file_name_ptr =
        if let Some(slash_pos) = path_buf[..actual_len].iter().rposition(|&b| b == b'/') {
            unsafe { path_buf.as_ptr().add(slash_pos + 1) }
        } else {
            path_buf.as_ptr()
        };
    let fs_e = unsafe { (*dirp).v_fs_e };
    let dir_ino = unsafe { (*dirp).v_inode_nr };
    let r = unsafe {
        crate::vfs::request::req_mkdir(
            fs_e,
            dir_ino,
            file_name_ptr,
            fp.fp_effuid,
            fp.fp_effgid,
            mode,
        )
    };
    unsafe { mount::put_vnode(dirp) };
    r
}

/// Perform the `mknod(path, mode, dev)` system call.
///
/// C source: `minix/servers/vfs/open.c` â€” `do_mknod()` (line 144)
pub fn do_mknod() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let mode = r_u32(&glob.fs_m_in, 24);
    let dev = r_u32(&glob.fs_m_in, 32);
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let dirp = unsafe { path::last_dir(&resolve, fp) };
    if dirp.is_null() {
        return ENOENT;
    }
    let fs_e = unsafe { (*dirp).v_fs_e };
    let dir_ino = unsafe { (*dirp).v_inode_nr };
    let r = unsafe {
        crate::vfs::request::req_mknod(
            fs_e,
            dir_ino,
            core::ptr::null(),
            fp.fp_effuid,
            fp.fp_effgid,
            mode,
            dev,
        )
    };
    unsafe { mount::put_vnode(dirp) };
    r
}

/// Perform the `symlink(target, linkpath)` system call.
///
/// C source: `minix/servers/vfs/open.c` â€” `do_slink()` (line 148)
pub fn do_slink() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let _link_addr = r_u64(&glob.fs_m_in, 24);
    let _link_len = r_u32(&glob.fs_m_in, 32) as usize;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let dirp = unsafe { path::last_dir(&resolve, fp) };
    if dirp.is_null() {
        return ENOENT;
    }
    let r = unsafe {
        crate::vfs::request::req_slink(
            (*dirp).v_fs_e,
            (*dirp).v_inode_nr,
            core::ptr::null(),
            fp.fp_effuid,
            fp.fp_effgid,
            core::ptr::null(),
        )
    };
    unsafe { mount::put_vnode(dirp) };
    r
}

/// Perform the `rmdir(path)` system call.
///
/// In the original C code, `VFS_RMDIR` maps to `do_unlink` (see `table.c` line 37).
/// This separate stub is kept for clarity and will dispatch to the same
/// internal logic once implemented.
///
/// C source: `minix/servers/vfs/link.c` â€” `do_unlink()` (also handles RMDIR, line 88)
pub fn do_rmdir() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    resolve.l_flags = PATH_RET_SYMLINK;
    let dirp = unsafe { path::last_dir(&resolve, fp) };
    if dirp.is_null() {
        return ENOENT;
    }
    let r = unsafe {
        crate::vfs::request::req_rmdir((*dirp).v_fs_e, (*dirp).v_inode_nr, core::ptr::null())
    };
    unsafe { mount::put_vnode(dirp) };
    r
}

// Permission operations

/// Perform the `access(path, mode)` system call.
///
/// C source: `minix/servers/vfs/protect.c` â€” `do_access()` (line 177)
pub fn do_access() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let amode = r_u32(&glob.fs_m_in, 24);
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    // Check access using real uid/gid via the proper forbidden() check.
    let vp_ref = unsafe { &*vp };
    let r = unsafe { crate::vfs::protect::check_access(fp, vp_ref, amode) };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `chmod(path, mode)` and `fchmod(fd, mode)` system calls.
///
/// C source: `minix/servers/vfs/protect.c` â€” `do_chmod()` (line 25)
/// Also handles `VFS_FCHMOD` (see `table.c` line 54: `CALL(VFS_FCHMOD) = do_chmod`).
pub fn do_chmod() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let rmode = r_u32(&glob.fs_m_in, 24);
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    // Permission check: must own file or be root.
    let vp_ref = unsafe { &*vp };
    let r = crate::vfs::protect::chmod_allowed(fp, vp_ref);
    if r != OK {
        unsafe { mount::put_vnode(vp) };
        return r;
    }
    let fs_e = vp_ref.v_fs_e;
    let inode_nr = vp_ref.v_inode_nr;
    let mut new_mode = rmode;
    crate::vfs::protect::chmod_strip_setgid(fp, vp_ref, &mut new_mode);
    let (r, new_mode) = unsafe { crate::vfs::request::req_chmod(fs_e, inode_nr, new_mode) };
    if r == OK {
        // Refresh the cached vnode mode from the reply (C protect.c
        // do_chmod: `vp->v_mode = new_mode`) — otherwise a later lookup
        // (e.g. exec) sees the stale mode without the chmod's bits.
        unsafe { (*vp).v_mode = new_mode };
    }
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `chown(path, owner, group)` and `fchown(fd, owner, group)` system calls.
///
/// C source: `minix/servers/vfs/protect.c` â€” `do_chown()` (line 179)
/// Also handles `VFS_FCHOWN` (see `table.c` line 55: `CALL(VFS_FCHOWN) = do_chown`).
pub fn do_chown() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let owner = r_u32(&glob.fs_m_in, 24) as u16;
    let group = r_u32(&glob.fs_m_in, 32) as u16;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    // Permission check: ownership validation for non-root.
    let vp_ref = unsafe { &*vp };
    let r = crate::vfs::protect::chown_allowed(fp, vp_ref, owner as i32, group as i32);
    if r != OK {
        unsafe { mount::put_vnode(vp) };
        return r;
    }
    let fs_e = vp_ref.v_fs_e;
    let inode_nr = vp_ref.v_inode_nr;
    let (r, new_mode) = unsafe { crate::vfs::request::req_chown(fs_e, inode_nr, owner, group) };
    if r == OK {
        // Refresh the cached vnode ownership/mode from the reply (C
        // protect.c do_chown sets v_uid/v_gid/v_mode) so later lookups
        // (e.g. exec's setuid check) see the chown's effects.
        unsafe {
            (*vp).v_uid = owner as i32;
            (*vp).v_gid = group as i32;
            (*vp).v_mode = new_mode;
        }
    }
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `umask(mode)` system call.
///
/// C source: `minix/servers/vfs/protect.c` â€” `do_umask()` (line 180)
pub fn do_umask() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let mask = r_i32(unsafe { &(*vfs_global()).fs_m_in }, UMASK_MODE_OFF) as u16;
    let old = fp.fp_umask;
    fp.fp_umask = mask & 0o777;
    old as i32
}

// Mount operations

/// Perform the `mount(special, path, rwflag, ...)` system call.
///
/// C source: `minix/servers/vfs/mount.c` â€” `do_mount()` (line 128)
pub fn do_mount() -> i32 {
    crate::vfs::mount::do_mount()
}

/// Perform the `umount(special)` system call.
///
/// C source: `minix/servers/vfs/mount.c` â€” `do_umount()` (line 129)
pub fn do_umount() -> i32 {
    crate::vfs::mount::do_umount()
}

/// Perform the `mapdriver(label, major, endpoint)` â€” register a device driver.
///
/// C source: `minix/servers/vfs/dmap.c` â€” `do_mapdriver()` (line 50)
pub fn do_mapdriver() -> i32 {
    crate::vfs::dmap::map_service(core::ptr::null())
}

// Time operations

/// Perform the `utimens(path, times, flag)` system call (and its friends).
///
/// C source: `minix/servers/vfs/time.c` â€” `do_utimens()` (line 26)
pub fn do_utimens() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let atime = r_u64(&glob.fs_m_in, 24) as i64;
    let mtime = r_u64(&glob.fs_m_in, 32) as i64;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    let r = unsafe { crate::vfs::request::req_utime((*vp).v_fs_e, (*vp).v_inode_nr, atime, mtime) };
    unsafe { mount::put_vnode(vp) };
    r
}

/// sysgetenv struct passed to VFSSETPARAM/VFSGETPARAM.
#[repr(C)]
struct Sysgetenv {
    key: u64,
    keylen: usize,
    val: u64,
    vallen: usize,
}

/// Perform VFS server control operations.
///
/// Validates the 'M' signature and dispatches VFSSETPARAM/VFSGETPARAM
/// by copying a sysgetenv struct from userspace via virtual_copy.
/// Handles the "verbose" parameter (0-4).
///
/// C source: `minix/servers/vfs/misc.c` â€” `do_svrctl()` (line 777)
pub fn do_svrctl() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let svrctl = r_u32(&glob.fs_m_in, 8);
    if ((svrctl >> 8) & 0xFF) != b'M' as u32 {
        return EINVAL;
    }

    let lower = svrctl & 0xFF;
    let ptr = r_u64(&glob.fs_m_in, 24);

    if lower == 130 || lower == 131 {
        // Copy sysgetenv from userspace.
        let mut env = Sysgetenv {
            key: 0,
            keylen: 0,
            val: 0,
            vallen: 0,
        };
        let r = unsafe {
            sys_vircopy(
                fp.fp_endpoint,
                ptr,
                SELF,
                &mut env as *mut Sysgetenv as u64,
                core::mem::size_of::<Sysgetenv>(),
            )
        };
        if r != 0 {
            return r;
        }

        if env.keylen == 0 || env.keylen > 63 || env.vallen >= 64 {
            return EINVAL;
        }

        // Copy the key string from userspace.
        let mut key_buf = [0u8; 64];
        let r = unsafe {
            kernel::vm::virtual_copy(
                kernel::table::endpoint_slot(fp.fp_endpoint),
                env.key,
                -1,
                key_buf.as_mut_ptr() as u64,
                env.keylen,
            )
        };
        if r != 0 {
            return r;
        }
        let key_len = key_buf.iter().position(|&b| b == 0).unwrap_or(env.keylen);
        let key = core::str::from_utf8(&key_buf[..key_len]).unwrap_or("");

        if lower == 130 {
            // VFSSETPARAM
            match key {
                "verbose" => {
                    let mut val_buf = [0u8; 64];
                    let r = unsafe {
                        kernel::vm::virtual_copy(
                            kernel::table::endpoint_slot(fp.fp_endpoint),
                            env.val,
                            -1,
                            val_buf.as_mut_ptr() as u64,
                            env.vallen,
                        )
                    };
                    if r != 0 {
                        return r;
                    }
                    let val_str =
                        core::str::from_utf8(&val_buf[..env.vallen.min(63)]).unwrap_or("0");
                    let val: i32 = val_str.trim().parse().unwrap_or(0);
                    if !(0..=4).contains(&val) {
                        return EINVAL;
                    }
                    unsafe {
                        (*vfs_global()).verbose = val;
                    }
                    OK
                }
                _ => EINVAL,
            }
        } else {
            // VFSGETPARAM
            match key {
                "verbose" => {
                    let v = unsafe { (*vfs_global()).verbose };
                    let s = alloc::format!("{}", v);
                    let bytes = s.as_bytes();
                    let copy_len = bytes.len().min(env.vallen);
                    let r = unsafe {
                        kernel::vm::virtual_copy(
                            -1,
                            bytes.as_ptr() as u64,
                            kernel::table::endpoint_slot(fp.fp_endpoint),
                            env.val,
                            copy_len,
                        )
                    };
                    if r != 0 {
                        return r;
                    }
                    copy_len as i32
                }
                _ => EINVAL,
            }
        }
    } else {
        EINVAL
    }
}

/// Perform the `getsysinfo(what, where, size)` â€” copy VFS data structures.
///
/// C source: `minix/servers/vfs/misc.c` â€” `do_getsysinfo()` (line 120)
pub fn do_getsysinfo() -> i32 {
    crate::vfs::misc::do_getsysinfo()
}

/// Handle a VM call to VFS.
///
/// VMâ†”VFS protocol: VM sends requests (FDLOOKUP/FDCLOSE/FDIO) to VFS
/// with m_type = VFS_VMCALL; VFS must reply with m_type = VM_VFS_REPLY
/// (and the result in VMV_RESULT) so VM can tell the difference between a
/// request from VFS and a reply to this call. The reply payload uses the
/// M10 layout (VMV_* offsets in `consts.rs`).
///
/// C source: `minix/servers/vfs/misc.c` â€” `do_vm_call()` (line 359)
pub fn do_vm_call() -> i32 {
    let glob = unsafe { &*vfs_global() };
    let req = r_i32(&glob.fs_m_in, VMCALL_REQ_OFF);
    let req_fd = r_i32(&glob.fs_m_in, VMCALL_FD_OFF);
    let req_id = r_u32(&glob.fs_m_in, VMCALL_REQID_OFF);
    let ep = r_i32(&glob.fs_m_in, VMCALL_ENDPOINT_OFF);
    let offset = r_u64(&glob.fs_m_in, VMCALL_OFFSET_OFF) as i64;
    let fault_va = r_u64(&glob.fs_m_in, VMCALL_FAULTVA_OFF);
    let length = r_u32(&glob.fs_m_in, VMCALL_LENGTH_OFF);

    let result = match req {
        VMVFSREQ_FDLOOKUP => {
            // Look up `req_fd` in the referenced process's fd table. For a
            // regular file it is dup'd into VFS's own fproc (the vmfd), so
            // later FDIO/FDCLOSE requests resolve the file from VFS's own
            // fd table alone; for a char device the driver's physical
            // range is fetched instead (no file to read, no vmfd).
            let slot = crate::vfs::misc::endpoint_to_slot(ep);
            let slot = match slot {
                Some(s) => s,
                None => return vm_call_reply(ep, ESRCH, req_id),
            };
            unsafe {
                let fproc_arr = core::ptr::addr_of_mut!((*vfs_global()).fproc) as *mut Fproc;
                let rfp = &mut *fproc_arr.add(slot);
                if req_fd < 0 || (req_fd as usize) >= OPEN_MAX {
                    return vm_call_reply(ep, EBADF, req_id);
                }
                let filp_idx = rfp.fp_filp[req_fd as usize];
                if filp_idx < 0 {
                    return vm_call_reply(ep, EBADF, req_id);
                }
                let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
                let filp = &*filp_arr.add(filp_idx as usize);
                let vp = filp.filp_vno;
                if vp.is_null() {
                    return vm_call_reply(ep, EBADF, req_id);
                }
                let mode = (*vp).v_mode;
                let glob_mut = &mut *vfs_global();
                if (mode & 0o170000) == 0o020000 {
                    // Char device: ask the driver for its device-memory
                    // physical range (dmap `map` hook). The reply carries
                    // IS_DEVICE + dev + phys + len; VM builds a VR_DIRECT
                    // region instead of a file region.
                    let dev = (*vp).v_dev;
                    let (r, phys, len) = crate::vfs::device::cdev_map_phys(dev);
                    if r != OK {
                        return vm_call_reply(ep, r, req_id);
                    }
                    vm_call_reply_device(&mut glob_mut.fs_m_out, dev, phys, len);
                } else {
                    let mut vmfd = 0i32;
                    let r = crate::vfs::misc::dupvm(rfp, req_fd, &mut vmfd);
                    if r != OK {
                        return vm_call_reply(ep, r, req_id);
                    }
                    // VMV_FD carries the vmfd; VMV_DEV/INO/SIZE_PAGES describe the
                    // backing file. Only regular files are supported (dupvm
                    // rejects block devices and non-files).
                    glob_mut.fs_m_out[VMV_FD_OFF..][..8]
                        .copy_from_slice(&(vmfd as u64).to_le_bytes());
                    glob_mut.fs_m_out[VMV_DEV_OFF..][..4]
                        .copy_from_slice(&(*vp).v_dev.to_le_bytes());
                    glob_mut.fs_m_out[VMV_INO_OFF..][..8]
                        .copy_from_slice(&((*vp).v_inode_nr as u64).to_le_bytes());
                    let size_pages = if (*vp).v_size > 0 {
                        ((*vp).v_size as u64).div_ceil(4096)
                    } else {
                        0
                    };
                    glob_mut.fs_m_out[VMV_SIZE_PAGES_OFF..][..8]
                        .copy_from_slice(&size_pages.to_le_bytes());
                    glob_mut.fs_m_out[VMV_ISDEV_OFF..][..4].copy_from_slice(&0u32.to_le_bytes());
                }
            }
            OK
        }
        VMVFSREQ_FDCLOSE => {
            // The vmfd lives in VFS's own fproc (the current fp — VM's
            // fproc slot, which dupvm filled), not in the target's table.
            let fp = unsafe { crate::vfs::glo::current_fp() };
            if fp.is_null() {
                return vm_call_reply(ep, ESRCH, req_id);
            }
            unsafe {
                let r = close_fd(&mut *fp, req_fd);
                if r != OK {
                    return vm_call_reply(ep, r, req_id);
                }
            }
            OK
        }
        VMVFSREQ_FDIO => {
            // Read a block of the vmfd's file directly into the faulting
            // page at `fault_va` in the target process's address space.
            // req_read's magic grant (cp_who_from = ep) makes the kernel
            // write through the target's CR3, so MFS's SAFECOPYTO lands in
            // the page VM just mapped there.
            let fp = unsafe { crate::vfs::glo::current_fp() };
            if fp.is_null() {
                return vm_call_reply(ep, ESRCH, req_id);
            }
            unsafe {
                if req_fd < 0 || (req_fd as usize) >= OPEN_MAX {
                    return vm_call_reply(ep, EBADF, req_id);
                }
                let fp_mut = &mut *fp;
                let filp_idx = fp_mut.fp_filp[req_fd as usize];
                if filp_idx < 0 {
                    return vm_call_reply(ep, EBADF, req_id);
                }

                let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
                let filp = &mut *filp_arr.add(filp_idx as usize);
                let vp = filp.filp_vno;
                if vp.is_null() {
                    return vm_call_reply(ep, EBADF, req_id);
                }

                // Do not disturb the file position: reads are positional
                // (the FS request carries the offset), so nothing to save.
                let fs_e = (*vp).v_fs_e;
                let inode_nr = (*vp).v_inode_nr;
                let (r, _new_pos) = crate::vfs::request::req_read(
                    fs_e,
                    inode_nr,
                    fault_va as *mut u8,
                    offset,
                    length,
                    ep,
                    0,
                );
                // Normalize the reply result: this port's req_read returns the
                // FS reply's m_type (the byte count, e.g. 4096) on success, but
                // VM's map_file_page treats any non-zero result as a
                // failure (C: `actual_read_write_peek` returns OK and the byte
                // count travels in the payload). Reply OK on a non-negative
                // result so a successful fill of the faulting page is not
                // mistaken for an error.
                if r < 0 { r } else { OK }
            }
        }
        _ => EINVAL,
    };

    vm_call_reply(ep, result, req_id)
}

/// Fill the FDLOOKUP reply for a char device: IS_DEVICE=1, the device
/// number, and the driver's physical range. Pure so the reply layout is
/// host-testable; VM reads these fields only when IS_DEVICE is set.
pub fn vm_call_reply_device(out: &mut [u8; 64], dev: u32, phys: u64, len: u64) {
    out[VMV_ISDEV_OFF..VMV_ISDEV_OFF + 4].copy_from_slice(&1u32.to_le_bytes());
    out[VMV_DEV_OFF..VMV_DEV_OFF + 4].copy_from_slice(&dev.to_le_bytes());
    out[VMV_PHYS_OFF..VMV_PHYS_OFF + 8].copy_from_slice(&phys.to_le_bytes());
    out[VMV_LEN_OFF..VMV_LEN_OFF + 8].copy_from_slice(&len.to_le_bytes());
    out[VMV_FD_OFF..VMV_FD_OFF + 8].copy_from_slice(&(-1i64).to_le_bytes());
}

/// Fill the VM_VFS_REPLY message, send it to VM asynchronously, and return
/// SUSPEND so the generic `reply()` path does not also send a blocking
/// reply.
///
/// Matching C `misc.c do_vm_call` (L461-472): the reply goes out with
/// `asynsend3` because VM may not be able to receive a blocking send — it
/// can be mid-fault-resolution, and a blocking SEND here would deadlock the
/// VM<->VFS pair (observed: VFS blocked SENDING the FDCLOSE reply while VM
/// handled su's exec'd-image page fault). The result code travels in
/// VMV_RESULT; the fixed m_type (written at byte 4) is VM_VFS_REPLY.
fn vm_call_reply(ep: i32, result: i32, req_id: u32) -> i32 {
    unsafe {
        let glob_mut = &mut *vfs_global();
        glob_mut.fs_m_out[VMV_ENDPOINT_OFF..][..4].copy_from_slice(&ep.to_le_bytes());
        glob_mut.fs_m_out[VMV_RESULT_OFF..][..4].copy_from_slice(&result.to_le_bytes());
        glob_mut.fs_m_out[VMV_REQID_OFF..][..4].copy_from_slice(&req_id.to_le_bytes());
        glob_mut.fs_m_out[4..8].copy_from_slice(&VM_VFS_REPLY.to_le_bytes());
        #[cfg(target_os = "minix")]
        {
            minix_rt::asynsend3(arch_common::com::VM_PROC_NR, glob_mut.fs_m_out.as_ptr(), 0);
        }
    }
    SUSPEND
}

/// Perform the `getrusage(who, buf)` system call.
///
/// C source: `minix/servers/vfs/misc.c` â€” `do_getrusage()` (line 959)
pub fn do_getrusage() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let buf_addr = r_u64(&glob.fs_m_in, 8);

    // Fill a minimal rusage struct using fproc fields.
    // struct rusage: ru_utime (2x i64), ru_stime (2x i64), then 12 more i64 fields
    let mut rusage = [0u8; 144];
    let text_size = fp.fp_text_size;
    let data_size = fp.fp_data_size;
    // ru_ixrss = text_size (offset 32 in rusage on x86_64)
    rusage[32..40].copy_from_slice(&text_size.to_le_bytes());
    // ru_idrss = data_size (offset 40)
    rusage[40..48].copy_from_slice(&data_size.to_le_bytes());
    // ru_isrss = default stack limit (offset 48)
    let stack_limit = 0x100000i64; // 1MB default
    rusage[48..56].copy_from_slice(&stack_limit.to_le_bytes());

    unsafe {
        kernel::vm::virtual_copy(
            -1,
            rusage.as_ptr() as u64,
            kernel::table::endpoint_slot(fp.fp_endpoint),
            buf_addr,
            144,
        )
    }
}

/// Perform the `gcov_flush()` system call â€” flush gcov coverage data.
///
/// C source: `minix/servers/vfs/gcov.c` â€” `do_gcov_flush()` (line 322)
/// Flush GCOV profiling data from a target process.
///
/// This is a GCC-specific feature (`-fprofile-arcs -ftest-coverage`)
/// that has no equivalent in Rust. The function is intentionally
/// unimplemented â€” returning ENOSYS is correct behavior.
///
/// C source: `minix/servers/vfs/gcov.c` â€” `do_gcov_flush()` (line 10)
pub fn do_gcov_flush() -> i32 {
    ENOSYS
}

/// Check file access permissions for a given process.
///
/// C source: `minix/servers/vfs/path.c` â€” `do_checkperms()` (line 161)
pub fn do_checkperms() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if sys_vircopy(
            fp.fp_endpoint,
            path_addr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        ) != 0
        {
            return EBADF;
        }
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return ENOENT;
    }
    let mode = unsafe { (*vp).v_mode };
    let r = if fp.fp_effuid == SU_UID {
        OK
    } else if (mode & 0o0001) == 0 {
        EACCES
    }
    // X_BIT for others
    else {
        OK
    };
    unsafe { mount::put_vnode(vp) };
    r
}

// lock_op is implemented in crate::vfs::lock — called from do_fcntl.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::glo::vfs_global;

    /// Set up test state: init VFS, set current fp at slot 0 with endpoint 0.
    unsafe fn setup() {
        let glob = vfs_global();
        let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
        let fp = &mut *fproc_arr.add(0);
        fp.fp_endpoint = 0;
        fp.fp_effuid = 0;
        fp.fp_realuid = 0;
        fp.fp_effgid = 0;
        fp.fp_realgid = 0;
        fp.fp_umask = 0o022;
        fp.fp_cloexec = 0;
        fp.fp_filp = [-1i32; OPEN_MAX];
        (*glob).fp = fp;
        (*glob).fs_m_in = [0u8; 64];
        (*glob).fs_m_out = [0u8; 64];
    }

    #[test]
    fn test_close_invalid_fd() {
        unsafe { setup() }
        // fd = -1 should fail
        unsafe {
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_close(), EBADF);
    }

    #[test]
    fn test_close_not_open() {
        unsafe { setup() }
        // fd=0 not open
        unsafe {
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(do_close(), EBADF);
    }

    #[test]
    fn test_close_valid() {
        unsafe {
            setup();
            let glob = vfs_global();
            // Allocate a filp and assign to fd 0
            let filp_idx = crate::vfs::filedes::alloc_filp();
            assert!(filp_idx >= 0);
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_filp[0] = filp_idx;

            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(do_close(), OK);
    }

    #[test]
    fn test_umask_sets_and_returns_old() {
        unsafe { setup() }
        unsafe {
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[UMASK_MODE_OFF..UMASK_MODE_OFF + 4].copy_from_slice(&0o077i32.to_le_bytes());
        }
        // First call should return the default (0o022)
        let old = do_umask();
        assert_eq!(old, 0o022);
        // Second call should return 0o077
        let old2 = do_umask();
        assert_eq!(old2, 0o077);
    }

    #[test]
    fn test_fcntl_getfd_on_closed_fd() {
        unsafe { setup() }
        unsafe {
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&3i32.to_le_bytes());
            fs_m_in[FCNTL_CMD_OFF..FCNTL_CMD_OFF + 4].copy_from_slice(&F_GETFD.to_le_bytes());
        }
        assert_eq!(do_fcntl(), EBADF);
    }

    #[test]
    fn test_fcntl_unknown_cmd() {
        unsafe { setup() }
        unsafe {
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[FCNTL_CMD_OFF..FCNTL_CMD_OFF + 4].copy_from_slice(&99i32.to_le_bytes());
        }
        assert_eq!(do_fcntl(), ENOSYS);
    }

    #[test]
    fn test_fcntl_setfd_cloexec() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            let filp_idx = crate::vfs::filedes::alloc_filp();
            fp.fp_filp[0] = filp_idx;

            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[FCNTL_CMD_OFF..FCNTL_CMD_OFF + 4].copy_from_slice(&F_SETFD.to_le_bytes());
            fs_m_in[FCNTL_ARG_OFF..FCNTL_ARG_OFF + 4].copy_from_slice(&1i32.to_le_bytes()); // FD_CLOEXEC
        }
        assert_eq!(do_fcntl(), OK);
        unsafe {
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &*fproc_arr.add(0);
            assert!(fp.fp_cloexec & 1 != 0, "cloexec should be set for fd 0");
        }
    }

    #[test]
    fn test_lseek_invalid_fd() {
        unsafe { setup() }
        unsafe {
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&999i32.to_le_bytes());
        }
        assert_eq!(do_lseek(), EBADF);
    }

    #[test]
    fn test_lseek_not_open() {
        unsafe { setup() }
        unsafe {
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(do_lseek(), EBADF);
    }

    #[test]
    fn test_lseek_seek_set() {
        unsafe {
            setup();
            let glob = vfs_global();
            let filp_idx = crate::vfs::filedes::alloc_filp();
            assert!(filp_idx >= 0);
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_filp[0] = filp_idx;

            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[LSEEK_OFF_OFF..LSEEK_OFF_OFF + 8].copy_from_slice(&42u64.to_le_bytes());
            fs_m_in[LSEEK_WHENCE_OFF..LSEEK_WHENCE_OFF + 4].copy_from_slice(&0i32.to_le_bytes()); // SEEK_SET
        }
        assert_eq!(do_lseek(), 42);
    }

    #[test]
    fn test_lseek_seek_cur() {
        unsafe {
            setup();
            let glob = vfs_global();
            let filp_idx = crate::vfs::filedes::alloc_filp();
            assert!(filp_idx >= 0);
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_filp[0] = filp_idx;

            // Set initial position to 100
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            (*filp_arr.add(filp_idx as usize)).filp_pos = 100;

            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[LSEEK_OFF_OFF..LSEEK_OFF_OFF + 8].copy_from_slice(&10u64.to_le_bytes());
            fs_m_in[LSEEK_WHENCE_OFF..LSEEK_WHENCE_OFF + 4].copy_from_slice(&1i32.to_le_bytes()); // SEEK_CUR
        }
        assert_eq!(do_lseek(), 110);
    }

    #[test]
    fn test_lseek_seek_end_unsupported() {
        unsafe {
            setup();
            let glob = vfs_global();
            let filp_idx = crate::vfs::filedes::alloc_filp();
            assert!(filp_idx >= 0);
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_filp[0] = filp_idx;

            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[LSEEK_WHENCE_OFF..LSEEK_WHENCE_OFF + 4].copy_from_slice(&2i32.to_le_bytes());
            // SEEK_END now uses vnode size; fails with EBADF when filp has no vnode
            assert_eq!(do_lseek(), EBADF);
        }
    }

    #[test]
    fn test_open_rejects_null_path() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..12].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[16..24].copy_from_slice(&0u64.to_le_bytes());
            let r = do_open();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_read_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_read(), EBADF);
    }

    #[test]
    fn test_read_no_rbit_on_wronly_filp() {
        unsafe {
            setup();
            let glob = vfs_global();
            let filp_idx = crate::vfs::filedes::alloc_filp();
            assert!(filp_idx >= 0);
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_filp[0] = filp_idx;
            // Set filp_mode to W_BIT only (no R_BIT)
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            (*filp_arr.add(filp_idx as usize)).filp_mode = 2; // W_BIT only
            (*filp_arr.add(filp_idx as usize)).filp_count = 1;
            (*filp_arr.add(filp_idx as usize)).filp_vno = core::ptr::null_mut();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(do_read(), EBADF);
    }

    #[test]
    fn test_read_not_open_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&5i32.to_le_bytes());
        }
        assert_eq!(do_read(), EBADF);
    }

    #[test]
    fn test_write_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_write(), EBADF);
    }

    #[test]
    fn test_getdents_invalid_fd_returns_ebadf() {
        unsafe { setup() }
        unsafe {
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_getdents(), EBADF);
    }

    #[test]
    fn test_fchdir_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_fchdir(), EBADF);
    }

    #[test]
    fn test_chroot_rejects_non_superuser() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_effuid = 1000;
        }
        assert_eq!(do_chroot(), EPERM);
    }

    #[test]
    fn test_ftruncate_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_ftruncate(), EBADF);
    }

    #[test]
    fn test_ioctl_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_ioctl(), EBADF);
    }

    #[test]
    fn test_fstat_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_fstat(), EBADF);
    }

    #[test]
    fn test_fstatvfs_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_fstatvfs(), EBADF);
    }

    #[test]
    fn test_truncate_empty_path_returns_host_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_truncate();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_chdir_empty_path_returns_host_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_chdir();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_stat_empty_path_returns_host_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_stat();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_access_returns_enoent_for_empty_path() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_access();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_do_getsysinfo_delegates() {
        unsafe {
            setup();
            // Initial state: fp_effuid defaults to 0 (superuser) but endpoint unknown
            let r = do_getsysinfo();
            // Will likely fail because fp is not properly set, but shouldn't panic
            assert_ne!(r, OK);
        }
    }

    #[test]
    fn test_sync_returns_ok() {
        assert_eq!(do_sync(), OK);
    }

    #[test]
    fn test_fsync_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_fsync(), EBADF);
    }

    #[test]
    fn test_creat_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_creat();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_link_empty_name1_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_link();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_mkdir_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_mkdir();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_mknod_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_mknod();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_rmdir_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_rmdir();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_chmod_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_chmod();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_chown_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_chown();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_utimens_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_utimens();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_checkperms_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_checkperms();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_rdlink_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_rdlink();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_slink_empty_path_returns_error() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes());
            let r = do_slink();
            assert!(r < 0);
        }
    }

    #[test]
    fn test_lock_op_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(unsafe { crate::vfs::lock::lock_op() }, EBADF);
    }

    #[test]
    fn test_getvfsstat_returns_error_on_host() {
        unsafe {
            setup();
            let r = do_getvfsstat();
            // On host, will either count 0 mounts or return ENOSYS
            assert!(r <= 0);
        }
    }

    #[test]
    fn test_select_blocking_suspends_when_nothing_ready() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[SEL_NFDS_OFF..SEL_NFDS_OFF + 4].copy_from_slice(&1i32.to_le_bytes());
        }
        // nfds=1, no fd sets, NULL timeout: nothing ready → the caller is
        // suspended (no reply) until a driver reports readiness.
        assert_eq!(do_select(), SUSPEND);
    }

    #[test]
    fn test_pipe2_returns_enosys_when_pfs_unmounted() {
        unsafe {
            setup();
            let glob = vfs_global();
            // No PFS mount: find_vmnt(PFS_PROC_NR) returns null
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..12].copy_from_slice(&0i32.to_le_bytes()); // flags = 0
        }
        assert_eq!(do_pipe2(), ENOSYS);
    }

    #[test]
    fn test_vm_call_reply_device_layout() {
        // The char-device FDLOOKUP reply: IS_DEVICE @ 12, phys @ 28 (u64,
        // overlapping dev/ino which are meaningless for devices), len @ 56,
        // vmfd = -1.
        let mut out = [0u8; 64];
        vm_call_reply_device(&mut out, 19 << 16, 0xFD00_0000, 0x100_0000);
        let is_dev = u32::from_le_bytes(out[VMV_ISDEV_OFF..VMV_ISDEV_OFF + 4].try_into().unwrap());
        assert_eq!(is_dev, 1);
        let phys = u64::from_le_bytes(out[VMV_PHYS_OFF..VMV_PHYS_OFF + 8].try_into().unwrap());
        assert_eq!(phys, 0xFD00_0000);
        let len = u64::from_le_bytes(out[VMV_LEN_OFF..VMV_LEN_OFF + 8].try_into().unwrap());
        assert_eq!(len, 0x100_0000);
        let vmfd = i64::from_le_bytes(out[VMV_FD_OFF..VMV_FD_OFF + 8].try_into().unwrap());
        assert_eq!(vmfd, -1);
    }

    #[test]
    fn test_vm_call_fdlookup_char_device_routes_to_driver() {
        unsafe {
            setup();
            crate::vfs::dmap::init_dmap();
            let glob = vfs_global();
            // filp 0 → vnode 0 = a char device (/dev/fb, major 19).
            let vnode_arr = core::ptr::addr_of_mut!((*glob).vnode) as *mut Vnode;
            let vp = &mut *vnode_arr.add(0);
            vp.v_mode = 0o020000; // char special
            vp.v_dev = 19u32 << 16;
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            let filp = &mut *filp_arr.add(0);
            filp.filp_vno = vp;
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_filp[0] = 0; // fd 0 → filp 0

            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[VMCALL_REQ_OFF..VMCALL_REQ_OFF + 4]
                .copy_from_slice(&VMVFSREQ_FDLOOKUP.to_le_bytes());
            fs_m_in[VMCALL_FD_OFF..VMCALL_FD_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[VMCALL_ENDPOINT_OFF..VMCALL_ENDPOINT_OFF + 4]
                .copy_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(do_vm_call(), SUSPEND);
        unsafe {
            let glob = vfs_global();
            let out = &(*glob).fs_m_out;
            let result =
                i32::from_le_bytes(out[VMV_RESULT_OFF..VMV_RESULT_OFF + 4].try_into().unwrap());
            // The device branch is taken (char vnode resolved, not EBADF/
            // EINVAL); the driver round trip cannot happen on host, so the
            // dmap lookup finds no driver → ENXIO.
            assert_eq!(result, ENXIO);
        }
    }

    #[test]
    fn test_vm_call_returns_einval_for_unknown_req() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[VMCALL_REQ_OFF..VMCALL_REQ_OFF + 4].copy_from_slice(&999i32.to_le_bytes());
        }
        // do_vm_call always replies with the fixed VM_VFS_REPLY type; the
        // result code travels in VMV_RESULT so VM can distinguish a reply
        // from a new request. The reply itself is sent asynchronously and
        // the function returns SUSPEND (matching C misc.c do_vm_call).
        assert_eq!(do_vm_call(), SUSPEND);
        unsafe {
            let glob = vfs_global();
            let out = &(*glob).fs_m_out;
            let result =
                i32::from_le_bytes(out[VMV_RESULT_OFF..VMV_RESULT_OFF + 4].try_into().unwrap());
            assert_eq!(result, EINVAL);
        }
    }

    #[test]
    fn test_getrusage_returns_result() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[8..16].copy_from_slice(&0u64.to_le_bytes()); // null buf
        }
        let r = do_getrusage();
        // Should return error (virtual_copy fails on host), not panic
        assert!(r < 0);
    }

    #[test]
    fn test_gcov_flush_returns_enosys() {
        assert_eq!(do_gcov_flush(), ENOSYS);
    }

    #[test]
    fn test_do_umount_rejects_non_superuser() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_effuid = 1000;
        }
        // do_umount delegates to mount::do_umount which checks EPERM
        let r = do_umount();
        assert_eq!(r, EPERM);
    }
}
