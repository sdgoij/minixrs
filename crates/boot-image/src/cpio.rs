//! CPIO newc archive writer for the initramfs.
//!
//! Ported from `tools/mkinitramfs.rs` into a pure, testable library
//! function so both the kernel `build.rs` and the `mkinitramfs` CLI can
//! share it.

use std::io::Write;

/// A single CPIO newc entry.
///
/// `mode` carries both the file-type bits and permissions, matching the
/// original tool's convention: 0o040755 (dir), 0o100755 (file),
/// 0o020777 (char device).
pub struct Entry {
    pub name: &'static str,
    pub mode: u32,
    pub data: Vec<u8>,
}

/// Build the boot initramfs: the base directories, the boot processes, and the trailer.
///
/// `bins` maps destination path → file content (already read from the
/// per-target release directory). Only the paths in [`manifest::INITRAMFS_BINS`] go in: the
/// kernel looks a boot process up here by path before any filesystem is mounted, and nothing
/// else reads the archive, so everything else the image ships belongs on the root filesystem.
///
pub fn standard_initramfs(bins: &[(&'static str, Vec<u8>)]) -> Vec<u8> {
    let mut entries = vec![
        Entry {
            name: "/",
            mode: 0o040755,
            data: Vec::new(),
        },
        Entry {
            name: "/bin",
            mode: 0o040755,
            data: Vec::new(),
        },
        Entry {
            name: "/sbin",
            mode: 0o040755,
            data: Vec::new(),
        },
        // `/lib` holds shared objects and `/libexec` the dynamic loader, for a boot process
        // that ever names a `PT_INTERP`: the loader has to be reachable before the root
        // filesystem is mounted. No boot process is dynamic today, so these are empty.
        Entry {
            name: "/lib",
            mode: 0o040755,
            data: Vec::new(),
        },
        Entry {
            name: "/libexec",
            mode: 0o040755,
            data: Vec::new(),
        },
    ];
    for (path, data) in bins {
        if !carries(path) {
            continue;
        }
        entries.push(Entry {
            name: path,
            mode: 0o100755,
            data: data.clone(),
        });
    }
    build_cpio(&entries)
}

/// Whether the initramfs carries `path`: it carries the boot processes and nothing else.
pub fn carries(path: &str) -> bool {
    crate::manifest::INITRAMFS_BINS.contains(&path)
}

/// Serialize entries as a CPIO newc archive.
pub fn build_cpio(entries: &[Entry]) -> Vec<u8> {
    let mut cpio = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        write_entry(&mut cpio, i as u32 + 1, entry);
    }
    write_entry(
        &mut cpio,
        0,
        &Entry {
            name: "TRAILER!!!",
            mode: 0,
            data: Vec::new(),
        },
    );
    cpio
}

fn write_entry(cpio: &mut Vec<u8>, ino: u32, entry: &Entry) {
    // Recompute the on-disk mode from the type bits + permission bits,
    // matching the original tool's write_entry().
    let file_mode = if entry.mode & 0o040000 != 0 {
        0o040000 | (entry.mode & 0o0777) // directory
    } else if entry.mode & 0o020000 != 0 {
        0o020000 | (entry.mode & 0o0777) // character device
    } else {
        0o100000 | (entry.mode & 0o0777) // regular file
    };

    let header = CpioNewcHeader::new(
        ino,
        file_mode,
        0, // uid
        0, // gid
        1, // nlink
        0, // mtime
        entry.data.len() as u32,
        0, // dev
        0, // rdev
        entry.name,
    );

    header.write(cpio).unwrap();

    // Filename including NUL, then padding to a 4-byte boundary.
    cpio.write_all(entry.name.as_bytes()).unwrap();
    cpio.write_all(&[0u8]).unwrap();
    while !cpio.len().is_multiple_of(4) {
        cpio.push(0u8);
    }

    // File data, then padding to a 4-byte boundary.
    cpio.write_all(&entry.data).unwrap();
    while !cpio.len().is_multiple_of(4) {
        cpio.push(0u8);
    }
}

/// CPIO newc header structure (110 bytes).
struct CpioNewcHeader {
    magic: [u8; 6],      // "070701"
    ino: [u8; 8],        // inode number
    mode: [u8; 8],       // file mode
    uid: [u8; 8],        // user id
    gid: [u8; 8],        // group id
    nlink: [u8; 8],      // number of links
    mtime: [u8; 8],      // modification time
    filesize: [u8; 8],   // size of file data
    dev_major: [u8; 8],  // device major
    dev_minor: [u8; 8],  // device minor
    rdev_major: [u8; 8], // device major (for special files)
    rdev_minor: [u8; 8], // device minor (for special files)
    namesize: [u8; 8],   // length of filename in bytes, including null
    check: [u8; 8],      // checksum (0 for newc)
}

impl CpioNewcHeader {
    #[allow(clippy::too_many_arguments)] // on-disk format fields, kept flat for fidelity
    fn new(
        ino: u32,
        mode: u32,
        uid: u32,
        gid: u32,
        nlink: u32,
        mtime: u32,
        filesize: u32,
        dev: u32,
        rdev: u32,
        name: &str,
    ) -> Self {
        let namesize = name.len() + 1; // +1 for the null terminator
        CpioNewcHeader {
            magic: *b"070701",
            ino: hex8(ino),
            mode: hex8(mode),
            uid: hex8(uid),
            gid: hex8(gid),
            nlink: hex8(nlink),
            mtime: hex8(mtime),
            filesize: hex8(filesize),
            dev_major: hex8(major(dev)),
            dev_minor: hex8(minor(dev)),
            rdev_major: hex8(major(rdev)),
            rdev_minor: hex8(minor(rdev)),
            namesize: hex8(namesize as u32),
            check: hex8(0),
        }
    }

    fn write<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&self.magic)?;
        w.write_all(&self.ino)?;
        w.write_all(&self.mode)?;
        w.write_all(&self.uid)?;
        w.write_all(&self.gid)?;
        w.write_all(&self.nlink)?;
        w.write_all(&self.mtime)?;
        w.write_all(&self.filesize)?;
        w.write_all(&self.dev_major)?;
        w.write_all(&self.dev_minor)?;
        w.write_all(&self.rdev_major)?;
        w.write_all(&self.rdev_minor)?;
        w.write_all(&self.namesize)?;
        w.write_all(&self.check)?;
        Ok(())
    }
}

