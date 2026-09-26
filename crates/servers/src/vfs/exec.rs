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
use kernel::elf::{Elf64Ehdr, Elf64Phdr, PT_INTERP, PT_LOAD};

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
// The ELF arm's occupant of the same word: the main program's ELF *header page* VA when a
// `PT_INTERP` loader runs first, 0 for a static image. The loader reads its entry and program
// headers from that page (see `ldso::rtld`).
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
const EXEC_LOAD_MAIN_HDR_OFF: usize = 56;

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
/// Maximum PT_LOAD segments mapped per image (bounds the stack array). An
/// image with a `PT_INTERP` is parsed twice — the loader's margins are its
/// own — so this is a per-image bound, not a per-exec one.
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

/// One ELF image read at exec time: its entry point, its `PT_LOAD` segments,
/// and where its `PT_INTERP` path lives (0 when it has none).
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
struct ExecImage {
    entry: u64,
    /// `(p_vaddr, p_memsz, p_offset, p_filesz, p_flags)`, one per `PT_LOAD`.
    segs: [(u64, u64, u64, u64, u32); MAX_EXEC_SEGS],
    nsegs: usize,
    /// File offset of the `PT_INTERP` string, or 0.
    interp_off: u64,
    /// Its length in the file, NUL included, or 0.
    interp_len: u64,
}

/// The interpreter a dynamically linked image asks for: its parsed image and
/// the file it came from. `vmfd` is a VM fd in VFS's own fproc that holds the
/// interpreter vnode's reference, so closing it releases the vnode.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
struct Interp {
    img: ExecImage,
    inode_nr: u32,
    dev: u32,
    vmfd: i32,
}

/// Parse an ELF header + program headers into an [`ExecImage`], validating every
/// `PT_LOAD` against the file size before the old image is torn down (C
/// `exec_elf.c`'s sanity check). `None` is `ENOEXEC`.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
fn parse_exec_image(hdr_buf: &[u8], hdr_len: usize, file_size: u64) -> Option<ExecImage> {
    let ehdr = hdr_buf.as_ptr() as *const Elf64Ehdr;
    let e_phoff = unsafe { (*ehdr).e_phoff } as usize;
    let e_phnum = unsafe { (*ehdr).e_phnum } as usize;
    let e_phentsize = unsafe { (*ehdr).e_phentsize } as usize;
    if e_phoff == 0 || e_phentsize == 0 || e_phnum == 0 || e_phoff + e_phnum * e_phentsize > hdr_len
    {
        return None;
    }
    let mut img = ExecImage {
        entry: unsafe { (*ehdr).e_entry },
        segs: [(0, 0, 0, 0, 0); MAX_EXEC_SEGS],
        nsegs: 0,
        interp_off: 0,
        interp_len: 0,
    };
    for i in 0..e_phnum {
        let ph = unsafe { &*(hdr_buf.as_ptr().add(e_phoff + i * e_phentsize) as *const Elf64Phdr) };
        if ph.p_type == PT_INTERP {
            img.interp_off = ph.p_offset;
            img.interp_len = ph.p_filesz;
            continue;
        }
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        if ph.p_offset + ph.p_filesz > file_size {
            return None;
        }
        if img.nsegs < MAX_EXEC_SEGS {
            img.segs[img.nsegs] = (ph.p_vaddr, ph.p_memsz, ph.p_offset, ph.p_filesz, ph.p_flags);
            img.nsegs += 1;
        }
    }
    if img.nsegs == 0 {
        return None;
    }
    Some(img)
}

/// The page-aligned union of an image's `PT_LOAD` extents — the range the kernel
/// clears in the fresh page table so the lazy file regions fault on first touch.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
fn image_range(img: &ExecImage) -> (u64, u64) {
    let mut start = u64::MAX;
    let mut end = 0u64;
    for &(vaddr, memsz, _off, _filesz, _flags) in &img.segs[..img.nsegs] {
        if vaddr < start {
            start = vaddr;
        }
        if vaddr + memsz > end {
            end = vaddr + memsz;
        }
    }
    (start & !0xFFF, (end + 0xFFF) & !0xFFF)
}

/// The `PROT_*` bits an ELF `p_flags` asks for.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
fn seg_prot(p_flags: u32) -> i32 {
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
    prot
}

