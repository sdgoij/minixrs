//! Memory-mapped file support — adapted from `minix/servers/vfs/pipe.c` (map_vnode)
//! and `minix/servers/vfs/exec.c` (vfs_memmap).
//!
//! Vnode remapping for named pipes and the VM_VFS_MMAP grant setup for ELF
//! loading. The VM↔VFS call handler (`do_vm_call`) is in `call.rs`.

use crate::vfs::consts::*;
use crate::vfs::types::Vnode;

/// Map a vnode to a specific FS endpoint (e.g., PFS for named pipes).
///
/// Sends REQ_NEWNODE to the target FS to create a mapped node, then
/// updates the vnode's v_mapfs_e and v_fs_e to point to the new FS.
///
/// If `vp->v_mapfs_e != NONE`, the vnode is already mapped — returns OK.
///
/// TODO: call req_newnode(fs_e, ...), update vp fields on success.
/// Real implementation needs: FS request wrappers (Phase 10.2), vmnt lookup.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/pipe.c` (map_vnode)
pub fn map_vnode(vp: *mut Vnode, map_to_fs_e: i32) -> i32 {
    let _ = (vp, map_to_fs_e);
    // TODO: if vp->v_mapfs_e != NONE, return OK
    // vmp = find_vmnt(map_to_fs_e)
    // req_newnode(map_to_fs_e, ...) -> NodeDetails
    // update vp->v_mapfs_e, vp->v_fs_e, vp->v_dev, vp->v_inode_nr
    ENOSYS
}

// VM_VFS_MMAP message layout (VFS → VM, m_type = VM_VFS_MMAP). Offsets are
// absolute message-byte offsets into the 64-byte buffer; the payload starts
// at byte 8. The C equivalent is `m_vm_vfs_mmap` in `<minix/ipc.h>`.
#[cfg(target_os = "minix")]
const VMMAP_WHO_OFF: usize = 8; // i32 — target endpoint
#[cfg(target_os = "minix")]
const VMMAP_FD_OFF: usize = 12; // i32 — VM fd (in VFS's own fproc)
#[cfg(target_os = "minix")]
const VMMAP_FLAGS_OFF: usize = 16; // i32 — MVM_WRITABLE etc.
#[cfg(target_os = "minix")]
const VMMAP_LEN_OFF: usize = 20; // u64 — mapped length (memsz rounded up)
#[cfg(target_os = "minix")]
const VMMAP_VADDR_OFF: usize = 28; // u64 — segment virtual address
#[cfg(target_os = "minix")]
const VMMAP_OFFSET_OFF: usize = 36; // u64 — file offset
#[cfg(target_os = "minix")]
const VMMAP_SIZE_OFF: usize = 44; // u64 — in-file end (p_offset + p_filesz)
#[cfg(target_os = "minix")]
const VMMAP_DEV_OFF: usize = 52; // u32 — backing file device
#[cfg(target_os = "minix")]
const VMMAP_INO_OFF: usize = 56; // u32 — backing file inode

/// Map a file-backed region for exec: VM creates a lazy VR_FILE region at
/// `vaddr` covering `len` bytes of `vmfd`'s file starting at `foffset`.
///
/// `vmfd` is a VM fd in VFS's own fproc (VFS opened the executable there
/// before calling this), so VM stores it in the region for later FDIO
/// requests. `file_size` is the segment's in-file end (`foffset + filesz`):
/// VM zero-fills pages at or past it (bss) instead of reading them from the
/// file. `clearend` is subsumed by that handling and is accepted for
/// signature compatibility with C `vfs_memmap`.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/exec.c` (vfs_memmap)
#[allow(clippy::too_many_arguments)]
pub fn vfs_memmap(
    proc_e: i32,
    foffset: i64,
    len: u64,
    dev: u32,
    inode_nr: u32,
    vmfd: i32,
    vaddr: u64,
    _clearend: u16,
    protflags: i32,
    file_size: u64,
) -> i32 {
    #[cfg(target_os = "minix")]
    unsafe {
        let mut msg = [0u8; 64];
        msg[4..8].copy_from_slice(&arch_common::com::VM_VFS_MMAP.to_le_bytes());
        msg[VMMAP_WHO_OFF..VMMAP_WHO_OFF + 4].copy_from_slice(&proc_e.to_le_bytes());
        msg[VMMAP_FD_OFF..VMMAP_FD_OFF + 4].copy_from_slice(&vmfd.to_le_bytes());
        msg[VMMAP_FLAGS_OFF..VMMAP_FLAGS_OFF + 4].copy_from_slice(&protflags.to_le_bytes());
        msg[VMMAP_LEN_OFF..VMMAP_LEN_OFF + 8].copy_from_slice(&len.to_le_bytes());
        msg[VMMAP_VADDR_OFF..VMMAP_VADDR_OFF + 8].copy_from_slice(&vaddr.to_le_bytes());
        msg[VMMAP_OFFSET_OFF..VMMAP_OFFSET_OFF + 8]
            .copy_from_slice(&(foffset as u64).to_le_bytes());
        msg[VMMAP_OFFSET_OFF..VMMAP_OFFSET_OFF + 8]
            .copy_from_slice(&(foffset as u64).to_le_bytes());
        msg[VMMAP_SIZE_OFF..VMMAP_SIZE_OFF + 8].copy_from_slice(&file_size.to_le_bytes());
        msg[VMMAP_DEV_OFF..VMMAP_DEV_OFF + 4].copy_from_slice(&dev.to_le_bytes());
        msg[VMMAP_INO_OFF..VMMAP_INO_OFF + 4].copy_from_slice(&inode_nr.to_le_bytes());

        let r = minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::VM_PROC_NR as u64,
            msg.as_mut_ptr() as u64,
        );
        if r < 0 {
            return r as i32;
        }
        i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]))
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (
            proc_e, foffset, len, dev, inode_nr, vmfd, vaddr, _clearend, protflags, file_size,
        );
        ENOSYS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_vnode_returns_enosys() {
        let mut vp = Vnode::default();
        assert_eq!(map_vnode(&mut vp, 0), ENOSYS);
    }
}
