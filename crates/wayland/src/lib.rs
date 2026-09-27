//! Wayland wire protocol and the core interfaces, shared by the server and
//! clients.
//!
//! The wire format (`wire`) and the interface tables (`protocol`) live here so
//! the server (`/sbin/wlserver`), our client and the host tests all read one
//! definition of the protocol. `WAYLAND.md` §6.11 scopes the phase this serves.
//!
//! This crate is pure protocol — no syscalls, no fd handling — so it is
//! host-testable and shared by both sides of the socket.

#![no_std]

pub mod client;
pub mod input;
pub mod protocol;
pub mod server;
pub mod shm;
pub mod wire;
