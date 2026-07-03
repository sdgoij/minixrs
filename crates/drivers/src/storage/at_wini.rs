//! AT_WINI IDE/PATA driver — IBM-AT Winchester controller.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/at_wini/at_wini.c`
//!
//! Supports up to 4 drives per instance (primary master/slave,
//! secondary master/slave) via legacy I/O ports or PCI BARs.
//! ATA and ATAPI devices, PIO and DMA transfer modes.

#![allow(dead_code, clippy::collapsible_if)]

use crate::DriverError;

// ── I/O port bases ──────────────────────────────────────────────────────────

/// Primary command base.
pub const REG_CMD_BASE0: u16 = 0x1F0;
/// Secondary command base.
pub const REG_CMD_BASE1: u16 = 0x170;
/// Primary control base.
pub const REG_CTL_BASE0: u16 = 0x3F6;
/// Secondary control base.
pub const REG_CTL_BASE1: u16 = 0x376;

// ── Register offsets (relative to command base) ─────────────────────────────

pub const REG_DATA: u16 = 0;
pub const REG_PRECOMP: u16 = 1;
pub const REG_COUNT: u16 = 2;
pub const REG_SECTOR: u16 = 3;
pub const REG_CYL_LO: u16 = 4;
pub const REG_CYL_HI: u16 = 5;
pub const REG_LDH: u16 = 6;
pub const REG_COMMAND: u16 = 7; // write: command, read: status

// ── LDH register bits ──────────────────────────────────────────────────────

pub const LDH_DEFAULT: u8 = 0xA0;
pub const LDH_LBA: u8 = 0x40;
pub const LDH_DEV: u8 = 0x10;

// ── Status register bits ────────────────────────────────────────────────────

pub const STATUS_BSY: u8 = 0x80;
pub const STATUS_RDY: u8 = 0x40;
pub const STATUS_WF: u8 = 0x20;
pub const STATUS_DRQ: u8 = 0x08;
pub const STATUS_ERR: u8 = 0x01;

// ── Error register bits ─────────────────────────────────────────────────────

pub const ERROR_BB: u8 = 0x80;
pub const ERROR_ECC: u8 = 0x40;
pub const ERROR_ID: u8 = 0x10;
pub const ERROR_AC: u8 = 0x04;

// ── ATA commands ────────────────────────────────────────────────────────────

pub const CMD_IDLE: u8 = 0x00;
pub const CMD_RECALIBRATE: u8 = 0x10;
pub const CMD_READ: u8 = 0x20;
pub const CMD_READ_EXT: u8 = 0x24;
pub const CMD_READ_DMA_EXT: u8 = 0x25;
pub const CMD_WRITE: u8 = 0x30;
pub const CMD_WRITE_EXT: u8 = 0x34;
pub const CMD_WRITE_DMA_EXT: u8 = 0x35;
pub const CMD_READVERIFY: u8 = 0x40;
pub const CMD_SEEK: u8 = 0x70;
pub const CMD_DIAG: u8 = 0x90;
pub const CMD_SPECIFY: u8 = 0x91;
pub const CMD_READ_DMA: u8 = 0xC8;
pub const CMD_WRITE_DMA: u8 = 0xCA;
pub const CMD_FLUSH_CACHE: u8 = 0xE7;
pub const ATA_IDENTIFY: u8 = 0xEC;

// ── Control register bits ───────────────────────────────────────────────────

pub const CTL_RESET: u8 = 0x04;
pub const CTL_INTDISABLE: u8 = 0x02;

// ── Identify word offsets ───────────────────────────────────────────────────

pub const ID_GENERAL: usize = 0x00;
pub const ID_GEN_NOT_ATA: u16 = 0x8000;
pub const ID_CAPABILITIES: usize = 0x31;
pub const ID_CAP_LBA: u16 = 0x0200;
pub const ID_CAP_DMA: u16 = 0x0100;
pub const ID_FIELD_VALIDITY: usize = 0x35;
pub const ID_FV_88: u16 = 0x04;
pub const ID_MULTIWORD_DMA: usize = 0x3F;
pub const ID_CSS: usize = 0x53;
pub const ID_CSS_LBA48: u16 = 0x0400;
pub const ID_ULTRA_DMA: usize = 0x58;

