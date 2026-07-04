#![allow(dead_code)]

//! PCI bus driver — device enumeration, BAR management, ACLs.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/bus/pci/pci.c`
//!
//! Provides PCI device scanning, configuration space access, BAR
//! resource management, and driver access control lists.

use crate::DriverError;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Check if a PCI device exists (vendor ID != 0xFFFF).
pub(crate) fn pci_device_exists(vendor: u16) -> bool {
    vendor != 0xFFFF && vendor != 0
}

/// Maximum number of PCI buses.
pub const NR_PCI_BUS: usize = 256;

/// Maximum number of PCI devices.
pub const NR_PCI_DEV: usize = 256;

/// Maximum number of PCI drivers (for ACLs).
pub const NR_DRIVERS: usize = 32;

/// Number of base-address registers.
pub const BAR_MAX: usize = 6;

/// Number of expansion ROM BARs.
pub const ROM_BARS: usize = 1;

/// I/O space BAR.
pub const PBF_IO: u8 = 0x01;
/// Not yet allocated.
pub const PBF_INCOMPLETE: u8 = 0x02;

/// Intel host bridge.
pub const PBT_INTEL_HOST: u8 = 1;
/// PCI-to-PCI bridge.
pub const PBT_PCIBRIDGE: u8 = 2;
/// CardBus bridge.
pub const PBT_CARDBUS: u8 = 3;

/// Device is in use by a driver.
pub const PDF_INUSE: u8 = 0x01;

/// A Base Address Register description.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Bar {
    pub flags: u8,
    pub nr: u8,
    pub base: u32,
    pub size: u32,
}

/// A PCI device.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PciDev {
    pub busnr: u8,
    pub dev: u8,
    pub func: u8,
    pub baseclass: u8,
    pub subclass: u8,
    pub infclass: u8,
    pub vid: u16,
    pub did: u16,
    pub sub_vid: u16,
    pub sub_did: u16,
    pub ilr: u8,
    pub flags: u8,
    pub driver_endpt: i32,
    pub bars: [Bar; BAR_MAX],
    pub bar_count: u8,
}

impl PciDev {
    const fn new() -> Self {
        Self {
            busnr: 0,
            dev: 0,
            func: 0,
            baseclass: 0,
            subclass: 0,
            infclass: 0,
            vid: 0,
            did: 0,
            sub_vid: 0,
            sub_did: 0,
            ilr: 0,
            flags: 0,
            driver_endpt: -1,
            bars: [
                Bar {
                    flags: 0,
                    nr: 0,
                    base: 0,
                    size: 0,
                },
                Bar {
                    flags: 0,
                    nr: 0,
                    base: 0,
                    size: 0,
                },
                Bar {
                    flags: 0,
                    nr: 0,
                    base: 0,
                    size: 0,
                },
                Bar {
                    flags: 0,
                    nr: 0,
                    base: 0,
                    size: 0,
                },
                Bar {
                    flags: 0,
                    nr: 0,
                    base: 0,
                    size: 0,
                },
                Bar {
                    flags: 0,
                    nr: 0,
                    base: 0,
                    size: 0,
                },
            ],
            bar_count: 0,
        }
    }
}

/// A PCI bus description.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PciBus {
    pub bridge_type: u8,
    pub need_init: bool,
    pub isa_bridge_dev: i32,
    pub isa_bridge_type: i32,
    pub dev_index: i32,
    pub bus_nr: u8,
}

impl PciBus {
    const fn new() -> Self {
        Self {
            bridge_type: 0,
            need_init: true,
            isa_bridge_dev: -1,
            isa_bridge_type: -1,
            dev_index: -1,
            bus_nr: 0,
        }
    }
}

/// A PCI ACL entry — which driver endpoint can access which device.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PciAcl {
    pub endpoint: i32,
    pub vid: u16,
    pub did: u16,
}

impl PciAcl {
    const fn new() -> Self {
        Self {
            endpoint: -1,
            vid: 0,
            did: 0,
        }
    }
}

struct PciDevicesCell(UnsafeCell<[PciDev; NR_PCI_DEV]>);
unsafe impl Sync for PciDevicesCell {}
impl PciDevicesCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([PciDev::new(); NR_PCI_DEV]))
    }
    fn get(&self) -> *mut [PciDev; NR_PCI_DEV] {
        self.0.get()
    }
}

struct PciBusesCell(UnsafeCell<[PciBus; NR_PCI_BUS]>);
unsafe impl Sync for PciBusesCell {}
impl PciBusesCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([PciBus::new(); NR_PCI_BUS]))
    }
    fn get(&self) -> *mut [PciBus; NR_PCI_BUS] {
        self.0.get()
    }
}

