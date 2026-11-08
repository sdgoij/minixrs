//! The widget layer: layout, focus, hit testing and painting.
//!
//! `.notes/gui.md` §2 puts widgets one layer above the scene. This crate holds
//! the part that needs no device: it turns an [`Event`] into state changes and a
//! widget tree into pass elements. Nothing here opens a file descriptor, touches
//! the framebuffer or calls GL, so `cargo test -p ui` pins focus movement, hit
//! testing and key handling on the host, and the guest gate only has to prove
//! the widgets are reachable.
//!
//! The widget set is a closed enum rather than a trait object for now, the same
//! choice `render::pass::Element` makes: the kinds are known, an exhaustive match
//! keeps every caller honest when one is added, and a window's widgets need no
//! allocator.

#![no_std]

pub mod button;
pub mod event;
pub mod field;
pub mod form;
pub mod paint;
pub mod theme;

pub use button::Button;
pub use event::{Event, Key, Response};
pub use field::Field;
pub use form::{Form, Widget};
pub use paint::Painter;
pub use theme::Theme;
