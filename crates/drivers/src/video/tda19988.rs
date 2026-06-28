#![allow(clippy::new_without_default)]

//! TDA19988 HDMI encoder driver — EDID block device via I2C.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/video/tda19988/tda19988.c`
//!
//! The TDA19988 is an HDMI encoder controlled over I2C (CEC address 0x34,
//! HDMI address 0x70).  This driver reads EDID data from connected displays
//! and exposes it as a block device.

use crate::DriverError;

// ═══════════════════════════════════════════════════════════════════════════
// I2C Bus Trait
// ═══════════════════════════════════════════════════════════════════════════

/// Abstract I2C bus operations.
pub trait I2cBus {
    /// Read bytes from a device register.
    fn read(&mut self, addr: u8, reg: u8, buf: &mut [u8]) -> Result<(), DriverError>;
    /// Write bytes to a device register.
    fn write(&mut self, addr: u8, reg: u8, data: &[u8]) -> Result<(), DriverError>;
}

/// Mock I2C bus for testing.
pub struct MockI2c {
    pub regs: [u8; 256],
}

impl MockI2c {
    pub fn new() -> Self {
        Self { regs: [0u8; 256] }
    }
}

impl I2cBus for MockI2c {
    fn read(&mut self, _addr: u8, reg: u8, buf: &mut [u8]) -> Result<(), DriverError> {
        let count = buf.len().min(256 - reg as usize);
        buf[..count].copy_from_slice(&self.regs[reg as usize..reg as usize + count]);
        Ok(())
    }

