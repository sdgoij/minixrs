//! VFS-side exec — the PM→VFS exec chain.
//!
//! Ported from `.refs/minix-3.3.0/minix/servers/vfs/exec.c` (`pm_exec`), but
//! the ELF loading itself is delegated to the kernel (`SYS_EXEC_LOAD`, kernel
//! call 63): VFS reads the binary from the filesystem into its own buffer and
//! passes it, together with the user-built exec frame, to the kernel, which
//! replaces the target's image. This mirrors the C flow's division of labor
//! (VFS resolves the file and builds the stack; the kernel installs the
//! image) without requiring VM file-backed mmap, which this port does not
//! have yet.

use crate::vfs::consts::*;
#[cfg(target_os = "none")]
use crate::vfs::glo::vfs_global;
#[cfg(target_os = "none")]
use crate::vfs::mount::put_vnode;
#[cfg(target_os = "none")]
use crate::vfs::path;
#[cfg(target_os = "none")]
use crate::vfs::request::req_read;
#[cfg(target_os = "none")]
use crate::vfs::types::{Fproc, Lookup};

#[cfg(target_os = "none")]
use arch_common::com::VFS_PROC_NR;

/// Size of the VFS Fproc table (`glo::NR_PROCS`).
#[cfg(target_os = "none")]
const NR_FPROCS: usize = 256;

/// SELF endpoint constant used by kernel calls (kernel::system::SELF).
#[cfg(target_os = "none")]
const SELF: i32 = 31742;

/// SYS_VIRCOPY kernel call number.
#[cfg(target_os = "none")]
const SYS_VIRCOPY: i32 = 15;
/// SYS_EXEC_LOAD kernel call number (arch-common::sys::EXEC_LOAD - KERNEL_CALL).
#[cfg(target_os = "none")]
const SYS_EXEC_LOAD: i32 = 63;

// Copy message offsets (match kernel do_copy_common).
#[cfg(target_os = "none")]
const COPY_SRC_ENDPT_OFF: usize = 48;
#[cfg(target_os = "none")]
const COPY_SRC_ADDR_OFF: usize = 8;
#[cfg(target_os = "none")]
const COPY_DST_ENDPT_OFF: usize = 16;
#[cfg(target_os = "none")]
const COPY_DST_ADDR_OFF: usize = 24;
#[cfg(target_os = "none")]
const COPY_NR_BYTES_OFF: usize = 32;
#[cfg(target_os = "none")]
const COPY_FLAGS_OFF: usize = 40;
#[cfg(target_os = "none")]
const CP_FLAG_TRY: i32 = 0x80;

// SYS_EXEC_LOAD message offsets (match kernel do_exec_load_handler).
#[cfg(target_os = "none")]
const EXEC_LOAD_ENDPT_OFF: usize = 8;
#[cfg(target_os = "none")]
const EXEC_LOAD_ELF_PTR_OFF: usize = 16;
#[cfg(target_os = "none")]
const EXEC_LOAD_ELF_LEN_OFF: usize = 24;
#[cfg(target_os = "none")]
const EXEC_LOAD_FRAME_PTR_OFF: usize = 32;
#[cfg(target_os = "none")]
const EXEC_LOAD_FRAME_LEN_OFF: usize = 40;
#[cfg(target_os = "none")]
const EXEC_LOAD_PC_OFF: usize = 16;
#[cfg(target_os = "none")]
const EXEC_LOAD_NEWSP_OFF: usize = 24;

/// Upper bound for the exec stack frame (matches C's `ARG_MAX`-style limit).
#[cfg(target_os = "none")]
const EXEC_FRAME_MAX: usize = 16384;
/// Upper bound for an executable image read by VFS.
#[cfg(target_os = "none")]
const EXEC_ELF_MAX: usize = 1024 * 1024;

