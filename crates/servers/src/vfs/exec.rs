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
// Both arms read a file at exec time — the ELF arm a header, the module arm the whole program.
#[cfg(target_os = "minix")]
use crate::vfs::request::req_read;
// `Filp` is narrower than the other two: it is here for the ELF arm's vmfd, which nothing on
// wasm opens.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
use crate::vfs::types::Filp;
#[cfg(target_os = "minix")]
use crate::vfs::types::{Fproc, Lookup};

#[cfg(target_os = "minix")]
use arch_common::com::VFS_PROC_NR;
#[cfg(target_os = "minix")]
use arch_common::com::VM_PROC_NR;
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
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
//   frame_ptr @ 40, frame_len @ 48, path_ptr @ 56.
// The reply reuses 16/24 for PC/newsp (the request fields are consumed
// by then).
//
// entry and code_start are the *image* on both arms — for ELF the entry point and the start of
// the code range VM maps, for wasm the address of the module's bytes and their length. The
// comment in the kernel's `do_exec_load_handler` has the long version; the short one is that the
// wasm image is bytes rather than a mapping, because the host instantiates them.
#[cfg(target_os = "minix")]
const EXEC_LOAD_ENDPT_OFF: usize = 8;
// The entry/code-range fields carry the image on both arms (`pm_exec` says how), so the two the
// module arm re-uses are not gated; what stays ELF-only is the code range's *end* and the
// PC/newsp reply, which the module arm has no use for.
#[cfg(target_os = "minix")]
const EXEC_LOAD_ENTRY_OFF: usize = 16;
#[cfg(target_os = "minix")]
const EXEC_LOAD_CODE_START_OFF: usize = 24;
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
const EXEC_LOAD_CODE_END_OFF: usize = 32;
#[cfg(target_os = "minix")]
const EXEC_LOAD_FRAME_PTR_OFF: usize = 40;
#[cfg(target_os = "minix")]
const EXEC_LOAD_FRAME_LEN_OFF: usize = 48;
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
const EXEC_LOAD_PC_OFF: usize = 16;
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
const EXEC_LOAD_NEWSP_OFF: usize = 24;
// The path field is what the wasm arm reads instead. VFS sends it on every arch (one code
// path for the request) and the ELF arm ignores it: there the entry point and the code range
// already name the image.
#[cfg(all(target_os = "minix", target_arch = "wasm32"))]
const EXEC_LOAD_PATH_PTR_OFF: usize = 56;

// VM_EXEC_NEWMEM: target endpoint in m1i1 (payload bytes 8..12).
// (The call number comes from arch_common::com::VM_EXEC_NEWMEM.)

/// Upper bound for the exec stack frame (matches C's `ARG_MAX`-style limit).
#[cfg(target_os = "minix")]
const EXEC_FRAME_MAX: usize = 16384;
/// ELF headers read at exec time (ehdr + program headers for typical
/// binaries). The image itself is never read whole: VM demand-pages it from
/// the file through file-backed regions, so there is no executable size cap.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
const EXEC_HDR_MAX: usize = 8192;
/// Maximum PT_LOAD segments mapped per exec (bounds the stack array).
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
const MAX_EXEC_SEGS: usize = 8;

// Scratch frame buffer (VFS is effectively single-threaded; C uses a static
// `mbuf[ARG_MAX]` for the same purpose). The ELF image is never buffered in
// VFS — only the headers are read, into a small stack array in `pm_exec`.
#[cfg(target_os = "minix")]
static mut EXEC_FRAME_BUF: [u8; EXEC_FRAME_MAX] = [0u8; EXEC_FRAME_MAX];

/// Room for a wasm module read at exec time. The image *is* the bytes on this arch, so unlike the
/// ELF arm — which reads a header and lets VM demand-page the rest — this arm has to hold the
/// whole program. 256 KiB fits the port's one program module (141 KiB Asyncify'd) with room to
/// grow, and a module that will not fit is refused rather than truncated: a truncated wasm module
/// is not a smaller program, it is an invalid one.
#[cfg(target_arch = "wasm32")]
const EXEC_MODULE_MAX: usize = 256 * 1024;

