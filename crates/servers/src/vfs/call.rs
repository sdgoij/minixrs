//! VFS call handler functions — adapted from the following C sources:
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
use crate::vfs::stadir;
use crate::vfs::stadir::close_fd;
use crate::vfs::types::*;

/// Common: fd field offset in payload.
const FD_OFF: usize = 8;
/// lseek: offset (u64).
const LSEEK_OFF_OFF: usize = 12;
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

// =============================================================================
// File operations
// =============================================================================

/// Perform the `open(name, flags)` system call (O_CREAT *not* set).
///
/// C source: `minix/servers/vfs/open.c` — `do_open()` (line 39)
/// Perform the `open(name, flags)` system call (O_CREAT *not* set).
///
/// C source: `minix/servers/vfs/open.c` — `do_open()` (line 39)
pub fn do_open() -> i32 {
    // Parse the path from the message.
    let glob = unsafe { &*vfs_global() };
    let fp = match unsafe { glob.fp.as_mut() } {
        Some(fp) => fp,
        None => return EINVAL,
    };

    // Message layout: flags (offset 8), path_addr (offset 16), path_len (offset 24)
    let flags = r_i32(&glob.fs_m_in, 8) as u32;
    let path_addr = r_u64(&glob.fs_m_in, 16);
    let path_len = r_u32(&glob.fs_m_in, 24) as usize;

    // Check O_CREAT is not set.
    if flags & (1 << 12) != 0 {
        // O_CREAT
        return EINVAL;
    }

    // Copy path from userspace.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        let r = kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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

    // Parse access mode bits.
    let accmode = flags & 0o3;
    let _bits = match accmode {
        0 => 1, // O_RDONLY -> R_BIT
        1 => 2, // O_WRONLY -> W_BIT
        2 => 3, // O_RDWR -> R_BIT|W_BIT
        _ => return EINVAL,
    };

    // Get a free fd and filp slot.
    let mut fd = 0i32;
    let r = unsafe { filedes::get_fd(fp, 0, &mut fd) };
    if r != OK {
        return r;
    }

    // Resolve the path.
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        // Clear the fd slot.
        fp.fp_filp[fd as usize] = -1;
        return ENOENT;
    }

    // Check file type.
    let mode = unsafe { (*vp).v_mode };
    match mode & 0o170000 {
        // S_IFMT
        0o100000 => { // S_IFREG - regular file
            // Would check write permission if O_TRUNC.
        }
        0o040000 => {
            // S_IFDIR
            // Directories may be read but not written.
            if accmode == 1 {
                unsafe { mount::put_vnode(vp) };
                fp.fp_filp[fd as usize] = -1;
                return EISDIR;
            }
        }
        0o020000 => { // S_IFCHR
            // Character device — would call cdev_open.
        }
        0o060000 => { // S_IFBLK
            // Block device — would call bdev_open.
        }
        _ => {
            unsafe { mount::put_vnode(vp) };
            fp.fp_filp[fd as usize] = -1;
            return ENXIO;
        }
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
    }

    // Release the vnode reference (the filp now holds it).
    unsafe { mount::put_vnode(vp) };

    fd
}

/// Perform the `creat(name, mode)` system call.
///
/// C source: `minix/servers/vfs/open.c` — `do_creat()` (line 59)
pub fn do_creat() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let _open_flags = r_u32(&glob.fs_m_in, 24); // O_TRUNC/O_EXCL handling deferred until FS rw layer
    let create_mode = r_u32(&glob.fs_m_in, 28);
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
    let (r, _nd) = unsafe {
        crate::vfs::request::req_create(
            (*dirp).v_fs_e,
            (*dirp).v_inode_nr,
            create_mode as i32,
            fp.fp_effuid,
            fp.fp_effgid,
            core::ptr::null(),
        )
    };
    unsafe { mount::put_vnode(dirp) };
    r
}

