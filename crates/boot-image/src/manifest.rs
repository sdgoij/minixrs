//! Boot-critical binary and device manifest.
//!
//! Single source of truth for which userland binaries go into both the
//! initramfs CPIO archive and the MinixFS root image, plus the device
//! nodes the initramfs must create.

/// Boot-critical binaries: destination path in the images → binary name
/// (the Cargo `[[bin]]` target name in the `userland`/`servers` crates).
pub const BOOT_BINS: &[(&str, &str)] = &[
    ("/sbin/init", "init"),
    ("/bin/sh", "sh"),
    ("/bin/cat", "cat"),
    ("/bin/echo", "echo"),
    ("/bin/ls", "ls"),
    ("/bin/mkdir", "mkdir"),
    ("/bin/rm", "rm"),
    ("/bin/cp", "cp"),
    ("/bin/ln", "ln"),
    ("/bin/chmod", "chmod"),
    ("/bin/sync", "sync"),
    ("/sbin/mknod", "mknod"),
    ("/sbin/reboot", "reboot"),
    ("/sbin/fsck", "fsck"),
    ("/sbin/pm", "pm"),
    ("/sbin/vfs", "vfs"),
    ("/sbin/vm", "vm"),
    ("/sbin/rs", "rs"),
    ("/sbin/ds", "ds"),
    ("/sbin/sched", "sched"),
    ("/sbin/tty", "tty"),
    ("/sbin/mfs", "mfs"),
    ("/sbin/pfs", "pfs"),
    ("/sbin/ramdisk", "ramdisk"),
];

/// Device nodes to create in the initramfs: (path, mode, major, minor).
pub const DEVICES: &[(&str, u32, u32, u32)] = &[
    ("/dev/tty00", 0o020777, 3, 0), // char device, major=3 (pseudo-tty), minor=0
    ("/dev/tty01", 0o020777, 3, 1), // char device, major=3, minor=1
    ("/dev/null", 0o020666, 1, 3),  // char device, major=1 (mem), minor=3
    ("/dev/console", 0o020600, 5, 0), // char device, major=5 (console), minor=0
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_bins_have_valid_paths() {
        for &(dest, bin) in BOOT_BINS {
            assert!(dest.starts_with('/'), "{dest} must be absolute");
            assert!(!bin.is_empty(), "binary name for {dest} must not be empty");
        }
    }

    #[test]
    fn boot_bins_are_unique() {
        let mut paths: Vec<&str> = BOOT_BINS.iter().map(|(d, _)| *d).collect();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), BOOT_BINS.len(), "duplicate destination paths");
    }
}
