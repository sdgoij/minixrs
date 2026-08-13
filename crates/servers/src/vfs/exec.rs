//! VFS-side exec — the PM→VFS exec chain.
//!
//! Ported from `.refs/minix-3.3.0/minix/servers/vfs/exec.c` (`pm_exec`), but
//! with the real MINIX file-backed mmap exec: VFS parses the ELF headers,
//! asks VM to create a fresh address space (VM_EXEC_NEWMEM) and map each
//! PT_LOAD segment as a lazy file-backed region (VM_VFS_MMAP), and the
//! kernel (`SYS_EXEC_LOAD`) only maps the stack/brk and programs the entry
//! registers. The image is demand-paged from the file, so there is no
//! whole-image read, no kernel ELF copy, and no executable size cap.

use crate::vfs::consts::*;
#[cfg(target_os = "minix")]
use crate::vfs::glo::vfs_global;
#[cfg(target_os = "minix")]
use crate::vfs::mount::put_vnode;
#[cfg(target_os = "minix")]
use crate::vfs::path;
#[cfg(target_os = "minix")]
use crate::vfs::request::req_read;
#[cfg(target_os = "minix")]
use crate::vfs::types::{Filp, Fproc, Lookup};

#[cfg(target_os = "minix")]
use arch_common::com::VFS_PROC_NR;
#[cfg(target_os = "minix")]
use arch_common::com::VM_PROC_NR;
#[cfg(target_os = "minix")]
use kernel::elf::{Elf64Ehdr, Elf64Phdr, PT_LOAD};

/// Size of the VFS Fproc table (`glo::NR_PROCS`).
#[cfg(target_os = "minix")]
const NR_FPROCS: usize = 256;

/// SELF endpoint constant used by kernel calls (kernel::system::SELF).
#[cfg(target_os = "minix")]
const SELF: i32 = 31742;

/// SYS_VIRCOPY kernel call number.
#[cfg(target_os = "minix")]
const SYS_VIRCOPY: i32 = 15;
/// SYS_EXEC_LOAD kernel call number (arch-common::sys::EXEC_LOAD - KERNEL_CALL).
#[cfg(target_os = "minix")]
const SYS_EXEC_LOAD: i32 = 63;

// Copy message offsets (match kernel do_copy_common).
#[cfg(target_os = "minix")]
const COPY_SRC_ENDPT_OFF: usize = 48;
#[cfg(target_os = "minix")]
const COPY_SRC_ADDR_OFF: usize = 8;
#[cfg(target_os = "minix")]
const COPY_DST_ENDPT_OFF: usize = 16;
#[cfg(target_os = "minix")]
const COPY_DST_ADDR_OFF: usize = 24;
#[cfg(target_os = "minix")]
const COPY_NR_BYTES_OFF: usize = 32;
#[cfg(target_os = "minix")]
const COPY_FLAGS_OFF: usize = 40;
#[cfg(target_os = "minix")]
const CP_FLAG_TRY: i32 = 0x80;

// SYS_EXEC_LOAD message offsets (match kernel do_exec_load_handler):
//   endpt @ 8, entry @ 16 (u64), code_start @ 24, code_end @ 32,
//   frame_ptr @ 40, frame_len @ 48.
// The reply reuses 16/24 for PC/newsp (the request fields are consumed
// by then).
#[cfg(target_os = "minix")]
const EXEC_LOAD_ENDPT_OFF: usize = 8;
#[cfg(target_os = "minix")]
const EXEC_LOAD_ENTRY_OFF: usize = 16;
#[cfg(target_os = "minix")]
const EXEC_LOAD_CODE_START_OFF: usize = 24;
#[cfg(target_os = "minix")]
const EXEC_LOAD_CODE_END_OFF: usize = 32;
#[cfg(target_os = "minix")]
const EXEC_LOAD_FRAME_PTR_OFF: usize = 40;
#[cfg(target_os = "minix")]
const EXEC_LOAD_FRAME_LEN_OFF: usize = 48;
#[cfg(target_os = "minix")]
const EXEC_LOAD_PC_OFF: usize = 16;
#[cfg(target_os = "minix")]
const EXEC_LOAD_NEWSP_OFF: usize = 24;

