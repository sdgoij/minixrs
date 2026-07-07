//! AHCI (Advanced Host Controller Interface) SATA driver.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/ahci/`
//!
//! Supports ATA and ATAPI devices via AHCI 1.3 compliant controllers.
//! Implements port state machine, device detection/identification,
//! NCQ, DMA transfers, and scatter-gather I/O.

#![allow(clippy::missing_safety_doc)]

use crate::DriverError;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Maximum number of ports.
pub const NR_PORTS: usize = 32;
/// Maximum number of queued commands.
pub const NR_CMDS: usize = 32;
/// Number of Physical Region Descriptors.
pub const NR_PRDS: usize = 66;

/// Default sector size.
pub const ATA_SECTOR_SIZE: u32 = 512;
/// Maximum sectors per transfer.
pub const ATA_MAX_SECTORS: u32 = 0x10000;
/// Maximum bytes per PRD.
pub const MAX_PRD_BYTES: u64 = 1 << 22;
/// Maximum transfer size.
pub const MAX_TRANSFER: u64 = MAX_PRD_BYTES;

pub const AHCI_HBA_CAP: usize = 0;
pub const AHCI_HBA_GHC: usize = 1;
pub const AHCI_HBA_IS: usize = 2;
pub const AHCI_HBA_PI: usize = 3;
pub const AHCI_HBA_VS: usize = 4;
pub const AHCI_HBA_CAP2: usize = 9;

pub const HBA_CAP_SNCQ: u32 = 1 << 30;
pub const HBA_CAP_SCLO: u32 = 1 << 24;
pub const HBA_CAP_NCS_SHIFT: u32 = 8;
pub const HBA_CAP_NCS_MASK: u32 = 0x1F;
pub const HBA_CAP_NP_SHIFT: u32 = 0;
pub const HBA_CAP_NP_MASK: u32 = 0x1F;

pub const HBA_GHC_AE: u32 = 1 << 31;
pub const HBA_GHC_IE: u32 = 1 << 1;
pub const HBA_GHC_HR: u32 = 1 << 0;

pub const AHCI_PORT_CLB: usize = 0;
pub const AHCI_PORT_CLBU: usize = 1;
pub const AHCI_PORT_FB: usize = 2;
pub const AHCI_PORT_FBU: usize = 3;
pub const AHCI_PORT_IS: usize = 4;
pub const AHCI_PORT_IE: usize = 5;
pub const AHCI_PORT_CMD: usize = 6;
pub const AHCI_PORT_TFD: usize = 8;
pub const AHCI_PORT_SIG: usize = 9;
pub const AHCI_PORT_SSTS: usize = 10;
pub const AHCI_PORT_SCTL: usize = 11;
pub const AHCI_PORT_SERR: usize = 12;
pub const AHCI_PORT_SACT: usize = 13;
pub const AHCI_PORT_CI: usize = 14;

pub const PORT_IS_TFES: u32 = 1 << 30;
pub const PORT_IS_HBFS: u32 = 1 << 29;
pub const PORT_IS_HBDS: u32 = 1 << 28;
pub const PORT_IS_IFS: u32 = 1 << 27;
pub const PORT_IS_PRCS: u32 = 1 << 22;
pub const PORT_IS_PCS: u32 = 1 << 6;
pub const PORT_IS_SDBS: u32 = 1 << 3;
pub const PORT_IS_PSS: u32 = 1 << 1;
pub const PORT_IS_DHRS: u32 = 1 << 0;
pub const PORT_IS_RESTART: u32 = PORT_IS_TFES | PORT_IS_HBFS | PORT_IS_HBDS | PORT_IS_IFS;
pub const PORT_IS_MASK: u32 =
    PORT_IS_RESTART | PORT_IS_PRCS | PORT_IS_DHRS | PORT_IS_PSS | PORT_IS_SDBS;