/// Perform the `close(fd)` system call.
///
/// C source: `minix/servers/vfs/open.c` — `do_close()` (line 664)
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
/// C source: `minix/servers/vfs/open.c` — `do_lseek()` (line 143)
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
/// C source: `minix/servers/vfs/read.c` — `do_read()` (line 31)
pub fn do_read() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let _buf_addr = r_u64(&glob.fs_m_in, 16);
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
        let filp = &*filp_arr.add(filp_idx as usize);
        if (filp.filp_mode & 1) == 0 {
            return EBADF;
        }
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        // For regular files, call req_read.
        let (r, _new_pos) = crate::vfs::request::req_read(
            (*vp).v_fs_e,
            (*vp).v_inode_nr,
            core::ptr::null_mut(),
            filp.filp_pos,
            count as u32,
            0,
            0,
        );
        r
    }
}

/// Perform the `write(fd, buf, count)` system call.
///
/// C source: `minix/servers/vfs/read.c` — `read_write()` (line 132)
pub fn do_write() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let _buf_addr = r_u64(&glob.fs_m_in, 16);
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
        let filp = &*filp_arr.add(filp_idx as usize);
        if (filp.filp_mode & 2) == 0 {
            return EBADF;
        }
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        let (r, _new_pos) = crate::vfs::request::req_write(
            (*vp).v_fs_e,
            (*vp).v_inode_nr,
            core::ptr::null(),
            filp.filp_pos,
            count as u32,
            0,
            0,
        );
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
    let _buf_addr = r_u64(&glob.fs_m_in, 16);
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
        let filp = &*filp_arr.add(filp_idx as usize);
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        let (r, _new_pos) = crate::vfs::request::req_getdents(
            (*vp).v_fs_e,
            (*vp).v_inode_nr,
            filp.filp_pos,
            core::ptr::null_mut(),
            count,
            0,
        );
        r
    }
}

/// Perform the `pipe2(flags)` system call.
///
/// C source: `minix/servers/vfs/pipe.c` — `do_pipe2()` (line 150)
/// Perform the `pipe2(flags)` system call.
///
/// Creates a pipe by allocating a vnode, calling req_newnode on PFS,
/// and setting up two file descriptors (read end, write end).
///
/// C source: `minix/servers/vfs/pipe.c` — `do_pipe2()` (line 40)
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

    // Create the pipe inode on PFS.
    let (_r, _nd) = unsafe {
        crate::vfs::request::req_newnode(
            PFS_PROC_NR,
            fp.fp_effuid,
            fp.fp_effgid,
            I_NAMED_PIPE,
            0xffff,
        )
    };

    // Apply flags to the pipe ends.
    let extra_flags = flags as u32 & !0o3;
    unsafe {
        let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
        (*filp_arr.add(filp0 as usize)).filp_count = 1;
        (*filp_arr.add(filp0 as usize)).filp_flags = extra_flags;
        (*filp_arr.add(filp1 as usize)).filp_count = 1;
        (*filp_arr.add(filp1 as usize)).filp_flags = 0o1 | extra_flags; // O_WRONLY + extra
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
/// C source: `minix/servers/vfs/device.c` — `do_ioctl()` (line 45)
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
        let dev = (*vp).v_dev;
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
/// C source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` — `do_fcntl()` (line 110)
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
            // Duplicate fd — allocate the lowest free fd >= arg.
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
        _ => ENOSYS,
    }
}

/// Perform the `copyfd(fd, newfd, flags)` — duplicate a file descriptor.
///
/// C source: `minix/servers/vfs/filedes.c` — `do_copyfd()` (line 82)
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
/// C source: `minix/servers/vfs/link.c` — `do_truncate()` (line 91)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
/// C source: `minix/servers/vfs/link.c` — `do_ftruncate()` (line 92)
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

/// Perform the `sync()` system call — flush all filesystem buffers.
///
/// Iterates all mounted filesystems and calls `req_sync` on each.
///
/// C source: `minix/servers/vfs/misc.c` — `do_sync()` (line 116)
pub fn do_sync() -> i32 {
    unsafe {
        let vmnt_arr = core::ptr::addr_of!((*vfs_global()).vmnt) as *const Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &*vmnt_arr.add(i);
            if vmp.m_fs_e >= 0 && vmp.m_dev != 0 {
                let _ = crate::vfs::request::req_sync(vmp.m_fs_e);
            }
        }
    }
    OK
}

