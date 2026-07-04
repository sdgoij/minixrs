//! System configuration from `minix/sys_config.h`


/// Maximum number of user processes.
pub const NR_PROCS: usize = 256;

/// Maximum number of system processes.
pub const NR_SYS_PROCS_CFG: usize = 64;


pub const FP_NONE: u32 = 0;
pub const FP_IEEE: u32 = 1;

/// FP format selection, defaulting to no hardware support.
pub const FP_FORMAT: u32 = FP_NONE;


pub const DEBUG_LOCK_CHECK: u32 = 1;


pub const KMESS_BUF_SIZE: usize = 10000;


pub const DEFAULT_STACK_LIMIT: usize = 4 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fp_format() {
        assert_eq!(FP_NONE, 0);
        assert_eq!(FP_IEEE, 1);
        assert_eq!(FP_FORMAT, FP_NONE);
    }

    #[test]
    fn test_debug_config() {
        assert_eq!(DEBUG_LOCK_CHECK, 1);
    }

    #[test]
    fn test_buf_size() {
        assert_eq!(KMESS_BUF_SIZE, 10000);
    }

    #[test]
    fn test_stack_limit() {
        assert_eq!(DEFAULT_STACK_LIMIT, 4 * 1024 * 1024);
    }
}