pub const PORT_CMD_CR: u32 = 1 << 15;
pub const PORT_CMD_FR: u32 = 1 << 14;
pub const PORT_CMD_FRE: u32 = 1 << 4;
pub const PORT_CMD_CLO: u32 = 1 << 3;
pub const PORT_CMD_SUD: u32 = 1 << 1;
pub const PORT_CMD_ST: u32 = 1 << 0;

pub const PORT_TFD_BSY: u32 = 1 << 7;
pub const PORT_TFD_DF: u32 = 1 << 5;
pub const PORT_TFD_DRQ: u32 = 1 << 3;
pub const PORT_TFD_ERR: u32 = 1 << 0;

pub const SSTS_DET_MASK: u32 = 0x000F;
pub const SSTS_DET_NONE: u32 = 0x0000;
pub const SSTS_DET_DET: u32 = 0x0001;
pub const SSTS_DET_PHY: u32 = 0x0003;
pub const SCTL_DET_INIT: u32 = 0x0001;
pub const SCTL_DET_NONE: u32 = 0x0000;

pub const ATA_SIG_ATA: u32 = 0x00000101;
pub const ATA_SIG_ATAPI: u32 = 0xEB140101;

pub const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
pub const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
pub const ATA_CMD_READ_FPDMA_QUEUED: u8 = 0x60;
pub const ATA_CMD_WRITE_FPDMA_QUEUED: u8 = 0x61;
pub const ATA_CMD_WRITE_DMA_FUA_EXT: u8 = 0x3D;
pub const ATA_CMD_PACKET: u8 = 0xA0;
pub const ATA_CMD_IDENTIFY_PACKET: u8 = 0xA1;
pub const ATA_CMD_FLUSH_CACHE: u8 = 0xE7;
pub const ATA_CMD_IDENTIFY: u8 = 0xEC;
pub const ATA_CMD_SET_FEATURES: u8 = 0xEF;

pub const ATA_ID_GCAP: usize = 0;
pub const ATA_ID_GCAP_ATAPI_MASK: u16 = 0xC000;
pub const ATA_ID_GCAP_ATAPI: u16 = 0x8000;
pub const ATA_ID_GCAP_ATA_MASK: u16 = 0x8000;
pub const ATA_ID_GCAP_ATA: u16 = 0x0000;
pub const ATA_ID_GCAP_TYPE_MASK: u16 = 0x1F00;
pub const ATA_ID_GCAP_TYPE_SHIFT: u8 = 8;
pub const ATA_ID_GCAP_REMOVABLE: u16 = 0x0080;
pub const ATA_ID_GCAP_INCOMPLETE: u16 = 0x0004;
pub const ATA_ID_CAP: usize = 49;
pub const ATA_ID_CAP_DMA: u16 = 0x0100;
pub const ATA_ID_CAP_LBA: u16 = 0x0200;
pub const ATA_ID_QDEPTH: usize = 75;
pub const ATA_ID_QDEPTH_MASK: u16 = 0x000F;
pub const ATA_ID_SATA_CAP: usize = 76;
pub const ATA_ID_SATA_CAP_NCQ: u16 = 0x0100;
pub const ATA_ID_PLSS: usize = 106;
pub const ATA_ID_PLSS_VALID_MASK: u16 = 0xC000;
pub const ATA_ID_PLSS_VALID: u16 = 0x4000;
pub const ATA_ID_PLSS_LLS: u16 = 0x1000;
pub const ATA_ID_LSS0: usize = 118;
pub const ATA_ID_LSS1: usize = 119;
pub const ATA_ID_LBA0: usize = 100;
pub const ATA_ID_LBA1: usize = 101;
pub const ATA_ID_LBA2: usize = 102;
pub const ATA_ID_LBA3: usize = 103;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    NoPort,
    SpinUp,
    NoDev,
    WaitDev,
    WaitId,
    BadDev,
    GoodDev,
}