/// Perform the `fsync(fd)` system call — flush a single file descriptor.
///
/// Validates the fd, gets the vnode from the filp, and calls `req_sync`
/// on the filesystem that owns the file.
///
/// C source: `minix/servers/vfs/misc.c` — `do_fsync()` (line 117)
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
/// C source: `minix/servers/vfs/select.c` — `do_select()` (line 30)
pub fn do_select() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let ops = r_u32(&glob.fs_m_in, 12);
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
        let dev = (*vp).v_dev;
        crate::vfs::device::cdev_select(dev, ops as i32)
    }
}

/// Perform the `chdir(name)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_chdir()` (line 50)
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
        let r = kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
/// C source: `minix/servers/vfs/stadir.c` — `do_fchdir()` (line 32)
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
/// C source: `minix/servers/vfs/stadir.c` — `do_chroot()` (line 83)
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
        let r = kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
/// C source: `minix/servers/vfs/stadir.c` — `do_stat()` (line 130)
pub fn do_stat() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };

    // Parse path from message.
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let _buf_addr = r_u64(&glob.fs_m_in, 24);

    // Copy path from userspace.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        let r = kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
        crate::vfs::request::req_stat(fs_e, inode_nr, fp.fp_endpoint, core::ptr::null_mut(), 0)
    };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `fstat(fd, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_fstat()` (line 155)
pub fn do_fstat() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let _buf_addr = r_u64(&glob.fs_m_in, 12);

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
            core::ptr::null_mut(),
            0,
        )
    }
}

/// Perform the `lstat(path, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_lstat()` (line 180)
pub fn do_lstat() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };

    // Parse path from message.
    let path_addr = r_u64(&glob.fs_m_in, 8);
    let path_len = r_u32(&glob.fs_m_in, 16) as usize;
    let _buf_addr = r_u64(&glob.fs_m_in, 24);

    // Copy path from userspace.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    unsafe {
        let r = kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
        crate::vfs::request::req_stat(fs_e, inode_nr, fp.fp_endpoint, core::ptr::null_mut(), 0)
    };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `statvfs(path, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_statvfs()` (line 256)
pub fn do_statvfs() -> i32 {
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
    let (r, _stat) = unsafe { crate::vfs::request::req_statvfs(fs_e) };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `fstatvfs(fd, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_fstatvfs()` (line 257)
pub fn do_fstatvfs() -> i32 {
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
        let (r, _stat) = crate::vfs::request::req_statvfs((*vp).v_fs_e);
        r
    }
}

/// Perform the `getvfsstat(buf, bufsize, flags)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_getvfsstat()` (line 258)
pub fn do_getvfsstat() -> i32 {
    // Get VFS-wide filesystem statistics.
    let glob = unsafe { &*vfs_global() };
    let _buf_addr = r_u64(&glob.fs_m_in, 8);
    let _size = r_u64(&glob.fs_m_in, 16);
    // Would iterate vmnt table, call req_statvfs for each, and copy to user.
    // Just stat all mounted filesystems.
    let mut stat_count = 0;
    unsafe {
        let vmnt_arr = core::ptr::addr_of!((*vfs_global()).vmnt) as *const Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &*vmnt_arr.add(i);
            if vmp.m_fs_e >= 0 && vmp.m_dev != 0 {
                let (_r, _stat) = crate::vfs::request::req_statvfs(vmp.m_fs_e);
                stat_count += 1;
            }
        }
    }
    if stat_count > 0 { stat_count } else { ENOSYS }
}

