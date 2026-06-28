//! Pipe read/write operations — adapted from `minix/fs/pfs/read.c`
//!
//! Pipes are unidirectional byte streams backed by a single shared buffer.
//! Reads consume data from the front of the buffer; writes append to the end.
//! The buffer holds at most `PIPE_BUF` bytes.

use crate::pfs::buffer::*;
use crate::pfs::consts::*;
use core::ptr;

/// Handle read/write requests for pipe inodes.
///
/// For a write: data is copied into the pipe buffer at position i_size.
/// For a read: data is copied out from position 0, then the remaining
/// data is shifted to the front.
///
/// Returns OK on success, or a negative errno on failure.
// Reference: read.c fs_readwrite()
pub fn fs_readwrite() -> i32 {
    todo!("fs_readwrite: not yet wired — requires IPC message parsing")
}

/// Perform the actual pipe read.
///
/// Reads `nbytes` from the pipe buffer at `inode_idx`, storing the data
/// in `dst`. On success, returns the number of bytes read.
///
/// If the pipe is empty, returns 0 (EOF for pipe reads).
// Reference: read.c fs_readwrite() — READING path
pub fn pipe_read(inode_idx: u16, dst: &mut [u8], nbytes: usize) -> i32 {
    unsafe {
        let inode = &*crate::pfs::glo::get_inode_ptr(inode_idx as usize);
        let dev = (*inode).i_dev;
        let inum = (*inode).i_num;
        let f_size = (*inode).i_size;

        if f_size <= 0 {
            return 0;
        }

        let nread = if (nbytes as i64) > f_size {
            f_size as usize
        } else {
            nbytes
        };
        let nread = nread.min(PIPE_BUF);

        // Get the buffer for this pipe
        let bp_idx = match get_block(dev, inum) {
            Some(idx) => idx,
            None => return EIO,
        };
        let bp = crate::pfs::glo::get_buf_ptr(bp_idx as usize);

        // Copy data from buffer to caller
        let src = ptr::slice_from_raw_parts((*bp).b_data.as_ptr(), nread);
        ptr::copy_nonoverlapping((*src).as_ptr(), dst.as_mut_ptr(), nread);

        // Shift remaining data to the front
        let remaining = (f_size as usize).saturating_sub(nread);
        if remaining > 0 {
            ptr::copy(
                (*bp).b_data.as_ptr().add(nread),
                (*bp).b_data.as_mut_ptr(),
                remaining,
            );
        }
        // Zero out the vacated portion using raw pointer write
        ptr::write_bytes((*bp).b_data.as_mut_ptr().add(remaining), 0u8, nread);

        // Update size
        let inode = &mut *crate::pfs::glo::get_inode_ptr(inode_idx as usize);
        inode.i_size = remaining as i64;
        inode.i_update = (inode.i_update as u32 | ATIME) as u8;

        put_block(dev, inum);

        nread as i32
    }
}

/// Perform the actual pipe write.
///
/// Writes `nbytes` from `src` into the pipe buffer at `inode_idx`.
/// Returns the number of bytes written, or an error if the buffer
/// would overflow.
// Reference: read.c fs_readwrite() — WRITING path
pub fn pipe_write(inode_idx: u16, src: &[u8], nbytes: usize) -> i32 {
    unsafe {
        let inode = &*crate::pfs::glo::get_inode_ptr(inode_idx as usize);
        let dev = (*inode).i_dev;
        let inum = (*inode).i_num;
        let position = (*inode).i_size as usize;

        if position + nbytes > PIPE_BUF {
            return EFBIG;
        }

        // Get the buffer for this pipe
        let bp_idx = match get_block(dev, inum) {
            Some(idx) => idx,
            None => return EIO,
        };
        let bp = crate::pfs::glo::get_buf_ptr(bp_idx as usize);

        // Copy data from caller to buffer
        ptr::copy_nonoverlapping(
            src.as_ptr(),
            (*bp).b_data.as_mut_ptr().add(position),
            nbytes,
        );

        // Update size and times
        let inode = &mut *crate::pfs::glo::get_inode_ptr(inode_idx as usize);
        inode.i_size = (position + nbytes) as i64;
        inode.i_update = (inode.i_update as u32 | CTIME | MTIME) as u8;

        put_block(dev, inum);

        nbytes as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pfs::glo;
    use crate::pfs::inode::init_inode_cache;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            init_inode_cache();
            crate::pfs::buffer::init_buffer_pool();
        }
    }

    #[test]
    fn test_pipe_read_empty() {
        init();
        let ip = crate::pfs::inode::get_inode(1, 1).unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(pipe_read(ip, &mut buf, 16), 0);
    }

    #[test]
    fn test_pipe_write_and_read() {
        init();
        let ip = crate::pfs::inode::get_inode(1, 2).unwrap();
        let data = b"Hello, pipe!";

        // Write
        let nwritten = pipe_write(ip, data, data.len());
        assert_eq!(nwritten, data.len() as i32);

        unsafe {
            assert_eq!((*glo::get_inode_ptr(ip as usize)).i_size, data.len() as i64);
        }

        // Read back
        let mut buf = [0u8; 64];
        let nread = pipe_read(ip, &mut buf, 64);
        assert_eq!(nread, data.len() as i32);
        assert_eq!(&buf[..nread as usize], data);

        // After reading, pipe should be empty
        unsafe {
            assert_eq!((*glo::get_inode_ptr(ip as usize)).i_size, 0);
        }
    }

    #[test]
    fn test_pipe_write_overflow() {
        init();
        let ip = crate::pfs::inode::get_inode(1, 3).unwrap();
        // Write exactly PIPE_BUF bytes
        let large = [0xABu8; PIPE_BUF];
        let nwritten = pipe_write(ip, &large, PIPE_BUF);
        assert_eq!(nwritten, PIPE_BUF as i32);

        // Another write should fail
        let small = [0xCDu8; 1];
        let nwritten2 = pipe_write(ip, &small, 1);
        assert_eq!(nwritten2, EFBIG);
    }

    #[test]
    fn test_pipe_write_then_read_partial() {
        init();
        let ip = crate::pfs::inode::get_inode(1, 4).unwrap();
        let data = b"ABCDEFGHIJ";
        pipe_write(ip, data, data.len());

        // Read only 4 bytes
        let mut buf = [0u8; 4];
        let nread = pipe_read(ip, &mut buf, 4);
        assert_eq!(nread, 4);
        assert_eq!(&buf, b"ABCD");

        // Read remaining 6 bytes
        let mut buf2 = [0u8; 16];
        let nread2 = pipe_read(ip, &mut buf2, 16);
        assert_eq!(nread2, 6);
        assert_eq!(&buf2[..6], b"EFGHIJ");
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_readwrite_panics() {
        fs_readwrite();
    }
}
