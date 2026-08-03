//! VFS pipe implementation — in-VFS pipe buffers.
//!
//! Unlike original MINIX which uses a separate PipeFS (PFS) server
//! process, this implementation manages pipe buffers directly within
//! the VFS server.  Pipes are synchronous: reads return `EAGAIN` when
//! empty (no suspension yet), writes succeed until the pipe buffer is
//! full.

use core::cell::UnsafeCell;

use crate::vfs::consts::{NR_FILPS, PIPE_BUF_SIZE};

/// Maximum number of concurrent pipe buffers.
const NR_PIPES: usize = 32;

/// Sentinel bit in `filp_pipe_ino` to distinguish pipe indices from
/// real inode numbers.
const PIPE_SENTINEL: u32 = 0x8000_0000;

/// Mask off the sentinel bit to get the raw pipe index.
#[inline]
pub fn pipe_index_from_filp(ino: u32) -> usize {
    (ino & !PIPE_SENTINEL) as usize
}

/// Encode a pipe index for storage in `filp_pipe_ino`.
#[inline]
pub fn pipe_index_for_filp(idx: usize) -> u32 {
    (idx as u32) | PIPE_SENTINEL
}

/// Returns `true` if a filp inode field refers to a pipe.
#[inline]
pub fn is_pipe_filp(ino: u32) -> bool {
    ino & PIPE_SENTINEL != 0
}

/// A single pipe buffer with independent read and write ends.
///
/// The number of open read/write ends is NOT stored here: it is derived
/// from the filp table on demand (see `pipe_refcounts`). A parallel counter
/// incremented in dup2/fork and decremented in close proved unreliable — a
/// single miscount silently broke EPIPE/EOF semantics.
pub struct Pipe {
    pub data: [u8; PIPE_BUF_SIZE as usize],
    pub head: usize,  // write cursor
    pub tail: usize,  // read cursor
    pub count: usize, // bytes currently buffered
}

impl Pipe {
    const fn new() -> Self {
        Self {
            data: [0u8; PIPE_BUF_SIZE as usize],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn is_full(&self) -> bool {
        self.count >= self.data.len()
    }

    /// Write up to `buf.len()` bytes into the pipe.
    /// Returns the number of bytes actually written.
    pub fn write(&mut self, buf: &[u8]) -> usize {
        let cap = self.data.len();
        let available = cap - self.count;
        let n = buf.len().min(available);
        for &b in buf.iter().take(n) {
            self.data[self.head] = b;
            self.head += 1;
            if self.head >= cap {
                self.head = 0;
            }
        }
        self.count += n;
        n
    }

    /// Read up to `buf.len()` bytes from the pipe.
    /// Returns the number of bytes actually read.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.count);
        for dst in buf.iter_mut().take(n) {
            *dst = self.data[self.tail];
            self.tail += 1;
            if self.tail >= self.tail.max(1).min(self.data.len()) {
                // wrap
            }
            if self.tail >= self.data.len() {
                self.tail = 0;
            }
        }
        self.count -= n;
        n
    }
}

struct PipeTable(UnsafeCell<[Pipe; NR_PIPES]>);

// Safety: single-threaded VFS server, UnsafeCell for interior mutability.
unsafe impl Sync for PipeTable {}

impl PipeTable {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { Pipe::new() }; NR_PIPES]))
    }
}

static PIPES: PipeTable = PipeTable::new();

/// Allocate a new pipe.  Returns the pipe index, or `None` if the table is
/// full.
pub fn alloc_pipe() -> Option<usize> {
    for i in 0..NR_PIPES {
        let (readers, writers) = pipe_refcounts(i);
        if readers == 0 && writers == 0 {
            let pipes = unsafe { &mut *PIPES.0.get() };
            pipes[i].head = 0;
            pipes[i].tail = 0;
            pipes[i].count = 0;
            return Some(i);
        }
    }
    None
}

/// Count the open read and write ends of a pipe by scanning the filp table.
///
/// Deriving the counts from the filps (which are refcounted consistently in
/// dup2, fork and close) keeps EPIPE/EOF semantics correct regardless of the
/// order of dup2/close/fork operations.
pub fn pipe_refcounts(idx: usize) -> (u32, u32) {
    let glob = unsafe { &mut *crate::vfs::glo::vfs_global() };
    let filp_arr = core::ptr::addr_of_mut!(glob.filp) as *mut crate::vfs::types::Filp;
    let mut readers = 0u32;
    let mut writers = 0u32;
    for i in 0..NR_FILPS {
        let f = unsafe { &*filp_arr.add(i) };
        if is_pipe_filp(f.filp_pipe_ino) && pipe_index_from_filp(f.filp_pipe_ino) == idx {
            if f.filp_mode & 1 != 0 {
                readers += 1;
            }
            if f.filp_mode & 2 != 0 {
                writers += 1;
            }
        }
    }
    (readers, writers)
}

