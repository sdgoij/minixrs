//! Minix V3 filesystem image writer.
//!
//! Ported from `tools/mkminixfs.rs` into a pure, testable library. The
//! `build_minixfs` helper assembles the standard root image (/, /bin,
//! /sbin, /etc, /tmp + the boot binaries) that MFS mounts at boot.

use std::path::Path;

use crate::manifest::{self, DEVICES};

pub const SUPER_MAGIC_V3: u16 = 0x4D5A;
pub const BLOCK_SIZE: usize = 4096;
pub const ZONE_SIZE: usize = 4096;
const LOG_ZONE_SIZE: i16 = 0;
pub const INODES: u32 = 128;
pub const NAMESIZE: usize = 60;

/// Default root-image size in blocks (16 MiB): the uutils coreutils
/// multicall is ~6 MiB stripped, and the rest of /bin + /sbin is ~6 MiB
/// more. `MINIXFS_BLOCKS` overrides it (the large-binary verification
/// needs a filesystem big enough for a ≥32 MiB executable).
const DEFAULT_BLOCKS: u32 = 4096;

pub const I_DIRECTORY: u16 = 0o040000;
pub const I_REGULAR: u16 = 0o100000;
pub const I_CHAR_SPECIAL: u16 = 0o020000;
pub const RWX_ALL: u16 = 0o755;

pub const ROOT_INODE: u32 = 1;

/// Directory entry (on-disk format: u32 inode + 60-byte name).
#[derive(Clone, Copy)]
struct Direct {
    d_ino: u32,
    d_name: [u8; NAMESIZE],
}

impl Direct {
    fn new(ino: u32, name: &str) -> Self {
        let mut d_name = [0u8; NAMESIZE];
        let bytes = name.as_bytes();
        let len = bytes.len().min(NAMESIZE - 1);
        d_name[..len].copy_from_slice(&bytes[..len]);
        Self { d_ino: ino, d_name }
    }

    fn write_into(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.d_ino.to_le_bytes());
        out[4..4 + NAMESIZE].copy_from_slice(&self.d_name);
    }
}

/// V2 inode (used for V3 too), serialized manually to stay unsafe-free.
#[derive(Clone, Copy)]
struct D2Inode {
    d2_mode: u16,
    d2_nlinks: u16,
    d2_uid: i16,
    d2_gid: u16,
    d2_size: i32,
    d2_atime: i32,
    d2_mtime: i32,
    d2_ctime: i32,
    d2_zone: [u32; 10],
}

impl D2Inode {
    fn new(mode: u16, size: u32) -> Self {
        Self {
            d2_mode: mode,
            d2_nlinks: 1,
            d2_uid: 0,
            d2_gid: 0,
            d2_size: size as i32,
            d2_atime: 0,
            d2_mtime: 0,
            d2_ctime: 0,
            d2_zone: [0u32; 10],
        }
    }

    fn write_into(&self, out: &mut [u8]) {
        out[0..2].copy_from_slice(&self.d2_mode.to_le_bytes());
        out[2..4].copy_from_slice(&self.d2_nlinks.to_le_bytes());
        out[4..6].copy_from_slice(&self.d2_uid.to_le_bytes());
        out[6..8].copy_from_slice(&self.d2_gid.to_le_bytes());
        out[8..12].copy_from_slice(&self.d2_size.to_le_bytes());
        out[12..16].copy_from_slice(&self.d2_atime.to_le_bytes());
        out[16..20].copy_from_slice(&self.d2_mtime.to_le_bytes());
        out[20..24].copy_from_slice(&self.d2_ctime.to_le_bytes());
        for (i, z) in self.d2_zone.iter().enumerate() {
            out[24 + i * 4..28 + i * 4].copy_from_slice(&z.to_le_bytes());
        }
    }
}

const INODE_SIZE: usize = 64; // 8 header bytes + 10 zone pointers

/// Minix V3 superblock (written at offset 1024 in block 1).
struct SuperBlock {
    s_ninodes: u32,
    s_nzones: u32,
    s_imap_blocks: i16,
    s_zmap_blocks: i16,
    s_firstdatazone_old: u32,
    s_log_zone_size: i16,
    s_flags: u16,
    s_max_size: i32,
    s_zones: u32,
    s_magic: i16,
    s_pad2: i16,
    s_block_size: u16,
    s_disk_version: u8,
}

