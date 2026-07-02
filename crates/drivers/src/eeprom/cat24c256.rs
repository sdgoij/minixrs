//! CAT24C256 EEPROM driver — 256K-bit (32KB) I2C EEPROM
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/eeprom/cat24c256/cat24c256.c`
//!
//! Supports the CAT24C256 EEPROM chip (BeagleBone onboard + cape expansion).
//! Valid I2C addresses: 0x50-0x57. Read chunk size: 128 bytes.
//! Write page size: 16 bytes.
//!
//! The driver depends on an I2C bus abstraction. `EepromBus` is a trait that
//! must be implemented by the caller to provide the actual I2C transactions.

use crate::DriverError;

// ── Constants ───────────────────────────────────────────────────────────────

/// Maximum I2C exec buffer length (matching I2C_EXEC_MAX_BUFLEN).
pub const I2C_EXEC_MAX_BUFLEN: usize = 256;

/// EEPROM size in bytes (256 Kbit = 32 KB).
pub const EEPROM_SIZE: usize = 32768;

/// Number of minor devices.
pub const NR_DEVS: usize = 1;

/// Minor device index.
pub const EEPROM_DEV: usize = 0;

/// Maximum read chunk size (I2C hardware limit).
pub const READ_CHUNK_SIZE: usize = 128;

/// Maximum write page size (CAT24C256 page size).
pub const WRITE_PAGE_SIZE: usize = 16;

/// Valid I2C slave addresses for CAT24C256.
pub const VALID_ADDRS: [u8; 9] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x00];

// ── I2C operation types ─────────────────────────────────────────────────────

/// I2C operation: read with stop.
pub const I2C_OP_READ_WITH_STOP: u8 = 0x01;

/// I2C operation: write with stop.
pub const I2C_OP_WRITE_WITH_STOP: u8 = 0x02;

// ── I2C exec ioctl structure ────────────────────────────────────────────────

/// I2C ioctl exec structure — describes a single I2C transaction.
///
/// Mirrors `minix_i2c_ioctl_exec_t` from the MINIX I2C framework.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct I2cExec {
    /// I2C operation (read/write with stop).
    pub op: u8,
    /// Slave address (7-bit).
    pub addr: u8,
    /// Command bytes (memory address to access).
    pub cmd: [u8; 2],
    /// Length of command data (1 or 2 bytes).
    pub cmdlen: u8,
    /// Data buffer for read/write.
    pub buf: [u8; I2C_EXEC_MAX_BUFLEN],
    /// Length of data buffer.
    pub buflen: u16,
}

impl I2cExec {
    /// Create a new zeroed I2C exec structure.
    pub const fn new() -> Self {
        Self {
            op: 0,
            addr: 0,
            cmd: [0u8; 2],
            cmdlen: 0,
            buf: [0u8; I2C_EXEC_MAX_BUFLEN],
            buflen: 0,
        }
    }
}

impl Default for I2cExec {
    fn default() -> Self {
        Self::new()
    }
}

// ── I2C bus abstraction ─────────────────────────────────────────────────────

/// Result of an I2C transaction.
pub type I2cResult = Result<(), DriverError>;

/// Trait for I2C bus operations.
///
/// The caller must provide an implementation that communicates with
/// the actual I2C controller hardware.
pub trait EepromBus {
    /// Execute an I2C transaction.
    ///
    /// For reads: writes `cmd[..cmdlen]` to the device, then reads
    /// `buflen` bytes into `buf`. For writes: writes `cmd[..cmdlen]`
    /// followed by `buf[..buflen]`.
    fn exec(&mut self, ioctl: &mut I2cExec) -> I2cResult;
}

// ── Open count tracking ─────────────────────────────────────────────────────

/// Per-device open count.
static mut OPEN_COUNT: [i32; NR_DEVS] = [0; NR_DEVS];

// ── Geometry ────────────────────────────────────────────────────────────────

/// Device geometry: base offset and size.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct EepromGeometry {
    pub base: u64,
    pub size: u64,
}

/// Default geometry for the CAT24C256.
pub const EEPROM_GEOMETRY: EepromGeometry = EepromGeometry {
    base: 0,
    size: EEPROM_SIZE as u64,
};

// ── Low-level I/O ───────────────────────────────────────────────────────────

/// Read up to 128 bytes from the EEPROM at a given memory address.
///
/// This is the hardware-level read operation. The I2C bus interface
/// can only transfer up to 128 bytes at a time.
///
/// # Safety
///
/// `bus` may perform hardware I/O. `buf` mutably aliases I2C buffer state.
pub unsafe fn cat24c256_read_chunk(
    bus: &mut dyn EepromBus,
    address: u8,
    memaddr: u16,
    buf: &mut [u8],
    nopage: bool,
) -> I2cResult {
    if buf.len() > READ_CHUNK_SIZE || buf.is_empty() {
        return Err(DriverError::InvalidArgument);
    }

    let mut ioctl = I2cExec::new();
    ioctl.op = I2C_OP_READ_WITH_STOP;
    ioctl.addr = address;

    if nopage {
        ioctl.cmd[0] = (memaddr & 0xFF) as u8;
        ioctl.cmdlen = 1;
    } else {
        ioctl.cmd[0] = ((memaddr >> 8) & 0xFF) as u8;
        ioctl.cmd[1] = (memaddr & 0xFF) as u8;
        ioctl.cmdlen = 2;
    }

    ioctl.buflen = buf.len() as u16;

    bus.exec(&mut ioctl)?;

    let len = buf.len().min(ioctl.buflen as usize);
    buf[..len].copy_from_slice(&ioctl.buf[..len]);

    Ok(())
}

/// Read from the EEPROM, splitting into 128-byte chunks as needed.
///
/// # Safety
///
/// `bus` may perform hardware I/O.
pub unsafe fn cat24c256_read(
    bus: &mut dyn EepromBus,
    address: u8,
    memaddr: u16,
    buf: &mut [u8],
    nopage: bool,
) -> I2cResult {
    if buf.is_empty() {
        return Ok(());
    }
    if memaddr.checked_add(buf.len() as u16).is_none() {
        return Err(DriverError::InvalidArgument);
    }

    let mut offset = 0usize;
    while offset < buf.len() {
        let chunk = (buf.len() - offset).min(READ_CHUNK_SIZE);
        unsafe {
            cat24c256_read_chunk(
                bus,
                address,
                memaddr.wrapping_add(offset as u16),
                &mut buf[offset..offset + chunk],
                nopage,
            )?;
        }
        offset += chunk;
    }

    Ok(())
}

/// Write up to 16 bytes to the EEPROM at a given memory address.
///
/// CAT24C256 page size is 16 bytes. Writes must be page-aligned and
/// must not cross a page boundary.
///
/// # Safety
///
/// `bus` may perform hardware I/O.
pub unsafe fn cat24c256_write_page(
    bus: &mut dyn EepromBus,
    address: u8,
    memaddr: u16,
    buf: &[u8],
    nopage: bool,
) -> I2cResult {
    let addrlen = if nopage { 1 } else { 2 };
    if buf.len() + addrlen > I2C_EXEC_MAX_BUFLEN {
        return Err(DriverError::InvalidArgument);
    }

    let mut ioctl = I2cExec::new();
    ioctl.op = I2C_OP_WRITE_WITH_STOP;
    ioctl.addr = address;

    if nopage {
        ioctl.buf[0] = (memaddr & 0xFF) as u8;
    } else {
        ioctl.buf[0] = ((memaddr >> 8) & 0xFF) as u8;
        ioctl.buf[1] = (memaddr & 0xFF) as u8;
    }
    ioctl.buf[addrlen..addrlen + buf.len()].copy_from_slice(buf);
    ioctl.buflen = (buf.len() + addrlen) as u16;

    bus.exec(&mut ioctl)?;

    Ok(())
}

/// Write to the EEPROM, splitting into 16-byte page writes as needed.
///
/// # Safety
///
/// `bus` may perform hardware I/O.
pub unsafe fn cat24c256_write(
    bus: &mut dyn EepromBus,
    address: u8,
    memaddr: u16,
    buf: &[u8],
    nopage: bool,
) -> I2cResult {
    if buf.is_empty() {
        return Ok(());
    }
    if memaddr.checked_add(buf.len() as u16).is_none() {
        return Err(DriverError::InvalidArgument);
    }

    let mut offset = 0usize;
    while offset < buf.len() {
        let chunk = (buf.len() - offset).min(WRITE_PAGE_SIZE);
        unsafe {
            cat24c256_write_page(
                bus,
                address,
                memaddr.wrapping_add(offset as u16),
                &buf[offset..offset + chunk],
                nopage,
            )?;
        }
        offset += chunk;
    }

    Ok(())
}

// ── Device management ─────────────────────────────────────────────────────

/// Open an EEPROM device.
pub fn cat24c256_open(minor: usize) -> Result<(), DriverError> {
    if minor >= NR_DEVS {
        return Err(DriverError::NotFound);
    }
    unsafe {
        OPEN_COUNT[minor] += 1;
    }
    Ok(())
}

/// Close an EEPROM device.
pub fn cat24c256_close(minor: usize) -> Result<(), DriverError> {
    if minor >= NR_DEVS {
        return Err(DriverError::NotFound);
    }
    unsafe {
        if OPEN_COUNT[minor] < 1 {
            return Err(DriverError::Io);
        }
        OPEN_COUNT[minor] -= 1;
    }
    Ok(())
}

/// Get device geometry for a minor device.
pub fn cat24c256_geometry(minor: usize) -> Result<EepromGeometry, DriverError> {
    if minor >= NR_DEVS {
        return Err(DriverError::NotFound);
    }
    Ok(EEPROM_GEOMETRY)
}

/// Check if an I2C address is valid for the CAT24C256.
pub fn is_valid_address(addr: u8) -> bool {
    VALID_ADDRS.contains(&addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock I2C bus that simulates EEPROM storage.
    struct MockEepromBus {
        storage: [u8; EEPROM_SIZE],
    }

    impl MockEepromBus {
        fn new() -> Self {
            let mut storage = [0u8; EEPROM_SIZE];
            for (i, byte) in storage.iter_mut().enumerate() {
                *byte = (i & 0xFF) as u8;
            }
            Self { storage }
        }
    }

    impl EepromBus for MockEepromBus {
        fn exec(&mut self, ioctl: &mut I2cExec) -> I2cResult {
            let memaddr = if ioctl.cmdlen == 2 {
                ((ioctl.cmd[0] as u16) << 8) | (ioctl.cmd[1] as u16)
            } else if ioctl.cmdlen == 1 {
                ioctl.cmd[0] as u16
            } else {
                0
            } as usize;

            match ioctl.op {
                I2C_OP_READ_WITH_STOP => {
                    let len = ioctl.buflen as usize;
                    if memaddr + len > EEPROM_SIZE {
                        return Err(DriverError::Io);
                    }
                    ioctl.buf[..len].copy_from_slice(&self.storage[memaddr..memaddr + len]);
                    Ok(())
                }
                I2C_OP_WRITE_WITH_STOP => {
                    // Address is embedded in buf[0..addrlen], data follows.
                    let addrlen = if ioctl.cmdlen > 0 {
                        ioctl.cmdlen as usize
                    } else {
                        // Write convention: address is first 1 or 2 bytes of buf.
                        // Determine actual address length from the original I2C protocol:
                        // CAT24C256 uses 2-byte address by default.
                        2
                    };
                    // Extract memory address from the embedded address bytes.
                    let actual_addr = if addrlen == 2 {
                        ((ioctl.buf[0] as u16) << 8) | (ioctl.buf[1] as u16)
                    } else {
                        ioctl.buf[0] as u16
                    } as usize;
                    let datalen = ioctl.buflen as usize - addrlen;
                    if actual_addr + datalen > EEPROM_SIZE {
                        return Err(DriverError::Io);
                    }
                    self.storage[actual_addr..actual_addr + datalen]
                        .copy_from_slice(&ioctl.buf[addrlen..ioctl.buflen as usize]);
                    Ok(())
                }
                _ => Err(DriverError::Unsupported),
            }
        }
    }

    #[test]
    fn test_valid_addresses() {
        assert!(is_valid_address(0x50));
        assert!(is_valid_address(0x57));
        assert!(!is_valid_address(0x49));
        assert!(!is_valid_address(0x58));
    }

    #[test]
    fn test_eeprom_constants() {
        assert_eq!(EEPROM_SIZE, 32768);
        assert_eq!(READ_CHUNK_SIZE, 128);
        assert_eq!(WRITE_PAGE_SIZE, 16);
        assert_eq!(NR_DEVS, 1);
    }

    #[test]
    fn test_geometry() {
        let geom = cat24c256_geometry(0).unwrap();
        assert_eq!(geom.base, 0);
        assert_eq!(geom.size, EEPROM_SIZE as u64);
        assert!(cat24c256_geometry(99).is_err());
    }

    #[test]
    fn test_open_close() {
        unsafe {
            OPEN_COUNT[0] = 0;
        }
        assert!(cat24c256_open(0).is_ok());
        assert!(cat24c256_open(99).is_err());
        assert!(cat24c256_close(0).is_ok());
    }

    #[test]
    fn test_close_unopened_fails() {
        unsafe {
            OPEN_COUNT[0] = 0;
        }
        assert!(cat24c256_close(0).is_err());
    }

    #[test]
    fn test_read_chunk_basic() {
        unsafe {
            let mut bus = MockEepromBus::new();
            let mut buf = [0u8; 16];
            cat24c256_read_chunk(&mut bus, 0x50, 0, &mut buf, false).unwrap();
            assert_eq!(buf[0], 0);
            assert_eq!(buf[1], 1);
            assert_eq!(buf[15], 15);
        }
    }

    #[test]
    fn test_read_chunk_nopage() {
        unsafe {
            let mut bus = MockEepromBus::new();
            let mut buf = [0u8; 8];
            cat24c256_read_chunk(&mut bus, 0x50, 0x100, &mut buf, true).unwrap();
            assert_eq!(buf[0], 0x00);
            assert_eq!(buf[1], 0x01);
        }
    }

    #[test]
    fn test_read_chunk_too_large() {
        unsafe {
            let mut bus = MockEepromBus::new();
            let mut buf = [0u8; 256];
            assert!(cat24c256_read_chunk(&mut bus, 0x50, 0, &mut buf, false).is_err());
        }
    }

    #[test]
    fn test_read_chunk_empty() {
        unsafe {
            let mut bus = MockEepromBus::new();
            assert!(cat24c256_read_chunk(&mut bus, 0x50, 0, &mut [], false).is_err());
        }
    }

    #[test]
    fn test_read_full_eeprom() {
        unsafe {
            let mut bus = MockEepromBus::new();
            let mut buf = [0u8; 512];
            cat24c256_read(&mut bus, 0x50, 0, &mut buf, false).unwrap();
            for (i, &byte) in buf.iter().enumerate() {
                assert_eq!(byte, (i & 0xFF) as u8, "byte {i} mismatch");
            }
        }
    }

    #[test]
    fn test_read_offset() {
        unsafe {
            let mut bus = MockEepromBus::new();
            let mut buf = [0u8; 256];
            cat24c256_read(&mut bus, 0x50, 0x100, &mut buf, false).unwrap();
            for (i, &byte) in buf.iter().enumerate() {
                assert_eq!(byte, ((0x100 + i) & 0xFF) as u8, "offset byte {i}");
            }
        }
    }

    #[test]
    fn test_write_page_basic() {
        unsafe {
            let mut bus = MockEepromBus::new();
            let data = [0xAAu8; 16];
            cat24c256_write_page(&mut bus, 0x50, 0, &data, false).unwrap();
            let mut buf = [0u8; 16];
            cat24c256_read_chunk(&mut bus, 0x50, 0, &mut buf, false).unwrap();
            assert_eq!(buf, data);
        }
    }

    #[test]
    fn test_write_multiple_pages() {
        unsafe {
            let mut bus = MockEepromBus::new();
            let data = [0xBBu8; 64];
            cat24c256_write(&mut bus, 0x50, 0, &data, false).unwrap();
            let mut buf = [0u8; 64];
            cat24c256_read(&mut bus, 0x50, 0, &mut buf, false).unwrap();
            assert_eq!(buf, data);
        }
    }

    #[test]
    fn test_write_across_boundary() {
        unsafe {
            let mut bus = MockEepromBus::new();
            let data = [0xCCu8; 64];
            cat24c256_write(&mut bus, 0x50, 0x3FF0, &data, false).unwrap();
            let mut buf = [0u8; 64];
            cat24c256_read(&mut bus, 0x50, 0x3FF0, &mut buf, false).unwrap();
            assert_eq!(buf, data);
        }
    }

    #[test]
    fn test_ioctl_exec_defaults() {
        let e = I2cExec::new();
        assert_eq!(e.op, 0);
        assert_eq!(e.addr, 0);
        assert_eq!(e.cmdlen, 0);
        assert_eq!(e.buflen, 0);
    }

    #[test]
    fn test_read_empty_buf() {
        unsafe {
            let mut bus = MockEepromBus::new();
            assert!(cat24c256_read(&mut bus, 0x50, 0, &mut [], false).is_ok());
        }
    }

    #[test]
    fn test_write_empty_buf() {
        unsafe {
            let mut bus = MockEepromBus::new();
            assert!(cat24c256_write(&mut bus, 0x50, 0, &[], false).is_ok());
        }
    }
}