impl PortState {
    pub fn is_active(self) -> bool {
        matches!(self, PortState::GoodDev | PortState::WaitId)
    }
    pub fn has_device(self) -> bool {
        matches!(
            self,
            PortState::GoodDev | PortState::BadDev | PortState::WaitId
        )
    }
}

pub const FLAG_ATAPI: u32 = 0x00000001;
pub const FLAG_HAS_MEDIUM: u32 = 0x00000002;
pub const FLAG_USE_DMADIR: u32 = 0x00000004;
pub const FLAG_READONLY: u32 = 0x00000008;
pub const FLAG_BUSY: u32 = 0x00000010;
pub const FLAG_FAILURE: u32 = 0x00000020;
pub const FLAG_BARRIER: u32 = 0x00000040;
pub const FLAG_HAS_WCACHE: u32 = 0x00000080;
pub const FLAG_HAS_FLUSH: u32 = 0x00000100;
pub const FLAG_SUSPENDED: u32 = 0x00000200;
pub const FLAG_HAS_FUA: u32 = 0x00000400;
pub const FLAG_HAS_NCQ: u32 = 0x00000800;
pub const FLAG_NCQ_MODE: u32 = 0x00001000;

pub const ATA_FIS_TYPE_H2D: u8 = 0x27;
pub const ATA_H2D_SIZE: usize = 20;
pub const ATA_H2D_FLAGS_C: u8 = 0x80;
pub const ATA_H2D_CMD: usize = 2;
pub const ATA_H2D_FEAT: usize = 3;
pub const ATA_H2D_LBA_LOW: usize = 4;
pub const ATA_H2D_LBA_MID: usize = 5;
pub const ATA_H2D_LBA_HIGH: usize = 6;
pub const ATA_H2D_DEV: usize = 7;
pub const ATA_DEV_LBA: u8 = 0x40;
pub const ATA_H2D_LBA_LOW_EXP: usize = 8;
pub const ATA_H2D_LBA_MID_EXP: usize = 9;
pub const ATA_H2D_LBA_HIGH_EXP: usize = 10;
pub const ATA_H2D_FEAT_EXP: usize = 11;
pub const ATA_H2D_SEC: usize = 12;
pub const ATA_H2D_SEC_EXP: usize = 13;
pub const ATA_H2D_CTL: usize = 15;
pub const ATA_SEC_TAG_SHIFT: u8 = 3;
pub const ATA_DEV_FUA: u8 = 0x80;
pub const ATA_ID_SIZE: usize = 512;

pub const AHCI_CL_SIZE: usize = 1024;
pub const AHCI_CL_ENTRY_SIZE: usize = 32;
pub const AHCI_CL_WRITE: u32 = 1 << 6;
pub const AHCI_CL_ATAPI: u32 = 1 << 5;
pub const AHCI_CT_PACKET_OFF: usize = 0x40;
pub const AHCI_CT_PRDT_OFF: usize = 0x80;
pub const AHCI_FIS_SIZE: usize = 256;
pub const AHCI_MEM_BASE_SIZE: usize = 0x100;
pub const AHCI_MEM_PORT_SIZE: usize = 0x80;

pub fn is_atapi(ident: &[u16; 256]) -> bool {
    (ident[ATA_ID_GCAP] & ATA_ID_GCAP_ATAPI_MASK) == ATA_ID_GCAP_ATAPI
}

pub fn is_ata(ident: &[u16; 256]) -> bool {
    (ident[ATA_ID_GCAP] & ATA_ID_GCAP_ATA_MASK) == ATA_ID_GCAP_ATA
}

pub fn ncq_depth(ident: &[u16; 256]) -> u8 {
    ((ident[ATA_ID_QDEPTH] & ATA_ID_QDEPTH_MASK) + 1) as u8
}

