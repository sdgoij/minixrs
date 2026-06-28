//! ProcFS root directory file definitions — adapted from `minix/fs/procfs/root.c`

use crate::procfs::buf::buf_write;
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
    File {
        name: "loadavg",
        mode: REG_ALL_MODE,
        data: FileData::Static(root_loadavg),
    },
    File {
        name: "kinfo",
        mode: REG_ALL_MODE,
        data: FileData::Static(root_kinfo),
    },
    File {
        name: "meminfo",
        mode: REG_ALL_MODE,
        data: FileData::Static(root_meminfo),
    },
    File {
        name: "dmap",
        mode: REG_ALL_MODE,
        data: FileData::Static(root_dmap),
    },
    File {
        name: "cpuinfo",
        mode: REG_ALL_MODE,
        data: FileData::Static(crate::procfs::cpuinfo::root_cpuinfo),
    },
    // Sentinel
    File {
        name: "",
        mode: 0,
        data: FileData::None,
    },
];

/// Print the system clock frequency.
///
/// TODO: replace with `sys_hz()` call.
fn root_hz() {
    buf_write("HZ\n");
}

/// Print the current uptime.
///
/// TODO: call `getticks(&ticks)` and `sys_hz()` to compute real uptime.
fn root_uptime() {
    buf_write("0\n");
}

/// Print load averages.
///
/// TODO: use `procfs_getloadavg()` to obtain real load values.
fn root_loadavg() {
    buf_write("0.00 0.00 0.00\n");
}

/// Print general kernel information.
///
/// TODO: call `sys_getkinfo(&kinfo)` to get `nr_procs` and `nr_tasks`.
fn root_kinfo() {
    buf_write("0 0\n");
}

/// Print general memory information.
///
/// TODO: call `vm_info_stats(&vsi)` to get real values.
fn root_meminfo() {
    buf_write("0 0 0 0 0\n");
}

/// Print device mapping information.
///
/// TODO: call `getsysinfo(VFS_PROC_NR, SI_DMAP_TAB, ...)`.
fn root_dmap() {
    // Nothing written (stub).
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::buf;

    #[test]
    fn root_hz_does_not_panic() {
        buf::buf_init(0, 64);
        root_hz();
        let (_, len) = buf::buf_get();
        assert!(len > 0);
    }

    #[test]
    fn root_loadavg_does_not_panic() {
        buf::buf_init(0, 64);
        root_loadavg();
        let (_, len) = buf::buf_get();
        assert!(len > 0);
    }

    #[test]
    fn root_files_has_sentinel() {
        let last = ROOT_FILES.last().unwrap();
        assert!(matches!(last.data, crate::procfs::types::FileData::None));
    }
}