fn hex8(v: u32) -> [u8; 8] {
    let s = format!("{v:08x}");
    let mut buf = [0u8; 8];
    buf.copy_from_slice(s.as_bytes());
    buf
}

fn major(dev: u32) -> u32 {
    (dev >> 8) & 0xFF
}

fn minor(dev: u32) -> u32 {
    dev & 0xFF
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_hex8(b: &[u8]) -> u32 {
        u32::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap()
    }

    #[test]
    fn header_is_110_bytes() {
        let h = CpioNewcHeader::new(1, 0o100644, 0, 0, 1, 0, 0, 0, 0, "test");
        assert_eq!(std::mem::size_of::<CpioNewcHeader>(), 110);
        let mut buf = Vec::new();
        h.write(&mut buf).unwrap();
        assert_eq!(buf.len(), 110);
        assert_eq!(&buf[0..6], b"070701");
    }

    #[test]
    fn simple_archive_roundtrip() {
        let entries = [
            Entry {
                name: "/",
                mode: 0o040755,
                data: Vec::new(),
            },
            Entry {
                name: "/bin/echo",
                mode: 0o100755,
                data: b"ELF1234".to_vec(),
            },
        ];
        let cpio = build_cpio(&entries);

        // First header: magic + namesize for "/" (2 incl. NUL).
        assert_eq!(&cpio[0..6], b"070701");
        let namesize = parse_hex8(&cpio[94..102]);
        assert_eq!(namesize, 2, "name \"/\" plus NUL");
        let mode = parse_hex8(&cpio[14..22]);
        assert_eq!(mode, 0o040755, "directory mode");

        // Entry 1 is 110 header + 2 name bytes = 112 (already 4-aligned).
        let mut off = 112;
        assert_eq!(&cpio[off..off + 6], b"070701", "second header");
        let namesize2 = parse_hex8(&cpio[off + 94..off + 102]);
        assert_eq!(namesize2, 10, "name \"/bin/echo\" plus NUL");
        let filesize = parse_hex8(&cpio[off + 54..off + 62]);
        assert_eq!(filesize, 7, "data length");

        // Entry 2 is 110 header + 10 name bytes = 232 (4-aligned).
        off += 110 + 10;
        assert_eq!(&cpio[off..off + 7], b"ELF1234", "file data");
    }

    #[test]
    fn standard_initramfs_has_trailer() {
        let cpio = standard_initramfs(&[("/sbin/init", b"x".to_vec())]);
        let tail = &cpio[cpio.len() - 0x200..];
        assert!(
            tail.windows(10).any(|w| w == b"TRAILER!!!"),
            "archive must end with the TRAILER!!! entry"
        );
    }

    fn names(cpio: &[u8], name: &[u8]) -> bool {
        cpio.windows(name.len()).any(|w| w == name)
    }

    /// The archive is the loader's ELF store, so a binary it does not load is weight in the
    /// kernel. `/bin/coreutils` alone is 6 MB, and it is on the root filesystem.
    #[test]
    fn the_initramfs_carries_the_boot_processes_and_nothing_else() {
        let cpio = standard_initramfs(&[
            ("/sbin/mfs", b"mfs-elf".to_vec()),
            ("/bin/coreutils", b"coreutils-elf".to_vec()),
            ("/bin/wlpanel", b"wlpanel-elf".to_vec()),
        ]);
        assert!(names(&cpio, b"/sbin/mfs"));
        assert!(!names(&cpio, b"/bin/coreutils"));
        assert!(!names(&cpio, b"/bin/wlpanel"));
        assert!(!names(&cpio, b"/etc/rc"));
    }

    /// A path that is not a `BOOT_BINS` entry cannot be read from the release directory, so a
    /// typo here would leave the loader without a boot process it aborts on.
    #[test]
    fn every_initramfs_entry_is_a_boot_bin() {
        for path in crate::manifest::INITRAMFS_BINS {
            assert!(
                crate::manifest::BOOT_BINS
                    .iter()
                    .any(|(dest, _)| dest == path),
                "{path} is not a BOOT_BINS destination"
            );
        }
    }
}