    fn write(&mut self, _addr: u8, reg: u8, data: &[u8]) -> Result<(), DriverError> {
        let count = data.len().min(256 - reg as usize);
        self.regs[reg as usize..reg as usize + count].copy_from_slice(&data[..count]);
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════════

/// Number of devices this driver handles.
pub const NR_DEVS: usize = 1;

/// I2C addresses.
pub const CEC_ADDR: u8 = 0x34;
pub const HDMI_ADDR: u8 = 0x70;

/// Standard EDID block length.
pub const EDID_LEN: usize = 128;

// ── CEC Registers ─────────────────────────────────────────────────────────

pub const CEC_STATUS_REG: u8 = 0xfe;
pub const CEC_STATUS_CONNECTED_MASK: u8 = 0x02;
pub const CEC_ENABLE_REG: u8 = 0xff;
pub const CEC_ENABLE_ALL_MASK: u8 = 0x87;

// ── HDMI Pages ────────────────────────────────────────────────────────────

pub const HDMI_CTRL_PAGE: u8 = 0x00;
pub const HDMI_PPL_PAGE: u8 = 0x02;
pub const HDMI_EDID_PAGE: u8 = 0x09;
pub const HDMI_INFO_PAGE: u8 = 0x10;
pub const HDMI_AUDIO_PAGE: u8 = 0x11;
pub const HDMI_HDCP_OTP_PAGE: u8 = 0x12;
pub const HDMI_GAMUT_PAGE: u8 = 0x13;
pub const HDMI_PAGELESS: u8 = 0xff;

// ── Control Page Registers ────────────────────────────────────────────────

pub const HDMI_CTRL_REV_LO_REG: u8 = 0x00;
pub const HDMI_CTRL_REV_HI_REG: u8 = 0x02;
pub const HDMI_CTRL_RESET_REG: u8 = 0x0a;
pub const HDMI_CTRL_RESET_DDC_MASK: u8 = 0x02;
pub const HDMI_CTRL_DDC_CTRL_REG: u8 = 0x0b;
pub const HDMI_CTRL_DDC_EN_MASK: u8 = 0x00;
pub const HDMI_CTRL_DDC_CLK_REG: u8 = 0x0c;
pub const HDMI_CTRL_DDC_CLK_EN_MASK: u8 = 0x01;
pub const HDMI_CTRL_INTR_CTRL_REG: u8 = 0x0f;
pub const HDMI_CTRL_INTR_EN_GLO_MASK: u8 = 0x04;
pub const HDMI_CTRL_INT_REG: u8 = 0x11;
pub const HDMI_CTRL_INT_EDID_MASK: u8 = 0x02;

// ── EDID Page Registers ───────────────────────────────────────────────────

pub const HDMI_EDID_DATA_REG: u8 = 0x00;
pub const HDMI_EDID_DEV_ADDR_REG: u8 = 0xfb;
pub const HDMI_EDID_DEV_ADDR: u8 = 0xa0;
pub const HDMI_EDID_OFFSET_REG: u8 = 0xfc;
pub const HDMI_EDID_OFFSET: u8 = 0x00;
pub const HDMI_EDID_SEG_PTR_ADDR_REG: u8 = 0xfc;
pub const HDMI_EDID_SEG_PTR_ADDR: u8 = 0x00;
pub const HDMI_EDID_SEG_ADDR_REG: u8 = 0xfe;
pub const HDMI_EDID_SEG_ADDR: u8 = 0x00;
pub const HDMI_EDID_REQ_REG: u8 = 0xfa;
pub const HDMI_EDID_REQ_READ_MASK: u8 = 0x01;

// ── HDCP & OTP Registers ──────────────────────────────────────────────────

pub const HDMI_HDCP_OTP_DDC_CLK_REG: u8 = 0x9a;
pub const HDMI_HDCP_OTP_DDC_CLK_MASK: u8 = 0x27;
pub const HDMI_HDCP_OTP_SOME_REG: u8 = 0x9b;
pub const HDMI_HDCP_OTP_SOME_MASK: u8 = 0x02;

// ── Pageless Register ─────────────────────────────────────────────────────

pub const HDMI_PAGE_SELECT_REG: u8 = 0xff;

// ── Revision ───────────────────────────────────────────────────────────────

pub const HDMI_REV_TDA19988: u16 = 0x0331;

// ═══════════════════════════════════════════════════════════════════════════
// Driver
// ═══════════════════════════════════════════════════════════════════════════

/// TDA19988 HDMI encoder driver parameterized over an I2C bus backend.
pub struct Tda19988Driver<B: I2cBus> {
    bus: B,
}

impl<B: I2cBus> Tda19988Driver<B> {
    pub fn new(bus: B) -> Self {
        Self { bus }
    }

    /// Select the HDMI register page.
    pub fn set_page(&mut self, page: u8) -> Result<(), DriverError> {
        self.bus.write(HDMI_ADDR, HDMI_PAGE_SELECT_REG, &[page])
    }

    /// Read a byte from an HDMI register.
    pub fn hdmi_read(&mut self, page: u8, reg: u8) -> Result<u8, DriverError> {
        if page != HDMI_PAGELESS {
            self.set_page(page)?;
        }
        let mut buf = [0u8];
        self.bus.read(HDMI_ADDR, reg, &mut buf)?;
        Ok(buf[0])
    }

    /// Write a byte to an HDMI register.
    pub fn hdmi_write(&mut self, page: u8, reg: u8, val: u8) -> Result<(), DriverError> {
        if page != HDMI_PAGELESS {
            self.set_page(page)?;
        }
        self.bus.write(HDMI_ADDR, reg, &[val])
    }

    /// Set bits in an HDMI register (OR mask).
    pub fn hdmi_set(&mut self, page: u8, reg: u8, mask: u8) -> Result<(), DriverError> {
        let val = self.hdmi_read(page, reg)?;
        self.hdmi_write(page, reg, val | mask)
    }

    /// Clear bits in an HDMI register (AND !mask).
    pub fn hdmi_clear(&mut self, page: u8, reg: u8, mask: u8) -> Result<(), DriverError> {
        let val = self.hdmi_read(page, reg)?;
        self.hdmi_write(page, reg, val & !mask)
    }

    /// Check if a display is connected (CEC status register).
    pub fn is_display_connected(&mut self) -> Result<bool, DriverError> {
        let mut buf = [0u8];
        self.bus.read(CEC_ADDR, CEC_STATUS_REG, &mut buf)?;
        Ok((buf[0] & CEC_STATUS_CONNECTED_MASK) != 0)
    }

    /// Check the chip revision.
    pub fn check_revision(&mut self) -> Result<u16, DriverError> {
        let lo = self.hdmi_read(HDMI_CTRL_PAGE, HDMI_CTRL_REV_LO_REG)? as u16;
        let hi = self.hdmi_read(HDMI_CTRL_PAGE, HDMI_CTRL_REV_HI_REG)? as u16;
        Ok((hi << 8) | lo)
    }

    /// Initialize the HDMI module.
    pub fn hdmi_init(&mut self) -> Result<(), DriverError> {
        // Enable DDC clock
        self.hdmi_set(
            HDMI_CTRL_PAGE,
            HDMI_CTRL_DDC_CLK_REG,
            HDMI_CTRL_DDC_CLK_EN_MASK,
        )?;
        // Enable DDC controller
        self.hdmi_clear(
            HDMI_CTRL_PAGE,
            HDMI_CTRL_DDC_CTRL_REG,
            HDMI_CTRL_DDC_EN_MASK,
        )?;
        // Set HDCP/OTP DDC clock
        self.hdmi_set(
            HDMI_HDCP_OTP_PAGE,
            HDMI_HDCP_OTP_DDC_CLK_REG,
            HDMI_HDCP_OTP_DDC_CLK_MASK,
        )?;
        Ok(())
    }

    /// Read the EDID block.
    pub fn read_edid(&mut self, data: &mut [u8]) -> Result<usize, DriverError> {
        let count = data.len().min(EDID_LEN);
        // Set EDID device address
        self.hdmi_write(HDMI_EDID_PAGE, HDMI_EDID_DEV_ADDR_REG, HDMI_EDID_DEV_ADDR)?;
        // Set EDID offset
        self.hdmi_write(HDMI_EDID_PAGE, HDMI_EDID_OFFSET_REG, HDMI_EDID_OFFSET)?;
        // Request EDID read
        self.hdmi_write(HDMI_EDID_PAGE, HDMI_EDID_REQ_REG, HDMI_EDID_REQ_READ_MASK)?;
        // Read EDID data bytes
        for byte in data.iter_mut().take(count) {
            *byte = self.hdmi_read(HDMI_EDID_PAGE, HDMI_EDID_DATA_REG)?;
        }
        Ok(count)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_driver() -> Tda19988Driver<MockI2c> {
        Tda19988Driver::new(MockI2c::new())
    }

    #[test]
    fn test_constants() {
        assert_eq!(CEC_ADDR, 0x34);
        assert_eq!(HDMI_ADDR, 0x70);
        assert_eq!(EDID_LEN, 128);
        assert_eq!(HDMI_REV_TDA19988, 0x0331);
        assert_eq!(NR_DEVS, 1);
    }

    #[test]
    fn test_cec_constants() {
        assert_eq!(CEC_STATUS_REG, 0xfe);
        assert_eq!(CEC_STATUS_CONNECTED_MASK, 0x02);
        assert_eq!(CEC_ENABLE_REG, 0xff);
        assert_eq!(CEC_ENABLE_ALL_MASK, 0x87);
    }

    #[test]
    fn test_hdmi_pages() {
        assert_eq!(HDMI_CTRL_PAGE, 0x00);
        assert_eq!(HDMI_EDID_PAGE, 0x09);
        assert_eq!(HDMI_PAGELESS, 0xff);
    }

    #[test]
    fn test_ctrl_registers() {
        assert_eq!(HDMI_CTRL_REV_LO_REG, 0x00);
        assert_eq!(HDMI_CTRL_REV_HI_REG, 0x02);
        assert_eq!(HDMI_CTRL_RESET_REG, 0x0a);
        assert_eq!(HDMI_CTRL_DDC_CTRL_REG, 0x0b);
        assert_eq!(HDMI_CTRL_DDC_CLK_REG, 0x0c);
    }

    #[test]
    fn test_edid_registers() {
        assert_eq!(HDMI_EDID_DATA_REG, 0x00);
        assert_eq!(HDMI_EDID_DEV_ADDR_REG, 0xfb);
        assert_eq!(HDMI_EDID_DEV_ADDR, 0xa0);
        assert_eq!(HDMI_EDID_OFFSET_REG, 0xfc);
        assert_eq!(HDMI_EDID_REQ_REG, 0xfa);
        assert_eq!(HDMI_EDID_REQ_READ_MASK, 0x01);
    }

    #[test]
    fn test_hdmi_page_select() {
        assert_eq!(HDMI_PAGE_SELECT_REG, 0xff);
    }

    #[test]
    fn test_hdmi_read_write() {
        let mut drv = make_driver();
        // Write then read back
        assert!(drv.hdmi_write(HDMI_CTRL_PAGE, 0x05, 0xAB).is_ok());
        let val = drv.hdmi_read(HDMI_CTRL_PAGE, 0x05).unwrap();
        assert_eq!(val, 0xAB);
    }

    #[test]
    fn test_hdmi_set_clear() {
        let mut drv = make_driver();
        // Start with 0x00, set bit 3 -> 0x08
        assert!(drv.hdmi_set(HDMI_CTRL_PAGE, 0x06, 0x08).is_ok());
        assert_eq!(drv.hdmi_read(HDMI_CTRL_PAGE, 0x06).unwrap(), 0x08);
        // Clear bit 3 -> 0x00
        assert!(drv.hdmi_clear(HDMI_CTRL_PAGE, 0x06, 0x08).is_ok());
        assert_eq!(drv.hdmi_read(HDMI_CTRL_PAGE, 0x06).unwrap(), 0x00);
    }

    #[test]
    fn test_check_revision() {
        let mut drv = make_driver();
        // Mock returns 0 for all registers, so revision should be 0.
        assert_eq!(drv.check_revision().unwrap(), 0);
    }

    #[test]
    fn test_read_edid() {
        let mut drv = make_driver();
        let mut buf = [0u8; 128];
        let n = drv.read_edid(&mut buf).unwrap();
        assert_eq!(n, 128);
    }

    #[test]
    fn test_hdmi_init() {
        let mut drv = make_driver();
        assert!(drv.hdmi_init().is_ok());
    }

    #[test]
    fn test_pageless_reg() {
        let mut drv = make_driver();
        // HDMI_PAGELESS writes shouldn't trigger a page select
        assert!(drv.hdmi_write(HDMI_PAGELESS, 0x00, 0x42).is_ok());
    }

    #[test]
    fn test_mock_i2c_new() {
        let mock = MockI2c::new();
        assert_eq!(mock.regs[0], 0);
        assert_eq!(mock.regs[255], 0);
    }

    #[test]
    fn test_is_display_connected_not_connected() {
        let mut drv = make_driver();
        // Mock returns all zeros, so display is not connected.
        assert!(!drv.is_display_connected().unwrap());
    }

    #[test]
    fn test_is_display_connected_connected() {
        let mut drv = make_driver();
        // Set the connected bit in the CEC status register.
        drv.bus.regs[CEC_STATUS_REG as usize] = CEC_STATUS_CONNECTED_MASK;
        assert!(drv.is_display_connected().unwrap());
    }

    #[test]
    fn test_i2c_bus_read_write() {
        let mut bus = MockI2c::new();
        assert!(bus.write(0x34, 0x10, &[0xCD]).is_ok());
        let mut buf = [0u8];
        assert!(bus.read(0x34, 0x10, &mut buf).is_ok());
        assert_eq!(buf[0], 0xCD);
    }

    #[test]
    fn test_driver_new() {
        let _ = make_driver();
        // Just verify the driver can be created.
    }

    #[test]
    fn test_page_and_register_values() {
        assert_eq!(HDMI_CTRL_PAGE, 0x00);
        assert_eq!(HDMI_PPL_PAGE, 0x02);
        assert_eq!(HDMI_INFO_PAGE, 0x10);
        assert_eq!(HDMI_AUDIO_PAGE, 0x11);
        assert_eq!(HDMI_HDCP_OTP_PAGE, 0x12);
        assert_eq!(HDMI_GAMUT_PAGE, 0x13);
    }

    #[test]
    fn test_edid_segment_registers() {
        assert_eq!(HDMI_EDID_SEG_PTR_ADDR_REG, 0xfc);
        assert_eq!(HDMI_EDID_SEG_ADDR_REG, 0xfe);
        assert_eq!(HDMI_EDID_SEG_ADDR, 0x00);
    }

    #[test]
    fn test_hdcp_registers() {
        assert_eq!(HDMI_HDCP_OTP_DDC_CLK_REG, 0x9a);
        assert_eq!(HDMI_HDCP_OTP_SOME_REG, 0x9b);
        assert_eq!(HDMI_HDCP_OTP_SOME_MASK, 0x02);
    }
}