#[cfg(target_arch = "wasm32")]
static mut EXEC_MODULE_BUF: [u8; EXEC_MODULE_MAX] = [0u8; EXEC_MODULE_MAX];

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
    /// New effective uid from the setuid bit, or -1 to keep the current one.
    pub euid: i32,
    /// New effective gid from the setgid bit, or -1 to keep the current one.
    pub egid: i32,
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
        euid: -1,
        egid: -1,
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
        euid: -1,
        egid: -1,
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
    // ELF-only: the device is what a file-backed region is mapped from.
    #[cfg(not(target_arch = "wasm32"))]
    let dev = unsafe { (*vp).v_dev };
    let file_size = unsafe { (*vp).v_size };
    if file_size <= 0 {
        unsafe { put_vnode(vp) };
        return err(ENOEXEC);
    }

    // Setuid/setgid exec (C get_read_vp): a setuid bit raises the
    // effective uid to the file's owner, a setgid bit the effective gid to
    // the file's group. The vnode table drops uid/gid from the lookup
    // reply, so fetch them via req_stat (C req_stats the executable into
    // execi->sb here too). -1 = keep the current id.
    let vp_mode = unsafe { (*vp).v_mode };
    let (new_euid, new_egid) = if vp_mode & (0o4000 | 0o2000) != 0 {
        let mut st: minix_std::fs::Stat = unsafe { core::mem::zeroed() };
        let r = unsafe {
            crate::vfs::request::req_stat(
                fs_e,
                inode_nr,
                VFS_PROC_NR,
                &mut st as *mut minix_std::fs::Stat as *mut u8,
                core::mem::size_of::<minix_std::fs::Stat>(),
            )
        };
        if r == OK {
            setuid_ids(vp_mode, st.st_uid, st.st_gid)
        } else {
            (-1, -1)
        }
    } else {
        (-1, -1)
    };

    // Open a VM fd on the executable in VFS's own fproc (C exec.c: the
    // vmfd lives in fproc[VM_PROC_NR]; VM_VFS_MMAP stores it in the region
    // and later FDIO requests read through it).
    //
    // ELF-only, and not merely because nothing on wasm would read through it: a vmfd is a
    // *reference* VFS holds on the executable so VM can demand-page it, and on wasm nothing
    // pages anything — the code is the new instance's own memory. Opening one here would
    // hand VM a file it never touches and leak the descriptor.
    #[cfg(not(target_arch = "wasm32"))]
    let mut vmfd: i32 = -1;
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
    if vmfd < 0 {
        unsafe { put_vnode(vp) };
        return err(ENOMEM);
    }

    // Closing the vmfd is how this arm gives up the executable's vnode: the filp was handed
    // the one reference `vp` carries, so closing it puts the vnode exactly once. C has
    // `/* dup_vnode(vp); */` at the same place, which marks that reference as transferred
    // rather than copied, and its exit path (pm_execfinal) puts the vnode only in the branch
    // where no filp was ever given one.
    #[cfg(not(target_arch = "wasm32"))]
    let close_vmfd = || unsafe {
        let glob_mut = &mut *vfs_global();
        let fproc_arr = core::ptr::addr_of_mut!((*glob_mut).fproc) as *mut Fproc;
        let vmf = &mut *fproc_arr.add((VM_PROC_NR & 0xFF) as usize);
        let _ = crate::vfs::stadir::close_fd(vmf, vmfd);
    };

    // Failure cleanup for the ELF arm. The module arm has its own, below: there is no
    // descriptor to close, and a helper that pretended otherwise would be a lie.
    #[cfg(not(target_arch = "wasm32"))]
    let fail = |s: i32| -> ExecResult {
        close_vmfd();
        err(s)
    };

    // ---------------------------------------------------------- the new image
    //
    // The two arches answer "what is the new program" differently, and everything from the
    // header read below to the kernel call is the ELF answer: VM maps the PT_LOAD segments
    // as file-backed regions and the kernel installs a page table over them. On wasm the
    // image is a wasm module — and since §7.2's step 4 the bytes come off the disk the caller
    // just looked up, so VFS reads the whole file and hands the kernel *where they are*. That
    // is the one thing this arm has to say, and the reason the executable should be a module
    // rather than an ELF: an ELF here would be read, described to a host that cannot run it,
    // and refused — loudly, which is the point.
    #[cfg(target_arch = "wasm32")]
    {
        let module_len = file_size as usize;
        if module_len == 0 {
            unsafe { put_vnode(vp) };
            return err(ENOEXEC);
        }
        if module_len > EXEC_MODULE_MAX {
            unsafe { put_vnode(vp) };
            return err(E2BIG);
        }

        // The whole file, at offset 0. A short read is not a smaller image: `req_read` returns
        // what it read, and a module that stops early would be compiled into a trap by the
        // engine rather than reported as the I/O problem it is.
        let (r, _pos) = unsafe {
            req_read(
                fs_e,
                inode_nr,
                core::ptr::addr_of_mut!(EXEC_MODULE_BUF).cast::<u8>(),
                0,
                module_len as u32,
                VFS_PROC_NR as i32,
                0,
            )
        };
        if r != module_len as i32 {
            unsafe { put_vnode(vp) };
            return err(EIO);
        }

        let mut kmsg = [0u8; 64];
        kmsg[EXEC_LOAD_ENDPT_OFF..EXEC_LOAD_ENDPT_OFF + 4].copy_from_slice(&proc_e.to_le_bytes());
        // The image: where its bytes are and how many, in *this* process's memory. The kernel
        // does not read them — only the host can put a module together, so the kernel's job is
        // to name them and the host's to find out what they are.
        kmsg[EXEC_LOAD_ENTRY_OFF..EXEC_LOAD_ENTRY_OFF + 8].copy_from_slice(
            &(core::ptr::addr_of!(EXEC_MODULE_BUF) as *const u8 as u64).to_le_bytes(),
        );
        kmsg[EXEC_LOAD_CODE_START_OFF..EXEC_LOAD_CODE_START_OFF + 8]
            .copy_from_slice(&(module_len as u64).to_le_bytes());
        kmsg[EXEC_LOAD_FRAME_PTR_OFF..EXEC_LOAD_FRAME_PTR_OFF + 8].copy_from_slice(
            &(core::ptr::addr_of!(EXEC_FRAME_BUF) as *const u8 as u64).to_le_bytes(),
        );
        kmsg[EXEC_LOAD_FRAME_LEN_OFF..EXEC_LOAD_FRAME_LEN_OFF + 8]
            .copy_from_slice(&(frame_len as u64).to_le_bytes());
        // `path_buf` is a local, so its address is only good for the duration of this call —
        // which is what the host's read of it asks for, since it is read while the kernel is
        // inside the call below.
        kmsg[EXEC_LOAD_PATH_PTR_OFF..EXEC_LOAD_PATH_PTR_OFF + 8]
            .copy_from_slice(&(path_buf.as_ptr() as u64).to_le_bytes());

        // The kernel asks the host for the module *first*, and only then is the process's
        // address space reset — the opposite order from the ELF arm, and deliberately. There
        // the segments have to be mapped before the kernel can install anything; here the
        // host either can compile those bytes or it cannot, and if it cannot, nothing about
        // this process has been touched and PM can report the error to the caller instead of
        // the caller being killed for an exec that never happened. That is what the first run
        // of this harness did: a failure after `vm_exec_newmem` reads as "the image was partly
        // replaced", which is `err_partial`, which kills the process — so a program the host
        // could not compile looked like a process that had simply stopped.
        let kresult = minix_rt::kernel_call(SYS_EXEC_LOAD, &mut kmsg);
        if kresult != 0 {
            unsafe { put_vnode(vp) };
            return err(kresult);
        }

        // The address space is reset here rather than above, and the new image still cannot
        // have run: the kernel made the process runnable, but the host's dispatch loop only
        // regains control once this call returns, so VM's bookkeeping is in place before the
        // module's first `brk`. VM's region list describes the *process*, not the image, and
        // the new program's allocator starts from a fresh heap; what is skipped is only the
        // mapping of file regions, which has no meaning when the code is the new instance's
        // own memory.
        if unsafe { vm_exec_newmem(proc_e) } != 0 {
            unsafe { put_vnode(vp) };
            return err_partial(ENOMEM);
        }

        // No PC and no stack pointer: the host's instance starts at the module's own entry,
        // and PM ignores both fields (`exec_restart` does not use them). A zero here says
        // "no such thing on this arch" rather than standing in for a value.
        return unsafe { finish_exec(fp, vp, true, 0, 0, new_euid, new_egid) };
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
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
        if e_phoff == 0
            || e_phentsize == 0
            || e_phnum == 0
            || e_phoff + e_phnum * e_phentsize > hdr_len
        {
            return fail(ENOEXEC);
        }

        // Collect PT_LOAD segments, validating each against the file size
        // before the old image is torn down (C: exec_elf.c sanity check).
        let mut segs: [(u64, u64, u64, u64, u32); MAX_EXEC_SEGS] = [(0, 0, 0, 0, 0); MAX_EXEC_SEGS];
        let mut nsegs = 0usize;
        for i in 0..e_phnum {
            let ph =
                unsafe { &*(hdr_buf.as_ptr().add(e_phoff + i * e_phentsize) as *const Elf64Phdr) };
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
                // No region took the vmfd, so VFS still owns it (C pm_execfinal closes an
                // unused vmfd; a used one travels into the region VM tears down).
                if !mapped_any {
                    close_vmfd();
                }
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
        kmsg[EXEC_LOAD_FRAME_PTR_OFF..EXEC_LOAD_FRAME_PTR_OFF + 8].copy_from_slice(
            &(core::ptr::addr_of!(EXEC_FRAME_BUF) as *const u8 as u64).to_le_bytes(),
        );
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

        // VM owns the vmfd now (it sends FDCLOSE when the last region using it dies), and
        // the vnode reference went with it, so `vp` is not released below. Only a vmfd no
        // region took is closed here, and closing it is what releases that reference.
        if !mapped_any {
            close_vmfd();
        }

        unsafe { finish_exec(fp, vp, false, pc, newsp, new_euid, new_egid) }
    }
}

