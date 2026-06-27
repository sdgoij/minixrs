//! Device major/minor numbers from `minix/dmap.h`

// ── Major device numbers ────────────────────────────────────────────────

pub const NONE_MAJOR: u32 = 0;
pub const MEMORY_MAJOR: u32 = 1;
pub const FLOPPY_MAJOR: u32 = 2;
pub const TTY_MAJOR: u32 = 4;
pub const CTTY_MAJOR: u32 = 5;
pub const PRINTER_MAJOR: u32 = 6;
pub const INET_MAJOR: u32 = 7;
pub const PTY_MAJOR: u32 = 9;
pub const FILTER_MAJOR: u32 = 11;
pub const AUDIO_MAJOR: u32 = 13;
pub const FBD_MAJOR: u32 = 14;
pub const LOG_MAJOR: u32 = 15;
pub const RANDOM_MAJOR: u32 = 16;
pub const HELLO_MAJOR: u32 = 17;
pub const UDS_MAJOR: u32 = 18;
pub const FB_MAJOR: u32 = 19;
pub const I2C0_MAJOR: u32 = 20;
pub const I2C1_MAJOR: u32 = 21;
pub const I2C2_MAJOR: u32 = 22;

// EEPROM major numbers (cat24c256)
pub const EEPROMB1S50_MAJOR: u32 = 23;
pub const EEPROMB1S51_MAJOR: u32 = 24;
pub const EEPROMB1S52_MAJOR: u32 = 25;
pub const EEPROMB1S53_MAJOR: u32 = 26;
pub const EEPROMB1S54_MAJOR: u32 = 27;
pub const EEPROMB1S55_MAJOR: u32 = 28;
pub const EEPROMB1S56_MAJOR: u32 = 29;
pub const EEPROMB1S57_MAJOR: u32 = 30;
pub const EEPROMB2S50_MAJOR: u32 = 31;
pub const EEPROMB2S51_MAJOR: u32 = 32;
pub const EEPROMB2S52_MAJOR: u32 = 33;
pub const EEPROMB2S53_MAJOR: u32 = 34;
pub const EEPROMB2S54_MAJOR: u32 = 35;
pub const EEPROMB2S55_MAJOR: u32 = 36;
pub const EEPROMB2S56_MAJOR: u32 = 37;
pub const EEPROMB2S57_MAJOR: u32 = 38;
pub const EEPROMB3S50_MAJOR: u32 = 39;
pub const EEPROMB3S51_MAJOR: u32 = 40;
pub const EEPROMB3S52_MAJOR: u32 = 41;
pub const EEPROMB3S53_MAJOR: u32 = 42;
pub const EEPROMB3S54_MAJOR: u32 = 43;
pub const EEPROMB3S55_MAJOR: u32 = 44;
pub const EEPROMB3S56_MAJOR: u32 = 45;
pub const EEPROMB3S57_MAJOR: u32 = 46;

// Sensor majors
pub const TSL2550B1S39_MAJOR: u32 = 47;
pub const TSL2550B2S39_MAJOR: u32 = 48;
pub const TSL2550B3S39_MAJOR: u32 = 49;
pub const SHT21B1S40_MAJOR: u32 = 50;
pub const SHT21B2S40_MAJOR: u32 = 51;
pub const SHT21B3S40_MAJOR: u32 = 52;
pub const BMP085B1S77_MAJOR: u32 = 53;
pub const BMP085B2S77_MAJOR: u32 = 54;
pub const BMP085B3S77_MAJOR: u32 = 55;

pub const INPUT_MAJOR: u32 = 64;
pub const USB_BASE_MAJOR: u32 = 65;

/// Number of (major) devices.
pub const NR_DEVICES: u32 = 134;

// ── Memory driver minor numbers ─────────────────────────────────────────

pub const RAM_DEV_OLD: u32 = 0;
pub const MEM_DEV: u32 = 1;
pub const KMEM_DEV: u32 = 2;
pub const NULL_DEV: u32 = 3;
pub const BOOT_DEV: u32 = 4;
pub const ZERO_DEV: u32 = 5;
pub const IMGRD_DEV: u32 = 6;
pub const RAM_DEV_FIRST: u32 = 7;

// ── Log driver minor numbers ────────────────────────────────────────────

pub const IS_KLOG_DEV: u32 = 0;

// ── Full device numbers ─────────────────────────────────────────────────

/// Device number of /dev/ram.
pub const DEV_RAM: u32 = 0x0100;

/// Device number of /dev/imgrd.
pub const DEV_IMGRD: u32 = 0x0106;

// ── Controller-to-IRQ mapping ───────────────────────────────────────────

/// Magic formula mapping controller index to IRQ number.
pub const fn ctrlr(n: u32) -> u32 {
    if n == 0 { 3 } else { 8 + 2 * (n - 1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_major_numbers() {
        assert_eq!(NONE_MAJOR, 0);
        assert_eq!(MEMORY_MAJOR, 1);
        assert_eq!(TTY_MAJOR, 4);
        assert_eq!(PTY_MAJOR, 9);
        assert_eq!(INPUT_MAJOR, 64);
        assert_eq!(USB_BASE_MAJOR, 65);
        assert_eq!(NR_DEVICES, 134);
    }

    #[test]
    fn test_memory_minors() {
        assert_eq!(RAM_DEV_OLD, 0);
        assert_eq!(MEM_DEV, 1);
        assert_eq!(KMEM_DEV, 2);
        assert_eq!(NULL_DEV, 3);
        assert_eq!(BOOT_DEV, 4);
        assert_eq!(ZERO_DEV, 5);
        assert_eq!(IMGRD_DEV, 6);
        assert_eq!(RAM_DEV_FIRST, 7);
    }

    #[test]
    fn test_ctrlr() {
        assert_eq!(ctrlr(0), 3);
        assert_eq!(ctrlr(1), 8);
        assert_eq!(ctrlr(2), 10);
        assert_eq!(ctrlr(3), 12);
        assert_eq!(ctrlr(4), 14);
    }

    #[test]
    fn test_full_device_numbers() {
        assert_eq!(DEV_RAM, 0x0100);
        assert_eq!(DEV_IMGRD, 0x0106);
    }
}