// VM_EXEC_NEWMEM: target endpoint in m1i1 (payload bytes 8..12).
// (The call number comes from arch_common::com::VM_EXEC_NEWMEM.)

/// Upper bound for the exec stack frame (matches C's `ARG_MAX`-style limit).
#[cfg(target_os = "minix")]
const EXEC_FRAME_MAX: usize = 16384;
/// ELF headers read at exec time (ehdr + program headers for typical
/// binaries). The image itself is never read whole: VM demand-pages it from
/// the file through file-backed regions, so there is no executable size cap.
#[cfg(target_os = "minix")]
const EXEC_HDR_MAX: usize = 8192;
/// Maximum PT_LOAD segments mapped per exec (bounds the stack array).
#[cfg(target_os = "minix")]
const MAX_EXEC_SEGS: usize = 8;

// Scratch frame buffer (VFS is effectively single-threaded; C uses a static
// `mbuf[ARG_MAX]` for the same purpose). The ELF image is never buffered in
// VFS — only the headers are read, into a small stack array in `pm_exec`.
#[cfg(target_os = "minix")]
static mut EXEC_FRAME_BUF: [u8; EXEC_FRAME_MAX] = [0u8; EXEC_FRAME_MAX];

/// Result of a VFS exec attempt.
#[derive(Debug, Clone, Copy)]
pub struct ExecResult {
    /// OK or a negative errno.
    pub status: i32,
    /// True when the failure happened after the old image was torn down
    /// (`VM_EXEC_NEWMEM` succeeded) — the process cannot continue and PM
    /// must kill it instead of replying the error.
    pub partial: bool,
    /// Entry point of the new image (valid on success).
    pub pc: u64,
    /// New user stack pointer (valid on success).
    pub newsp: u64,
}

/// An exec failure after the address space was replaced: PM must kill the
/// process.
#[cfg(target_os = "minix")]
fn err_partial(s: i32) -> ExecResult {
    ExecResult {
        status: s,
        partial: true,
        pc: 0,
        newsp: 0,
    }
}

