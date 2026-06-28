//! libminixfs — block cache for MFS-family filesystems.
//!
//! Ported from Minix 3.3.0 `libminixfs`.  Provides the buffer cache used by
//! MFS, ext2, and ISO 9660 filesystem modules.
//!
//! # Safety
//!
//! This crate uses global mutable state (static mut) accessed via raw pointers.
//! All public functions are `unsafe`.  Callers must ensure single-threaded
//! access or provide their own synchronisation.

pub mod cache;
pub mod constants;
pub mod credentials;
pub mod errors;
pub mod types;

pub use cache::*;
pub use constants::*;
pub use credentials::*;
pub use errors::*;
pub use types::*;