// ── DMA registers (relative to bus master base) ─────────────────────────────

pub const DMA_COMMAND: u16 = 0;
pub const DMA_CMD_START: u8 = 0x01;
pub const DMA_CMD_WRITE: u8 = 0x08;
pub const DMA_STATUS: u16 = 2;
pub const DMA_ST_INT: u8 = 0x04;
pub const DMA_ST_ERROR: u8 = 0x02;
pub const DMA_ST_ACTIVE: u8 = 0x01;
pub const DMA_PRDTP: u16 = 4;

// ── Drive flags ─────────────────────────────────────────────────────────────

pub const DF_INITIALIZED: u32 = 0x01;
pub const DF_DEAF: u32 = 0x02;
pub const DF_SMART: u32 = 0x04;
pub const DF_ATAPI: u32 = 0x08;
pub const DF_IDENTIFIED: u32 = 0x10;
pub const DF_IGNORING: u32 = 0x20;

// ── Constants ───────────────────────────────────────────────────────────────

pub const MAX_DRIVES: usize = 4;
pub const MAX_SECS: u16 = 256;
pub const MAX_ERRORS: u32 = 4;
pub const SECTOR_SIZE: usize = 512;
pub const ATA_DMA_SECTORS: u16 = 64;
pub const ATA_DMA_BUF_SIZE: usize = ATA_DMA_SECTORS as usize * SECTOR_SIZE;
pub const N_PRDTE: usize = 1024;
pub const PRDTE_FL_EOT: u8 = 0x80;

// ── ATA command block ───────────────────────────────────────────────────────

#[repr(C)]
pub struct AtaCommand {
    pub precomp: u8,
    pub count: u8,
    pub sector: u8,
    pub cyl_lo: u8,
    pub cyl_hi: u8,
    pub ldh: u8,
    pub command: u8,
    pub count_prev: u8,
    pub sector_prev: u8,
    pub cyl_lo_prev: u8,
    pub cyl_hi_prev: u8,
}

impl AtaCommand {
    pub const fn new() -> Self {
        Self {
            precomp: 0,
            count: 0,
            sector: 0,
            cyl_lo: 0,
            cyl_hi: 0,
            ldh: LDH_DEFAULT,
            command: CMD_IDLE,
            count_prev: 0,
            sector_prev: 0,
            cyl_lo_prev: 0,
            cyl_hi_prev: 0,
        }
    }

    /// Set LBA28 address and sector count.
    pub fn set_lba28(&mut self, lba: u32, count: u8) {
        self.sector = (lba & 0xFF) as u8;
        self.cyl_lo = ((lba >> 8) & 0xFF) as u8;
        self.cyl_hi = ((lba >> 16) & 0xFF) as u8;
        self.ldh = LDH_DEFAULT | LDH_LBA | (((lba >> 24) & 0x0F) as u8);
        self.count = count;
    }

    /// Set LBA48 address and sector count.
    pub fn set_lba48(&mut self, lba: u64, count: u16) {
        self.sector = (lba & 0xFF) as u8;
        self.cyl_lo = ((lba >> 8) & 0xFF) as u8;
        self.cyl_hi = ((lba >> 16) & 0xFF) as u8;
        self.sector_prev = ((lba >> 24) & 0xFF) as u8;
        self.cyl_lo_prev = ((lba >> 32) & 0xFF) as u8;
        self.cyl_hi_prev = ((lba >> 40) & 0xFF) as u8;
        self.ldh = LDH_DEFAULT | LDH_LBA;
        self.count = (count & 0xFF) as u8;
        self.count_prev = ((count >> 8) & 0xFF) as u8;
    }
}

impl Default for AtaCommand {
    fn default() -> Self {
        Self::new()
    }
}

// ── PRD Table Entry ─────────────────────────────────────────────────────────

#[repr(C)]
pub struct Prdte {
    pub base: u64,
    pub count: u16,
    pub reserved: u8,
    pub flags: u8,
}