/// The part of a successful exec that is the same on either arch: close the target's
/// CLOEXEC descriptors, release the executable's vnode, apply the credentials the file's
/// mode implies, and hand the entry point back to PM.
///
/// The two arms of `pm_exec` differ in how the image is installed and agree on everything
/// after that — so this is one function rather than two copies of it that would drift.
///
/// `release_vnode` is what the arms disagree about even here: the ELF arm gave `vp`'s only
/// reference to the vmfd's filp, which keeps the vnode alive for the demand paging that is
/// still to come and puts it when VM closes the fd. Putting it a second time here resets the
/// vnode under that filp — `v_fs_e` back to NONE and `v_inode_nr` to 0 — and VM's next FDIO
/// on the fd then reads a vnode that names no file.
///
/// # Safety
///
/// `fp` must be the target's VFS slot, and `vp` an executable vnode whose reference this
/// call consumes when `release_vnode` is set.
#[cfg(target_os = "minix")]
unsafe fn finish_exec(
    fp: &mut Fproc,
    vp: *mut crate::vfs::types::Vnode,
    release_vnode: bool,
    pc: u64,
    newsp: u64,
    new_euid: i32,
    new_egid: i32,
) -> ExecResult {
    // Close CLOEXEC fds on the target (C: clo_exec(fp)).
    for i in 0..OPEN_MAX {
        if fp.fp_filp[i] >= 0 && (fp.fp_cloexec & (1u64 << i)) != 0 {
            let _ = crate::vfs::stadir::close_fd(fp, i as i32);
        }
    }
    fp.fp_cloexec = 0;

    // The reference VFS took to resolve the path goes back, unless a filp already holds it.
    if release_vnode {
        unsafe { put_vnode(vp) };
    }

    // Apply the setuid/setgid exec to VFS's own fproc (C pm_exec: "If
    // after loading the image we're still allowed to run with setuid or
    // setgid, change credentials now"). PM applies the same ids to mproc
    // from the reply.
    if new_euid != -1 {
        fp.fp_effuid = new_euid as u16;
    }
    if new_egid != -1 {
        fp.fp_effgid = new_egid as u16;
    }

    ExecResult {
        status: OK,
        partial: false,
        pc,
        newsp,
        euid: new_euid,
        egid: new_egid,
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
        euid: -1,
        egid: -1,
    }
}