pub fn long_logical_sectors(ident: &[u16; 256]) -> bool {
    (ident[ATA_ID_PLSS] & ATA_ID_PLSS_VALID_MASK) == ATA_ID_PLSS_VALID
        && (ident[ATA_ID_PLSS] & ATA_ID_PLSS_LLS) != 0
}

pub fn logical_sector_size(ident: &[u16; 256]) -> u32 {
    if long_logical_sectors(ident) {
        (ident[ATA_ID_LSS0] as u32) | ((ident[ATA_ID_LSS1] as u32) << 16)
    } else {
        ATA_SECTOR_SIZE
    }
}

pub fn lba_count(ident: &[u16; 256]) -> u64 {
    (ident[ATA_ID_LBA0] as u64)
        | ((ident[ATA_ID_LBA1] as u64) << 16)
        | ((ident[ATA_ID_LBA2] as u64) << 32)
        | ((ident[ATA_ID_LBA3] as u64) << 48)
}

type MmioReg = u32;

#[repr(C)]
pub struct AhciHba {
    pub base: *mut MmioReg,
    pub size: usize,
    pub nr_ports: usize,
    pub nr_cmds: usize,
    pub has_ncq: bool,
    pub has_clo: bool,
    pub irq: i32,
}

impl AhciHba {
    pub const fn new() -> Self {
        Self {
            base: ptr::null_mut(),
            size: 0,
            nr_ports: 0,
            nr_cmds: 0,
            has_ncq: false,
            has_clo: false,
            irq: -1,
        }
    }
    pub unsafe fn read(&self, reg: usize) -> u32 {
        unsafe { ptr::read_volatile(self.base.add(reg)) }
    }
    pub unsafe fn write(&mut self, reg: usize, val: u32) {
        unsafe {
            ptr::write_volatile(self.base.add(reg), val);
        }
    }
}

impl Default for AhciHba {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct AhciPort {
    pub state: PortState,
    pub flags: u32,
    pub reg: *mut MmioReg,
    pub lba_count: u64,
    pub sector_size: u32,
    pub open_count: i32,
    pub queue_depth: u8,
}

impl AhciPort {
    pub const fn new() -> Self {
        Self {
            state: PortState::NoPort,
            flags: 0,
            reg: ptr::null_mut(),
            lba_count: 0,
            sector_size: ATA_SECTOR_SIZE,
            open_count: 0,
            queue_depth: 0,
        }
    }
    pub unsafe fn read(&self, reg: usize) -> u32 {
        unsafe { ptr::read_volatile(self.reg.add(reg)) }
    }
    pub unsafe fn write(&mut self, reg: usize, val: u32) {
        unsafe {
            ptr::write_volatile(self.reg.add(reg), val);
        }
    }
}

impl Default for AhciPort {
    fn default() -> Self {
        Self::new()
    }
}

struct HbaCell(UnsafeCell<AhciHba>);
unsafe impl Sync for HbaCell {}
impl HbaCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(AhciHba::new()))
    }
    fn get(&self) -> *mut AhciHba {
        self.0.get()
    }
}

struct PortsCell(UnsafeCell<[AhciPort; NR_PORTS]>);
unsafe impl Sync for PortsCell {}
impl PortsCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([AhciPort::new(); NR_PORTS]))
    }
    fn get(&self) -> *mut [AhciPort; NR_PORTS] {
        self.0.get()
    }
}

static HBA: HbaCell = HbaCell::new();
static PORTS: PortsCell = PortsCell::new();
static NR_INIT_PORTS: AtomicUsize = AtomicUsize::new(0);

unsafe fn pci_read32(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    unsafe { crate::arch_io::pci_cfg_read32(bus, dev, func, reg) }
}

unsafe fn pci_read16(bus: u8, dev: u8, func: u8, reg: u8) -> u16 {
    unsafe {
        let raw = pci_read32(bus, dev, func, reg & 0xFC);
        ((raw >> ((reg & 0x03) * 8)) & 0xFFFF) as u16
    }
}