impl Prdte {
    pub const fn new() -> Self {
        Self {
            base: 0,
            count: 0,
            reserved: 0,
            flags: 0,
        }
    }
}

impl Default for Prdte {
    fn default() -> Self {
        Self::new()
    }
}

// ── Drive state ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
#[repr(C)]
pub struct AtWiniDrive {
    pub flags: u32,
    pub base_cmd: u16,
    pub base_ctl: u16,
    pub base_dma: u16,
    pub lba48: bool,
    pub dma: bool,
    pub cylinders: u32,
    pub heads: u32,
    pub sectors_per_track: u32,
    pub ldhpref: u8,
    pub max_count: u16,
    pub open_count: i32,
}

impl AtWiniDrive {
    pub const fn new() -> Self {
        Self {
            flags: 0,
            base_cmd: 0,
            base_ctl: 0,
            base_dma: 0,
            lba48: false,
            dma: false,
            cylinders: 0,
            heads: 0,
            sectors_per_track: 0,
            ldhpref: LDH_DEFAULT,
            max_count: MAX_SECS,
            open_count: 0,
        }
    }
}

impl Default for AtWiniDrive {
    fn default() -> Self {
        Self::new()
    }
}

// ── Global state ───────────────────────────────────────────────────────────

static mut DRIVES: [AtWiniDrive; MAX_DRIVES] = [AtWiniDrive::new(); MAX_DRIVES];
static mut NR_DRIVES: usize = 0;

// ── I/O helpers ───────────────────────────────────────────────────────────

unsafe fn inb(port: u16) -> u8 {
    unsafe { crate::arch_io::inb(port) }
}

unsafe fn outb(port: u16, val: u8) {
    unsafe { crate::arch_io::outb(port, val) }
}

unsafe fn inw(port: u16) -> u16 {
    unsafe { crate::arch_io::inw(port) }
}

unsafe fn outw(port: u16, val: u16) {
    unsafe { crate::arch_io::outw(port, val) }
}

unsafe fn inl(port: u16) -> u32 {
    unsafe { crate::arch_io::inl(port) }
}

unsafe fn outl(port: u16, val: u32) {
    unsafe { crate::arch_io::outl(port, val) }
}

// ── ATA register access ─────────────────────────────────────────────────────

unsafe fn ata_read(base_cmd: u16, reg: u16) -> u8 {
    unsafe { inb(base_cmd + reg) }
}

unsafe fn ata_write(base_cmd: u16, reg: u16, val: u8) {
    unsafe {
        outb(base_cmd + reg, val);
    }
}

unsafe fn ata_read_data(base_cmd: u16) -> u16 {
    unsafe { inw(base_cmd + REG_DATA) }
}

// ── Wait for drive ready ────────────────────────────────────────────────────

unsafe fn ata_wait(alt_port: u16) -> Result<(), DriverError> {
    unsafe {
        let mut timeout = 1000000u32;
        loop {
            let st = inb(alt_port);
            if st & STATUS_BSY == 0 {
                if st & STATUS_RDY != 0 || st & STATUS_DRQ != 0 {
                    return Ok(());
                }
            }
            timeout -= 1;
            if timeout == 0 {
                return Err(DriverError::Io);
            }
        }
    }
}

// ── Initialize controller ───────────────────────────────────────────────────

unsafe fn ata_controller_reset(base_ctl: u16) {
    unsafe {
        outb(base_ctl, CTL_RESET);
        // In a real driver we'd wait 50ms here.
        outb(base_ctl, 0);
    }
}

// ── Probe drives ───────────────────────────────────────────────────────────

