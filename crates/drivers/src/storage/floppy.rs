//! Floppy disk driver — NEC PD765 FDC.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/floppy/floppy.c`
//!
//! Supports 360K, 720K, 1.2M, and 1.44M floppy diskettes.
//! Uses I/O ports 0x3F2–0x3F7 and legacy DMA channel 2.

// ── I/O ports ──────────────────────────────────────────────────────────────

/// Digital output register (motor/drive control).
pub const DOR: u16 = 0x3F2;
/// FDC main status register.
pub const FDC_STATUS: u16 = 0x3F4;
/// FDC data register.
pub const FDC_DATA: u16 = 0x3F5;
/// Transfer rate register.
pub const FDC_RATE: u16 = 0x3F7;

// ── FDC status register bits ───────────────────────────────────────────────

pub const FDC_BUSY: u8 = 0x10;
pub const FDC_DIR: u8 = 0x40; // 1 = controller ready to send data
pub const FDC_MASTER: u8 = 0x80; // Data register accessible

// ── DOR bits ───────────────────────────────────────────────────────────────

pub const DOR_MOTOR_SHIFT: u8 = 4;
pub const DOR_ENABLE_INT: u8 = 0x0C;
pub const DOR_RESET: u8 = 0x00;

// ── FDC commands ───────────────────────────────────────────────────────────

pub const FDC_SEEK: u8 = 0x0F;
pub const FDC_READ: u8 = 0xE6;
pub const FDC_WRITE: u8 = 0xC5;
pub const FDC_SENSE: u8 = 0x08;
pub const FDC_RECALIBRATE: u8 = 0x07;
pub const FDC_SPECIFY: u8 = 0x03;
pub const FDC_READ_ID: u8 = 0x4A;
pub const FDC_FORMAT: u8 = 0x4D;

// ── Status registers returned by controller ────────────────────────────────

pub const ST0: usize = 0;
pub const ST1: usize = 1;
pub const ST2: usize = 2;
pub const ST_CYL: usize = 3;
pub const ST_HEAD: usize = 4;
pub const ST_SEC: usize = 5;

// ── ST0 bits ───────────────────────────────────────────────────────────────

pub const ST0_BITS_TRANS: u8 = 0xD8;
pub const TRANS_ST0: u8 = 0x00;
pub const ST0_BITS_SEEK: u8 = 0xF8;
pub const SEEK_ST0: u8 = 0x20;

// ── ST1 bits ───────────────────────────────────────────────────────────────

pub const ST1_BAD_SECTOR: u8 = 0x05;
pub const ST1_WRITE_PROTECT: u8 = 0x02;

// ── ST2 bits ───────────────────────────────────────────────────────────────

pub const ST2_BAD_CYL: u8 = 0x1F;

// ── DMA ports ──────────────────────────────────────────────────────────────

pub const DMA_ADDR: u16 = 0x004;
pub const DMA_TOP: u16 = 0x081;
pub const DMA_COUNT: u16 = 0x005;
pub const DMA_FLIPFLOP: u16 = 0x00C;
pub const DMA_MODE: u16 = 0x00B;
pub const DMA_INIT: u16 = 0x00A;

pub const DMA_READ: u8 = 0x46;
pub const DMA_WRITE: u8 = 0x4A;

// ── Drive constants ────────────────────────────────────────────────────────

pub const NR_DRIVES: usize = 2;
pub const NR_HEADS: u8 = 2;
pub const MAX_SECTORS: u8 = 18;
pub const SECTOR_SIZE: usize = 512;
pub const SECTOR_SIZE_CODE: u8 = 2;
pub const DTL: u8 = 0xFF;
pub const BASE_SECTOR: u8 = 1;
pub const HC_SIZE: usize = 2880;

// ── Drive states ──────────────────────────────────────────────────────────

pub const UNCALIBRATED: u8 = 0;
pub const CALIBRATED: u8 = 1;
pub const NO_SECTOR: u16 = 0xFFFF;
pub const NO_CYL: i16 = -1;
pub const NO_DENS: u8 = 100;
pub const BSY_IDLE: u8 = 0;
pub const BSY_IO: u8 = 1;
pub const BSY_WAKEN: u8 = 2;

// ── Error codes ────────────────────────────────────────────────────────────

pub const ERR_SEEK: i32 = -1;
pub const ERR_TRANSFER: i32 = -2;
pub const ERR_STATUS: i32 = -3;
pub const ERR_READ_ID: i32 = -4;
pub const ERR_RECALIBRATE: i32 = -5;
pub const ERR_DRIVE: i32 = -6;
pub const ERR_WR_PROTECT: i32 = -7;
pub const ERR_TIMEOUT: i32 = -8;

// ── Density table ──────────────────────────────────────────────────────────

/// Density parameter entry for a floppy diskette/drive combination.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Density {
    pub secpt: u8,    // sectors per track
    pub cyls: u8,     // tracks per side
    pub steps: u8,    // steps per cylinder (2 = double step)
    pub test_sec: u8, // sector to try for density test
    pub rate: u8,     // data rate (2=250k, 1=300k, 0=500kbps)
    pub gap: u8,      // gap size
    pub spec1: u8,    // first SPECIFY byte (SRT/HUT)
}

