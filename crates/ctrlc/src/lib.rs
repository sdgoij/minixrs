//! A stand-in for `ctrlc`, for minix.
//!
//! Upstream installs a handler on a separate thread fed by a signal, through
//! `nix`, whose signal tables have no minix arm. The port has no safe way to run
//! a Rust closure from a signal handler either: the only async-signal-safe work
//! is setting a flag or calling `_exit`, and a closure that sleeps and exits (as
//! the compiler's does) cannot run there.
//!
//! So the handler is not installed and `SIGINT` keeps its default action, which
//! terminates the process at once. That is the behaviour a user pressing Ctrl-C
//! during a long compile expects, and it is preferable to a handler that only
//! sets a flag codegen never reads.

use std::fmt;

/// Why installing a handler failed. Never produced here, and present so callers
/// that name the type compile.
#[derive(Debug)]
pub struct Error;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("minix installs no Ctrl-C handler")
    }
}

impl std::error::Error for Error {}

/// Accept a handler and leave `SIGINT` at its default action.
///
/// The closure is dropped without being called; see the module comment for why.
pub fn set_handler<F>(_handler: F) -> Result<(), Error>
where
    F: FnMut() + Send + 'static,
{
    Ok(())
}