impl SuperBlock {
    fn write_into(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.s_ninodes.to_le_bytes());
        out[4..8].copy_from_slice(&self.s_nzones.to_le_bytes());
        out[8..10].copy_from_slice(&self.s_imap_blocks.to_le_bytes());
        out[10..12].copy_from_slice(&self.s_zmap_blocks.to_le_bytes());
        out[12..16].copy_from_slice(&self.s_firstdatazone_old.to_le_bytes());
        out[16..18].copy_from_slice(&self.s_log_zone_size.to_le_bytes());
        out[18..20].copy_from_slice(&self.s_flags.to_le_bytes());
        out[20..24].copy_from_slice(&self.s_max_size.to_le_bytes());
        out[24..28].copy_from_slice(&self.s_zones.to_le_bytes());
        out[28..30].copy_from_slice(&self.s_magic.to_le_bytes());
        out[30..32].copy_from_slice(&self.s_pad2.to_le_bytes());
        out[32..34].copy_from_slice(&self.s_block_size.to_le_bytes());
        out[34] = self.s_disk_version;
    }
}

/// In-progress Minix V3 filesystem image.
pub struct MinixFs {
    data: Vec<u8>,
    total_blocks: u32,
    inodes: u32,
    inode_blocks: u32,
    imap_blocks: u16,
    zmap_blocks: u16,
    first_data_zone: u32,
    next_inode: u32,
    next_zone: u32,
    inode_bitmap: Vec<u8>,
    zone_bitmap: Vec<u8>,
    inode_table: Vec<D2Inode>,
}

impl MinixFs {
    pub fn new(total_blocks: u32, inodes: u32) -> Self {
        let bits_per_block = BLOCK_SIZE as u32 * 8;
        let imap_blocks = inodes.div_ceil(bits_per_block) as u16;
        let zmap_blocks = total_blocks.div_ceil(bits_per_block) as u16;
        let inodes_per_block = BLOCK_SIZE as u32 / INODE_SIZE as u32;
        let inode_blocks = inodes.div_ceil(inodes_per_block);

        // First data zone = superblock + bitmaps + inode table.
        let first_data_zone = 2 + imap_blocks as u32 + zmap_blocks as u32 + inode_blocks;

        let mut fs = Self {
            data: vec![0u8; total_blocks as usize * BLOCK_SIZE],
            total_blocks,
            inodes,
            inode_blocks,
            imap_blocks,
            zmap_blocks,
            first_data_zone,
            next_inode: ROOT_INODE + 1,
            next_zone: first_data_zone,
            inode_bitmap: vec![0u8; imap_blocks as usize * BLOCK_SIZE],
            zone_bitmap: vec![0u8; zmap_blocks as usize * BLOCK_SIZE],
            inode_table: Vec::new(),
        };

        // Reserve inode 1 (root).  MINIX inode numbers are 1-based and the
        // on-disk inode table stores inode N at slot (N-1); there is no
        // dummy inode 0.
        fs.inode_table.push(D2Inode::new(0, 0)); // ino 1 (set up later)
        fs.set_inode_used(ROOT_INODE);

        fs
    }

    /// Create a directory with `.` and `..` entries and wire it into the
    /// inode table; returns the zone holding the directory data.
    pub fn create_directory(&mut self, dir_ino: u32, parent_ino: u32) -> u32 {
        let zone = self.alloc_zone();

        let dot = Direct::new(dir_ino, ".");
        let dotdot = Direct::new(parent_ino, "..");
        let mut dir_data = vec![0u8; 2 * (4 + NAMESIZE)];
        dot.write_into(&mut dir_data[0..4 + NAMESIZE]);
        dotdot.write_into(&mut dir_data[4 + NAMESIZE..2 * (4 + NAMESIZE)]);
        self.write_zone(zone, &dir_data);

        let idx = (dir_ino - 1) as usize;
        if idx < self.inode_table.len() {
            self.inode_table[idx].d2_zone[0] = zone;
            self.inode_table[idx].d2_mode = I_DIRECTORY | RWX_ALL;
            self.inode_table[idx].d2_size = dir_data.len() as i32;
        }

        zone
    }

