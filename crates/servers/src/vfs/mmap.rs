//! Memory-mapped file support — adapted from `minix/servers/vfs/pipe.c` (map_vnode)
//! and `minix/servers/vfs/exec.c` (vfs_memmap).
//!
//! Vnode remapping for named pipes and mmap grant setup for ELF loading.
//! The VM↔VFS call handler (`do_vm_call`) is in `call.rs`.

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

/// Create a grant-based mmap region for a process's executable.
///
/// Called during exec to map the ELF binary segments into the process's
/// address space via VM.
///
/// TODO: call minix_vfs_mmap(...) with grant region setup.
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
    clearend: u16,
    protflags: i32,
) -> i32 {
    let _ = (
        proc_e, foffset, len, dev, inode_nr, vmfd, vaddr, clearend, protflags,
    );
    ENOSYS
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
