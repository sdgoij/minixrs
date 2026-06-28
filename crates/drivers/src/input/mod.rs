//! PS/2 input driver — keyboard and mouse.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/hid/pckbd/`

#![allow(clippy::identity_op)]

pub mod constants;
pub mod controller;
pub mod driver;
pub mod keyboard;
pub mod mouse;
pub mod scanmap;