/// Map a mode's setuid/setgid bits plus the file's owner/group to the
/// new-ids protocol values (C `get_read_vp`): -1 keeps the current id.
#[cfg(any(test, target_os = "minix"))]
fn setuid_ids(mode: u32, uid: u32, gid: u32) -> (i32, i32) {
    let euid = if mode & 0o4000 != 0 { uid as i32 } else { -1 };
    let egid = if mode & 0o2000 != 0 { gid as i32 } else { -1 };
    (euid, egid)
}

#[cfg(test)]
mod tests {
    use super::setuid_ids;

    #[test]
    fn no_setid_bits_keep_current_ids() {
        assert_eq!(setuid_ids(0o755, 0, 0), (-1, -1));
        assert_eq!(setuid_ids(0o100644, 1000, 1000), (-1, -1));
    }

    #[test]
    fn setuid_bit_elevates_to_owner() {
        assert_eq!(setuid_ids(0o4755, 0, 0), (0, -1));
        assert_eq!(setuid_ids(0o4755, 1000, 50), (1000, -1));
    }

    #[test]
    fn setgid_bit_elevates_to_group() {
        assert_eq!(setuid_ids(0o2755, 1000, 60), (-1, 60));
    }

    #[test]
    fn both_bits_set_both_ids() {
        assert_eq!(setuid_ids(0o6755, 1000, 60), (1000, 60));
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
