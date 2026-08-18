//! Pipe read/write operations — adapted from `minix/fs/pfs/read.c`
//!
//! Pipes are unidirectional byte streams backed by a single shared buffer
//! in PFS's block cache. Reads consume data from the front of the buffer;
//! writes append to the end. The buffer holds at most `PIPE_BUF` bytes.

use crate::pfs::buffer::*;
use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::*;
use crate::pfs::types::Buf;

/// SAFECOPYTO kernel call number (KERNEL_CALL + 32).
const SAFECOPYTO_CALL: i32 = 32;
/// SAFECOPYFROM kernel call number (KERNEL_CALL + 31).
const SAFECOPYFROM_CALL: i32 = 31;

/// Handle read/write requests for pipe inodes (REQ_READ / REQ_WRITE).
///
/// The request carries the inode number (u32@0), seek_pos (i64@8), the
/// grant for the user buffer (i32@16), and the byte count (u64@24). Data
/// moves between the caller (VFS) and the pipe buffer through the grant
/// (SYS_SAFECOPY), matching C `fs_readwrite`. On success the reply carries
/// the new pipe size (seek_pos) at payload[0] and the handler returns the
/// byte count transferred.
// Reference: read.c fs_readwrite()
pub fn fs_readwrite() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();
        let data_ptr = core::ptr::addr_of_mut!((*pfs).m_in_data) as *const u8;
        let inum = core::ptr::read_unaligned(data_ptr.add(0) as *const u32);
        let _pos = core::ptr::read_unaligned(data_ptr.add(8) as *const i64);
        let grant = core::ptr::read_unaligned(data_ptr.add(16) as *const i32);
        let nbytes = core::ptr::read_unaligned(data_ptr.add(24) as *const u64) as usize;

        let is_write = (*pfs).m_in_type == REQ_WRITE;

        let rip_idx = match find_inode(inum) {
            Some(i) => i,
            None => return -EINVAL,
        };
        let inode = &*glo::get_inode_ptr(rip_idx as usize);
        if (*inode).i_mode & I_TYPE != I_NAMED_PIPE {
            return -EIO;
        }
        if nbytes > PIPE_BUF {
            return -EFBIG;
        }

        let dev = (*inode).i_dev;
        get_inode(dev, inum); // mark inode in use (C: get_inode)
        let bp_idx = match get_block(dev, inum) {
            Some(i) => i,
            None => {
                put_inode(Some(rip_idx));
                return -EIO;
            }
        };
        let bp = glo::get_buf_ptr(bp_idx as usize);

        let mut cum_io = 0usize;
        let r;
        if is_write {
            let position = (*inode).i_size as usize;
            if position + nbytes > PIPE_BUF {
                put_inode(Some(rip_idx));
                put_block(dev, inum);
                return -EFBIG;
            }
            let dst =
                core::slice::from_raw_parts_mut((*bp).b_data.as_mut_ptr().add(position), nbytes);
            let mut kmsg = [0u8; 64];
            kmsg[8..12].copy_from_slice(&(*pfs).m_source.to_ne_bytes());
            kmsg[12..16].copy_from_slice(&grant.to_ne_bytes());
            kmsg[16..24].copy_from_slice(&0u64.to_ne_bytes());
            kmsg[24..32].copy_from_slice(&(dst.as_mut_ptr() as u64).to_ne_bytes());
            kmsg[32..40].copy_from_slice(&(dst.len() as u64).to_ne_bytes());
            r = minix_rt::kernel_call(SAFECOPYFROM_CALL, &mut kmsg);
            if r == 0 {
                cum_io = nbytes;
                let inode_mut = &mut *glo::get_inode_ptr(rip_idx as usize);
                inode_mut.i_size = (position + nbytes) as i64;
                inode_mut.i_update = (inode_mut.i_update as u32 | CTIME | MTIME) as u8;
            }
        } else {
            let f_size = (*inode).i_size;
            let nrbytes = (nbytes as i64).min(f_size) as usize;
            let src = core::slice::from_raw_parts((*bp).b_data.as_ptr(), nrbytes);
            let mut kmsg = [0u8; 64];
            kmsg[8..12].copy_from_slice(&(*pfs).m_source.to_ne_bytes());
            kmsg[12..16].copy_from_slice(&grant.to_ne_bytes());
            kmsg[16..24].copy_from_slice(&0u64.to_ne_bytes());
            kmsg[24..32].copy_from_slice(&(src.as_ptr() as u64).to_ne_bytes());
            kmsg[32..40].copy_from_slice(&(src.len() as u64).to_ne_bytes());
            r = minix_rt::kernel_call(SAFECOPYTO_CALL, &mut kmsg);
            if r == 0 {
                cum_io = nrbytes;
                shift_buffer(bp, f_size as usize, nrbytes);
                let inode_mut = &mut *glo::get_inode_ptr(rip_idx as usize);
                inode_mut.i_size = (f_size as usize).saturating_sub(nrbytes) as i64;
                inode_mut.i_update = (inode_mut.i_update as u32 | ATIME) as u8;
            }
        }

        // Reply: new pipe size (seek_pos) at payload[0].
        let inode = &*glo::get_inode_ptr(rip_idx as usize);
        let out = core::ptr::addr_of_mut!((*pfs).m_out_data) as *mut u8;
        core::ptr::write_unaligned(out.add(0) as *mut i64, (*inode).i_size);

        put_inode(Some(rip_idx));
        put_block(dev, inum);

        if r != 0 { r } else { cum_io as i32 }
    }
}

