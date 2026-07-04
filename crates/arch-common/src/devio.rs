//! Device I/O types from `minix/devio.h`


/// I/O port address (16-bit).
pub type PortT = u16;


#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PvbPair {
    pub port: u16,
    pub value: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PvwPair {
    pub port: u16,
    pub value: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PvlPair {
    pub port: u16,
    pub value: u32,
}


#[deprecated = "No longer in use"]
pub const MASK_GRANULARITY: u32 = 0x000F;

#[deprecated = "No longer in use"]
pub const PVB_FLAG: u8 = b'b';
#[deprecated = "No longer in use"]
pub const PVW_FLAG: u8 = b'w';
#[deprecated = "No longer in use"]
pub const PVL_FLAG: u8 = b'l';

#[deprecated = "No longer in use"]
pub const MASK_IN_OR_OUT: u32 = 0x00F0;
#[deprecated = "No longer in use"]
pub const DEVIO_INPUT: u32 = 0x0010;
#[deprecated = "No longer in use"]
pub const DEVIO_OUTPUT: u32 = 0x0020;

#[deprecated = "No longer in use"]
pub const PV_BUF_SIZE: u32 = 64;
/// Deprecated: no longer in use.
pub const MAX_PVB_PAIRS: u32 = 64 / 3; // sizeof(pvb_pair_t) ≈ 3
pub const MAX_PVW_PAIRS: u32 = 64 / 4; // sizeof(pvw_pair_t) ≈ 4
pub const MAX_PVL_PAIRS: u32 = 64 / 6; // sizeof(pvl_pair_t) ≈ 6

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_port_type() {
        assert_eq!(size_of::<PortT>(), 2);
    }

    #[test]
    fn test_pair_sizes() {
        assert_eq!(size_of::<PvbPair>(), 4); // u16(2) + u8(1) + padding(1) = 4
        assert_eq!(size_of::<PvwPair>(), 4); // u16(2) + u16(2) = 4
        assert_eq!(size_of::<PvlPair>(), 8); // u16(2) + padding(2) + u32(4) = 8
    }
}