/// Perform the `readlink(path, buf, bufsize)` system call.
///
/// C source: `minix/servers/vfs/link.c` — `do_rdlink()` (line 94)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
/// C source: `minix/servers/vfs/link.c` — `do_link()` (line 30)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            name1_addr,
            -1,
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            name2_addr,
            -1,
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
/// C source: `minix/servers/vfs/link.c` — `do_unlink()` (line 88)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
/// C source: `minix/servers/vfs/link.c` — `do_rename()` (line 89)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            name1_addr,
            -1,
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            name2_addr,
            -1,
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
/// C source: `minix/servers/vfs/open.c` — `do_mkdir()` (line 145)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
        crate::vfs::request::req_mkdir(
            fs_e,
            dir_ino,
            core::ptr::null(),
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
/// C source: `minix/servers/vfs/open.c` — `do_mknod()` (line 144)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
/// C source: `minix/servers/vfs/open.c` — `do_slink()` (line 148)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
/// C source: `minix/servers/vfs/link.c` — `do_unlink()` (also handles RMDIR, line 88)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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

// =============================================================================
// Permission operations
// =============================================================================

/// Perform the `access(path, mode)` system call.
///
/// C source: `minix/servers/vfs/protect.c` — `do_access()` (line 177)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
    // Check access using real uid/gid.
    let mode = unsafe { (*vp).v_mode };
    let r = if fp.fp_realuid == SU_UID {
        OK // root can do anything
    } else if (amode & 4) != 0 && (mode & 4) == 0 {
        // R_OK
        EACCES
    } else if (amode & 2) != 0 && (mode & 2) == 0 {
        // W_OK
        EACCES
    } else if (amode & 1) != 0 && (mode & 1) == 0 {
        // X_OK
        EACCES
    } else {
        OK
    };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `chmod(path, mode)` and `fchmod(fd, mode)` system calls.
///
/// C source: `minix/servers/vfs/protect.c` — `do_chmod()` (line 25)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
    let inode_nr = unsafe { (*vp).v_inode_nr };
    let (r, _new_mode) = unsafe { crate::vfs::request::req_chmod(fs_e, inode_nr, rmode) };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `chown(path, owner, group)` and `fchown(fd, owner, group)` system calls.
///
/// C source: `minix/servers/vfs/protect.c` — `do_chown()` (line 179)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
    let inode_nr = unsafe { (*vp).v_inode_nr };
    let (r, _new_mode) = unsafe { crate::vfs::request::req_chown(fs_e, inode_nr, owner, group) };
    unsafe { mount::put_vnode(vp) };
    r
}

/// Perform the `umask(mode)` system call.
///
/// C source: `minix/servers/vfs/protect.c` — `do_umask()` (line 180)
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

// =============================================================================
// Mount operations
// =============================================================================

/// Perform the `mount(special, path, rwflag, ...)` system call.
///
/// C source: `minix/servers/vfs/mount.c` — `do_mount()` (line 128)
pub fn do_mount() -> i32 {
    crate::vfs::mount::do_mount()
}

/// Perform the `umount(special)` system call.
///
/// C source: `minix/servers/vfs/mount.c` — `do_umount()` (line 129)
pub fn do_umount() -> i32 {
    crate::vfs::mount::do_umount()
}

/// Perform the `mapdriver(label, major, endpoint)` — register a device driver.
///
/// C source: `minix/servers/vfs/dmap.c` — `do_mapdriver()` (line 50)
pub fn do_mapdriver() -> i32 {
    crate::vfs::dmap::map_service(core::ptr::null())
}

// =============================================================================
// Time operations
// =============================================================================

