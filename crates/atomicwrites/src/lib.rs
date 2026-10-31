//! Atomic file writes, as the `atomicwrites` API is used by the shell's settings
//! store.
//!
//! The published crate reaches `rustix` 0.38 for `renameat`/`linkat`, and this
//! tree's rustix fork is 1.x, so the same two operations are spelled with std:
//! `rename` over the target for `AllowOverwrite`, and `hard_link` plus
//! `remove_file` for `DisallowOverwrite` (the link fails when the target
//! exists, which is the guarantee the caller asked for).
//!
//! The temporary file is created beside the target, with `create_new` (the VFS
//! honours `O_EXCL`), so the move is a rename within one filesystem.

use std::fmt;
use std::fs;
use std::io;
use std::path;
use std::sync::atomic::{AtomicU64, Ordering};

pub use OverwriteBehavior::{AllowOverwrite, DisallowOverwrite};

/// Whether to allow overwriting if the target file exists.
#[derive(Clone, Copy)]
pub enum OverwriteBehavior {
    /// Overwrite files silently.
    AllowOverwrite,

    /// Don't overwrite files. `AtomicFile::write` raises `AlreadyExists` when
    /// the target is already there, after the data has been written to the
    /// temporary file.
    DisallowOverwrite,
}

/// Represents an error raised by [`AtomicFile::write`].
#[derive(Debug)]
pub enum Error<E> {
    /// The error originated in the library itself, while it was creating a
    /// temporary file or moving the file into place.
    Internal(io::Error),
    /// The error originated in the user-supplied callback.
    User(E),
}

impl From<Error<io::Error>> for io::Error {
    fn from(e: Error<io::Error>) -> Self {
        match e {
            Error::Internal(x) | Error::User(x) => x,
        }
    }
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Internal(e) => e.fmt(f),
            Error::User(e) => e.fmt(f),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for Error<E> {}

/// A file that is written under a temporary name and then moved into place.
pub struct AtomicFile {
    /// Path to the final file that is atomically written.
    path: path::PathBuf,
    overwrite: OverwriteBehavior,
    /// Directory the temporary file is created in. It must be on the target's
    /// filesystem for the move to be a rename.
    tmpdir: path::PathBuf,
}

impl AtomicFile {
    /// Helper for writing to the file at `path` atomically.
    ///
    /// With [`DisallowOverwrite`], `write` returns an `AlreadyExists` error when
    /// the file is already there.
    pub fn new<P>(path: P, overwrite: OverwriteBehavior) -> Self
    where
        P: AsRef<path::Path>,
    {
        let p = path.as_ref();
        AtomicFile::new_with_tmpdir(
            p,
            overwrite,
            p.parent().unwrap_or_else(|| path::Path::new(".")),
        )
    }

    /// Like [`AtomicFile::new`], but the temporary file is created in `tmpdir`.
    pub fn new_with_tmpdir<P, Q>(path: P, overwrite: OverwriteBehavior, tmpdir: Q) -> Self
    where
        P: AsRef<path::Path>,
        Q: AsRef<path::Path>,
    {
        AtomicFile {
            path: path.as_ref().to_path_buf(),
            overwrite,
            tmpdir: tmpdir.as_ref().to_path_buf(),
        }
    }

    /// Get the target filepath.
    pub fn path(&self) -> &path::Path {
        &self.path
    }

    /// Open a temporary file, call `f` on it (which is supposed to write to it),
    /// then move the file atomically to [`Self::path`].
    pub fn write<T, E, F>(&self, f: F) -> Result<T, Error<E>>
    where
        F: FnOnce(&mut fs::File) -> Result<T, E>,
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        self.write_with_options(f, options)
    }

    /// [`AtomicFile::write`] with custom [`fs::OpenOptions`] for the temporary
    /// file.
    pub fn write_with_options<T, E, F>(&self, f: F, options: fs::OpenOptions) -> Result<T, Error<E>>
    where
        F: FnOnce(&mut fs::File) -> Result<T, E>,
    {
        let tmp = TempPath::create(&self.tmpdir).map_err(Error::Internal)?;
        let rv = {
            let mut file = options.open(&tmp.path).map_err(Error::Internal)?;
            let r = f(&mut file).map_err(Error::User)?;
            file.sync_all().map_err(Error::Internal)?;
            r
        };
        let moved = match self.overwrite {
            AllowOverwrite => replace_atomic(&tmp.path, &self.path),
            DisallowOverwrite => move_atomic(&tmp.path, &self.path),
        };
        moved.map_err(Error::Internal)?;
        // The temporary name is gone (renamed or linked away); the guard's
        // removal is a no-op now.
        tmp.release();
        Ok(rv)
    }
}

/// A temporary file name that is removed when it is dropped unless released.
struct TempPath {
    path: path::PathBuf,
    released: bool,
}

/// A counter mixed into the name, so two writes in the same instant differ.
static NEXT: AtomicU64 = AtomicU64::new(0);

impl TempPath {
    fn create(dir: &path::Path) -> io::Result<Self> {
        for _ in 0..64 {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let mut path = dir.to_path_buf();
            path.push(format!(".atomicwrite.{}.{}", std::process::id(), n));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => {
                    return Ok(Self {
                        path,
                        released: false,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "64 atomic-write temporary names in a row were taken",
        ))
    }

    fn release(mut self) {
        self.released = true;
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if !self.released {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Move `src` over `dst`, replacing it.
fn replace_atomic(src: &path::Path, dst: &path::Path) -> io::Result<()> {
    fs::rename(src, dst)
}

/// Move `src` to `dst`, failing with `AlreadyExists` when `dst` is there.
///
/// A hard link is the primitive with that guarantee on this port; it is what
/// the published crate did after `RENAME_NOREPLACE` answered "no such call".
fn move_atomic(src: &path::Path, dst: &path::Path) -> io::Result<()> {
    fs::hard_link(src, dst)?;
    fs::remove_file(src)
}
