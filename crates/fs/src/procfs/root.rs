//! ProcFS root directory file definitions — adapted from `minix/fs/procfs/root.c`
//!
//! Only the files whose content this port can produce truthfully are listed. C's `root_files`
//! also has `loadavg`, `meminfo`, `dmap`, `ipcvecs` and `mounts`; each needs a source this port
//! does not have yet, and a placeholder line would be a lie a reader cannot tell from data, so
//! they stay out of the tree until their source exists (`KNOWN_ISSUES.md` item 7 records what
//! each one needs).

use crate::procfs::buf::buf_write_fmt;
use crate::procfs::consts::*;
use crate::procfs::types::{File, FileData};

/// Static files in the ProcFS root directory.
///
/// Terminated by an entry with a `None` data field.
pub static ROOT_FILES: &[File] = &[
    File {
        name: "hz",
        mode: REG_ALL_MODE,
        data: FileData::Static(root_hz),
    },
    File {
        name: "uptime",
        mode: REG_ALL_MODE,
        data: FileData::Static(root_uptime),
    },
    // Sentinel
    File {
        name: "",
        mode: 0,
        data: FileData::None,
    },
];

/// `SYS_TIMES`, the kernel call that reports the clock (KERNEL_CALL + 25).
///
/// The same call and the same reply fields ext2's `clock_time` reads
/// (`crates/fs/src/ext2/utility.rs`); `do_times_handler` in the kernel fills them.
#[cfg(target_os = "minix")]
const SYS_TIMES: i32 = 25;
#[cfg(target_os = "minix")]
const TIMES_REPLY_BOOTTICKS_OFF: usize = 8;
#[cfg(target_os = "minix")]
const TIMES_REPLY_BOOTTIME_OFF: usize = 16;
#[cfg(target_os = "minix")]
const TIMES_REPLY_HZ_OFF: usize = 40;

/// The kernel clock, as `(monotonic ticks, ticks per second, boot time)`.
///
/// `None` when the call fails, which is what C's handlers treat as "print nothing".
#[cfg(target_os = "minix")]
fn clock() -> Option<(u64, u64, i64)> {
    let mut msg = [0u8; 64];
    if minix_rt::kernel_call(SYS_TIMES, &mut msg) != OK {
        return None;
    }
    let ticks = u64::from_ne_bytes(
        msg[TIMES_REPLY_BOOTTICKS_OFF..TIMES_REPLY_BOOTTICKS_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let boot_time = i64::from_ne_bytes(
        msg[TIMES_REPLY_BOOTTIME_OFF..TIMES_REPLY_BOOTTIME_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let hz = u64::from_ne_bytes(
        msg[TIMES_REPLY_HZ_OFF..TIMES_REPLY_HZ_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    Some((ticks, hz, boot_time))
}

/// A host build has no kernel clock; nothing there reads `/proc`.
#[cfg(not(target_os = "minix"))]
fn clock() -> Option<(u64, u64, i64)> {
    None
}

/// The uptime as C's `root_uptime` prints it: `100 * ticks / hz`, split into whole seconds and
/// the two decimal digits, so `%ld.%02ld` and this agree.
///
/// Split out from the handler because it is the part worth testing: the division is C's, and a
/// tick rate of zero is a division by zero C would have taken a trap on.
pub fn uptime_hundredths(ticks: u64, hz: u64) -> (u64, u64) {
    if hz == 0 {
        return (0, 0);
    }
    let hundredths = 100 * ticks / hz;
    (hundredths / 100, hundredths % 100)
}

/// Print the system clock frequency (`%lu\n` in C).
fn root_hz() {
    if let Some((_, hz, _)) = clock() {
        buf_write_fmt(format_args!("{hz}\n"));
    }
}

/// Print the current uptime, in seconds with two decimals.
fn root_uptime() {
    if let Some((ticks, hz, _)) = clock() {
        let (secs, hundredths) = uptime_hundredths(ticks, hz);
        buf_write_fmt(format_args!("{secs}.{hundredths:02}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::buf;

    #[test]
    fn uptime_splits_into_seconds_and_hundredths() {
        // 60 Hz: one second is 60 ticks; 3.45 s is 207 ticks.
        assert_eq!(uptime_hundredths(0, 60), (0, 0));
        assert_eq!(uptime_hundredths(60, 60), (1, 0));
        assert_eq!(uptime_hundredths(207, 60), (3, 45));
        // A tick rate that does not divide an hour still floors, as C's integer division does.
        assert_eq!(uptime_hundredths(1000, 60), (16, 66));
    }

    #[test]
    fn uptime_with_a_zero_tick_rate_is_zero_not_a_panic() {
        assert_eq!(uptime_hundredths(1234, 0), (0, 0));
    }

    #[test]
    fn root_files_has_sentinel() {
        let last = ROOT_FILES.last().unwrap();
        assert!(matches!(last.data, crate::procfs::types::FileData::None));
    }

    #[test]
    fn root_files_lists_only_implemented_files() {
        // Every entry but the sentinel must name a file whose content this port can produce; a
        // stub whose output is a constant masquerades as data, so the list stays short on purpose.
        let names: [&str; 2] = ["hz", "uptime"];
        for (i, name) in names.iter().enumerate() {
            assert_eq!(ROOT_FILES[i].name, *name);
        }
        assert_eq!(ROOT_FILES.len(), names.len() + 1);
    }

    #[test]
    fn handlers_print_nothing_without_a_kernel() {
        // The host has no SYS_TIMES, and C's handlers also print nothing when the call fails.
        buf::buf_init(0, 64);
        root_hz();
        root_uptime();
        let (_, len) = buf::buf_get();
        assert_eq!(len, 0);
    }
}