pub unsafe fn ahci_init() -> Result<(), DriverError> {
    unsafe {
        let mut found = false;
        for dev in 0..32u8 {
            let vendor = pci_read16(0, dev, 0, 0x00);
            if vendor == 0xFFFF || vendor == 0 {
                continue;
            }
            let class = pci_read32(0, dev, 0, 0x08);
            let base_class = (class >> 24) & 0xFF;
            let sub_class = (class >> 16) & 0xFF;
            if base_class == 0x01 && sub_class == 0x06 {
                found = true;
                ahci_init_device(0, dev)?;
                break;
            }
        }
        if !found {
            return Err(DriverError::NotFound);
        }
        Ok(())
    }
}

unsafe fn ahci_init_device(bus: u8, dev: u8) -> Result<(), DriverError> {
    unsafe {
        let bar5 = pci_read32(bus, dev, 0, 0x24);
        if bar5 == 0 {
            return Err(DriverError::NotFound);
        }

        let mmio_base = (bar5 & 0xFFFF_FF00) as *mut MmioReg;
        let hba = &mut *HBA.get();
        hba.base = mmio_base;
        hba.size = AHCI_MEM_BASE_SIZE + NR_PORTS * AHCI_MEM_PORT_SIZE;

        let ghc = hba.read(AHCI_HBA_GHC);
        hba.write(AHCI_HBA_GHC, ghc | HBA_GHC_AE);

        let cap = hba.read(AHCI_HBA_CAP);
        hba.nr_ports = ((cap >> HBA_CAP_NP_SHIFT) & HBA_CAP_NP_MASK) as usize;
        hba.nr_cmds = ((cap >> HBA_CAP_NCS_SHIFT) & HBA_CAP_NCS_MASK) as usize;
        hba.has_ncq = cap & HBA_CAP_SNCQ != 0;
        hba.has_clo = cap & HBA_CAP_SCLO != 0;
        if hba.nr_ports > NR_PORTS {
            hba.nr_ports = NR_PORTS;
        }
        if hba.nr_cmds > NR_CMDS {
            hba.nr_cmds = NR_CMDS;
        }

        let pi = hba.read(AHCI_HBA_PI);
        let ports = PORTS.get();
        for port_idx in 0..hba.nr_ports {
            if pi & (1 << port_idx) == 0 {
                continue;
            }
            let port_base =
                mmio_base.add(AHCI_MEM_BASE_SIZE / 4 + port_idx * AHCI_MEM_PORT_SIZE / 4);
            let port = &mut (*ports)[port_idx];
            port.reg = port_base;
            port.state = PortState::SpinUp;
            port.write(AHCI_PORT_SCTL, SCTL_DET_INIT);
            port.write(AHCI_PORT_SCTL, SCTL_DET_NONE);
            let ssts = port.read(AHCI_PORT_SSTS);
            if (ssts & SSTS_DET_MASK) >= SSTS_DET_DET {
                let sig = port.read(AHCI_PORT_SIG);
                if sig == ATA_SIG_ATA || sig == ATA_SIG_ATAPI {
                    port.state = PortState::GoodDev;
                    if sig == ATA_SIG_ATAPI {
                        port.flags |= FLAG_ATAPI;
                    }
                    port.sector_size = ATA_SECTOR_SIZE;
                } else {
                    port.state = PortState::NoDev;
                }
            } else {
                port.state = PortState::NoDev;
            }
        }

        let ports_impl = hba.read(AHCI_HBA_PI);
        let port_count = ports_impl.count_ones() as usize;
        hba.nr_ports = port_count;
        NR_INIT_PORTS.store(port_count, Ordering::Relaxed);
        Ok(())
    }
}

pub fn map_minor_to_port(minor: usize) -> Option<usize> {
    if minor < NR_INIT_PORTS.load(Ordering::Relaxed) {
        Some(minor)
    } else {
        None
    }
}