/// Number of density entries.
pub const NT: usize = 7;

/// Seven density table entries (360K through 1.44M).
pub const FDENSITY: [Density; NT] = [
    Density {
        secpt: 9,
        cyls: 40,
        steps: 1,
        test_sec: 4 * 9,
        rate: 2,
        gap: 0x2A,
        spec1: 0xDF,
    },
    Density {
        secpt: 15,
        cyls: 80,
        steps: 1,
        test_sec: 14,
        rate: 0,
        gap: 0x1B,
        spec1: 0xDF,
    },
    Density {
        secpt: 9,
        cyls: 40,
        steps: 2,
        test_sec: 2 * 9,
        rate: 2,
        gap: 0x2A,
        spec1: 0xDF,
    },
    Density {
        secpt: 9,
        cyls: 80,
        steps: 1,
        test_sec: 4 * 9,
        rate: 2,
        gap: 0x2A,
        spec1: 0xDF,
    },
    Density {
        secpt: 9,
        cyls: 40,
        steps: 2,
        test_sec: 2 * 9,
        rate: 1,
        gap: 0x23,
        spec1: 0xDF,
    },
    Density {
        secpt: 9,
        cyls: 80,
        steps: 1,
        test_sec: 4 * 9,
        rate: 1,
        gap: 0x23,
        spec1: 0xDF,
    },
    Density {
        secpt: 18,
        cyls: 80,
        steps: 1,
        test_sec: 17,
        rate: 0,
        gap: 0x1B,
        spec1: 0xCF,
    },
];

/// Test order for density detection.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct TestOrder {
    pub density: u8,
    pub class_mask: u8,
}

pub const TEST_ORDER: [TestOrder; NT - 1] = [
    TestOrder {
        density: 6,
        class_mask: (1 << 3) | (1 << 6),
    }, // 1.44M
    TestOrder {
        density: 1,
        class_mask: (1 << 1) | (1 << 4) | (1 << 5),
    }, // 1.2M
    TestOrder {
        density: 3,
        class_mask: (1 << 2) | (1 << 3) | (1 << 6),
    }, // 720K
    TestOrder {
        density: 4,
        class_mask: (1 << 1) | (1 << 4) | (1 << 5),
    }, // 360K
    TestOrder {
        density: 5,
        class_mask: (1 << 1) | (1 << 4) | (1 << 5),
    }, // 720K
    TestOrder {
        density: 2,
        class_mask: (1 << 2) | (1 << 3),
    }, // 360K
];

// ── Drive state ────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FloppyDrive {
    pub calibrated: u8,
    pub density: u8,
    pub current_cyl: i16,
    pub current_sec: u16,
    pub motor_on: bool,
    pub busy: u8,
}

impl FloppyDrive {
    pub const fn new() -> Self {
        Self {
            calibrated: UNCALIBRATED,
            density: NO_DENS,
            current_cyl: NO_CYL,
            current_sec: NO_SECTOR,
            motor_on: false,
            busy: BSY_IDLE,
        }
    }
}

impl Default for FloppyDrive {
    fn default() -> Self {
        Self::new()
    }
}

/// Global floppy drive state.
static mut FLOPPY_DRIVES: [FloppyDrive; NR_DRIVES] = [FloppyDrive::new(); NR_DRIVES];

// ── Public API ─────────────────────────────────────────────────────────────

/// Initialize a floppy drive.
pub fn floppy_init_drive(drive: usize) {
    unsafe {
        if drive < NR_DRIVES {
            FLOPPY_DRIVES[drive] = FloppyDrive::new();
        }
    }
}

/// Get drive state.
pub fn floppy_drive(drive: usize) -> Option<&'static FloppyDrive> {
    unsafe {
        if drive < NR_DRIVES {
            Some(&FLOPPY_DRIVES[drive])
        } else {
            None
        }
    }
}

/// Get a mutable pointer to a drive.
pub fn floppy_drive_mut(drive: usize) -> *mut FloppyDrive {
    unsafe {
        if drive < NR_DRIVES {
            core::ptr::addr_of_mut!(FLOPPY_DRIVES[drive])
        } else {
            core::ptr::null_mut()
        }
    }
}

/// Get the density entry for a given index.
pub fn floppy_density(index: usize) -> Option<&'static Density> {
    if index < NT {
        Some(&FDENSITY[index])
    } else {
        None
    }
}

/// Compute number of sectors for a density entry.
pub fn floppy_density_sectors(d: &Density) -> usize {
    d.secpt as usize * d.cyls as usize * NR_HEADS as usize
}

/// Check if a density is valid for the given class mask.
pub fn floppy_density_class(density: u8, class_mask: u8) -> bool {
    if (density as usize) < NT {
        (class_mask & (1 << density)) != 0
    } else {
        false
    }
}

