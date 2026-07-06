//! Builds a Minix V3 filesystem image from the initramfs file tree.
//!
//! The image is a raw Minix V3 filesystem that MFS can mount. It contains
//! all boot-critical userland binaries from the initramfs build directory.
//!
//! Usage: rustc tools/mkminixfs.rs --edition 2021 -o target/mkminixfs.exe
//!        && target/mkminixfs.exe
//!
//! Output: target/minixfs.img (raw Minix V3 filesystem image)
//!         target/minixfs_data.rs (Rust source with embedded bytes)

use std::fs;
use std::path::Path;

const SUPER_MAGIC_V3: u16 = 0x4D5A;
const BLOCK_SIZE: usize = 4096;
const ZONE_SIZE: usize = 4096;
const LOG_ZONE_SIZE: i16 = 0;
const INODES: u32 = 128;
const NAMESIZE: usize = 60;

// Mode bits
const I_DIRECTORY: u16 = 0o040000;
const I_REGULAR: u16 = 0o100000;
const RWX_ALL: u16 = 0o755;

// Root inode
const ROOT_INODE: u32 = 1;

/// Directory entry (on-disk format, packed).
#[repr(C, packed)]
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
}

/// V2 inode (on-disk format, used for V3 too).
#[repr(C)]
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
}