/// Get a mutable reference to a pipe by index.  Returns `None` if the index
/// is out of range.
pub fn get_pipe(idx: usize) -> Option<&'static mut Pipe> {
    let pipes = unsafe { &mut *PIPES.0.get() };
    if idx < NR_PIPES {
        Some(&mut pipes[idx])
    } else {
        None
    }
}

/// Release both ends and reset the pipe buffer for reuse.
/// Called when both ends are closed.
pub fn release_pipe(idx: usize) {
    let (readers, writers) = pipe_refcounts(idx);
    if readers == 0
        && writers == 0
        && let Some(p) = get_pipe(idx)
    {
        p.head = 0;
        p.tail = 0;
        p.count = 0;
    }
}

/// Read up to `count` bytes from pipe `idx` into the user buffer at
/// `user_buf` in `user_e`'s address space (used by `do_read`).
///
/// Returns the number of bytes read, 0 for EOF (pipe empty and no writers),
/// EAGAIN when the pipe is empty but a writer is still open (no suspension
/// in this port), or a negative errno.
pub fn pipe_read_user(idx: usize, user_e: i32, user_buf: u64, count: usize) -> i32 {
    if count == 0 {
        return 0;
    }
    let pipe = match get_pipe(idx) {
        Some(p) => p,
        None => return -9, // EBADF
    };
    let (_, writers) = pipe_refcounts(idx);
    if pipe.is_empty() {
        // EOF once all writers are gone; EAGAIN while a writer exists
        // (matching C's non-blocking read on an empty pipe).
        return if writers > 0 { -11 } else { 0 }; // EAGAIN / EOF
    }
    let n = count.min(PIPE_BUF_SIZE as usize).min(pipe.count);
    let mut chunk = [0u8; 256];
    let mut done = 0usize;
    let mut off = 0usize;
    while done < n {
        let want = (n - done).min(chunk.len());
        let got = pipe.read(&mut chunk[..want]);
        if got == 0 {
            break;
        }
        // Copy the data into the reader's address space via the kernel
        // (SYS_VIRCOPY). The local kernel::vm::virtual_copy would run
        // privileged CR3 instructions in user mode (VFS links the kernel
        // crate), faulting in ring 3 and silently failing the copy.
        let r = unsafe {
            crate::vfs::call::sys_vircopy(
                crate::vfs::call::SELF,
                chunk.as_ptr() as u64,
                user_e,
                user_buf + off as u64,
                got,
            )
        };
        if r != 0 {
            break;
        }
        done += got;
        off += got;
    }
    done as i32
}

/// Write up to `count` bytes from the user buffer at `user_buf` in
/// `user_e`'s address space into pipe `idx` (used by `do_write`).
///
/// Returns the number of bytes written, EPIPE when no reader is open,
/// or a negative errno.
pub fn pipe_write_user(idx: usize, user_e: i32, user_buf: u64, count: usize) -> i32 {
    if count == 0 {
        return 0;
    }
    let pipe = match get_pipe(idx) {
        Some(p) => p,
        None => return -9, // EBADF
    };
    let (readers, _) = pipe_refcounts(idx);
    if readers == 0 {
        return -32; // EPIPE
    }
    let available = PIPE_BUF_SIZE as usize - pipe.count;
    if available == 0 {
        return -11; // EAGAIN — pipe full (no suspension in this port)
    }
    let n = count.min(available);
    let mut chunk = [0u8; 256];
    let mut done = 0usize;
    let mut off = 0usize;
    while done < n {
        let want = (n - done).min(chunk.len());
        // Copy the writer's data into VFS via the kernel (SYS_VIRCOPY).
        // The local kernel::vm::virtual_copy would run privileged CR3
        // instructions in user mode (VFS links the kernel crate),
        // faulting in ring 3 and silently failing the copy.
        let r = unsafe {
            crate::vfs::call::sys_vircopy(
                user_e,
                user_buf + off as u64,
                crate::vfs::call::SELF,
                chunk.as_mut_ptr() as u64,
                want,
            )
        };
        if r != 0 {
            break;
        }
        let got = pipe.write(&chunk[..want]);
        if got == 0 {
            break;
        }
        done += got;
        off += got;
    }
    done as i32
}