    /// Create a named directory under `root_zone` and link it into the root;
    /// returns the zone holding the directory data.
    pub fn add_directory(&mut self, root_zone: u32, name: &str) -> u32 {
        let zone = self.alloc_zone();
        let ino = self.alloc_inode(I_DIRECTORY | RWX_ALL, 64);
        self.write_inode_zone(ino, zone);
        self.add_dirent(root_zone, ino, name);
        self.add_dirent(zone, ino, ".");
        self.add_dirent(zone, ROOT_INODE, "..");
        zone
    }

    /// Add a regular file to `dir_zone`; returns its inode number.
    pub fn add_file(&mut self, dir_zone: u32, name: &str, data: &[u8]) -> u32 {
        self.add_file_meta(dir_zone, name, data, I_REGULAR | RWX_ALL, 0, 0)
    }

    /// Add a regular file with explicit mode/owner.
    pub fn add_file_meta(
        &mut self,
        dir_zone: u32,
        name: &str,
        data: &[u8],
        mode: u16,
        uid: u16,
        gid: u16,
    ) -> u32 {
        let zones_needed = data.len().div_ceil(self.zone_size());
        let mut zones = Vec::with_capacity(zones_needed);
        for i in 0..zones_needed {
            let z = self.alloc_zone();
            let start = i * self.zone_size();
            let end = (start + self.zone_size()).min(data.len());
            self.write_zone(z, &data[start..end]);
            zones.push(z);
        }

        let ino = self.alloc_inode(mode, data.len() as u32);
        let idx = (ino - 1) as usize;
        if idx < self.inode_table.len() {
            self.inode_table[idx].d2_uid = uid as i16;
            self.inode_table[idx].d2_gid = gid;
            // Zone map: 7 direct zones, then single/double-indirect blocks
            // (zone_size/4 = 1024 entries each). MFS read_map resolves
            // i_ndzones=7 direct + i_nindirs indirect + nindirs^2
            // double-indirect zones, so files larger than 7 + 1024 blocks
            // (the builder's old single-indirect-only ceiling, ~4 MiB)
            // need the zone[8] double-indirect chain.
            let indir_entries = self.zone_size() / 4; // u32 zone numbers
            let n_direct = zones_needed.min(7);
            self.inode_table[idx].d2_zone[..n_direct].copy_from_slice(&zones[..n_direct]);
            let mut next = n_direct;
            if next < zones_needed {
                let indir_zone = self.alloc_zone();
                let mut indir_data = vec![0u8; self.zone_size()];
                let n_single = (zones_needed - next).min(indir_entries);
                for (j, z) in zones.iter().skip(next).take(n_single).enumerate() {
                    indir_data[j * 4..j * 4 + 4].copy_from_slice(&z.to_le_bytes());
                }
                self.write_zone(indir_zone, &indir_data);
                self.inode_table[idx].d2_zone[7] = indir_zone;
                next += n_single;
            }
            if next < zones_needed {
                let dindir_zone = self.alloc_zone();
                let mut dindir_data = vec![0u8; self.zone_size()];
                let mut d_idx = 0usize;
                while next < zones_needed {
                    let sindir_zone = self.alloc_zone();
                    let mut sdata = vec![0u8; self.zone_size()];
                    let n = (zones_needed - next).min(indir_entries);
                    for (j, z) in zones.iter().skip(next).take(n).enumerate() {
                        sdata[j * 4..j * 4 + 4].copy_from_slice(&z.to_le_bytes());
                    }
                    self.write_zone(sindir_zone, &sdata);
                    dindir_data[d_idx * 4..d_idx * 4 + 4]
                        .copy_from_slice(&sindir_zone.to_le_bytes());
                    next += n;
                    d_idx += 1;
                }
                self.write_zone(dindir_zone, &dindir_data);
                self.inode_table[idx].d2_zone[8] = dindir_zone;
            }
            self.inode_table[idx].d2_size = data.len() as i32;
        }

        self.add_dirent(dir_zone, ino, name);
        ino
    }