struct PciAclCell(UnsafeCell<[PciAcl; NR_DRIVERS]>);
unsafe impl Sync for PciAclCell {}
impl PciAclCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([PciAcl::new(); NR_DRIVERS]))
    }
    fn get(&self) -> *mut [PciAcl; NR_DRIVERS] {
        self.0.get()
    }
}

/// All detected PCI devices.
static PCI_DEVICES: PciDevicesCell = PciDevicesCell::new();

/// All PCI bus descriptors.
static PCI_BUSES: PciBusesCell = PciBusesCell::new();

/// Number of detected devices.
static NR_PCI_DEVICES: AtomicUsize = AtomicUsize::new(0);

/// Number of detected buses.
static NR_PCI_BUSES: AtomicUsize = AtomicUsize::new(0);

/// PCI ACL table.
static PCI_ACL: PciAclCell = PciAclCell::new();

/// Initialize the PCI subsystem.
///
/// Scans all buses for devices by probing vendor ID at each dev/func.
/// Fills the device table with discovered devices.
///
/// # Safety
///
/// Must be called exactly once during boot.
pub unsafe fn pci_init() {
    unsafe {
        NR_PCI_DEVICES.store(0, Ordering::Relaxed);
        NR_PCI_BUSES.store(0, Ordering::Relaxed);

        // Scan bus 0 devices 0-31, functions 0-7.
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let vendor = crate::arch_io::pci_cfg_read16(0, dev, func, 0x00);
                if pci_device_exists(vendor) {
                    let did = crate::arch_io::pci_cfg_read16(0, dev, func, 0x02);
                    let class_raw = crate::arch_io::pci_cfg_read32(0, dev, func, 0x08);
                    let baseclass = ((class_raw >> 24) & 0xFF) as u8;
                    let subclass = ((class_raw >> 16) & 0xFF) as u8;
                    let infclass = ((class_raw >> 8) & 0xFF) as u8;
                    let _header = crate::arch_io::pci_cfg_read8(0, dev, func, 0x0E);
                    let ilr = crate::arch_io::pci_cfg_read8(0, dev, func, 0x3F);

                    let mut pd = PciDev::new();
                    pd.busnr = 0;
                    pd.dev = dev;
                    pd.func = func;
                    pd.baseclass = baseclass;
                    pd.subclass = subclass;
                    pd.infclass = infclass;
                    pd.vid = vendor;
                    pd.did = did;
                    pd.sub_vid = 0;
                    pd.sub_did = 0;
                    pd.ilr = ilr;

                    pci_add_device(0, dev, func, &mut pd);
                }
                // Single-function device — skip remaining functions.
                if func == 0 {
                    let header = crate::arch_io::pci_cfg_read8(0, dev, 0, 0x0E);
                    if header & 0x80 == 0 {
                        break;
                    }
                }
            }
        }
    }
}

/// Add a discovered device to the table.
unsafe fn pci_add_device(busnr: u8, dev: u8, func: u8, pd: &mut PciDev) {
    unsafe {
        let devs = PCI_DEVICES.get();
        let idx = NR_PCI_DEVICES.load(Ordering::Relaxed);
        if idx >= NR_PCI_DEV {
            return;
        }

        // Read BARs.
        for i in 0..BAR_MAX {
            let offset = 0x10 + (i as u8) * 4;
            let bar_val = crate::arch_io::pci_cfg_read32(busnr, dev, func, offset);
            if bar_val == 0 {
                continue;
            }
            pd.bars[i].base = bar_val;
            pd.bars[i].nr = i as u8;
            pd.bars[i].flags = if bar_val & 1 != 0 { PBF_IO } else { 0 };
            pd.bar_count = (i + 1) as u8;
        }

        (*devs)[idx] = *pd;
        NR_PCI_DEVICES.store(idx + 1, Ordering::Relaxed);
    }
}