/// Minix V3 superblock (first 28 bytes + V3 fields).
#[repr(C)]
struct SuperBlock {
    s_ninodes: u32,
    s_nzones: u32, // zone1_t
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

struct FsImage {
    data: Vec<u8>,
    #[allow(dead_code)]
    /// Block size in bytes (reserved for variable block size support;
    /// currently only 4096 is used, via the BLOCK_SIZE constant).
    block_size: usize,
    zone_size: usize,
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

impl FsImage {
    fn new(total_blocks: u32, inodes: u32) -> Self {
        // Calculate layout
        let imap_blocks =
            ((inodes as u32 + BLOCK_SIZE as u32 * 8 - 1) / (BLOCK_SIZE as u32 * 8)) as u16;
        let zmap_blocks =
            ((total_blocks as u32 + BLOCK_SIZE as u32 * 8 - 1) / (BLOCK_SIZE as u32 * 8)) as u16;
        let inode_size = std::mem::size_of::<D2Inode>() as u32;
        let inodes_per_block = BLOCK_SIZE as u32 / inode_size;
        let inode_blocks = (inodes + inodes_per_block - 1) / inodes_per_block;

        // First data zone = superblock + bitmaps + inode table
        let first_data_zone = 2 + imap_blocks as u32 + zmap_blocks as u32 + inode_blocks;

        let mut fs = Self {
            data: vec![0u8; (total_blocks as usize) * BLOCK_SIZE],
            block_size: BLOCK_SIZE,
            zone_size: ZONE_SIZE,
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

        // Reserve inode 1 (root).  In MINIX, inode numbers are 1-based;
        // the on-disk inode table stores inode N at slot (N-1).  Slot 0
        // corresponds to inode 1, slot 1 to inode 2, etc.  No dummy inode 0.
        fs.inode_table.push(D2Inode::new(0, 0)); // ino 1 (will be set up later)
        fs.set_inode_used(ROOT_INODE);

        fs
    }

    fn set_inode_used(&mut self, ino: u32) {
        let bit = (ino - 1) as usize;
        self.inode_bitmap[bit / 8] |= 1 << (bit % 8);
    }

    fn set_zone_used(&mut self, zone: u32) {
        let bit = zone as usize;
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
        let off = (zone as usize) * self.zone_size;
        if off + data.len() > self.data.len() {
            panic!(
                "zone {} out of bounds (max offset {})",
                zone,
                self.data.len()
            );
        }
        self.data[off..off + data.len()].copy_from_slice(data);
    }

    fn write_inode_zone(&mut self, ino: u32, zone: u32) {
        // Find the inode in the table and set its first direct zone.
        // inode_table index = (ino - 1): slot 0 = inode 1.
        let idx = (ino - 1) as usize;
        if idx < self.inode_table.len() {
            self.inode_table[idx].d2_zone[0] = zone;
        }
    }

    /// Create a directory with `.` and `..` entries.
    fn create_directory(&mut self, dir_ino: u32, parent_ino: u32, _name: &str) -> u32 {
        let zone = self.alloc_zone();

        // Write `.` and `..` entries
        let dot = Direct::new(dir_ino, ".");
        let dotdot = Direct::new(parent_ino, "..");
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                &dot as *const Direct as *const u8,
                std::mem::size_of::<Direct>(),
            )
        });
        dir_data.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                &dotdot as *const Direct as *const u8,
                std::mem::size_of::<Direct>(),
            )
        });

        self.write_zone(zone, &dir_data);

        // Update the inode's first zone pointer, mode, and size.
        // inode_table index = (dir_ino - 1): slot 0 = inode 1.
        let idx = (dir_ino - 1) as usize;
        if idx < self.inode_table.len() {
            self.inode_table[idx].d2_zone[0] = zone;
            self.inode_table[idx].d2_mode = I_DIRECTORY | RWX_ALL;
            self.inode_table[idx].d2_size = dir_data.len() as i32;
        }

        zone
    }

    /// Add a file to a directory.
    fn add_dirent(&mut self, dir_zone: u32, file_ino: u32, name: &str) {
        // Read the existing directory data
        let off = (dir_zone as usize) * self.zone_size;
        let mut dir_data = self.data[off..off + self.zone_size].to_vec();

        // Find a free slot or append
        let entry_size = std::mem::size_of::<Direct>();
        let max_entries = self.zone_size / entry_size;
        let mut found = false;
        for i in 0..max_entries {
            let e_off = i * entry_size;
            let ino_bytes = &dir_data[e_off..e_off + 4];
            let existing_ino = u32::from_le_bytes(ino_bytes.try_into().unwrap());
            if existing_ino == 0 {
                // Free slot — use it
                let entry = Direct::new(file_ino, name);
                let entry_bytes = unsafe {
                    std::slice::from_raw_parts(&entry as *const Direct as *const u8, entry_size)
                };
                dir_data[e_off..e_off + entry_size].copy_from_slice(entry_bytes);
                found = true;
                break;
            }
        }
        if !found {
            panic!("directory zone {} is full", dir_zone);
        }

        self.data[off..off + self.zone_size].copy_from_slice(&dir_data);
    }

    /// Add a regular file, returns its inode number.
    fn add_file(&mut self, dir_zone: u32, name: &str, data: &[u8]) -> u32 {
        // Allocate zone(s) for the file
        let zones_needed = (data.len() + self.zone_size - 1) / self.zone_size;
        let first_zone = self.alloc_zone();

        // Write file data
        self.write_zone(first_zone, data);

        // For multi-zone files, write additional zones
        for i in 1..zones_needed {
            let z = self.alloc_zone();
            let start = i * self.zone_size;
            let end = (start + self.zone_size).min(data.len());
            self.write_zone(z, &data[start..end]);
        }

        // Create inode
        let ino = self.alloc_inode(I_REGULAR | RWX_ALL, data.len() as u32);
        // Set first zone pointer and size.
        // inode_table index = (ino - 1): slot 0 = inode 1.
        let idx = (ino - 1) as usize;
        if idx < self.inode_table.len() {
            self.inode_table[idx].d2_zone[0] = first_zone;
            self.inode_table[idx].d2_size = data.len() as i32;
        }

        // Add directory entry
        self.add_dirent(dir_zone, ino, name);

        ino
    }

    /// Finalise and write the superblock, bitmaps, and inode table.
    fn finalise(mut self) -> Vec<u8> {
        let inode_size = std::mem::size_of::<D2Inode>() as u32;
        let _inodes_per_block = BLOCK_SIZE as u32 / inode_size;

        // Write superblock at offset 1024 in block 1
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
        let sb_bytes = unsafe {
            std::slice::from_raw_parts(
                &sb as *const SuperBlock as *const u8,
                std::mem::size_of::<SuperBlock>(),
            )
        };
        self.data[1024..1024 + sb_bytes.len()].copy_from_slice(sb_bytes);

        // Write inode bitmap at block 2
        let imap_block = 2usize;
        let imap_off = imap_block * BLOCK_SIZE;
        let imap_len = (self.imap_blocks as usize) * BLOCK_SIZE;
        self.data[imap_off..imap_off + imap_len].copy_from_slice(&self.inode_bitmap[..imap_len]);

        // Write zone bitmap after inode bitmap
        let zmap_block = imap_block + self.imap_blocks as usize;
        let zmap_off = zmap_block * BLOCK_SIZE;
        let zmap_len = (self.zmap_blocks as usize) * BLOCK_SIZE;
        self.data[zmap_off..zmap_off + zmap_len].copy_from_slice(&self.zone_bitmap[..zmap_len]);

        // Write inode table after zone bitmap
        let itable_block = zmap_block + self.zmap_blocks as usize;
        let itable_off = itable_block * BLOCK_SIZE;
        let itable = unsafe {
            std::slice::from_raw_parts(
                self.inode_table.as_ptr() as *const u8,
                self.inode_table.len() * std::mem::size_of::<D2Inode>(),
            )
        };
        let itable_len = (self.inode_blocks as usize) * BLOCK_SIZE;
        if itable.len() <= itable_len {
            self.data[itable_off..itable_off + itable.len()].copy_from_slice(itable);
        } else {
            self.data[itable_off..itable_off + itable_len].copy_from_slice(&itable[..itable_len]);
        }

        self.data
    }
}