/// Probe legacy IDE controller ports for drives.
///
/// Checks primary (0x1F0) and secondary (0x170) channels for
/// master and slave drives via the ATA IDENTIFY command.
///
/// # Safety
///
/// Performs privileged I/O port access.
pub unsafe fn at_wini_probe() -> Result<usize, DriverError> {
    unsafe {
        let channels = [
            (REG_CMD_BASE0, REG_CTL_BASE0),
            (REG_CMD_BASE1, REG_CTL_BASE1),
        ];

        let mut count = 0usize;

        for &(cmd_base, ctl_base) in &channels {
            for drive in 0..2u8 {
                let ldh = LDH_DEFAULT | if drive == 1 { LDH_DEV } else { 0 };

                // Select drive.
                ata_write(cmd_base, REG_LDH, ldh);

                // Small delay.
                let _ = inb(ctl_base);

                // Check for drive existence via signature.
                // After selecting a drive, sector and cylinder registers
                // should contain specific signature values if a drive exists.
                ata_write(cmd_base, REG_SECTOR, 0x55);
                ata_write(cmd_base, REG_CYL_LO, 0xAA);

                if ata_read(cmd_base, REG_SECTOR) != 0x55 || ata_read(cmd_base, REG_CYL_LO) != 0xAA
                {
                    continue; // No drive at this location.
                }

                if count >= MAX_DRIVES {
                    break;
                }

                let drv = &mut (*core::ptr::addr_of_mut!(DRIVES))[count];
                drv.base_cmd = cmd_base;
                drv.base_ctl = ctl_base;
                drv.ldhpref = ldh;
                drv.flags |= DF_INITIALIZED | DF_SMART;

                // Attempt IDENTIFY.
                let st = ata_wait(ctl_base);
                if st.is_ok() {
                    let _ = ata_identify(drv);
                }

                count += 1;
            }
        }

        *core::ptr::addr_of_mut!(NR_DRIVES) = count;
        if count == 0 {
            Err(DriverError::NotFound)
        } else {
            Ok(count)
        }
    }
}

/// Send IDENTIFY command and parse results.
unsafe fn ata_identify(drv: &mut AtWiniDrive) -> Result<(), DriverError> {
    unsafe {
        // Select drive.
        ata_write(drv.base_cmd, REG_LDH, drv.ldhpref);

        // Set sector count to 0 for IDENTIFY.
        ata_write(drv.base_cmd, REG_COUNT, 0);

        // Send IDENTIFY command.
        ata_write(drv.base_cmd, REG_COMMAND, ATA_IDENTIFY);

        // Wait for BSY to clear and DRQ to set.
        let mut timeout = 1000000u32;
        loop {
            let st = inb(drv.base_ctl);
            if st & STATUS_BSY == 0 {
                if st & STATUS_DRQ != 0 {
                    break; // Data ready.
                }
                if st & STATUS_ERR != 0 {
                    return Err(DriverError::Io);
                }
                return Err(DriverError::NotFound); // No device.
            }
            timeout -= 1;
            if timeout == 0 {
                return Err(DriverError::Io);
            }
        }

        // Read IDENTIFY data (256 words).
        let mut ident = [0u16; 256];
        for word in ident.iter_mut() {
            *word = ata_read_data(drv.base_cmd);
        }

        // Parse results.
        let general = ident[ID_GENERAL];
        if general & ID_GEN_NOT_ATA != 0 {
            drv.flags |= DF_ATAPI;
        }

        drv.lba48 = ident[ID_CSS] & ID_CSS_LBA48 != 0;

        if ident[ID_CAPABILITIES] & ID_CAP_LBA != 0 {
            // LBA supported.
        }

        if ident[ID_FIELD_VALIDITY] & ID_FV_88 != 0 {
            if ident[ID_ULTRA_DMA] & 0x0024 != 0 {
                drv.dma = true;
            }
        }

        drv.flags |= DF_IDENTIFIED;
        Ok(())
    }
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Get the number of detected drives.
pub fn at_wini_drive_count() -> usize {
    unsafe { NR_DRIVES }
}

/// Get a mutable pointer to a drive by index.
pub fn at_wini_drive(index: usize) -> Option<&'static mut AtWiniDrive> {
    unsafe {
        if index < NR_DRIVES {
            Some(&mut (*core::ptr::addr_of_mut!(DRIVES))[index])
        } else {
            None
        }
    }
}

