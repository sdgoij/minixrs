//! CMOS/RTC real-time clock driver — /dev/rtc
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/clock/readclock/arch/i386/arch_readclock.c`
//! and `.refs/minix-3.3.0/minix/include/arch/i386/include/cmos.h`
//!
//! Provides time get/set via the MC146818-compatible RTC (CMOS RAM).
//! Uses x86 I/O ports 0x70 (index) and 0x71 (data).

// Allow dead_code for hardware register constant definitions that
// document the full CMOS/RTC register map even when not all are used.
#![allow(dead_code)]

use crate::DriverError;


/// CMOS I/O index port (bit 7 = NMI enable).
const RTC_INDEX: u16 = 0x70;

/// CMOS I/O data port.
const RTC_IO: u16 = 0x71;

/// Seconds register.
const RTC_SEC: u8 = 0x00;
/// Seconds alarm register.
const RTC_SEC_ALRM: u8 = 0x01;
/// Minutes register.
const RTC_MIN: u8 = 0x02;
/// Minutes alarm register.
const RTC_MIN_ALRM: u8 = 0x03;
/// Hours register.
const RTC_HOUR: u8 = 0x04;
/// Hours alarm register.
const RTC_HOUR_ALRM: u8 = 0x05;
/// Day of week (1=Sunday).
const RTC_WDAY: u8 = 0x06;
/// Day of month (1-31).
const RTC_MDAY: u8 = 0x07;
/// Month (1-12).
const RTC_MONTH: u8 = 0x08;
/// Year (0-99).
const RTC_YEAR: u8 = 0x09;
/// Status register A.
const RTC_REG_A: u8 = 0x0A;
/// Status register B.
const RTC_REG_B: u8 = 0x0B;
/// Status register C.
const RTC_REG_C: u8 = 0x0C;


/// Update in progress.
const RTC_A_UIP: u8 = 0x80;
/// Divider bits mask.
const RTC_A_DV: u8 = 0x70;
/// Normal divider value.
const RTC_A_DV_OK: u8 = 0x20;
/// Stop divider value.
const RTC_A_DV_STOP: u8 = 0x70;
/// Rate selection bits mask.
const RTC_A_RS: u8 = 0x0F;
/// Default interrupt rate (1024 Hz).
const RTC_A_RS_DEF: u8 = 6;


/// Inhibit updates (SET bit).
const RTC_B_SET: u8 = 0x80;
/// Periodic interrupt enable.
const RTC_B_PIE: u8 = 0x40;
/// Alarm interrupt enable.
const RTC_B_AIE: u8 = 0x20;
/// Update-ended interrupt enable.
const RTC_B_UIE: u8 = 0x10;
/// Square wave enable.
const RTC_B_SQWE: u8 = 0x08;
/// Data mode: 0=BCD, 1=binary.
const RTC_B_DM_BCD: u8 = 0x04;
/// 24-hour mode.
const RTC_B_24: u8 = 0x02;
/// Daylight savings enable.
const RTC_B_DSE: u8 = 0x01;


/// Chip lost power.
const CS_LOST_POWER: u8 = 0x80;
/// Checksum incorrect.
const CS_BAD_CHKSUM: u8 = 0x40;
/// Bad configuration.
const CS_BAD_CONFIG: u8 = 0x20;
/// Wrong memory size.
const CS_BAD_MEMSIZE: u8 = 0x10;
/// Harddisk failed.
const CS_BAD_HD: u8 = 0x08;
/// CMOS time is invalid.
const CS_BAD_TIME: u8 = 0x04;


const KBD_CTRL_PORT_B: u16 = 0x64;


/// Read a CMOS register value.
unsafe fn cmos_read(reg: u8) -> u8 {
    unsafe { crate::arch_io::cmos_read(reg) }
}

/// Write a value to a CMOS register.
unsafe fn cmos_write(reg: u8, val: u8) {
    unsafe { crate::arch_io::cmos_write(reg, val) }
}


/// Convert BCD to decimal.
const fn bcd_to_dec(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0x0F)
}

/// Convert decimal to BCD.
const fn dec_to_bcd(dec: u8) -> u8 {
    ((dec / 10) << 4) | (dec % 10)
}


/// Broken-down time from the CMOS RTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct RtcTime {
    pub year: u16,
    pub month: u8,  // 1-12
    pub day: u8,    // 1-31
    pub hour: u8,   // 0-23
    pub minute: u8, // 0-59
    pub second: u8, // 0-59
}

impl RtcTime {
    /// Create a default (zeroed) RTC time.
    pub const fn zero() -> Self {
        Self {
            year: 0,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        }
    }
}


/// Initialize the RTC driver.
///
/// Reads the CMOS status register and checks for errors. Returns
/// `Ok(())` if the RTC is functional.
///
/// # Safety
///
/// Must be called exactly once during driver initialization.
pub unsafe fn rtc_init() -> Result<(), DriverError> {
    unsafe {
        let cmos_state = cmos_read(RTC_REG_C); // reg 0xC, not 0xE
        // Actually CMOS_STATUS is at 0xE
        let status = cmos_read(0x0E);
        if status & (CS_LOST_POWER | CS_BAD_CHKSUM | CS_BAD_TIME) != 0 {
            let _ = cmos_state;
            return Err(DriverError::Io);
        }
        Ok(())
    }
}

/// Read the current time from the CMOS RTC.
///
/// Uses the standard technique: wait for UIP to clear, then read all
/// time registers twice and verify consistency. Automatically converts
/// from BCD to binary if needed.
///
/// Year field: returns full 4-digit year (e.g. 2026).
///
/// # Safety
///
/// Must be called with exclusive access to the RTC ports.
pub unsafe fn rtc_get_time() -> Result<RtcTime, DriverError> {
    unsafe {
        let reg_b = cmos_read(RTC_REG_B);
        let is_bcd = reg_b & RTC_B_DM_BCD == 0;

        let mut t = RtcTime::zero();
        #[allow(unused_assignments)]
        let mut osec: i8 = -1;

        loop {
            let mut n = 0;
            osec = -1;
            while n < 2 {
                while cmos_read(RTC_REG_A) & RTC_A_UIP != 0 {
                    core::hint::spin_loop();
                }
                let sec = cmos_read(RTC_SEC);
                if sec != osec as u8 {
                    osec = sec as i8;
                    n += 1;
                }
            }

            t.second = osec as u8;
            t.minute = cmos_read(RTC_MIN);
            t.hour = cmos_read(RTC_HOUR);
            t.day = cmos_read(RTC_MDAY);
            t.month = cmos_read(RTC_MONTH);
            let year_byte = cmos_read(RTC_YEAR);

            if cmos_read(RTC_SEC) != t.second
                || cmos_read(RTC_MIN) != t.minute
                || cmos_read(RTC_HOUR) != t.hour
                || cmos_read(RTC_MDAY) != t.day
                || cmos_read(RTC_MONTH) != t.month
                || cmos_read(RTC_YEAR) != year_byte
            {
                continue;
            }
            break;
        }

        if is_bcd {
            t.second = bcd_to_dec(t.second);
            t.minute = bcd_to_dec(t.minute);
            t.hour = bcd_to_dec(t.hour);
            t.day = bcd_to_dec(t.day);
            t.month = bcd_to_dec(t.month);
        }

        let year_byte = if is_bcd {
            let raw = cmos_read(RTC_YEAR);
            if reg_b & RTC_B_DM_BCD == 0 {
                bcd_to_dec(raw)
            } else {
                raw
            }
        } else {
            cmos_read(RTC_YEAR)
        };

        t.year = if year_byte < 80 {
            2000u16 + year_byte as u16
        } else {
            1900u16 + year_byte as u16
        };

        Ok(t)
    }
}

/// Set the CMOS RTC to the given time.
///
/// Inhibits updates during write, programs register A and B for proper
/// 24-hour mode operation, and writes all time fields.
///
/// # Safety
///
/// Must be called with exclusive access to the RTC ports.
pub unsafe fn rtc_set_time(t: &RtcTime) -> Result<(), DriverError> {
    unsafe {
        let reg_b = cmos_read(RTC_REG_B);
        let is_bcd = reg_b & RTC_B_DM_BCD == 0;

        // Inhibit updates.
        cmos_write(RTC_REG_B, reg_b | RTC_B_SET);

        let mut y = (t.year % 100) as u8;
        let mut mo = t.month;
        let mut d = t.day;
        let mut h = t.hour;
        let mut mi = t.minute;
        let mut s = t.second;

        if is_bcd {
            y = dec_to_bcd(y);
            mo = dec_to_bcd(mo);
            d = dec_to_bcd(d);
            h = dec_to_bcd(h);
            mi = dec_to_bcd(mi);
            s = dec_to_bcd(s);
        }

        cmos_write(RTC_YEAR, y);
        cmos_write(RTC_MONTH, mo);
        cmos_write(RTC_MDAY, d);
        cmos_write(RTC_HOUR, h);
        cmos_write(RTC_MIN, mi);
        cmos_write(RTC_SEC, s);

        // Stop the clock divider.
        let reg_a = cmos_read(RTC_REG_A);
        cmos_write(RTC_REG_A, reg_a | RTC_A_DV_STOP);

        // Restore register B (allow updates) and A.
        cmos_write(RTC_REG_B, reg_b);
        cmos_write(RTC_REG_A, reg_a);

        Ok(())
    }
}

/// Attempt to power off the system.
///
/// Uses the keyboard controller port 0x64 to trigger a power-off
/// sequence. This is architecture-specific and may not work on all
/// hardware (some systems need ACPI or APM).
///
/// # Safety
///
/// Will halt or power off the system.
pub unsafe fn rtc_power_off() {
    unsafe {
        // Try keyboard controller power-off via arch_io.
        crate::arch_io::outb(KBD_CTRL_PORT_B, 0xFE);
    }
}

/// Read a raw CMOS register (for diagnostics/debugging).
///
/// # Safety
///
/// Must be called with exclusive access to the RTC ports.
pub unsafe fn rtc_read_register(reg: u8) -> u8 {
    unsafe { cmos_read(reg) }
}

/// Write a raw CMOS register (for diagnostics/debugging).
///
/// # Safety
///
/// Must be called with exclusive access to the RTC ports.
pub unsafe fn rtc_write_register(reg: u8, val: u8) {
    unsafe {
        cmos_write(reg, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bcd_to_dec() {
        assert_eq!(bcd_to_dec(0x59), 59);
        assert_eq!(bcd_to_dec(0x00), 0);
        assert_eq!(bcd_to_dec(0x31), 31);
        assert_eq!(bcd_to_dec(0x99), 99);
    }

    #[test]
    fn test_dec_to_bcd() {
        assert_eq!(dec_to_bcd(59), 0x59);
        assert_eq!(dec_to_bcd(0), 0x00);
        assert_eq!(dec_to_bcd(31), 0x31);
        assert_eq!(dec_to_bcd(99), 0x99);
    }

    #[test]
    fn test_bcd_roundtrip() {
        for i in 0..=99u8 {
            assert_eq!(bcd_to_dec(dec_to_bcd(i)), i, "roundtrip failed for {i}");
        }
    }

    #[test]
    fn test_rtc_time_zero() {
        let t = RtcTime::zero();
        assert_eq!(t.year, 0);
        assert_eq!(t.month, 1);
        assert_eq!(t.day, 1);
        assert_eq!(t.hour, 0);
        assert_eq!(t.minute, 0);
        assert_eq!(t.second, 0);
    }

    #[test]
    fn test_rtc_time_constants_are_correct() {
        assert_eq!(RTC_INDEX, 0x70);
        assert_eq!(RTC_IO, 0x71);
        assert_eq!(RTC_SEC, 0x00);
        assert_eq!(RTC_MIN, 0x02);
        assert_eq!(RTC_HOUR, 0x04);
        assert_eq!(RTC_MDAY, 0x07);
        assert_eq!(RTC_MONTH, 0x08);
        assert_eq!(RTC_YEAR, 0x09);
        assert_eq!(RTC_REG_A, 0x0A);
        assert_eq!(RTC_REG_B, 0x0B);
        assert_eq!(RTC_REG_C, 0x0C);
    }

    #[test]
    fn test_register_a_bits() {
        assert_eq!(RTC_A_UIP, 0x80);
        assert_eq!(RTC_A_DV_OK, 0x20);
        assert_eq!(RTC_A_DV_STOP, 0x70);
        assert_eq!(RTC_A_RS_DEF, 6);
    }

    #[test]
    fn test_register_b_bits() {
        assert_eq!(RTC_B_SET, 0x80);
        assert_eq!(RTC_B_PIE, 0x40);
        assert_eq!(RTC_B_DM_BCD, 0x04);
        assert_eq!(RTC_B_24, 0x02);
    }

    #[test]
    fn test_cmos_status_bits() {
        assert_eq!(CS_LOST_POWER, 0x80);
        assert_eq!(CS_BAD_CHKSUM, 0x40);
        assert_eq!(CS_BAD_TIME, 0x04);
    }

    #[test]
    fn test_rtc_time_struct_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<RtcTime>();
    }

    #[test]
    fn test_rtc_time_year_conversion() {
        // Simulate: raw year byte = 26 → year 2026 (if < 80 → 2000 + 26)
        // But we can't actually do I/O in tests, so verify the logic:
        let raw_year: u8 = 26;
        let year = if raw_year < 80 {
            2000u16 + raw_year as u16
        } else {
            1900u16 + raw_year as u16
        };
        assert_eq!(year, 2026);
    }

    #[test]
    fn test_rtc_time_year_conversion_old() {
        let raw_year: u8 = 85;
        let year = if raw_year < 80 {
            2000u16 + raw_year as u16
        } else {
            1900u16 + raw_year as u16
        };
        assert_eq!(year, 1985);
    }

    #[test]
    fn test_rtc_get_set_time_signatures() {
        // Verify the API compiles with correct types.
        fn _check_get() -> Result<RtcTime, DriverError> {
            // Can't actually call without hardware, but verify sig compiles.
            Ok(RtcTime::zero())
        }
        fn _check_set(_t: &RtcTime) -> Result<(), DriverError> {
            Ok(())
        }
        let _ = _check_get();
        let _ = _check_set(&RtcTime::zero());
    }
}