fn main() {
    let workspace = Path::new(".");
    let target_dir = workspace.join("target");

    // Parse optional architecture argument: "x86_64" (default) or "riscv64"
    let arch = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "x86_64".to_string());

    // Binaries are in the cargo target output directory
    let target_out_dir = match arch.as_str() {
        "riscv64" => "riscv64gc-unknown-none-elf",
        _ => "x86_64-pc-minix",
    };
    let release_dir = target_dir.join(target_out_dir).join("release");

    eprintln!("Using binaries from: {}", release_dir.display());
    // Root + /bin directory + a few files = ~8 MB should be plenty
    let total_blocks = 2048u32; // 2048 * 4096 = 8 MB

    let mut fs = FsImage::new(total_blocks, INODES);

    // Create root directory (inode 1)
    let root_zone = fs.create_directory(ROOT_INODE, ROOT_INODE, "/");

    // Create /bin directory
    let bin_zone = fs.alloc_zone();
    let bin_ino = fs.alloc_inode(I_DIRECTORY | RWX_ALL, 64);
    fs.set_zone_used(bin_zone);
    fs.write_inode_zone(bin_ino, bin_zone);
    fs.add_dirent(root_zone, bin_ino, "bin");
    fs.add_dirent(bin_zone, bin_ino, "."); // bin/.
    fs.add_dirent(bin_zone, ROOT_INODE, ".."); // bin/..

    // Create /sbin directory
    let sbin_zone = fs.alloc_zone();
    let sbin_ino = fs.alloc_inode(I_DIRECTORY | RWX_ALL, 64);
    fs.set_zone_used(sbin_zone);
    fs.write_inode_zone(sbin_ino, sbin_zone);
    fs.add_dirent(root_zone, sbin_ino, "sbin");
    fs.add_dirent(sbin_zone, sbin_ino, ".");
    fs.add_dirent(sbin_zone, ROOT_INODE, "..");

    // Create /etc directory
    let _etc_ino = {
        let etc_zone = fs.alloc_zone();
        let etc_ino = fs.alloc_inode(I_DIRECTORY | RWX_ALL, 64);
        fs.set_zone_used(etc_zone);
        fs.write_inode_zone(etc_ino, etc_zone);
        fs.add_dirent(root_zone, etc_ino, "etc");
        fs.add_dirent(etc_zone, etc_ino, ".");
        fs.add_dirent(etc_zone, ROOT_INODE, "..");
        etc_ino
    };

    // Create /tmp directory
    let _tmp_ino = {
        let tmp_zone = fs.alloc_zone();
        let tmp_ino = fs.alloc_inode(I_DIRECTORY | 0o777, 64);
        fs.set_zone_used(tmp_zone);
        fs.write_inode_zone(tmp_ino, tmp_zone);
        fs.add_dirent(root_zone, tmp_ino, "tmp");
        fs.add_dirent(tmp_zone, tmp_ino, ".");
        fs.add_dirent(tmp_zone, ROOT_INODE, "..");
        tmp_ino
    };

    // Add binaries from initramfs build output
    let boot_bins = [
        "/sbin/init",
        "/sbin/pm",
        "/sbin/vfs",
        "/sbin/vm",
        "/sbin/rs",
        "/sbin/ds",
        "/sbin/sched",
        "/sbin/tty",
        "/sbin/ramdisk",
        "/bin/sh",
        "/bin/cat",
        "/bin/echo",
        "/bin/ls",
        "/bin/mkdir",
        "/bin/rm",
        "/bin/cp",
        "/bin/ln",
        "/bin/chmod",
        "/bin/sync",
        "/sbin/mknod",
        "/sbin/reboot",
        "/sbin/fsck",
    ];

    for &dest in &boot_bins {
        let bin_name = Path::new(dest).file_name().unwrap().to_str().unwrap();
        let src_path = release_dir.join(bin_name);

        if src_path.exists() {
            let data = fs::read(&src_path).unwrap_or_default();
            if data.is_empty() {
                eprintln!("  WARNING: {} is empty", src_path.display());
                continue;
            }
            let parent_dir = Path::new(dest).parent().unwrap().to_str().unwrap();
            let parent_zone = match parent_dir {
                "/bin" => bin_zone,
                "/sbin" => sbin_zone,
                _ => root_zone,
            };
            let _ino = fs.add_file(parent_zone, bin_name, &data);
            println!("  {} -> ino={} size={}", dest, _ino, data.len());
        } else {
            eprintln!(
                "  WARNING: {} not found at {}",
                bin_name,
                src_path.display()
            );
        }
    }

    // Finalise the image
    let image = fs.finalise();

    // Write the raw image
    let img_path = target_dir.join("minixfs.img");
    fs::write(&img_path, &image).unwrap();
    println!("minixfs.img: {} bytes written", image.len());

    // Generate Rust source stub that includes the raw binary via include_bytes!
    // This is vastly faster than emitting an 8-million-entry array literal.
    // The path uses CARGO_MANIFEST_DIR so it resolves unambiguously regardless
    // of whether include! preserves the includer's or the included file's path.
    let rs_path = target_dir.join("minixfs_data.rs");
    let rs = format!(
        concat!(
            "// Auto-generated by tools/mkminixfs.rs\n",
            "#[allow(dead_code)]\n",
            "#[allow(unused_attributes)]\n",
            "#[unsafe(link_section = \".minixfs\")]\n",
            "#[used]\n",
            "pub static MINIXFS_IMG: [u8; {}] = *include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../target/minixfs.img\"));\n",
            "pub const MINIXFS_IMG_LEN: usize = {};\n",
        ),
        image.len(),
        image.len(),
    );
    fs::write(&rs_path, &rs).unwrap();
    println!("minixfs_data.rs: {} bytes written", rs.len());
    println!("Done.");
}