    /// Add a character-device node to `dir_zone`; returns its inode number.
    ///
    /// The device number (major << 16 | minor) is stored in the inode's
    /// first zone pointer — MFS's `fs_lookup` reports `i_zone[0]` as the
    /// vnode device, which VFS's `cdev_*` decodes as major = dev >> 16,
    /// minor = dev & 0xFFFF.
    pub fn add_device(&mut self, dir_zone: u32, name: &str, mode: u16, dev: u32) -> u32 {
        let ino = self.alloc_inode(I_CHAR_SPECIAL | mode, 0);
        let idx = (ino - 1) as usize;
        if idx < self.inode_table.len() {
            self.inode_table[idx].d2_zone[0] = dev;
        }
        self.add_dirent(dir_zone, ino, name);
        ino
    }

    /// Finalize and write the superblock, bitmaps, and inode table.
    pub fn finalise(mut self) -> Vec<u8> {
        let sb = SuperBlock {
            s_ninodes: self.inodes,
            s_nzones: self.total_blocks,
            s_imap_blocks: self.imap_blocks as i16,
            s_zmap_blocks: self.zmap_blocks as i16,
            s_firstdatazone_old: self.first_data_zone,
            s_log_zone_size: LOG_ZONE_SIZE,
            s_flags: 0,
            s_max_size: 0x7FFFFFFF,
            s_zones: self.total_blocks,
            s_magic: SUPER_MAGIC_V3 as i16,
            s_pad2: 0,
            s_block_size: BLOCK_SIZE as u16,
            s_disk_version: 0,
        };
        let mut sb_bytes = [0u8; 35];
        sb.write_into(&mut sb_bytes);
        self.data[1024..1024 + sb_bytes.len()].copy_from_slice(&sb_bytes);

        // Inode bitmap at block 2, zone bitmap after it.
        // Bit 0 is reserved (there is no inode 0); MFS's alloc_bit starts at
        // s_isearch and would otherwise hand out inode 0.
        self.inode_bitmap[0] |= 1;
        let imap_block = 2usize;
        let imap_off = imap_block * BLOCK_SIZE;
        let imap_len = self.imap_blocks as usize * BLOCK_SIZE;
        self.data[imap_off..imap_off + imap_len].copy_from_slice(&self.inode_bitmap[..imap_len]);

        // The ZMAP covers zones first_data_zone-1 .. total_blocks-1; mark
        // first_data_zone-1 (the last non-data block) used so alloc_zone
        // never returns it.
        self.set_zone_used(self.first_data_zone - 1);
        let zmap_block = imap_block + self.imap_blocks as usize;
        let zmap_off = zmap_block * BLOCK_SIZE;
        let zmap_len = self.zmap_blocks as usize * BLOCK_SIZE;
        self.data[zmap_off..zmap_off + zmap_len].copy_from_slice(&self.zone_bitmap[..zmap_len]);

        // Inode table after the zone bitmap.
        let itable_block = zmap_block + self.zmap_blocks as usize;
        let itable_off = itable_block * BLOCK_SIZE;
        let itable_len = self.inode_blocks as usize * BLOCK_SIZE;
        for (i, inode) in self.inode_table.iter().enumerate() {
            let off = itable_off + i * INODE_SIZE;
            if off + INODE_SIZE <= itable_off + itable_len {
                let mut buf = [0u8; INODE_SIZE];
                inode.write_into(&mut buf);
                self.data[off..off + INODE_SIZE].copy_from_slice(&buf);
            }
        }

        self.data
    }

    fn zone_size(&self) -> usize {
        ZONE_SIZE
    }

    fn set_inode_used(&mut self, ino: u32) {
        // On-disk convention (MINIX `imap_bit`): inode N is bit N of the
        // inode bitmap, and bit 0 is reserved (there is no inode 0). MFS's
        // alloc_bit returns the bit number directly as the inode number, so
        // inode 1 must be bit 1 — a bit N-1 mapping shifts every inode by one
        // and makes MFS hand out the console's inode as free (observed:
        // the first create reused /dev/console's inode 37 and every write
        // went to the tty instead of the new file).
        let bit = ino as usize;
        self.inode_bitmap[bit / 8] |= 1 << (bit % 8);
    }

    fn set_zone_used(&mut self, zone: u32) {
        let bit = (zone - (self.first_data_zone - 1)) as usize;
        if bit / 8 < self.zone_bitmap.len() {
            self.zone_bitmap[bit / 8] |= 1 << (bit % 8);
        }
    }

    fn alloc_zone(&mut self) -> u32 {
        let z = self.next_zone;
        self.next_zone += 1;
        self.set_zone_used(z);
        z
    }