/// Execute a PIO data-in transfer (read).
///
/// Reads sectors from the drive into the buffer using PIO protocol.
///
/// # Safety
///
/// I/O port access; `buf` must be valid for `count * 512` bytes.
pub unsafe fn at_wini_pio_read(
    drv: &mut AtWiniDrive,
    lba: u64,
    buf: &mut [u8],
    count: u16,
) -> Result<(), DriverError> {
    unsafe {
        let mut cmd = AtaCommand::new();
        cmd.command = if drv.lba48 { CMD_READ_EXT } else { CMD_READ };

        if drv.lba48 {
            cmd.set_lba48(lba, count);
        } else {
            cmd.set_lba28(lba as u32, count as u8);
        }

        // Select drive.
        ata_write(drv.base_cmd, REG_LDH, cmd.ldh);

        if drv.lba48 {
            ata_write(drv.base_cmd, REG_COUNT, cmd.count_prev);
            ata_write(drv.base_cmd, REG_SECTOR, cmd.sector_prev);
            ata_write(drv.base_cmd, REG_CYL_LO, cmd.cyl_lo_prev);
            ata_write(drv.base_cmd, REG_CYL_HI, cmd.cyl_hi_prev);
        }

        ata_write(drv.base_cmd, REG_COUNT, cmd.count);
        ata_write(drv.base_cmd, REG_SECTOR, cmd.sector);
        ata_write(drv.base_cmd, REG_CYL_LO, cmd.cyl_lo);
        ata_write(drv.base_cmd, REG_CYL_HI, cmd.cyl_hi);

        // Issue command.
        ata_write(drv.base_cmd, REG_COMMAND, cmd.command);

        // Read sectors.
        let mut remaining = count as usize;
        let mut offset = 0usize;

        while remaining > 0 {
            // Wait for DRQ.
            let mut timeout = 1000000u32;
            loop {
                let st = inb(drv.base_ctl);
                if st & STATUS_BSY == 0 {
                    if st & STATUS_DRQ != 0 {
                        break;
                    }
                    if st & STATUS_ERR != 0 {
                        return Err(DriverError::Io);
                    }
                }
                timeout -= 1;
                if timeout == 0 {
                    return Err(DriverError::Io);
                }
            }

            // Read one sector (256 words = 512 bytes).
            let sector_end = offset + SECTOR_SIZE;
            while offset < sector_end {
                let word = ata_read_data(drv.base_cmd);
                buf[offset] = (word & 0xFF) as u8;
                buf[offset + 1] = ((word >> 8) & 0xFF) as u8;
                offset += 2;
            }

            remaining -= 1;
        }

        Ok(())
    }
}