/// Perform the `utimens(path, times, flag)` system call (and its friends).
///
/// C source: `minix/servers/vfs/time.c` — `do_utimens()` (line 26)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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
/// C source: `minix/servers/vfs/misc.c` — `do_svrctl()` (line 777)
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
            kernel::vm::virtual_copy(
                kernel::table::endpoint_slot(fp.fp_endpoint),
                ptr,
                -1,
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

/// Perform the `getsysinfo(what, where, size)` — copy VFS data structures.
///
/// C source: `minix/servers/vfs/misc.c` — `do_getsysinfo()` (line 120)
pub fn do_getsysinfo() -> i32 {
    crate::vfs::misc::do_getsysinfo()
}

/// Handle a VM call to VFS.
///
/// VM↔VFS protocol: VM sends requests (FDLOOKUP/FDCLOSE/FDIO) to VFS
/// through the SYS_VMCALL path. VFS must reply with VM_VFS_REPLY
/// so VM can distinguish replies from new requests.
///
/// C source: `minix/servers/vfs/misc.c` — `do_vm_call()` (line 359)
pub fn do_vm_call() -> i32 {
    let glob = unsafe { &*vfs_global() };
    let req = r_i32(&glob.fs_m_in, VMCALL_REQ_OFF);
    let req_fd = r_i32(&glob.fs_m_in, VMCALL_FD_OFF);
    let _req_id = r_u32(&glob.fs_m_in, VMCALL_REQID_OFF);
    let ep = r_i32(&glob.fs_m_in, VMCALL_ENDPOINT_OFF);
    let offset = r_u64(&glob.fs_m_in, VMCALL_OFFSET_OFF) as i64;
    let length = r_u32(&glob.fs_m_in, VMCALL_LENGTH_OFF);

    match req {
        VMVFSREQ_FDLOOKUP => {
            let slot = crate::vfs::misc::endpoint_to_slot(ep);
            let slot = match slot {
                Some(s) => s,
                None => return ESRCH,
            };
            unsafe {
                let fproc_arr = core::ptr::addr_of_mut!((*vfs_global()).fproc) as *mut Fproc;
                let rfp = &mut *fproc_arr.add(slot);
                let mut vmfd = 0i32;
                let r = crate::vfs::misc::dupvm(rfp, req_fd, &mut vmfd);
                if r != OK {
                    return r;
                }

                let filp_idx = rfp.fp_filp[req_fd as usize];
                let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
                let filp = &*filp_arr.add(filp_idx as usize);
                let vp = filp.filp_vno;

                let glob_mut = &mut *vfs_global();
                glob_mut.fs_m_out[VMV_ENDPOINT_OFF..][..4].copy_from_slice(&ep.to_le_bytes());
                glob_mut.fs_m_out[VMV_RESULT_OFF..][..4].copy_from_slice(&OK.to_le_bytes());
                glob_mut.fs_m_out[VMV_FD_OFF..][..8].copy_from_slice(&(vmfd as u64).to_le_bytes());
                glob_mut.fs_m_out[VMV_DEV_OFF..][..4].copy_from_slice(&(*vp).v_dev.to_le_bytes());
                glob_mut.fs_m_out[VMV_INO_OFF..][..8]
                    .copy_from_slice(&((*vp).v_inode_nr as u64).to_le_bytes());
                glob_mut.fs_m_out[0..4].copy_from_slice(&VM_VFS_REPLY.to_le_bytes());
            }
            OK
        }
        VMVFSREQ_FDCLOSE => {
            let slot = crate::vfs::misc::endpoint_to_slot(ep);
            let slot = match slot {
                Some(s) => s,
                None => return ESRCH,
            };
            unsafe {
                let fproc_arr = core::ptr::addr_of_mut!((*vfs_global()).fproc) as *mut Fproc;
                let rfp = &mut *fproc_arr.add(slot);
                let _ = stadir::close_fd(rfp, req_fd);
                let glob_mut = &mut *vfs_global();
                glob_mut.fs_m_out[VMV_ENDPOINT_OFF..][..4].copy_from_slice(&ep.to_le_bytes());
                glob_mut.fs_m_out[VMV_RESULT_OFF..][..4].copy_from_slice(&OK.to_le_bytes());
                glob_mut.fs_m_out[0..4].copy_from_slice(&VM_VFS_REPLY.to_le_bytes());
            }
            OK
        }
        VMVFSREQ_FDIO => {
            // Peek at file data (for VM pagefault handling).
            // Seek to the offset, then read without consuming.
            let slot = crate::vfs::misc::endpoint_to_slot(ep);
            let slot = match slot {
                Some(s) => s,
                None => return ESRCH,
            };

            unsafe {
                let fproc_arr = core::ptr::addr_of_mut!((*vfs_global()).fproc) as *mut Fproc;
                let rfp = &mut *fproc_arr.add(slot);
                if req_fd < 0 || (req_fd as usize) >= OPEN_MAX {
                    return EBADF;
                }
                let filp_idx = rfp.fp_filp[req_fd as usize];
                if filp_idx < 0 {
                    return EBADF;
                }

                let filp_arr = core::ptr::addr_of_mut!((*vfs_global()).filp) as *mut Filp;
                let filp = &mut *filp_arr.add(filp_idx as usize);
                let vp = filp.filp_vno;
                if vp.is_null() {
                    return EBADF;
                }

                // Seek to offset.
                let old_pos = filp.filp_pos;
                filp.filp_pos = offset;

                // Peek at file data (read without consuming/advancing position).
                let fs_e = (*vp).v_fs_e;
                let inode_nr = (*vp).v_inode_nr;
                let r = crate::vfs::request::req_peek(fs_e, inode_nr, offset, length);

                // Always restore position — peek does not consume data.
                filp.filp_pos = old_pos;

                let glob_mut = &mut *vfs_global();
                glob_mut.fs_m_out[VMV_ENDPOINT_OFF..][..4].copy_from_slice(&ep.to_le_bytes());
                glob_mut.fs_m_out[VMV_RESULT_OFF..][..4].copy_from_slice(&r.to_le_bytes());
                glob_mut.fs_m_out[0..4].copy_from_slice(&VM_VFS_REPLY.to_le_bytes());
                r
            }
        }
        _ => EINVAL,
    }
}

/// Perform the `getrusage(who, buf)` system call.
///
/// C source: `minix/servers/vfs/misc.c` — `do_getrusage()` (line 959)
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

/// Perform the `gcov_flush()` system call — flush gcov coverage data.
///
/// C source: `minix/servers/vfs/gcov.c` — `do_gcov_flush()` (line 322)
/// Flush GCOV profiling data from a target process.
///
/// This is a GCC-specific feature (`-fprofile-arcs -ftest-coverage`)
/// that has no equivalent in Rust. The function is intentionally
/// unimplemented — returning ENOSYS is correct behavior.
///
/// C source: `minix/servers/vfs/gcov.c` — `do_gcov_flush()` (line 10)
pub fn do_gcov_flush() -> i32 {
    ENOSYS
}

/// Check file access permissions for a given process.
///
/// C source: `minix/servers/vfs/path.c` — `do_checkperms()` (line 161)
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
        if kernel::vm::virtual_copy(
            kernel::table::endpoint_slot(fp.fp_endpoint),
            path_addr,
            -1,
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

pub fn lock_op() -> i32 {
    let fp = match current_fp() {
        Some(fp) => fp,
        None => return EINVAL,
    };
    let glob = unsafe { &*vfs_global() };
    let fd = r_i32(&glob.fs_m_in, FD_OFF);
    let _cmd = r_i32(&glob.fs_m_in, 12);
    let _typ = r_i32(&glob.fs_m_in, 16);
    let _start = r_u64(&glob.fs_m_in, 24) as i64;
    let _len = r_u64(&glob.fs_m_in, 32) as i64;
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    if fp.fp_filp[fd as usize] < 0 {
        return EBADF;
    }
    // Advisory file locking — supported as no-op (OK for F_SETLK/F_SETLKW).
    // Real implementation would allocate FileLock entries, check conflicts,
    // and manage the lock table.
    OK
}

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
        assert_eq!(lock_op(), EBADF);
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
    fn test_select_invalid_fd_returns_ebadf() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[FD_OFF..FD_OFF + 4].copy_from_slice(&(-1i32).to_le_bytes());
        }
        assert_eq!(do_select(), EBADF);
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
    fn test_vm_call_returns_einval_for_unknown_req() {
        unsafe {
            setup();
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[VMCALL_REQ_OFF..VMCALL_REQ_OFF + 4].copy_from_slice(&999i32.to_le_bytes());
        }
        assert_eq!(do_vm_call(), EINVAL);
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