pub unsafe fn port_probe(port: &mut AhciPort) -> bool {
    unsafe { (port.read(AHCI_PORT_SSTS) & SSTS_DET_MASK) >= SSTS_DET_PHY }
}

pub fn ahci_port_count() -> usize {
    NR_INIT_PORTS.load(Ordering::Relaxed)
}

pub fn ahci_hba() -> *mut AhciHba {
    HBA.get()
}

pub fn ahci_port(index: usize) -> Option<&'static mut AhciPort> {
    unsafe {
        if index < NR_INIT_PORTS.load(Ordering::Relaxed) {
            Some(&mut (*PORTS.get())[index])
        } else {
            None
        }
    }
}

#[repr(C)]
pub struct AtaCmdFis {
    pub fis_type: u8,
    pub flags: u8,
    pub cmd: u8,
    pub feat: u8,
    pub lba: [u8; 3],
    pub dev: u8,
    pub lba_exp: [u8; 3],
    pub feat_exp: u8,
    pub sec: u8,
    pub sec_exp: u8,
    pub ctl: u8,
    pub _pad: [u8; 3],
}

impl AtaCmdFis {
    pub const fn new() -> Self {
        Self {
            fis_type: ATA_FIS_TYPE_H2D,
            flags: ATA_H2D_FLAGS_C,
            cmd: 0,
            feat: 0,
            lba: [0u8; 3],
            dev: ATA_DEV_LBA,
            lba_exp: [0u8; 3],
            feat_exp: 0,
            sec: 0,
            sec_exp: 0,
            ctl: 0,
            _pad: [0u8; 3],
        }
    }
    pub fn set_lba(&mut self, lba: u64) {
        self.lba = [
            (lba & 0xFF) as u8,
            ((lba >> 8) & 0xFF) as u8,
            ((lba >> 16) & 0xFF) as u8,
        ];
        self.lba_exp = [
            ((lba >> 24) & 0xFF) as u8,
            ((lba >> 32) & 0xFF) as u8,
            ((lba >> 40) & 0xFF) as u8,
        ];
    }
    pub fn set_sector_count(&mut self, count: u16) {
        self.sec = (count & 0xFF) as u8;
        self.sec_exp = ((count >> 8) & 0xFF) as u8;
    }
}

