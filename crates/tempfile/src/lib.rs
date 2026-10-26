//! `tempfile::tempfile_in`, and nothing else.
//!
//! The published crate gates its implementation on `cfg(unix)`/`cfg(windows)`, and this port is
//! neither (it does not declare `target_family = "unix"`, `.notes/wayland.md` 6.13/4b), so it
//! compiles the "unsupported platform" stub and every call fails with `Unsupported`. Smithay's
//! only use is `tempfile_in`, to hand an xkb keymap to a client by descriptor
//! (`smithay::input::keyboard::KeymapFile`), so this crate is that one function.
//!
//! It is a stand-in to be patched into a foreign graph, like `crates/errno`: it is excluded from
//! the root workspace and consumed only through `[patch.crates-io]`.
//!
//! **The file is deliberately not unlinked.** MFS drops an inode to zero links and frees it even
//! while a descriptor holds it, so unlinking here would leave the client with an empty keymap; the
//! name therefore stays behind (one small file per `wl_keyboard` bind). A name table that
//! survives an unlink is the general fix, and the same one POSIX shared memory would need.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// A counter mixed into the name, so two files created in the same nanosecond differ.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Creates a fresh file in `dir`, opened read-write.
///
/// # Errors
///
/// Any error from creating the file, or `AlreadyExists` if 64 names in a row were taken.
pub fn tempfile_in<P: AsRef<Path>>(dir: P) -> io::Result<File> {
    let dir = dir.as_ref();
    for _ in 0..64 {
        let path = dir.join(format!(".tmp-{:016x}", unique()));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok(file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused temporary file name",
    ))
}

/// Nanoseconds since the epoch mixed with a per-process counter.
fn unique() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ NEXT.fetch_add(1, Ordering::Relaxed).rotate_left(32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom, Write};

    #[test]
    fn creates_a_readable_file_per_call() {
        let dir = std::env::temp_dir();
        let mut first = tempfile_in(&dir).expect("first file");
        let second = tempfile_in(&dir).expect("second file");
        first.write_all(b"keymap").expect("write");
        first.seek(SeekFrom::Start(0)).expect("seek");
        let mut got = Vec::new();
        first.read_to_end(&mut got).expect("read");
        assert_eq!(got, b"keymap");
        assert_eq!(second.metadata().expect("second metadata").len(), 0);
    }
}