// Scratch buffers (VFS is effectively single-threaded; C uses a static
// `mbuf[ARG_MAX]` for the same purpose).
#[cfg(target_os = "none")]
static mut EXEC_FRAME_BUF: [u8; EXEC_FRAME_MAX] = [0u8; EXEC_FRAME_MAX];
#[cfg(target_os = "none")]
static mut EXEC_ELF_BUF: [u8; EXEC_ELF_MAX] = [0u8; EXEC_ELF_MAX];

/// Result of a VFS exec attempt.
#[derive(Debug, Clone, Copy)]
pub struct ExecResult {
    /// OK or a negative errno.
    pub status: i32,
    /// Entry point of the new image (valid on success).
    pub pc: u64,
    /// New user stack pointer (valid on success).
    pub newsp: u64,
}

/// Perform the exec of `path` for process `proc_e`, whose userland built a
/// stack frame at `frame_ptr` (len `frame_len`) — matching C `pm_exec()`.
///
/// # Safety
///
/// `path_ptr`/`frame_ptr` must be valid user VAs in the target process's
/// address space.
#[cfg(target_os = "none")]
pub unsafe fn pm_exec(
    proc_e: i32,
    path_ptr: u64,
    path_len: usize,
    frame_ptr: u64,
    frame_len: usize,
    _ps_str: u64,
) -> ExecResult {
    let err = |s: i32| ExecResult {
        status: s,
        pc: 0,
        newsp: 0,
    };

    if frame_len == 0 || frame_len > EXEC_FRAME_MAX {
        return err(E2BIG);
    }
    if path_len == 0 || path_len > PATH_MAX - 1 {
        return err(ENAMETOOLONG);
    }
    if path_ptr == 0 || frame_ptr == 0 {
        return err(EFAULT);
    }

    // Fetch the stack frame from the user before destroying the old image.
    // (C: sys_datacopy_wrapper(fp->fp_endpoint, frame, SELF, mbuf, frame_len))
    let r = unsafe {
        sys_vircopy(
            proc_e,
            frame_ptr,
            SELF,
            core::ptr::addr_of_mut!(EXEC_FRAME_BUF) as *mut u8 as u64,
            frame_len,
        )
    };
    if r != 0 {
        return err(EFAULT);
    }

    // Fetch the executable path.
    let mut path_buf = [0u8; PATH_MAX];
    let copy_len = path_len.min(PATH_MAX - 1);
    let r = unsafe {
        sys_vircopy(
            proc_e,
            path_ptr,
            SELF,
            path_buf.as_mut_ptr() as u64,
            copy_len,
        )
    };
    if r != 0 {
        return err(EFAULT);
    }
    let actual_len = path_buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    if actual_len == 0 {
        return err(ENOENT);
    }

    // Resolve the executable vnode (C: Get_read_vp / lookup). The exec
    // target is the process that called execve, NOT the PM process that
    // forwarded the request, so look up its Fproc slot by endpoint.
    let glob = unsafe { &mut *vfs_global() };
    let slot = (proc_e & 0xFF) as usize;
    if slot >= NR_FPROCS {
        return err(EINVAL);
    }
    let fp = unsafe { &mut *(&mut glob.fproc[slot] as *mut Fproc) };
    if fp.fp_endpoint != proc_e {
        return err(EINVAL);
    }
    let mut resolve = Lookup::default();
    resolve.l_path[..actual_len].copy_from_slice(&path_buf[..actual_len]);
    resolve.l_path_len = actual_len;
    let vp = unsafe { path::eat_path(&resolve, fp) };
    if vp.is_null() {
        return err(ENOENT);
    }
    let fs_e = unsafe { (*vp).v_fs_e };
    let inode_nr = unsafe { (*vp).v_inode_nr };
    let file_size = unsafe { (*vp).v_size };
    let vp_ok = file_size > 0 && (file_size as usize) <= EXEC_ELF_MAX;
    if vp_ok {
        // Read the whole binary into the ELF buffer (C reads the segments via
        // libexec; we read the file and let the kernel do the segment copy).
        let (r, _pos) = unsafe {
            req_read(
                fs_e,
                inode_nr,
                core::ptr::addr_of_mut!(EXEC_ELF_BUF) as *mut u8,
                0,
                file_size as u32,
                VFS_PROC_NR as i32,
                0,
            )
        };
        if r != file_size as i32 {
            unsafe { put_vnode(vp) };
            return err(EIO);
        }
    }
    unsafe { put_vnode(vp) };
    if !vp_ok {
        return err(ENOEXEC);
    }

    // Hand the ELF + frame to the kernel, which replaces the image and makes
    // the target runnable at the new entry point.
    let mut kmsg = [0u8; 64];
    kmsg[EXEC_LOAD_ENDPT_OFF..EXEC_LOAD_ENDPT_OFF + 4].copy_from_slice(&proc_e.to_le_bytes());
    kmsg[EXEC_LOAD_ELF_PTR_OFF..EXEC_LOAD_ELF_PTR_OFF + 8]
        .copy_from_slice(&(core::ptr::addr_of!(EXEC_ELF_BUF) as *const u8 as u64).to_le_bytes());
    kmsg[EXEC_LOAD_ELF_LEN_OFF..EXEC_LOAD_ELF_LEN_OFF + 8]
        .copy_from_slice(&(file_size as u64).to_le_bytes());
    kmsg[EXEC_LOAD_FRAME_PTR_OFF..EXEC_LOAD_FRAME_PTR_OFF + 8]
        .copy_from_slice(&(core::ptr::addr_of!(EXEC_FRAME_BUF) as *const u8 as u64).to_le_bytes());
    kmsg[EXEC_LOAD_FRAME_LEN_OFF..EXEC_LOAD_FRAME_LEN_OFF + 8]
        .copy_from_slice(&(frame_len as u64).to_le_bytes());
    let kresult = minix_rt::kernel_call(SYS_EXEC_LOAD, &mut kmsg);
    if kresult != 0 {
        return err(kresult);
    }

    let pc = u64::from_le_bytes(
        kmsg[EXEC_LOAD_PC_OFF..EXEC_LOAD_PC_OFF + 8]
            .try_into()
            .unwrap(),
    );
    let newsp = u64::from_le_bytes(
        kmsg[EXEC_LOAD_NEWSP_OFF..EXEC_LOAD_NEWSP_OFF + 8]
            .try_into()
            .unwrap(),
    );

    // Close CLOEXEC fds on the target (C: clo_exec(fp)).
    for i in 0..OPEN_MAX {
        if fp.fp_filp[i] >= 0 && (fp.fp_cloexec & (1u64 << i)) != 0 {
            let _ = crate::vfs::stadir::close_fd(fp, i as i32);
        }
    }
    fp.fp_cloexec = 0;

    ExecResult {
        status: OK,
        pc,
        newsp,
    }
}

/// Host stub — the exec path cannot run outside the MINIX target.
///
/// # Safety
///
/// All parameters are ignored in this host stub; the function is `unsafe`
/// only to mirror the `target_os = "none"` signature.
#[cfg(not(target_os = "none"))]
pub unsafe fn pm_exec(
    _proc_e: i32,
    _path_ptr: u64,
    _path_len: usize,
    _frame_ptr: u64,
    _frame_len: usize,
    _ps_str: u64,
) -> ExecResult {
    ExecResult {
        status: ENOSYS,
        pc: 0,
        newsp: 0,
    }
}

/// Copy `bytes` between process address spaces via SYS_VIRCOPY.
///
/// # Safety
///
/// `src_addr`/`dst_addr` must be valid for `bytes` in their respective
/// address spaces.
#[cfg(target_os = "none")]
unsafe fn sys_vircopy(
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
    minix_rt::kernel_call(SYS_VIRCOPY, &mut msg)
}
