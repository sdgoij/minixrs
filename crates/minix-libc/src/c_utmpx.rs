//! C `utmpx.h`: the login-record cursor over the session file.
//!
//! The layout and the file live in `minix-std` (`crates/minix-std/src/utmpx.rs`),
//! which is also the side the boot image's writers use, so this module is the
//! C ABI over one implementation rather than a second parser.
//!
//! The whole file is read on the first `getutxent`, because the entry that call
//! returns has to stay valid until the next one: the readers (`who`, `users`,
//! `pinky`, `uptime`) walk the file entry by entry and copy fields out as they
//! go. `setutxent` and `endutxent` both drop what was read, so the next walk
//! sees the file as it is then.

#[cfg(target_os = "minix")]
use core::ffi::{c_char, c_int};

#[cfg(target_os = "minix")]
use minix_std::utmpx::{EMPTY_RECORD, MAX_RECORDS, Utmpx};

/// The path `utmpxname` (or the default) has selected.
#[cfg(target_os = "minix")]
const PATH_MAX: usize = 128;

#[cfg(target_os = "minix")]
const EINVAL: i32 = 22;
#[cfg(target_os = "minix")]
const ENAMETOOLONG: i32 = 36;

#[cfg(target_os = "minix")]
static mut UT_PATH: [u8; PATH_MAX] = [0; PATH_MAX];
#[cfg(target_os = "minix")]
static mut UT_PATH_LEN: usize = 0;
/// The entries read from `UT_PATH`, and the cursor into them.
#[cfg(target_os = "minix")]
static mut UT_RECORDS: [Utmpx; MAX_RECORDS] = [EMPTY_RECORD; MAX_RECORDS];
#[cfg(target_os = "minix")]
static mut UT_COUNT: usize = 0;
#[cfg(target_os = "minix")]
static mut UT_CURSOR: usize = 0;
/// Whether `UT_RECORDS` still describes `UT_PATH`; `endutxent` and `setutxent`
/// clear it so the next `getutxent` re-reads.
#[cfg(target_os = "minix")]
static mut UT_LOADED: bool = false;

/// The selected path, or `DEFAULT_FILE` when `utmpxname` has not run.
#[cfg(target_os = "minix")]
unsafe fn current_path() -> &'static [u8] {
    unsafe {
        if UT_PATH_LEN == 0 {
            minix_std::utmpx::DEFAULT_FILE
        } else {
            &UT_PATH[..UT_PATH_LEN]
        }
    }
}

#[cfg(target_os = "minix")]
unsafe fn load() {
    unsafe {
        UT_CURSOR = 0;
        // A raw pointer, not a reference: the static's address is taken while
        // no other reference to it exists, and `read_records` only writes
        // through the slice it is handed.
        let recs = core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(UT_RECORDS).cast::<Utmpx>(),
            MAX_RECORDS,
        );
        UT_COUNT = minix_std::utmpx::read_records(current_path(), recs).unwrap_or(0);
        UT_LOADED = true;
    }
}

/// Rewind to the first entry; the file is read again on the next `getutxent`,
/// so a writer that ran in between is seen.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setutxent() {
    unsafe {
        UT_LOADED = false;
        UT_COUNT = 0;
        UT_CURSOR = 0;
    }
}

/// Drop the cursor and everything read from the file.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn endutxent() {
    unsafe {
        UT_LOADED = false;
        UT_COUNT = 0;
        UT_CURSOR = 0;
    }
}

/// The next entry, or null at the end of the file.
///
/// The pointer is this library's own static entry: it is valid until the next
/// call, which is the traditional (thread-unsafe) `getutxent` contract.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getutxent() -> *mut Utmpx {
    unsafe {
        if !UT_LOADED {
            load();
        }
        if UT_CURSOR >= UT_COUNT {
            return core::ptr::null_mut();
        }
        let slot = core::ptr::addr_of_mut!(UT_RECORDS[UT_CURSOR]);
        UT_CURSOR += 1;
        slot
    }
}

/// Read the entries from `file` instead of the default session file.
///
/// Answers 0, or -1 with `errno` set when `file` is null or its path would not
/// fit in the library's copy. As with glibc, a path that names no file is not
/// an error here: the entries simply come back empty.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn utmpxname(file: *const c_char) -> c_int {
    if file.is_null() {
        return crate::fail(EINVAL);
    }
    // SAFETY: a C string the caller guarantees is NUL-terminated.
    let bytes = unsafe { core::ffi::CStr::from_ptr(file) }.to_bytes();
    if bytes.len() > PATH_MAX {
        return crate::fail(ENAMETOOLONG);
    }
    unsafe {
        UT_PATH[..bytes.len()].copy_from_slice(bytes);
        UT_PATH_LEN = bytes.len();
        UT_LOADED = false;
        UT_COUNT = 0;
        UT_CURSOR = 0;
    }
    0
}

#[cfg(test)]
mod tests {
    use minix_std::utmpx::{BOOT_TIME, DEAD_PROCESS, USER_PROCESS, Utmpx};

    /// The C header's sizes, restated here so a change to the record layout
    /// fails a host test rather than a guest's first `who`.
    #[test]
    fn the_record_is_the_header_size() {
        assert_eq!(core::mem::size_of::<Utmpx>(), 520);
        assert_eq!(core::mem::align_of::<Utmpx>(), 8);
    }

    #[test]
    fn kinds_match_the_header() {
        assert_eq!(BOOT_TIME, 2);
        assert_eq!(USER_PROCESS, 7);
        assert_eq!(DEAD_PROCESS, 8);
    }
}