    fn alloc_inode(&mut self, mode: u16, size: u32) -> u32 {
        let ino = self.next_inode;
        self.next_inode += 1;
        self.inode_table.push(D2Inode::new(mode, size));
        self.set_inode_used(ino);
        ino
    }

    fn write_zone(&mut self, zone: u32, data: &[u8]) {
        let off = zone as usize * self.zone_size();
        assert!(
            off + data.len() <= self.data.len(),
            "zone {zone} out of bounds (max offset {})",
            self.data.len()
        );
        self.data[off..off + data.len()].copy_from_slice(data);
    }

    fn write_inode_zone(&mut self, ino: u32, zone: u32) {
        let idx = (ino - 1) as usize;
        if idx < self.inode_table.len() {
            self.inode_table[idx].d2_zone[0] = zone;
        }
    }

    fn add_dirent(&mut self, dir_zone: u32, file_ino: u32, name: &str) {
        let off = dir_zone as usize * self.zone_size();
        let entry_size = 4 + NAMESIZE;
        let max_entries = self.zone_size() / entry_size;
        let mut slot = None;
        for i in 0..max_entries {
            let e_off = i * entry_size;
            let existing_ino =
                u32::from_le_bytes(self.data[off + e_off..off + e_off + 4].try_into().unwrap());
            if existing_ino == 0 {
                slot = Some(i);
                break;
            }
        }
        let slot = slot.unwrap_or_else(|| panic!("directory zone {dir_zone} is full"));

        let entry = Direct::new(file_ino, name);
        let mut entry_bytes = [0u8; 4 + NAMESIZE];
        entry.write_into(&mut entry_bytes);
        let e_off = slot * entry_size;
        self.data[off + e_off..off + e_off + entry_size].copy_from_slice(&entry_bytes);

        // Grow the directory inode size to encompass the new entry.
        let new_size = ((slot + 1) * entry_size) as i32;
        for entry in self.inode_table.iter_mut() {
            if entry.d2_zone[0] == dir_zone && (entry.d2_size as usize) < (slot + 1) * entry_size {
                entry.d2_size = new_size;
                break;
            }
        }
    }
}