/// Move the data left after a read consumed `discard` bytes from the front
/// of the pipe buffer, leaving `keep` bytes at the start.
///
/// # Safety
///
/// `bp` must point to a valid `Buf` with `keep + discard <= PIPE_BUF`.
// Reference: read.c fs_readwrite() — READING path (memmove)
fn shift_buffer(bp: *mut Buf, keep: usize, discard: usize) {
    unsafe {
        if keep > 0 && discard > 0 {
            core::ptr::copy(
                (*bp).b_data.as_ptr().add(discard),
                (*bp).b_data.as_mut_ptr(),
                keep,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            init_inode_cache();
            init_buffer_pool();
        }
    }

    #[test]
    fn test_fs_readwrite_unknown_inode() {
        init();
        let r = fs_readwrite();
        assert_eq!(r, -EINVAL);
    }

    #[test]
    fn test_fs_readwrite_rejects_non_pipe() {
        init();
        let ip = get_inode(1, 5).unwrap();
        unsafe {
            (*glo::get_inode_ptr(ip as usize)).i_mode = I_REGULAR;
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, 5);
            core::ptr::write_unaligned(data.add(24) as *mut u64, 16);
        }
        assert_eq!(fs_readwrite(), -EIO);
    }

    #[test]
    fn test_fs_readwrite_efbig_oversize_request() {
        init();
        let ip = get_inode(1, 6).unwrap();
        unsafe {
            (*glo::get_inode_ptr(ip as usize)).i_mode = I_NAMED_PIPE;
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, 6);
            core::ptr::write_unaligned(data.add(24) as *mut u64, (PIPE_BUF + 1) as u64);
        }
        assert_eq!(fs_readwrite(), -EFBIG);
    }

    #[test]
    fn test_shift_buffer_moves_data_left() {
        init();
        let bp = get_block(1, 7).unwrap();
        let buf = unsafe { glo::get_buf_ptr(bp as usize) };
        unsafe {
            (&mut (*buf).b_data)[..5].copy_from_slice(b"ABCDE");
            // Consume 2 bytes: keep "CDE" at the front.
            shift_buffer(buf, 3, 2);
            assert_eq!(&(&(*buf).b_data)[..3], b"CDE");
        }
        put_block(1, 7);
    }
}
