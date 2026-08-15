//! Public-domain VGA 8×16 font for ASCII 0x20..0x7E. The table lives in
//! `minix-std` (shared with the window server); this module re-exports it
//! for the userland commands.

pub use minix_std::font::FONT_8X16;
