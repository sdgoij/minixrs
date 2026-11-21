//! ProcFS miscellaneous utilities — adapted from `minix/fs/procfs/util.c`

use crate::procfs::types::Load;

/// `_LOAD_UNIT_SECS`: the seconds one history slot covers.
const LOAD_UNIT_SECS: u32 = 6;
/// `_LOAD_HISTORY`: fifteen minutes of slots.
const LOAD_HISTORY: usize = (15 * 60 / LOAD_UNIT_SECS) as usize;

/// `struct loadinfo`, the kernel's load-average history
/// (`crates/arch-common/src/types.rs`'s `LoadInfo`).
///
/// Copied out of the kernel as bytes, so the field order matters and the
/// padding has to be spelled out: `proc_load_history` is 150 `u16`s, then the
/// two slot fields, then an 8-aligned clock.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LoadInfo {
    pub proc_load_history: [u16; LOAD_HISTORY],
    pub proc_last_slot: u16,
    pad: u16,
    pub last_clock: i64,
}

/// `SYS_GETINFO` (kernel call 26) and its `GET_LOADINFO` request, with the
/// message's fields at C's union offsets plus the 8-byte header. Only the
/// kernel-side arm reads them, so a host build has no use for them.
#[cfg(target_os = "minix")]
const SYS_GETINFO: i32 = 26;
#[cfg(target_os = "minix")]
const GET_LOADINFO: i32 = 15;
#[cfg(target_os = "minix")]
const GETINFO_REQUEST_OFF: usize = 8;
#[cfg(target_os = "minix")]
const GETINFO_VAL_PTR_OFF: usize = 16;
#[cfg(target_os = "minix")]
const GETINFO_VAL_LEN_OFF: usize = 24;

/// The kernel's load history, or `None` when the call fails.
#[cfg(target_os = "minix")]
fn loadinfo() -> Option<LoadInfo> {
    let mut info = LoadInfo {
        proc_load_history: [0; LOAD_HISTORY],
        proc_last_slot: 0,
        pad: 0,
        last_clock: 0,
    };
    let size = core::mem::size_of::<LoadInfo>();
    let mut msg = [0u8; 64];
    msg[GETINFO_REQUEST_OFF..GETINFO_REQUEST_OFF + 4].copy_from_slice(&GET_LOADINFO.to_ne_bytes());
    msg[GETINFO_VAL_PTR_OFF..GETINFO_VAL_PTR_OFF + 8]
        .copy_from_slice(&((core::ptr::addr_of_mut!(info)) as u64).to_ne_bytes());
    msg[GETINFO_VAL_LEN_OFF..GETINFO_VAL_LEN_OFF + 4].copy_from_slice(&(size as i32).to_ne_bytes());
    if minix_rt::kernel_call(SYS_GETINFO, &mut msg) != 0 {
        return None;
    }
    Some(info)
}

/// A host build has no kernel to ask.
#[cfg(not(target_os = "minix"))]
fn loadinfo() -> Option<LoadInfo> {
    None
}

/// Retrieve system load average information.
///
/// Fills all three entries of `loads` with the process load and the number of
/// ticks the sample covers, and returns 3; 0 when the kernel did not answer.
///
/// `system_hz` is the caller's: C's version calls `sys_hz()` here, and the
/// procfs handlers already read it for `/proc/hz`, so this takes it as a
/// parameter instead of asking the kernel twice.
pub fn procfs_getloadavg(loads: &mut [Load; 3], system_hz: u32) -> i32 {
    let info = match loadinfo() {
        Some(info) => info,
        None => return 0,
    };
    let minutes = [1u32, 5, 15];

    // How many ticks are missing from the newest-filled slot: it has not been
    // around for a whole slot yet, so it must not count its full number.
    let ticks_per_slot = (LOAD_UNIT_SECS as u64) * (system_hz as u64);
    let unfilled_ticks = if ticks_per_slot == 0 {
        0
    } else {
        ticks_per_slot - (info.last_clock as u64 % ticks_per_slot)
    };

    for (p, minutes) in minutes.iter().enumerate() {
        let slots = (*minutes * 60 / LOAD_UNIT_SECS) as usize;
        let mut proc_load: i64 = 0;
        for h in 0..slots {
            let slot = (info.proc_last_slot as usize + LOAD_HISTORY - h) % LOAD_HISTORY;
            proc_load += i64::from(info.proc_load_history[slot]);
        }
        let covered = (slots as u64) * ticks_per_slot;
        loads[p] = Load {
            proc_load,
            ticks: covered.saturating_sub(unfilled_ticks),
        };
    }
    3
}

/// One load figure as C's `%ld.%02ld` prints it: whole and hundredths.
///
/// C divides `100 * proc_load / ticks` as a `long` and splits it with `ldiv`,
/// so the hundredths truncate rather than round. A sample of no ticks has no
/// average; C would have divided by zero, which is not a value to reproduce.
pub fn load_hundredths(load: &Load) -> (i64, i64) {
    if load.ticks == 0 {
        return (0, 0);
    }
    let scaled = 100 * load.proc_load / load.ticks as i64;
    (scaled / 100, scaled % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_load_of_one_for_the_whole_window_is_100() {
        // 6 s slots over 1 minute: 10 slots of 60 ticks each at 10 Hz.
        let load = Load {
            proc_load: 600,
            ticks: 600,
        };
        assert_eq!(load_hundredths(&load), (1, 0));
    }

    #[test]
    fn hundredths_truncate_like_c() {
        // 1 of 3 ticks is 33.33%, and `100 * 1 / 3` floors.
        let load = Load {
            proc_load: 1,
            ticks: 3,
        };
        assert_eq!(load_hundredths(&load), (0, 33));
    }

    #[test]
    fn an_empty_sample_is_zero_rather_than_a_division_by_zero() {
        let load = Load {
            proc_load: 7,
            ticks: 0,
        };
        assert_eq!(load_hundredths(&load), (0, 0));
    }

    /// The kernel's `LoadInfo` is copied byte for byte, so its size is the
    /// contract: 150 slots, three `u16`s, one `i64`.
    #[test]
    fn the_kernel_struct_is_the_size_the_copy_assumes() {
        assert_eq!(LOAD_HISTORY, 150);
        assert_eq!(core::mem::size_of::<LoadInfo>(), 150 * 2 + 4 + 8);
    }
}
