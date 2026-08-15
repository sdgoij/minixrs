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
    ("/bin/id", "id"),
    ("/bin/su", "su"),
    ("/bin/sugid", "sugid"),
    ("/bin/sync", "sync"),
    ("/bin/ping", "ping"),
    ("/bin/udp", "udp"),
    ("/bin/tcp", "tcp"),
    ("/bin/tcpserver", "tcpserver"),
    ("/bin/udp_echo", "udp_echo"),
    // `/bin/keytest` is the PS/2 keyboard event consumer: polls /dev/kbd
    // and prints each decoded HID event (page, code, press).
    ("/bin/keytest", "keytest"),
    // `/bin/coreutils` is the uutils multicall binary, built from the
    // coreutils submodule for x86_64 only so far. `just coreutils-x86` builds
    // the `feat_minix` feature set (56 utilities: text tools, checksums,
    // filesystem basics); the kernel build.rs skips it on other arches.
    ("/bin/coreutils", "coreutils"),
    // `/bin/hello` is the std smoke-test binary: it is NOT a `userland`
    // cargo bin. Rebuild it with `tools/build-std-hello.py [target]`
    // (compiles `tools/std-hello.rs` with the rust fork's stage1 compiler
    // against the fork's std), then re-run `target/mkboot` + `target/mkfs`
    // to embed it.
    ("/bin/hello", "hello"),
    // `/bin/helloc` is the C smoke-test binary: freestanding C linked
    // against `minix-libc`. Rebuild it with `tools/build-c-hello.py`
    // (clang + the fork's stage1 rustc), then re-assemble the images.
    // Built for x86_64 only today (build-c-hello.py is x86-only); the
    // kernel build.rs skips them on other arches.
    ("/bin/helloc", "helloc"),
    // `/bin/ctest` is the second C smoke test: uses the full libc (errno,
    // malloc family, printf/stdio, strings). Same build path as helloc.
    ("/bin/ctest", "ctest"),
    // `/bin/threadtest` is the thread smoke test: spawns kernel threads
    // that share the process address space, do PM IPC, and are joined back.
    ("/bin/threadtest", "threadtest"),
    // `/bin/allocprobe` is the allocator QEMU probe: alloc/free churn
    // through the rt mmap allocator while asserting the VM region count
    // returns to baseline (see tools/alloc_churn_probe.py).
    ("/bin/allocprobe", "allocprobe"),
    // `/bin/mmapfd` is the file-backed mmap test: maps a file with
    // MAP_PRIVATE, verifies the mapping matches the file page-by-page
    // (exercising VM's FDIO demand path), and checks private writes do not
    // modify the file.
    ("/bin/mmapfd", "mmapfd"),
    // `/bin/fbmmap` is the char-device mmap test: maps /dev/fb with
    // MAP_SHARED and draws (K3). `paint`/`restore` subcommands keep
    // fb_screendump.py green after a probe run.
    ("/bin/fbmmap", "fbmmap"),
    ("/bin/kill", "kill"),
    ("/bin/sigtest", "sigtest"),
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
    ("/sbin/virtio_blk", "virtio_blk"),
    ("/sbin/virtio_net", "virtio_net"),
    ("/sbin/net", "net"),
    ("/sbin/devman", "devman"),
    ("/sbin/fb", "fb"),
    ("/sbin/input", "input"),
];

/// Root-filesystem data files baked into the minixfs image: destination
/// path, content, mode, uid, gid (binaries come from [`BOOT_BINS`]).
///
/// The test user's password field is the `$5$` demo scheme
/// (`sha256(salt || password)` hex, see `minix-std/src/passwd.rs`):
/// `sha256("minix" || "test123")`.
pub const BOOT_FILES: &[(&str, &[u8], u16, u16, u16)] = &[
    (
        "/etc/passwd",
        b"root::0:0:root:/:/bin/sh\n\
test:$5$minix$ccc32ce88881621a7b1b6bc05c4880ad85c63114ccda648b1f23b5afe4a21ab5:1000:1000:test user:/:/bin/sh\n",
        0o644,
        0,
        0,
    ),
    // Root-owned 0600 file for the permission-enforcement probe: the
    // `test` user must be denied, root must be allowed.
    ("/etc/secret", b"top secret\n", 0o600, 0, 0),
];

/// Device nodes to create in the initramfs: (path, mode, major, minor).
pub const DEVICES: &[(&str, u32, u32, u32)] = &[
    ("/dev/tty00", 0o020777, 3, 0), // char device, major=3 (pseudo-tty), minor=0
    ("/dev/tty01", 0o020777, 3, 1), // char device, major=3, minor=1
    ("/dev/null", 0o020666, 1, 3),  // char device, major=1 (mem), minor=3
    ("/dev/console", 0o020600, 5, 0), // char device, major=5 (console), minor=0
    ("/dev/ip", 0o020600, 14, 0),   // char device, major=14 (net), minor=0
    ("/dev/udp", 0o020600, 14, 1),  // char device, major=14 (net), minor=1 — UDP socket
    ("/dev/tcp", 0o020600, 14, 2),  // char device, major=14 (net), minor=2 — TCP socket
    ("/dev/fb", 0o020666, 19, 0),   // char device, major=19 (fb), minor=0
    ("/dev/kbd", 0o020600, 20, 0),  // char device, major=20 (input), minor=0
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