/// Build the standard root filesystem image containing `files` (destination
/// path → content). The parent directories /bin and /sbin are created
/// automatically; anything else lands in the root. Sized by
/// [`DEFAULT_BLOCKS`] unless the `MINIXFS_BLOCKS` env var overrides it
/// (used by the large-binary verification, which needs a filesystem big
/// enough for a ≥32 MiB executable).
pub fn build_minixfs(files: &[(&'static str, Vec<u8>)]) -> Vec<u8> {
    let total_blocks = std::env::var("MINIXFS_BLOCKS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_BLOCKS);
    let mut fs = MinixFs::new(total_blocks, INODES);

    let root_zone = fs.create_directory(ROOT_INODE, ROOT_INODE);
    let bin_zone = fs.add_directory(root_zone, "bin");
    let sbin_zone = fs.add_directory(root_zone, "sbin");
    let etc_zone = fs.add_directory(root_zone, "etc");
    let _tmp_ino = fs.add_directory(root_zone, "tmp");
    let dev_zone = fs.add_directory(root_zone, "dev");

    for (dest, data) in files {
        if data.is_empty() {
            continue;
        }
        let bin_name = Path::new(dest).file_name().unwrap().to_str().unwrap();
        // String match, not Path::parent(): on Windows a POSIX-style dest
        // ("MINIXFS_EXTRA=/bin/big=..." through MSYS) can parse to a
        // Windows root-relative path whose parent() is not "/bin".
        let parent_zone = if dest.starts_with("/bin/") {
            bin_zone
        } else if dest.starts_with("/sbin/") {
            sbin_zone
        } else if dest.starts_with("/etc/") {
            etc_zone
        } else {
            root_zone
        };
        fs.add_file(parent_zone, bin_name, data);
    }

    // Data files (passwd, root-only secret) with explicit mode/owner.
    for &(dest, data, mode, uid, gid) in manifest::BOOT_FILES {
        let name = Path::new(dest).file_name().unwrap().to_str().unwrap();
        let parent_zone = if dest.starts_with("/etc/") {
            etc_zone
        } else {
            root_zone
        };
        fs.add_file_meta(parent_zone, name, data, mode, uid, gid);
    }

    // Character-device nodes (major << 16 | minor, matching VFS's cdev
    // decoding). TTY.md 1C.1 relies on /dev/console resolving so init can
    // route stdio through the tty.
    for &(path, mode, major, minor) in DEVICES {
        let name = Path::new(path).file_name().unwrap().to_str().unwrap();
        fs.add_device(dev_zone, name, mode as u16, (major << 16) | minor);
    }

    fs.finalise()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superblock_magic_and_size() {
        let empty: &[(&'static str, Vec<u8>)] = &[];
        let image = build_minixfs(empty);
        assert_eq!(image.len(), DEFAULT_BLOCKS as usize * BLOCK_SIZE);
        // Superblock magic at offset 1024 + 28 (s_magic field).
        let magic = u16::from_le_bytes(image[1024 + 28..1024 + 30].try_into().unwrap());
        assert_eq!(magic, SUPER_MAGIC_V3);
    }

    #[test]
    fn device_nodes_in_image() {
        // With no files, inodes are deterministic: 1=root, 2=bin, 3=sbin,
        // 4=etc, 5=tmp, 6=dev, then the four devices in manifest order
        // (tty00, tty01, null, console). The console inode must be a
        // char-special node whose zone[0] carries (major << 16 | minor) so
        // MFS reports the device number VFS's cdev_* expects.
        let empty: &[(&'static str, Vec<u8>)] = &[];
        let image = build_minixfs(empty);

        let imap_blocks = i16::from_le_bytes(image[1024 + 8..1024 + 10].try_into().unwrap());
        let zmap_blocks = i16::from_le_bytes(image[1024 + 10..1024 + 12].try_into().unwrap());
        let itable_off = (2 + imap_blocks as usize + zmap_blocks as usize) * BLOCK_SIZE;

        // Inode numbering: root(1) bin(2) sbin(3) etc(4) tmp(5) dev(6),
        // then BOOT_FILES passwd(7) secret(8), then devices: tty00(9)
        // tty01(10) null(11) console(12) ip(13) udp(14) tcp(15).
        let console_ino = 12usize;
        let off = itable_off + (console_ino - 1) * INODE_SIZE;
        let mode = u16::from_le_bytes(image[off..off + 2].try_into().unwrap());
        let dev = u32::from_le_bytes(image[off + 24..off + 28].try_into().unwrap());
        assert_eq!(
            mode & 0o170000,
            I_CHAR_SPECIAL,
            "console must be char-special"
        );
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(dev, 5u32 << 16, "console major 5 minor 0");

        // /dev/tty00: major 3 minor 0.
        let tty00_off = itable_off + 8 * INODE_SIZE;
        let tty00_dev =
            u32::from_le_bytes(image[tty00_off + 24..tty00_off + 28].try_into().unwrap());
        assert_eq!(tty00_dev, 3u32 << 16);
    }

    #[test]
    fn single_file_roundtrip() {
        let data = b"hello minix".to_vec();
        let image = build_minixfs(&[("/bin/echo", data)]);

        // Derive the on-disk layout from the superblock itself (imap at
        // block 2, zmap after it, inode table after the zmap).
        let imap_blocks = i16::from_le_bytes(image[1024 + 8..1024 + 10].try_into().unwrap());
        let zmap_blocks = i16::from_le_bytes(image[1024 + 10..1024 + 12].try_into().unwrap());
        let itable_off = (2 + imap_blocks as usize + zmap_blocks as usize) * BLOCK_SIZE;

        // Root inode (inode 1) must be a directory with a non-zero first zone.
        let root_mode = u16::from_le_bytes(image[itable_off..itable_off + 2].try_into().unwrap());
        assert_eq!(root_mode, I_DIRECTORY | RWX_ALL);
        let root_zone =
            u32::from_le_bytes(image[itable_off + 24..itable_off + 28].try_into().unwrap());
        assert!(root_zone >= 2, "root data zone must be allocated");

        // "echo" inode (inode 7: 1=root, 2=bin, 3=sbin, 4=etc, 5=tmp,
        // 6=dev) must be a regular file of size 11.
        let echo_ino_off = itable_off + 6 * INODE_SIZE;
        let echo_mode =
            u16::from_le_bytes(image[echo_ino_off..echo_ino_off + 2].try_into().unwrap());
        assert_eq!(echo_mode, I_REGULAR | RWX_ALL);
        let echo_size = i32::from_le_bytes(
            image[echo_ino_off + 8..echo_ino_off + 12]
                .try_into()
                .unwrap(),
        );
        assert_eq!(echo_size, 11);

        // The "echo" dirent must land in the /bin directory (inode 2's
        // data zone), not the root or the bitmap blocks.
        let bin_zone = u32::from_le_bytes(
            image[itable_off + INODE_SIZE + 24..itable_off + INODE_SIZE + 28]
                .try_into()
                .unwrap(),
        );
        let bin_off = bin_zone as usize * BLOCK_SIZE;
        let bin_ino = u32::from_le_bytes(
            image[bin_off + 2 * (4 + NAMESIZE)..bin_off + 2 * (4 + NAMESIZE) + 4]
                .try_into()
                .unwrap(),
        );
        let bin_name = &image[bin_off + 2 * (4 + NAMESIZE) + 4..bin_off + 2 * (4 + NAMESIZE) + 9];
        assert_eq!(bin_ino, 7, "echo dirent must point at inode 7");
        assert_eq!(&bin_name[..4], b"echo", "echo dirent must be in /bin");
    }

    #[test]
    fn inode_offsets_in_on_disk_table() {
        // inode N lives at slot N-1 in the on-disk table; slot 0 is inode 1.
        let mut fs = MinixFs::new(2048, INODES);
        fs.create_directory(ROOT_INODE, ROOT_INODE);
        let zone = fs.add_directory(fs.first_data_zone, "bin");
        let ino = fs.add_file(zone, "x", b"123");
        assert_eq!(ino, 3, "inodes: 1=root, 2=bin, 3=x");
        let _ = fs.finalise();
    }

    #[test]
    fn inode_bitmap_uses_on_disk_bit_numbering() {
        // MFS (MINIX `imap_bit`) maps inode N to bit N of the inode bitmap,
        // with bit 0 reserved. A bit N-1 mapping shifts every inode by one:
        // MFS's alloc_bit then treats the console inode as free and reuses it
        // on the first create, so new files inherit its S_IFCHR mode and all
        // writes route to the tty (observed before the fix).
        let empty: &[(&'static str, Vec<u8>)] = &[];
        let image = build_minixfs(empty);

        let imap_blocks = i16::from_le_bytes(image[1024 + 8..1024 + 10].try_into().unwrap());
        let zmap_blocks = i16::from_le_bytes(image[1024 + 10..1024 + 12].try_into().unwrap());
        let itable_off = (2 + imap_blocks as usize + zmap_blocks as usize) * BLOCK_SIZE;

        // Console is inode 10 in the empty image (root, bin, sbin, etc, tmp,
        // dev + tty00, tty01, null). Its imap bit (10) must be set, bit 0
        // must be reserved, and bit 1 (inode 1, root) must be set.
        let imap = &image[2 * BLOCK_SIZE..3 * BLOCK_SIZE];
        assert_eq!(imap[0] & 1, 1, "bit 0 must be reserved (no inode 0)");
        assert_eq!((imap[0] >> 1) & 1, 1, "inode 1 (root) must be in use");
        assert_eq!(
            (imap[10 / 8] >> (10 % 8)) & 1,
            1,
            "inode 10 (console) must be in use"
        );

        // Every inode in the table must be marked in use (bit N), so MFS's
        // alloc_bit never hands out an inode that already has table data.
        // The builder writes slot N-1 for inode N; walk the table. The
        // empty image has 6 dirs (root, bin, sbin, etc, tmp, dev) + 2 data
        // files (passwd, secret) + 9 devices (tty00, tty01, null, console,
        // ip, udp, tcp, fb, kbd) = 17 inodes.
        let n_inodes = 17usize;
        for ino in 1..=n_inodes {
            assert_eq!(
                (imap[ino / 8] >> (ino % 8)) & 1,
                1,
                "inode {ino} must be marked in use at bit {ino}"
            );
        }
        // And the next bit (inode 18) is free — the first allocatable inode.
        assert_eq!(
            imap[2] & 0b100,
            0,
            "inode 18 must be free for the first create"
        );
        let _ = itable_off;
    }
}
