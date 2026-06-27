//! Endpoint numbering from `minix/endpoint.h`

use crate::consts::{NR_PROCS, NR_SYS_PROCS, NR_TASKS};

/// Total number of process table slots.
pub const NR_PROCS_TOTAL: usize = NR_TASKS + NR_SYS_PROCS + NR_PROCS;

/// Generate an endpoint value from process number and generation.
pub fn endpoint(proc_nr: i32, generation: i32) -> i32 {
    (generation << 16) | (proc_nr & 0xffff)
}

/// Extract process number from an endpoint.
pub fn proc_nr_from_ep(endpoint: i32) -> i32 {
    // Sign-extend if the process number is negative
    let nr = endpoint << 16 >> 16;
    nr
}

/// Extract generation from an endpoint.
pub fn generation_from_ep(endpoint: i32) -> i32 {
    endpoint >> 16
}

/// Special endpoint values.
pub const ANY: i32 = 0x0000ffff;
pub const NONE: i32 = 0x0000fffe;
pub const SELF: i32 = 0x0000fffd;

/// Check if an endpoint is valid.
pub fn is_valid_endpoint(ep: i32) -> bool {
    ep != NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_endpoint_roundtrip() {
        let ep = endpoint(3, 1);
        assert_eq!(proc_nr_from_ep(ep), 3);
        assert_eq!(generation_from_ep(ep), 1);
    }

    #[test]
    fn test_negative_proc_nr() {
        let ep = endpoint(-1, 0);
        assert_eq!(proc_nr_from_ep(ep), -1);
    }

    #[test]
    fn test_special_endpoints() {
        assert_eq!(ANY, 0x0000ffff);
        assert_eq!(NONE, 0x0000fffe);
        assert_eq!(SELF, 0x0000fffd);
    }
}
