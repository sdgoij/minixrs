//! Error types for block cache operations.

use core::fmt;

/// Errno-style error codes used by the filesystem layer.
pub const OK: i32 = 0;
pub const EINVAL: i32 = -22;
pub const EIO: i32 = -5;
pub const ENOSPC: i32 = -28;
pub const ENXIO: i32 = -6;
pub const ENOSYS: i32 = -38;
pub const END_OF_FILE: i32 = -204;

/// Filesystem-level error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// Invalid argument.
    InvalidArgument,
    /// I/O error.
    IoError,
    /// No space left.
    NoSpace,
    /// Device not configured.
    NoDevice,
    /// Function not implemented.
    NotImplemented,
    /// End of file.
    EndOfFile,
    /// Other errno value.
    Errno(i32),
}

impl FsError {
    /// Create an `FsError` from a raw errno value.
    pub fn from_errno(errno: i32) -> Self {
        match errno {
            OK => unreachable!("OK is not an error"),
            EINVAL => FsError::InvalidArgument,
            EIO => FsError::IoError,
            ENOSPC => FsError::NoSpace,
            ENXIO => FsError::NoDevice,
            ENOSYS => FsError::NotImplemented,
            END_OF_FILE => FsError::EndOfFile,
            other => FsError::Errno(other),
        }
    }

    /// Convert to a raw errno value.
    pub fn to_errno(&self) -> i32 {
        match self {
            FsError::InvalidArgument => EINVAL,
            FsError::IoError => EIO,
            FsError::NoSpace => ENOSPC,
            FsError::NoDevice => ENXIO,
            FsError::NotImplemented => ENOSYS,
            FsError::EndOfFile => END_OF_FILE,
            FsError::Errno(e) => *e,
        }
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::InvalidArgument => write!(f, "Invalid argument"),
            FsError::IoError => write!(f, "I/O error"),
            FsError::NoSpace => write!(f, "No space left on device"),
            FsError::NoDevice => write!(f, "Device not configured"),
            FsError::NotImplemented => write!(f, "Function not implemented"),
            FsError::EndOfFile => write!(f, "End of file"),
            FsError::Errno(e) => write!(f, "Unknown error {}", e),
        }
    }
}