/// Perform the exec of `path` for process `proc_e`, whose userland built a
/// stack frame at `frame_ptr` (len `frame_len`) — matching C `pm_exec()`.
///
/// # Safety
///
/// `path_ptr`/`frame_ptr` must be valid user VAs in the target process's
/// address space.
#[cfg(target_os = "minix")]
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
        partial: false,
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
    let dev = unsafe { (*vp).v_dev };
    let file_size = unsafe { (*vp).v_size };
    if file_size <= 0 {
        unsafe { put_vnode(vp) };
        return err(ENOEXEC);
    }

    // Open a VM fd on the executable in VFS's own fproc (C exec.c: the
    // vmfd lives in fproc[VM_PROC_NR]; VM_VFS_MMAP stores it in the region
    // and later FDIO requests read through it).
    let mut vmfd: i32 = -1;
    unsafe {
        let glob_mut = &mut *vfs_global();
        let fproc_arr = core::ptr::addr_of_mut!((*glob_mut).fproc) as *mut Fproc;
        let vmf = &mut *fproc_arr.add((VM_PROC_NR & 0xFF) as usize);
        let mut fd = 0i32;
        if crate::vfs::filedes::get_fd(vmf, 0, &mut fd) == OK {
            let filp_idx = crate::vfs::filedes::alloc_filp();
            if filp_idx >= 0 {
                let filp_arr = core::ptr::addr_of_mut!((*glob_mut).filp) as *mut Filp;
                let filp = &mut *filp_arr.add(filp_idx as usize);
                filp.filp_vno = vp;
                filp.filp_count = 1;
                filp.filp_mode = 1; // R_BIT
                vmf.fp_filp[fd as usize] = filp_idx;
                vmfd = fd;
            }
        }
    }
    if vmfd < 0 {
        unsafe { put_vnode(vp) };
        return err(ENOMEM);
    }

    // Failure cleanup: close the vmfd (VFS owns it until a VM_VFS_MMAP
    // succeeds) and release the vnode reference.
    let fail = |s: i32| -> ExecResult {
        unsafe {
            let glob_mut = &mut *vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob_mut).fproc) as *mut Fproc;
            let vmf = &mut *fproc_arr.add((VM_PROC_NR & 0xFF) as usize);
            let _ = crate::vfs::stadir::close_fd(vmf, vmfd);
            put_vnode(vp);
        }
        err(s)
    };

    // Read only the ELF headers (ehdr + program headers). The image itself
    // is never read whole: VM demand-pages the segments from the file.
    let hdr_len = (file_size as usize).min(EXEC_HDR_MAX);
    let mut hdr_buf = [0u8; EXEC_HDR_MAX];
    let (r, _pos) = unsafe {
        req_read(
            fs_e,
            inode_nr,
            hdr_buf.as_mut_ptr(),
            0,
            hdr_len as u32,
            VFS_PROC_NR as i32,
            0,
        )
    };
    if r != hdr_len as i32 {
        return fail(EIO);
    }

    // Parse the ELF header.
    let ehdr = hdr_buf.as_ptr() as *const Elf64Ehdr;
    let e_phoff = unsafe { (*ehdr).e_phoff } as usize;
    let e_phnum = unsafe { (*ehdr).e_phnum } as usize;
    let e_phentsize = unsafe { (*ehdr).e_phentsize } as usize;
    let entry = unsafe { (*ehdr).e_entry };
    if e_phoff == 0 || e_phentsize == 0 || e_phnum == 0 || e_phoff + e_phnum * e_phentsize > hdr_len
    {
        return fail(ENOEXEC);
    }

    // Collect PT_LOAD segments, validating each against the file size
    // before the old image is torn down (C: exec_elf.c sanity check).
    let mut segs: [(u64, u64, u64, u64, u32); MAX_EXEC_SEGS] = [(0, 0, 0, 0, 0); MAX_EXEC_SEGS];
    let mut nsegs = 0usize;
    for i in 0..e_phnum {
        let ph = unsafe { &*(hdr_buf.as_ptr().add(e_phoff + i * e_phentsize) as *const Elf64Phdr) };
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        if ph.p_offset + ph.p_filesz > file_size as u64 {
            return fail(ENOEXEC);
        }
        if nsegs < MAX_EXEC_SEGS {
            segs[nsegs] = (ph.p_vaddr, ph.p_memsz, ph.p_offset, ph.p_filesz, ph.p_flags);
            nsegs += 1;
        }
    }
    if nsegs == 0 {
        return fail(ENOEXEC);
    }

    // Code range = union of PT_LOAD segment extents, page-aligned. The
    // kernel clears this range in the fresh exec'd page table so the lazy
    // file regions fault on first touch instead of aliasing identity RAM.
    let mut code_start = u64::MAX;
    let mut code_end = 0u64;
    for &(vaddr, memsz, _off, _filesz, _p_flags) in &segs[..nsegs] {
        if vaddr < code_start {
            code_start = vaddr;
        }
        let seg_end = vaddr + memsz;
        if seg_end > code_end {
            code_end = seg_end;
        }
    }
    let code_start = code_start & !0xFFF;
    let code_end = (code_end + 0xFFF) & !0xFFF;

    // Fresh address space for the new image: VM clears the old region list
    // (closing file vmfds) and re-establishes the heap; the kernel builds
    // the fresh page table at SYS_EXEC_LOAD time.
    if vm_exec_newmem(proc_e) != 0 {
        return fail(ENOMEM);
    }

    // Map each PT_LOAD segment as a lazy file-backed region. Pages are
    // demand-paged from the file on first touch; pages at or past the
    // segment's in-file end (bss / partial tails) are zero-filled by VM.
    let mut mapped_any = false;
    for &(vaddr, memsz, off, filesz, p_flags) in &segs[..nsegs] {
        // ELF p_flags: PF_X=0x1, PF_W=0x2, PF_R=0x4 → PROT_READ/WRITE/EXEC.
        // The exec bit must reach VM's do_vfs_mmap, which marks the region
        // VR_EXEC; on RISC-V an executable region without the X PTE bit
        // faults on every instruction fetch.
        let mut prot = 0;
        if p_flags & 0x04 != 0 {
            prot |= minix_std::vmem::PROT_READ;
        }
        if p_flags & 0x02 != 0 {
            prot |= minix_std::vmem::PROT_WRITE;
        }
        if p_flags & 0x01 != 0 {
            prot |= minix_std::vmem::PROT_EXEC;
        }
        let r = crate::vfs::mmap::vfs_memmap(
            proc_e,
            off as i64,
            memsz,
            dev,
            inode_nr,
            vmfd,
            vaddr,
            0,
            prot,
            off + filesz,
        );
        if r != 0 {
            return err_partial(r);
        }
        mapped_any = true;
    }

    // Hand the entry + frame to the kernel, which builds the fresh page
    // table (clearing the code range), maps stack/brk into it, sets up the
    // frame and registers, and makes the target runnable at the new entry
    // point.
    let mut kmsg = [0u8; 64];
    kmsg[EXEC_LOAD_ENDPT_OFF..EXEC_LOAD_ENDPT_OFF + 4].copy_from_slice(&proc_e.to_le_bytes());
    kmsg[EXEC_LOAD_ENTRY_OFF..EXEC_LOAD_ENTRY_OFF + 8].copy_from_slice(&entry.to_le_bytes());
    kmsg[EXEC_LOAD_CODE_START_OFF..EXEC_LOAD_CODE_START_OFF + 8]
        .copy_from_slice(&code_start.to_le_bytes());
    kmsg[EXEC_LOAD_CODE_END_OFF..EXEC_LOAD_CODE_END_OFF + 8]
        .copy_from_slice(&code_end.to_le_bytes());
    kmsg[EXEC_LOAD_FRAME_PTR_OFF..EXEC_LOAD_FRAME_PTR_OFF + 8]
        .copy_from_slice(&(core::ptr::addr_of!(EXEC_FRAME_BUF) as *const u8 as u64).to_le_bytes());
    kmsg[EXEC_LOAD_FRAME_LEN_OFF..EXEC_LOAD_FRAME_LEN_OFF + 8]
        .copy_from_slice(&(frame_len as u64).to_le_bytes());
    let kresult = minix_rt::kernel_call(SYS_EXEC_LOAD, &mut kmsg);
    if kresult != 0 {
        return err_partial(kresult);
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

    // VM owns the vmfd now (it sends FDCLOSE when the last region using it
    // dies); VFS only releases its vnode reference.
    let _ = mapped_any;
    unsafe { put_vnode(vp) };

    ExecResult {
        status: OK,
        partial: false,
        pc,
        newsp,
    }
}

/// Send VM_EXEC_NEWMEM: have VM build a fresh address space for `target`
/// (new page table, cleared region list, heap region) and bind it.
///
/// # Safety
///
/// `target` must be a valid user-process endpoint.
#[cfg(target_os = "minix")]
unsafe fn vm_exec_newmem(target: i32) -> i32 {
    let mut msg = [0u8; 64];
    msg[4..8].copy_from_slice(&arch_common::com::VM_EXEC_NEWMEM.to_le_bytes());
    msg[8..12].copy_from_slice(&target.to_le_bytes());
    let r = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            VM_PROC_NR as u64,
            msg.as_mut_ptr() as u64,
        )
    };
    if r < 0 {
        return r as i32;
    }
    i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]))
}

/// Host stub — the exec path cannot run outside the MINIX target.
///
/// # Safety
///
/// All parameters are ignored in this host stub; the function is `unsafe`
/// only to mirror the `target_os = "minix"` signature.
#[cfg(not(target_os = "minix"))]
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
        partial: false,
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
#[cfg(target_os = "minix")]
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