/// Convert a minor device number to a drive index.
pub fn floppy_minor_to_drive(minor: usize) -> Option<usize> {
    let drive = (minor >> 2) & 0x03;
    if drive < NR_DRIVES { Some(drive) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_floppy_constants() {
        assert_eq!(DOR, 0x3F2);
        assert_eq!(FDC_STATUS, 0x3F4);
        assert_eq!(FDC_DATA, 0x3F5);
        assert_eq!(FDC_RATE, 0x3F7);
    }

    #[test]
    fn test_fdc_commands() {
        assert_eq!(FDC_SEEK, 0x0F);
        assert_eq!(FDC_READ, 0xE6);
        assert_eq!(FDC_WRITE, 0xC5);
        assert_eq!(FDC_SENSE, 0x08);
        assert_eq!(FDC_RECALIBRATE, 0x07);
        assert_eq!(FDC_SPECIFY, 0x03);
    }

    #[test]
    fn test_drive_constants() {
        assert_eq!(NR_DRIVES, 2);
        assert_eq!(NR_HEADS, 2);
        assert_eq!(MAX_SECTORS, 18);
        assert_eq!(SECTOR_SIZE, 512);
        assert_eq!(HC_SIZE, 2880);
    }

    #[test]
    fn test_density_table_size() {
        assert_eq!(NT, 7);
        assert_eq!(FDENSITY.len(), 7);
    }

    #[test]
    fn test_density_144() {
        let d = &FDENSITY[6];
        assert_eq!(d.secpt, 18);
        assert_eq!(d.cyls, 80);
        assert_eq!(d.rate, 0);
        assert_eq!(d.gap, 0x1B);
    }

    #[test]
    fn test_density_12() {
        let d = &FDENSITY[1];
        assert_eq!(d.secpt, 15);
        assert_eq!(d.cyls, 80);
        assert_eq!(d.rate, 0);
    }

    #[test]
    fn test_density_720() {
        let d = &FDENSITY[3];
        assert_eq!(d.secpt, 9);
        assert_eq!(d.cyls, 80);
        assert_eq!(d.rate, 2);
    }

    #[test]
    fn test_density_360() {
        let d = &FDENSITY[0];
        assert_eq!(d.secpt, 9);
        assert_eq!(d.cyls, 40);
        assert_eq!(d.rate, 2);
    }

    #[test]
    fn test_floppy_drive_new() {
        let f = FloppyDrive::new();
        assert_eq!(f.calibrated, UNCALIBRATED);
        assert_eq!(f.density, NO_DENS);
        assert_eq!(f.current_cyl, NO_CYL);
        assert_eq!(f.current_sec, NO_SECTOR);
        assert!(!f.motor_on);
    }

    #[test]
    fn test_floppy_drive_default() {
        let f: FloppyDrive = Default::default();
        assert_eq!(f.calibrated, 0);
    }

    #[test]
    fn test_floppy_minor_to_drive() {
        assert_eq!(floppy_minor_to_drive(0), Some(0));
        assert_eq!(floppy_minor_to_drive(1), Some(0));
        assert_eq!(floppy_minor_to_drive(4), Some(1));
        assert_eq!(floppy_minor_to_drive(8), None);
    }

    #[test]
    fn test_floppy_density_sectors() {
        let d144 = &FDENSITY[6];
        assert_eq!(floppy_density_sectors(d144), 18 * 80 * 2);

        let d12 = &FDENSITY[1];
        assert_eq!(floppy_density_sectors(d12), 15 * 80 * 2);

        let d360 = &FDENSITY[0];
        assert_eq!(floppy_density_sectors(d360), 9 * 40 * 2);
    }

    #[test]
    fn test_density_class() {
        assert!(floppy_density_class(6, 1 << 6));
        assert!(!floppy_density_class(6, 1 << 3));
        assert!(floppy_density_class(3, (1 << 3) | (1 << 6)));
    }

    #[test]
    fn test_test_order() {
        assert_eq!(TEST_ORDER[0].density, 6);
        assert_eq!(TEST_ORDER[0].class_mask, (1 << 3) | (1 << 6));
        assert_eq!(TEST_ORDER.len(), NT - 1);
    }

    #[test]
    fn test_floppy_init() {
        floppy_init_drive(0);
        let f = floppy_drive(0).unwrap();
        assert_eq!(f.density, NO_DENS);
    }

    #[test]
    fn test_floppy_drive_mut() {
        unsafe {
            let ptr = floppy_drive_mut(0);
            assert!(!ptr.is_null());
            (*ptr).density = 6;
            assert_eq!((*ptr).density, 6);

            let null_ptr = floppy_drive_mut(99);
            assert!(null_ptr.is_null());
        }
    }

    #[test]
    fn test_floppy_density_out_of_range() {
        assert!(floppy_density(99).is_none());
    }

    #[test]
    fn test_status_bits() {
        assert_eq!(FDC_BUSY, 0x10);
        assert_eq!(FDC_DIR, 0x40);
        assert_eq!(FDC_MASTER, 0x80);
    }

    #[test]
    fn test_st0_bits() {
        assert_eq!(ST0_BITS_TRANS, 0xD8);
        assert_eq!(TRANS_ST0, 0x00);
        assert_eq!(SEEK_ST0, 0x20);
    }
}
