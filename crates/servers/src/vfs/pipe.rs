//! VFS pipe implementation — in-VFS pipe buffers.
//!
//! Unlike original MINIX which uses a separate PipeFS (PFS) server
//! process, this implementation manages pipe buffers directly within
//! the VFS server.  Pipes are synchronous: reads return `EAGAIN` when
//! empty (no suspension yet), writes succeed until the pipe buffer is
//! full.

use core::cell::UnsafeCell;

use crate::vfs::consts::PIPE_BUF_SIZE;

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
pub struct Pipe {
    pub data: [u8; PIPE_BUF_SIZE as usize],
    pub head: usize,  // write cursor
    pub tail: usize,  // read cursor
    pub count: usize, // bytes currently buffered
    pub readers: u32, // number of open read ends
    pub writers: u32, // number of open write ends
}

impl Pipe {
    const fn new() -> Self {
        Self {
            data: [0u8; PIPE_BUF_SIZE as usize],
            head: 0,
            tail: 0,
            count: 0,
            readers: 0,
            writers: 0,
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
    let pipes = unsafe { &mut *PIPES.0.get() };
    for (i, p) in pipes.iter_mut().enumerate() {
        if p.readers == 0 && p.writers == 0 {
            p.readers = 1;
            p.writers = 1;
            p.head = 0;
            p.tail = 0;
            p.count = 0;
            return Some(i);
        }
    }
    None
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

/// Decrement the reader count for a pipe.
pub fn release_read_end(idx: usize) {
    if let Some(p) = get_pipe(idx)
        && p.readers > 0
    {
        p.readers -= 1;
    }
}

/// Decrement the writer count for a pipe.
pub fn release_write_end(idx: usize) {
    if let Some(p) = get_pipe(idx)
        && p.writers > 0
    {
        p.writers -= 1;
    }
}

/// Release both ends and reset the pipe buffer for reuse.
/// Called when both ends are closed.
pub fn release_pipe(idx: usize) {
    if let Some(p) = get_pipe(idx)
        && p.readers == 0
        && p.writers == 0
    {
        p.head = 0;
        p.tail = 0;
        p.count = 0;
    }
}