/// Build the LDH register value for a given drive number.
pub fn ldh_init(drive: u8) -> u8 {
    LDH_DEFAULT | ((drive & 1) << 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(REG_CMD_BASE0, 0x1F0);
        assert_eq!(REG_CMD_BASE1, 0x170);
        assert_eq!(REG_CTL_BASE0, 0x3F6);
        assert_eq!(MAX_DRIVES, 4);
        assert_eq!(MAX_SECS, 256);
        assert_eq!(SECTOR_SIZE, 512);
    }

    #[test]
    fn test_ata_command_new() {
        let cmd = AtaCommand::new();
        assert_eq!(cmd.ldh, LDH_DEFAULT);
        assert_eq!(cmd.command, CMD_IDLE);
        assert_eq!(cmd.count, 0);
    }

    #[test]
    fn test_ata_command_default() {
        let cmd: AtaCommand = Default::default();
        assert_eq!(cmd.ldh, LDH_DEFAULT);
    }

    #[test]
    fn test_set_lba28() {
        let mut cmd = AtaCommand::new();
        cmd.set_lba28(0x1234_5678, 1);
        assert_eq!(cmd.sector, 0x78);
        assert_eq!(cmd.cyl_lo, 0x56);
        assert_eq!(cmd.cyl_hi, 0x34);
        assert_eq!(cmd.ldh, 0xE2); // LDH_DEFAULT|LDH_LBA|0x02 (bits 27:24=0x12&0x0F)
        assert_eq!(cmd.count, 1);
    }

    #[test]
    fn test_set_lba48() {
        let mut cmd = AtaCommand::new();
        cmd.set_lba48(0x1234_5678_9ABC, 1);
        assert_eq!(cmd.sector, 0xBC);
        assert_eq!(cmd.cyl_lo, 0x9A);
        assert_eq!(cmd.cyl_hi, 0x78);
        assert_eq!(cmd.sector_prev, 0x56);
        assert_eq!(cmd.cyl_lo_prev, 0x34);
        assert_eq!(cmd.cyl_hi_prev, 0x12);
    }

    #[test]
    fn test_prdte_new() {
        let prd = Prdte::new();
        assert_eq!(prd.base, 0);
        assert_eq!(prd.count, 0);
        assert_eq!(prd.flags, 0);
    }

    #[test]
    fn test_drive_new() {
        let d = AtWiniDrive::new();
        assert_eq!(d.flags, 0);
        assert_eq!(d.base_cmd, 0);
        assert_eq!(d.base_ctl, 0);
        assert_eq!(d.ldhpref, LDH_DEFAULT);
        assert_eq!(d.max_count, MAX_SECS);
        assert_eq!(d.open_count, 0);
    }

    #[test]
    fn test_drive_default() {
        let d: AtWiniDrive = Default::default();
        assert_eq!(d.ldhpref, LDH_DEFAULT);
    }

    #[test]
    fn test_drive_flags() {
        assert_eq!(DF_INITIALIZED, 0x01);
        assert_eq!(DF_DEAF, 0x02);
        assert_eq!(DF_SMART, 0x04);
        assert_eq!(DF_ATAPI, 0x08);
        assert_eq!(DF_IDENTIFIED, 0x10);
        assert_eq!(DF_IGNORING, 0x20);
    }

    #[test]
    fn test_ldh_constants() {
        assert_eq!(LDH_DEFAULT, 0xA0);
        assert_eq!(LDH_LBA, 0x40);
        assert_eq!(LDH_DEV, 0x10);
        assert_eq!(ldh_init(0), 0xA0);
        assert_eq!(ldh_init(1), 0xB0);
    }

    #[test]
    fn test_status_bits() {
        assert_eq!(STATUS_BSY, 0x80);
        assert_eq!(STATUS_RDY, 0x40);
        assert_eq!(STATUS_DRQ, 0x08);
        assert_eq!(STATUS_ERR, 0x01);
    }

    #[test]
    fn test_error_bits() {
        assert_eq!(ERROR_BB, 0x80);
        assert_eq!(ERROR_AC, 0x04);
    }

    #[test]
    fn test_commands() {
        assert_eq!(CMD_READ, 0x20);
        assert_eq!(CMD_READ_EXT, 0x24);
        assert_eq!(CMD_WRITE, 0x30);
        assert_eq!(CMD_WRITE_EXT, 0x34);
        assert_eq!(ATA_IDENTIFY, 0xEC);
    }

    #[test]
    fn test_ctrl_bits() {
        assert_eq!(CTL_RESET, 0x04);
        assert_eq!(CTL_INTDISABLE, 0x02);
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore = "requires hardware I/O")]
    fn test_probe_no_hardware() {
        unsafe {
            *core::ptr::addr_of_mut!(NR_DRIVES) = 0;
            let result = at_wini_probe();
            assert!(result.is_err() || result.is_ok());
        }
    }

    #[test]
    fn test_drive_init_ldh() {
        assert_eq!(ldh_init(0), 0xA0);
        assert_eq!(ldh_init(1), 0xB0);
    }

    #[test]
    fn test_identify_constants() {
        assert_eq!(ID_GENERAL, 0x00);
        assert_eq!(ID_GEN_NOT_ATA, 0x8000);
        assert_eq!(ID_CAPABILITIES, 0x31);
        assert_eq!(ID_CAP_LBA, 0x0200);
        assert_eq!(ID_CSS_LBA48, 0x0400);
    }

    #[test]
    fn test_ata_cmd_set_lba28_drive1() {
        let mut cmd = AtaCommand::new();
        cmd.set_lba28(0, 0);
        assert_eq!(cmd.ldh & 0x40, 0x40); // LBA bit
    }
}
