//! VBFS configuration and option parsing — adapted from `minix/fs/vbfs/vbfs.c`

// Errno values (no_std, no libc dependency).
const EINVAL: i32 = -22;

/// Maximum path length for share name and prefix.
pub const PATH_MAX: usize = 1024;

/// Default file creation mask (octal).
pub const DEFAULT_FILE_MASK: u32 = 0o755;
/// Default directory creation mask (octal).
pub const DEFAULT_DIR_MASK: u32 = 0o755;

/// VBFS parameters passed to the SFFS library.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct SffsParams {
    pub p_prefix: [u8; PATH_MAX],
    pub p_uid: u32,
    pub p_gid: u32,
    pub p_file_mask: u32,
    pub p_dir_mask: u32,
    pub p_case_insens: u32,
}

impl Default for SffsParams {
    fn default() -> Self {
        Self {
            p_prefix: [0; PATH_MAX],
            p_uid: 0,
            p_gid: 0,
            p_file_mask: DEFAULT_FILE_MASK,
            p_dir_mask: DEFAULT_DIR_MASK,
            p_case_insens: 0,
        }
    }
}

/// Option descriptor for parsing `-o key=value` arguments.
#[repr(C)]
pub struct OptSetEntry {
    pub name: &'static str,
    pub opt_type: OptType,
    pub ptr: OptTarget,
    pub max_len: usize,
}

pub enum OptType {
    String,
    Int,
}

pub enum OptTarget {
    String(*mut u8),
    Int(*mut u32),
}

// SAFETY: OptTarget is only used in single-threaded init context.
// Raw pointer targets are accessed under explicit unsafe blocks.
unsafe impl Sync for OptTarget {}
unsafe impl Send for OptTarget {}

/// Parse a single `-o key=value` option against the option table.
///
/// Returns `Ok(())` on success, or an errno on failure.
#[allow(clippy::collapsible_if)]
pub fn optset_parse(table: &[OptSetEntry], opt_str: &str) -> Result<(), i32> {
    let eq_pos = opt_str.find('=').unwrap_or(opt_str.len());
    let key = &opt_str[..eq_pos];
    let value = if eq_pos < opt_str.len() {
        &opt_str[eq_pos + 1..]
    } else {
        ""
    };

    for entry in table {
        if entry.name == key {
            match &entry.opt_type {
                OptType::String => {
                    if let OptTarget::String(buf) = &entry.ptr {
                        let bytes = value.as_bytes();
                        let copy_len = bytes.len().min(entry.max_len - 1);
                        // SAFETY: caller guarantees buffer is valid and sized
                        unsafe {
                            core::ptr::copy_nonoverlapping(bytes.as_ptr(), *buf, copy_len);
                            core::ptr::write((*buf).add(copy_len), 0u8);
                        }
                        return Ok(());
                    }
                }
                OptType::Int => {
                    if let OptTarget::Int(val) = &entry.ptr {
                        if let Ok(n) = value.parse::<u32>() {
                            unsafe {
                                core::ptr::write(*val, n);
                            }
                            return Ok(());
                        }
                    }
                }
            }
            return Err(EINVAL);
        }
    }
    Err(EINVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_params() {
        let p = SffsParams::default();
        assert_eq!(p.p_uid, 0);
        assert_eq!(p.p_gid, 0);
        assert_eq!(p.p_file_mask, DEFAULT_FILE_MASK);
        assert_eq!(p.p_dir_mask, DEFAULT_DIR_MASK);
        assert_eq!(p.p_case_insens, 0);
    }

    #[test]
    fn test_optset_parse_unknown_key() {
        let table: [OptSetEntry; 0] = [];
        let r = optset_parse(&table, "unknown=1");
        assert_eq!(r, Err(EINVAL));
    }

    #[test]
    fn test_optset_parse_int() {
        // Use a local variable behind a raw pointer to avoid static_mut_refs
        let mut val: u32 = 0;
        let table = [OptSetEntry {
            name: "uid",
            opt_type: OptType::Int,
            ptr: OptTarget::Int(&raw mut val),
            max_len: 10,
        }];
        let r = optset_parse(&table, "uid=42");
        assert_eq!(r, Ok(()));
        assert_eq!(val, 42);
    }

    #[test]
    fn test_optset_parse_string() {
        let mut buf: [u8; 64] = [0; 64];
        let table = [OptSetEntry {
            name: "share",
            opt_type: OptType::String,
            ptr: OptTarget::String(&raw mut buf as *mut u8),
            max_len: 64,
        }];
        let r = optset_parse(&table, "share=myshare");
        assert_eq!(r, Ok(()));
        let len = buf.iter().position(|&c| c == 0).unwrap_or(64);
        let s = core::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(s, "myshare");
    }
}
