//! A stand-in for `jobserver`, for minix.
//!
//! Upstream's `cfg(unix)` backend is a cross-process protocol: it reads and
//! writes tokens on a pipe whose ends are inherited by children. The minix PAL
//! has neither half, a spawned `Command` cannot be handed an extra descriptor
//! and there is no `pre_exec` to set one up, so that backend does not compile
//! here. Minix also has no `make` above it to share tokens with, and every token
//! a compiler running on minix needs is a token for its own codegen threads, so
//! a semaphore in this process is the whole of the protocol.
//!
//! `Client::from_env_ext` reports `Unsupported` rather than looking at the
//! environment: there is no protocol to speak with a parent jobserver.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::Builder;

/// A jobserver whose tokens live in this process.
#[derive(Debug)]
pub struct Client {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    tokens: Mutex<usize>,
    released: Condvar,
}

impl Inner {
    fn take(&self) {
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        while *tokens == 0 {
            tokens = self.released.wait(tokens).unwrap_or_else(|e| e.into_inner());
        }
        *tokens -= 1;
    }

    fn try_take(&self) -> bool {
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if *tokens == 0 {
            false
        } else {
            *tokens -= 1;
            true
        }
    }

    fn give(&self) {
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        *tokens += 1;
        drop(tokens);
        self.released.notify_one();
    }
}

impl Clone for Client {
    fn clone(&self) -> Client {
        Client { inner: Arc::clone(&self.inner) }
    }
}

/// An acquired token, given back when it is dropped.
#[derive(Debug)]
pub struct Acquired {
    inner: Arc<Inner>,
    /// Set by `drop_without_releasing`, which passes the token to a new owner
    /// instead of giving it back here.
    disabled: bool,
}

impl Acquired {
    /// Hand the token to whoever this value is passed to, without returning it.
    pub fn drop_without_releasing(mut self) {
        self.disabled = true;
    }
}

impl Drop for Acquired {
    fn drop(&mut self) {
        if !self.disabled {
            self.inner.give();
        }
    }
}

/// The handle onto a helper thread, which ends when this is dropped.
#[derive(Debug)]
pub struct HelperThread {
    requests: Option<Sender<()>>,
}

impl HelperThread {
    /// Ask the helper to acquire one token and hand it to the callback.
    pub fn request_token(&self) {
        if let Some(requests) = &self.requests {
            // The channel is unbounded and only closes when this handle drops,
            // so a send can only fail after the callback has no reader left.
            let _ = requests.send(());
        }
    }
}

impl Drop for HelperThread {
    fn drop(&mut self) {
        // Closing the channel ends the helper's loop. The thread is not joined:
        // a helper already blocked in `acquire` would keep the join pending, and
        // a process that dropped this is on its way out.
        drop(self.requests.take());
    }
}

/// What `Client::from_env_ext` found. `client` is always `Err` here.
pub struct FromEnv {
    pub client: Result<Client, FromEnvError>,
    pub var: Option<(&'static str, OsString)>,
}

/// Why inheriting a jobserver failed.
#[derive(Debug)]
pub struct FromEnvError {
    kind: FromEnvErrorKind,
}

impl FromEnvError {
    /// Which of the failure cases this is.
    pub fn kind(&self) -> FromEnvErrorKind {
        self.kind
    }
}

impl fmt::Display for FromEnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            FromEnvErrorKind::NoEnvVar => "no jobserver environment variable",
            FromEnvErrorKind::NoJobserver => "no `--jobserver-auth` argument",
            FromEnvErrorKind::NegativeFd => "a jobserver file descriptor is negative",
            FromEnvErrorKind::CannotParse => "the jobserver argument cannot be parsed",
            FromEnvErrorKind::Unsupported => {
                "jobserver inheritance is not supported on this platform"
            }
        })
    }
}

impl std::error::Error for FromEnvError {}

/// The failure classes a caller distinguishes. Mirrors upstream's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromEnvErrorKind {
    NoEnvVar,
    NoJobserver,
    NegativeFd,
    CannotParse,
    Unsupported,
}

impl Client {
    /// A client with `limit` tokens.
    pub fn new(limit: usize) -> io::Result<Client> {
        Ok(Client { inner: Arc::new(Inner { tokens: Mutex::new(limit), released: Condvar::new() }) })
    }

    /// There is no jobserver to inherit on minix.
    ///
    /// # Safety
    ///
    /// Upstream takes ownership of inherited descriptors, which is why its is
    /// `unsafe`; there are none here, and the signature is kept so callers are
    /// unchanged.
    pub unsafe fn from_env_ext(_check_pipe: bool) -> FromEnv {
        FromEnv {
            client: Err(FromEnvError { kind: FromEnvErrorKind::Unsupported }),
            var: None,
        }
    }

    /// Block until a token is free.
    pub fn acquire(&self) -> io::Result<Acquired> {
        self.inner.take();
        Ok(Acquired { inner: Arc::clone(&self.inner), disabled: false })
    }

    /// Take a token if one is free.
    pub fn try_acquire(&self) -> io::Result<Option<Acquired>> {
        if self.inner.try_take() {
            Ok(Some(Acquired { inner: Arc::clone(&self.inner), disabled: false }))
        } else {
            Ok(None)
        }
    }

    /// Take a token without an RAII guard. The caller owes a `release_raw`.
    pub fn acquire_raw(&self) -> io::Result<()> {
        self.acquire().map(|token| token.drop_without_releasing())
    }

    /// Give back a token that `acquire_raw` took.
    pub fn release_raw(&self) -> io::Result<()> {
        self.inner.give();
        Ok(())
    }

    /// How many tokens are free right now.
    pub fn available(&self) -> io::Result<usize> {
        let tokens = self.inner.tokens.lock().unwrap_or_else(|e| e.into_inner());
        Ok(*tokens)
    }

    /// Let a caller acquire tokens through a request/channel pair.
    pub fn into_helper_thread<F>(self, mut f: F) -> io::Result<HelperThread>
    where
        F: FnMut(io::Result<Acquired>) + Send + 'static,
    {
        let (requests, queue) = channel::<()>();
        Builder::new().spawn(move || {
            while queue.recv().is_ok() {
                f(self.acquire());
            }
        })?;
        Ok(HelperThread { requests: Some(requests) })
    }
}