/// Find a PCI device by vendor/device ID.
pub fn pci_find_device(vid: u16, did: u16) -> Option<usize> {
    unsafe {
        let devs = PCI_DEVICES.get();
        let n = NR_PCI_DEVICES.load(Ordering::Relaxed);
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            let pd = &(*devs)[i];
            if pd.vid == vid && pd.did == did && pd.flags & PDF_INUSE == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Get a reference to a PCI device by index.
pub fn pci_get_device(index: usize) -> Option<&'static PciDev> {
    unsafe {
        let devs = PCI_DEVICES.get();
        let n = NR_PCI_DEVICES.load(Ordering::Relaxed);
        if index < n {
            Some(&(*devs)[index])
        } else {
            None
        }
    }
}

/// Get the number of discovered PCI devices.
pub fn pci_device_count() -> usize {
    NR_PCI_DEVICES.load(Ordering::Relaxed)
}

/// Grant a driver access to a PCI device.
///
/// # Safety
///
/// Must be called with exclusive access to the ACL table.
pub unsafe fn pci_acl_add(endpoint: i32, vid: u16, did: u16) -> Result<(), DriverError> {
    unsafe {
        let acl = PCI_ACL.get();
        #[allow(clippy::needless_range_loop)]
        for i in 0..NR_DRIVERS {
            let entry = &mut (*acl)[i];
            if entry.endpoint == -1 {
                entry.endpoint = endpoint;
                entry.vid = vid;
                entry.did = did;
                return Ok(());
            }
        }
        Err(DriverError::Busy)
    }
}

/// Check if a driver endpoint has access to a device.
///
/// # Safety
///
/// Must be called with exclusive access to the ACL table.
pub unsafe fn pci_acl_check(endpoint: i32, vid: u16, did: u16) -> bool {
    unsafe {
        let acl = PCI_ACL.get();
        #[allow(clippy::needless_range_loop)]
        for i in 0..NR_DRIVERS {
            let entry = &(*acl)[i];
            if entry.endpoint == endpoint && entry.vid == vid && entry.did == did {
                return true;
            }
        }
        false
    }
}

/// Remove a driver's ACL entries.
///
/// # Safety
///
/// Must be called with exclusive access to the ACL table.
pub unsafe fn pci_acl_remove(endpoint: i32) {
    unsafe {
        let acl = PCI_ACL.get();
        #[allow(clippy::needless_range_loop)]
        for i in 0..NR_DRIVERS {
            let entry = &mut (*acl)[i];
            if entry.endpoint == endpoint {
                *entry = PciAcl::new();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pci_constants() {
        assert_eq!(NR_PCI_DEV, 256);
        assert_eq!(NR_DRIVERS, 32);
        assert_eq!(BAR_MAX, 6);
        assert_eq!(PBF_IO, 0x01);
    }

    #[test]
    fn test_pci_device_new_is_empty() {
        let d = PciDev::new();
        assert_eq!(d.vid, 0);
        assert_eq!(d.did, 0);
        assert_eq!(d.driver_endpt, -1);
        assert_eq!(d.bar_count, 0);
    }

    #[test]
    fn test_pci_acl_add_and_check() {
        unsafe {
            // Reset ACL table
            let acl = PCI_ACL.get();
            #[allow(clippy::needless_range_loop)]
            for i in 0..NR_DRIVERS {
                (*acl)[i] = PciAcl::new();
            }

            assert!(pci_acl_add(100, 0x8086, 0x1234).is_ok());
            assert!(pci_acl_check(100, 0x8086, 0x1234));
            assert!(!pci_acl_check(100, 0x8086, 0x5678));
            assert!(!pci_acl_check(200, 0x8086, 0x1234));
        }
    }

    #[test]
    fn test_pci_acl_remove() {
        unsafe {
            let acl = PCI_ACL.get();
            #[allow(clippy::needless_range_loop)]
            for i in 0..NR_DRIVERS {
                (*acl)[i] = PciAcl::new();
            }

            assert!(pci_acl_add(42, 0x10EC, 0x8168).is_ok());
            assert!(pci_acl_check(42, 0x10EC, 0x8168));
            pci_acl_remove(42);
            assert!(!pci_acl_check(42, 0x10EC, 0x8168));
        }
    }

    #[test]
    fn test_pci_acl_table_full() {
        unsafe {
            let acl = PCI_ACL.get();
            #[allow(clippy::needless_range_loop)]
            for i in 0..NR_DRIVERS {
                (*acl)[i] = PciAcl::new();
            }

            for i in 0..NR_DRIVERS {
                assert!(pci_acl_add(i as i32, 1, 2).is_ok());
            }
            // Next one should fail.
            assert!(pci_acl_add(99, 1, 2).is_err());
        }
    }

    #[test]
    fn test_pci_bus_new() {
        let b = PciBus::new();
        assert!(b.need_init);
        assert_eq!(b.bridge_type, 0);
    }

    #[test]
    fn test_pci_acl_new() {
        let a = PciAcl::new();
        assert_eq!(a.endpoint, -1);
        assert_eq!(a.vid, 0);
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore = "requires hardware PCI access")]
    fn test_pci_init_does_not_panic() {
        unsafe {
            pci_init();
        }
    }
}