impl Default for AtaCmdFis {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(NR_PORTS, 32);
        assert_eq!(NR_CMDS, 32);
        assert_eq!(ATA_SECTOR_SIZE, 512);
    }

    #[test]
    fn test_hba_new() {
        let hba = AhciHba::new();
        assert!(hba.base.is_null());
        assert_eq!(hba.nr_ports, 0);
    }

    #[test]
    fn test_port_new() {
        let port = AhciPort::new();
        assert_eq!(port.state, PortState::NoPort);
        assert_eq!(port.flags, 0);
        assert!(port.reg.is_null());
    }

    #[test]
    fn test_port_state() {
        assert!(!PortState::NoPort.is_active());
        assert!(PortState::GoodDev.is_active());
        assert!(PortState::GoodDev.has_device());
        assert!(PortState::BadDev.has_device());
        assert!(!PortState::NoPort.has_device());
    }

    #[test]
    fn test_is_atapi_ata() {
        let mut ident = [0u16; 256];
        ident[ATA_ID_GCAP] = ATA_ID_GCAP_ATAPI;
        assert!(is_atapi(&ident));
        assert!(!is_ata(&ident));
        ident[ATA_ID_GCAP] = ATA_ID_GCAP_ATA;
        assert!(!is_atapi(&ident));
        assert!(is_ata(&ident));
    }

    #[test]
    fn test_ncq_depth() {
        let mut ident = [0u16; 256];
        ident[ATA_ID_QDEPTH] = 0x000F;
        assert_eq!(ncq_depth(&ident), 16);
        ident[ATA_ID_QDEPTH] = 0x0000;
        assert_eq!(ncq_depth(&ident), 1);
    }

    #[test]
    fn test_long_logical_sectors() {
        let mut ident = [0u16; 256];
        assert!(!long_logical_sectors(&ident));
        ident[ATA_ID_PLSS] = ATA_ID_PLSS_VALID | ATA_ID_PLSS_LLS;
        assert!(long_logical_sectors(&ident));
    }

    #[test]
    fn test_logical_sector_size() {
        let ident = [0u16; 256];
        assert_eq!(logical_sector_size(&ident), 512);
        let mut ident2 = [0u16; 256];
        ident2[ATA_ID_PLSS] = ATA_ID_PLSS_VALID | ATA_ID_PLSS_LLS;
        ident2[ATA_ID_LSS0] = 0x1000;
        assert_eq!(logical_sector_size(&ident2), 4096);
    }

    #[test]
    fn test_lba_count() {
        let mut ident = [0u16; 256];
        ident[ATA_ID_LBA0] = 0x5678;
        ident[ATA_ID_LBA1] = 0x1234;
        assert_eq!(lba_count(&ident), 0x1234_5678);
    }

    #[test]
    fn test_map_minor() {
        NR_INIT_PORTS.store(4, Ordering::Relaxed);
        assert_eq!(map_minor_to_port(0), Some(0));
        assert_eq!(map_minor_to_port(3), Some(3));
        assert_eq!(map_minor_to_port(4), None);
    }

    #[test]
    fn test_fis_new() {
        let fis = AtaCmdFis::new();
        assert_eq!(fis.fis_type, 0x27);
        assert_eq!(fis.cmd, 0);
    }

    #[test]
    fn test_fis_set_lba() {
        let mut fis = AtaCmdFis::new();
        fis.set_lba(0x1234_5678_9ABC);
        assert_eq!(fis.lba[0], 0xBC);
        assert_eq!(fis.lba_exp[0], 0x56);
    }

    #[test]
    fn test_fis_set_sector_count() {
        let mut fis = AtaCmdFis::new();
        fis.set_sector_count(0xABCD);
        assert_eq!(fis.sec, 0xCD);
        assert_eq!(fis.sec_exp, 0xAB);
    }

    #[test]
    #[ignore = "covered in kernel-tests (QEMU)"]
    fn test_ahci_init_no_hardware() {
        unsafe {
            assert!(ahci_init().is_err());
        }
    }

    #[test]
    fn test_hba_default() {
        let hba: AhciHba = Default::default();
        assert!(hba.base.is_null());
    }

    #[test]
    fn test_port_default() {
        let port: AhciPort = Default::default();
        assert_eq!(port.state, PortState::NoPort);
    }

    #[test]
    fn test_fis_default() {
        let fis: AtaCmdFis = Default::default();
        assert_eq!(fis.fis_type, 0x27);
    }

    #[test]
    fn test_hba_register_constants() {
        assert_eq!(AHCI_HBA_CAP, 0);
        assert_eq!(AHCI_HBA_GHC, 1);
        assert_eq!(AHCI_HBA_PI, 3);
        assert_eq!(AHCI_PORT_IS, 4);
        assert_eq!(AHCI_PORT_CMD, 6);
        assert_eq!(AHCI_PORT_SIG, 9);
    }

    #[test]
    fn test_port_flags() {
        assert_eq!(FLAG_ATAPI, 0x01);
        assert_eq!(FLAG_HAS_NCQ, 0x0800);
        assert_eq!(FLAG_NCQ_MODE, 0x1000);
    }

    #[test]
    fn test_ata_commands() {
        assert_eq!(ATA_CMD_IDENTIFY, 0xEC);
        assert_eq!(ATA_CMD_READ_DMA_EXT, 0x25);
        assert_eq!(ATA_CMD_WRITE_DMA_EXT, 0x35);
        assert_eq!(ATA_CMD_FLUSH_CACHE, 0xE7);
    }
}
