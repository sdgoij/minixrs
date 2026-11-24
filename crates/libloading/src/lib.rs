//! A stand-in for `libloading`, for minix.
//!
//! Upstream picks the four `RTLD_*` values from a `cfg` over `target_os` and
//! `compile_error!`s on anything else, so its package cannot be built for this
//! target as published. The port has one loader (`tools/c-include/dlfcn.h`), so
//! this crate has one implementation and no dispatch.
//!
//! The surface is the subset the compiler names: `Library::new`, `Library::get`,
//! `Symbol` as a `Deref` target with `into_raw` and `try_as_raw_ptr`, and an
//! `Error` whose `Display` carries `dlerror`'s message. The port's loader never
//! unloads an object, so `Library` has no `Drop`: a handle stays valid for the
//! process, which is what every caller here already assumes.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::Path;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

/// Bind on first use. The port's loader binds eagerly, so this and `RTLD_NOW`
/// ask the same thing of it.
const RTLD_LAZY: c_int = 0x00001;
/// Symbols reachable only through the object's own handle.
const RTLD_LOCAL: c_int = 0x00000;

/// A loaded shared object.
pub struct Library {
    handle: *mut c_void,
}

// SAFETY: the handle is only read through `&self` (`dlsym`), never changed, and
// POSIX requires the `dl*` family to be thread-safe. The loader owns the object
// the handle names.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl fmt::Debug for Library {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Library").field("handle", &self.handle).finish()
    }
}

/// A symbol resolved from a [`Library`].
pub struct Symbol<'lib, T> {
    pointer: *mut T,
    _library: PhantomData<&'lib Library>,
}

/// Why a load or a lookup failed.
#[derive(Debug)]
pub enum Error {
    /// `dlopen` returned null.
    DlOpen(String),
    /// `dlsym` returned null.
    DlSym(String),
    /// Named only by the compiler's Windows retry check. The minix loader never
    /// produces it, and the variant exists so that check compiles.
    LoadLibraryExW { code: u32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::DlOpen(message) | Error::DlSym(message) => f.write_str(message),
            Error::LoadLibraryExW { code } => write!(f, "LoadLibraryExW failed with code {code}"),
        }
    }
}

impl std::error::Error for Error {}

/// The loader's own message for the call that just failed.
fn last_error() -> String {
    // SAFETY: `dlerror` hands back a NUL-terminated string owned by the loader,
    // and no other loader call intervenes between it and here.
    let raw = unsafe { dlerror() };
    if raw.is_null() {
        "the dynamic loader reported no error".to_string()
    } else {
        // SAFETY: as above, `raw` is a NUL-terminated string.
        unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned()
    }
}

impl Library {
    /// Load `filename` eagerly, as `libloading::Library::new` does.
    ///
    /// # Safety
    ///
    /// Loading an object runs its initialisers, with the soundness
    /// requirements `dlopen` documents.
    pub unsafe fn new(filename: impl AsRef<Path>) -> Result<Library, Error> {
        let name = CString::new(filename.as_ref().as_os_str().as_encoded_bytes())
            .map_err(|_| Error::DlOpen("the path contains a NUL byte".to_string()))?;
        // SAFETY: the caller upholds `dlopen`'s contract.
        let handle = unsafe { dlopen(name.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
        if handle.is_null() { Err(Error::DlOpen(last_error())) } else { Ok(Library { handle }) }
    }

    /// Resolve `symbol`, which may carry a trailing NUL as `libloading`'s
    /// symbol-name conversion allows.
    ///
    /// # Safety
    ///
    /// `T` must be the symbol's real type.
    pub unsafe fn get<T>(&self, symbol: &[u8]) -> Result<Symbol<'_, T>, Error> {
        let symbol = symbol.strip_suffix(&[0]).unwrap_or(symbol);
        let name = CString::new(symbol)
            .map_err(|_| Error::DlSym("the symbol contains a NUL byte".to_string()))?;
        // SAFETY: `self.handle` is a live handle and the caller upholds `T`.
        let pointer = unsafe { dlsym(self.handle, name.as_ptr()) }.cast::<T>();
        if pointer.is_null() {
            Err(Error::DlSym(last_error()))
        } else {
            Ok(Symbol { pointer, _library: PhantomData })
        }
    }
}

impl<T> Deref for Symbol<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `get` produced a pointer to a `T`-typed symbol, and the caller
        // promised `T` is its real type.
        unsafe { &*self.pointer }
    }
}

impl<T> Symbol<'_, T> {
    /// Give up the symbol as a raw pointer, as `libloading::Symbol` does.
    pub fn into_raw(self) -> *mut T {
        self.pointer
    }
}

impl Symbol<'_, *mut c_void> {
    /// The one method beyond `Deref` and `into_raw` that a caller here names. A
    /// `*mut c_void` symbol is its own raw pointer.
    ///
    /// # Safety
    ///
    /// As `dlsym`: the caller is responsible for the pointed-to code's type.
    pub unsafe fn try_as_raw_ptr(self) -> Option<*mut c_void> {
        // SAFETY: `get` wrote a `*mut c_void` at this slot.
        Some(unsafe { *self.pointer })
    }
}