/// The union of the protections of every `PT_LOAD` whose page range covers the
/// page segment `i` starts on, this segment's own included.
///
/// ELF lets adjacent segments share a partial page — a `.text` whose end is not
/// page-aligned runs into the following `.rodata` on the same page — while a page
/// carries one protection and a VM region one flag set. VM maps the segments in
/// this order and trims the earlier region out of a page the later one claims, so
/// the shared page ends up with this segment's protection, and the two segments'
/// protections are not the same: on RISC-V the earlier one's contains the execute
/// bit (the page holds the tail of `.text`, including the `.plt`) and this one's
/// does not, so the page would lose `PTE_X` — which makes every fetch from it
/// fault again on the retry, forever, because each retry reinstalls the same page
/// table entry. The page needs the union of the two, and the region that owns it
/// is the only place to put it.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
fn shared_page_prot(img: &ExecImage, i: usize) -> i32 {
    let page = img.segs[i].0 & !0xFFF;
    let mut prot = 0;
    for &(vaddr, memsz, _off, _filesz, p_flags) in &img.segs[..img.nsegs] {
        let lo = vaddr & !0xFFF;
        let hi = (vaddr + memsz + 0xFFF) & !0xFFF;
        if page >= lo && page < hi {
            prot |= seg_prot(p_flags);
        }
    }
    prot
}

/// Map every `PT_LOAD` of `img` as a lazy file-backed region. Returns the first
/// error (0 on success) and whether any region took `vmfd` — a taken fd belongs
/// to VM, which closes it when the last region using it dies, so VFS must not
/// close it a second time.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
fn map_exec_image(proc_e: i32, img: &ExecImage, dev: u32, inode_nr: u32, vmfd: i32) -> (i32, bool) {
    let mut mapped_any = false;
    for (i, &(vaddr, memsz, off, filesz, p_flags)) in img.segs[..img.nsegs].iter().enumerate() {
        // ELF p_flags: PF_X=0x1, PF_W=0x2, PF_R=0x4 → PROT_EXEC/WRITE/READ. The
        // exec bit must reach VM's do_vfs_mmap, which marks the region VR_EXEC;
        // on RISC-V an executable region without the X PTE bit faults on every
        // instruction fetch.
        let own = seg_prot(p_flags);
        let shared = shared_page_prot(img, i);
        let mut prot = own | shared;
        if shared & !own != 0 {
            // The execute bit here is the shared page's, not this segment's: the
            // region holds data (`.rodata`, `.dynsym`, a string constant), and
            // VM's exec pre-fault is what makes a kernel-mode copy of a process's
            // buffer work at all — a copy cannot fault a page in. A region that
            // is executable purely for the page it shares is still data, so ask
            // for its pages to be pre-faulted anyway. (`for_prefault` in
            // `vm/mod.rs` reads this.)
            prot |= minix_std::vmem::PROT_PREFAULT;
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
            return (r, mapped_any);
        }
        mapped_any = true;
    }
    (0, mapped_any)
}

/// Open a VM fd on `vp` in VFS's own fproc (the `VM_PROC_NR` slot) and return it
/// (C exec.c: the vmfd lives in `fproc[VM_PROC_NR]`; `VM_VFS_MMAP` stores it in
/// the region and later FDIO requests read through it). The filp takes the one
/// vnode reference `vp` carries, so closing the fd is what puts it. Returns -1
/// when no descriptor could be had, leaving the reference with the caller.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
unsafe fn open_vmfd(vp: *mut crate::vfs::types::Vnode) -> i32 {
    unsafe {
        let glob_mut = &mut *vfs_global();
        let fproc_arr = core::ptr::addr_of_mut!((*glob_mut).fproc) as *mut Fproc;
        let vmf = &mut *fproc_arr.add((VM_PROC_NR & 0xFF) as usize);
        let mut fd = 0i32;
        if crate::vfs::filedes::get_fd(vmf, 0, &mut fd) != OK {
            return -1;
        }
        let filp_idx = crate::vfs::filedes::alloc_filp();
        if filp_idx < 0 {
            return -1;
        }
        let filp_arr = core::ptr::addr_of_mut!((*glob_mut).filp) as *mut Filp;
        let filp = &mut *filp_arr.add(filp_idx as usize);
        filp.filp_vno = vp;
        filp.filp_count = 1;
        filp.filp_mode = 1; // R_BIT
        vmf.fp_filp[fd as usize] = filp_idx;
        fd
    }
}

