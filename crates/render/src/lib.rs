//! The renderer's scene core: integer geometry, damage regions and the
//! per-frame pass list.
//!
//! `.notes/gui.md` §9 asks for the pass list to land with the first renderer
//! milestone rather than be retrofitted, so this crate starts with the part
//! that needs no GL. It is `no_std`, allocation-only and host-testable; the
//! GLES backend, the glyph atlas and the dmabuf import path join it in the
//! same crate, which is the Renderer layer of `gui.md` §2.
//!
//! Nothing here touches a file descriptor, the framebuffer or the loader, so
//! `cargo test -p render` pins the ordering and damage rules that the guest
//! gates will later exercise end to end.

#![no_std]

extern crate alloc;

pub mod backend;
pub mod geom;
pub mod pass;
pub mod pixels;
pub mod region;
pub mod text;

#[cfg(target_os = "minix")]
pub mod gl;

pub use backend::Backend;
pub use geom::{Point, Rect};
pub use pass::{Color, DrawOp, Element, Pass, TextureId};
pub use region::Region;