/// Close a VM fd opened by [`open_vmfd`], releasing the executable's vnode with
/// it. A close failure here has no recovery — the descriptor is being torn down
/// either way — so it is not propagated.
#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
unsafe fn close_vmfd(fd: i32) {
    unsafe {
        let glob_mut = &mut *vfs_global();
        let fproc_arr = core::ptr::addr_of_mut!((*glob_mut).fproc) as *mut Fproc;
        let vmf = &mut *fproc_arr.add((VM_PROC_NR & 0xFF) as usize);
        let _ = crate::vfs::stadir::close_fd(vmf, fd);
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
    //
    // A dynamically linked image takes a second one, for the loader: each image is demand-
    // paged from its own file (see the PT_INTERP branch below).
    #[cfg(not(target_arch = "wasm32"))]
    let main_vmfd = unsafe { open_vmfd(vp) };
    #[cfg(not(target_arch = "wasm32"))]
    if main_vmfd < 0 {
        unsafe { put_vnode(vp) };
        return err(ENOMEM);
    }

    // Close each fd in `close` that is still open (a negative entry names one that was never
    // opened). A vmfd a region has taken belongs to VM and must not be closed here; every
    // caller of `fail` is before the segments are mapped, so none is taken yet.
    // The module arm has its own cleanup, below: there is no descriptor to close, and a
    // helper that pretended otherwise would be a lie.
    #[cfg(not(target_arch = "wasm32"))]
    let fail = |s: i32, close: [i32; 2]| -> ExecResult {
        unsafe {
            for fd in close {
                if fd >= 0 {
                    close_vmfd(fd);
                }
            }
        }
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
        // is never read whole: VM demand-pages the segments from the file. The
        // interpreter's headers come into the same buffer later — whatever is in it
        // now has been copied out into `main` — so one buffer serves both images.
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
            return fail(EIO, [main_vmfd, -1]);
        }
        let main = match parse_exec_image(&hdr_buf[..hdr_len], hdr_len, file_size as u64) {
            Some(img) => img,
            None => return fail(ENOEXEC, [main_vmfd, -1]),
        };

        // A `PT_INTERP` makes this a dynamically linked image: what the kernel enters is the
        // interpreter (the loader), and the loader is handed the main program's ELF header
        // page so it can find `e_entry`, `PT_DYNAMIC`, and the program headers.
        let mut interp: Option<Interp> = None;
        if main.interp_len != 0 {
            let plen = main.interp_len as usize;
            if plen >= PATH_MAX {
                return fail(ENOEXEC, [main_vmfd, -1]);
            }
            // The path is a NUL-terminated string in the image, and not necessarily on the
            // page the headers are on, so read it from the file rather than search the header
            // buffer. `p_filesz` includes the NUL (ELF says so), and a header that says
            // otherwise is malformed rather than a path of a different length.
            let mut interp_path = [0u8; PATH_MAX];
            let (r, _pos) = unsafe {
                req_read(
                    fs_e,
                    inode_nr,
                    interp_path.as_mut_ptr(),
                    main.interp_off as i64,
                    main.interp_len as u32,
                    VFS_PROC_NR as i32,
                    0,
                )
            };
            if r != main.interp_len as i32 || interp_path[plen - 1] != 0 {
                return fail(ENOEXEC, [main_vmfd, -1]);
            }
            let name_len = plen - 1;
            if name_len == 0 {
                return fail(ENOEXEC, [main_vmfd, -1]);
            }

            let mut ireq = Lookup::default();
            ireq.l_path[..name_len].copy_from_slice(&interp_path[..name_len]);
            ireq.l_path_len = name_len;
            let ivp = unsafe { path::eat_path(&ireq, fp) };
            if ivp.is_null() {
                return fail(ENOENT, [main_vmfd, -1]);
            }
            let ifs_e = unsafe { (*ivp).v_fs_e };
            let iino = unsafe { (*ivp).v_inode_nr };
            let idev = unsafe { (*ivp).v_dev };
            let isize = unsafe { (*ivp).v_size };
            if isize <= 0 {
                unsafe { put_vnode(ivp) };
                return fail(ENOEXEC, [main_vmfd, -1]);
            }
            // From here the loader's vnode reference lives in `ivmfd`'s filp, so it is
            // released by closing `ivmfd` and not by `put_vnode`.
            let ivmfd = unsafe { open_vmfd(ivp) };
            if ivmfd < 0 {
                unsafe { put_vnode(ivp) };
                return fail(ENOMEM, [main_vmfd, -1]);
            }

            let ihdr_len = (isize as usize).min(EXEC_HDR_MAX);
            let (r, _pos) = unsafe {
                req_read(
                    ifs_e,
                    iino,
                    hdr_buf.as_mut_ptr(),
                    0,
                    ihdr_len as u32,
                    VFS_PROC_NR as i32,
                    0,
                )
            };
            if r != ihdr_len as i32 {
                return fail(EIO, [main_vmfd, ivmfd]);
            }
            let img = match parse_exec_image(&hdr_buf[..ihdr_len], ihdr_len, isize as u64) {
                Some(img) => img,
                None => return fail(ENOEXEC, [main_vmfd, ivmfd]),
            };
            // A loader that itself asks for a loader would recurse, and the register
            // convention carries one header page; refuse it rather than run the inner image
            // with no interpreter of its own.
            if img.interp_len != 0 {
                return fail(ENOEXEC, [main_vmfd, ivmfd]);
            }
            interp = Some(Interp {
                img,
                inode_nr: iino,
                dev: idev,
                vmfd: ivmfd,
            });
        }
        let interp_vmfd = interp.as_ref().map_or(-1, |i| i.vmfd);

        // The image the kernel enters is the interpreter when there is one.
        let entry = match &interp {
            Some(i) => i.img.entry,
            None => main.entry,
        };

        // Code range = union of the images' PT_LOAD extents, page-aligned. The
        // kernel clears this range in the fresh exec'd page table so the lazy
        // file regions fault on first touch instead of aliasing identity RAM.
        let (mut code_start, mut code_end) = image_range(&main);
        if let Some(i) = &interp {
            let (s, e) = image_range(&i.img);
            code_start = code_start.min(s);
            code_end = code_end.max(e);
        }

        // The main program's ELF header page: file offset 0 as the image lays it out, which
        // is the VA the loader reads `e_entry` and the program headers from. A `PT_LOAD`
        // beginning at file offset 0 already maps it; the usual case has the first segment
        // at 0x1000, and then the page is mapped separately, read-only. A static image needs
        // none of this and passes 0.
        let mut hdr_va = 0u64;
        let mut hdr_map = false;
        if interp.is_some() {
            let mut lowest = u64::MAX;
            for &(vaddr, _memsz, off, _filesz, _flags) in &main.segs[..main.nsegs] {
                if let Some(b) = vaddr.checked_sub(off)
                    && b < lowest
                {
                    lowest = b;
                }
            }
            if lowest != u64::MAX {
                hdr_va = lowest & !0xFFF;
            }
            let covered = main.segs[..main.nsegs]
                .iter()
                .any(|&(_, _, off, _, _)| off == 0);
            hdr_map = hdr_va != 0 && !covered;
            if hdr_map && hdr_va < code_start {
                code_start = hdr_va;
            }
        }

        // Fresh address space for the new image: VM clears the old region list
        // (closing file vmfds) and re-establishes the heap; the kernel builds
        // the fresh page table at SYS_EXEC_LOAD time.
        if vm_exec_newmem(proc_e) != 0 {
            return fail(ENOMEM, [main_vmfd, interp_vmfd]);
        }

        // Map each image's PT_LOAD segments as lazy file-backed regions. Pages are
        // demand-paged from the file on first touch; pages at or past the segment's
        // in-file end (bss / partial tails) are zero-filled by VM. A failure here is
        // after the address space was replaced, so it is `partial`: PM kills the
        // process rather than reporting the error to a caller that has no image left.
        let mut interp_taken = false;
        if let Some(i) = &interp {
            let (r, taken) = map_exec_image(proc_e, &i.img, i.dev, i.inode_nr, i.vmfd);
            interp_taken = taken;
            if r != 0 {
                if !interp_taken {
                    unsafe { close_vmfd(i.vmfd) };
                }
                unsafe { close_vmfd(main_vmfd) };
                return err_partial(r);
            }
        }
        let (r, main_taken) = map_exec_image(proc_e, &main, dev, inode_nr, main_vmfd);
        if r != 0 {
            // A vmfd no region took is still VFS's to close; a taken one belongs to VM
            // (C pm_execfinal closes an unused vmfd; a used one travels into the region VM
            // tears down).
            if !main_taken {
                unsafe { close_vmfd(main_vmfd) };
            }
            if interp_vmfd >= 0 && !interp_taken {
                unsafe { close_vmfd(interp_vmfd) };
            }
            return err_partial(r);
        }

        // The header page, read-only, at the image's base. It is mapped through the main's
        // vmfd because it is the main's file. At this point the main's own segments have
        // already taken that fd, so only the loader's can still be VFS's to close.
        if hdr_map {
            let r = crate::vfs::mmap::vfs_memmap(
                proc_e,
                0,
                0x1000,
                dev,
                inode_nr,
                main_vmfd,
                hdr_va,
                0,
                minix_std::vmem::PROT_READ,
                0x1000,
            );
            if r != 0 {
                if interp_vmfd >= 0 && !interp_taken {
                    unsafe { close_vmfd(interp_vmfd) };
                }
                return err_partial(r);
            }
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
        // The main program's ELF header page, for the loader `entry` names. Zero for a
        // static image, where no register consumer reads it.
        kmsg[EXEC_LOAD_MAIN_HDR_OFF..EXEC_LOAD_MAIN_HDR_OFF + 8]
            .copy_from_slice(&hdr_va.to_le_bytes());
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

        // VM owns every vmfd a region took now (it sends FDCLOSE when the last region using
        // it dies), and those vnode references went with them, so `vp` is not released below.
        // Only a vmfd no region took is closed here, and closing it is what releases its
        // reference.
        if !main_taken {
            unsafe { close_vmfd(main_vmfd) };
        }
        if interp_vmfd >= 0 && !interp_taken {
            unsafe { close_vmfd(interp_vmfd) };
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
