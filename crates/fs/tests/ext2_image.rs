//! End-to-end ext2 read/write path against a real `mke2fs` image.
//!
//! This lives in its own test binary on purpose: the block cache and the
//! server globals are process-wide, so a test that mounts a filesystem and
//! performs I/O would otherwise share them with the other servers' unit tests
//! (which rely on state this test changes, e.g. `iso9660::block_read`'s
//! "buffer pool not initialised" case).
//!
//! The fixture is produced by the reference implementation, not by this port:
//!
//! ```text
//! mkdir -p root/sub
//! printf 'hello ext2\n' > root/hello.txt
//! printf 'inner file\n' > root/sub/inner.txt
//! ln -s hello.txt root/short-link
//! ln -s $(printf 'a%.0s' {1..80}) root/long-link
//! mke2fs -q -t ext2 -b 1024 -I 128 -N 32 -O ^resize_inode,^dir_index \
//!        -d root -U 00000000-0000-0000-0000-000000000000 img 512
//! ```
//!
//! so what is being tested is the server's ability to read and write what a
//! real ext2 filesystem looks like on disk.

use fs::block_io;
use fs::ext2::consts::*;
use fs::ext2::glo;
use fs::ext2::inode::{get_inode, put_inode};
use fs::ext2::link::{fs_ftrunc, fs_link, fs_rdlink, fs_rename, fs_unlink};
use fs::ext2::misc::{fs_bpeek, fs_flush};
use fs::ext2::mount::fs_readsuper;
use fs::ext2::open::{fs_create, fs_mkdir};
use fs::ext2::path::fs_lookup;
use fs::ext2::protect::fs_getdents;
use fs::ext2::read::{fs_readwrite, read_map};
use fs::ext2::stadir::build_stat;
use fs::ext2::super_::get_super;
use fs::ext2::time::fs_utime;
use fs::ext2::write::write_map;

/// 512 KiB image holding a few files, two symlinks and one subdirectory.
const IMAGE: &[u8] = include_bytes!("../testdata/ext2-mini.img");

/// Device number the fixture is mounted as: any value but `NO_DEV` works,
/// since the host has no block driver to register.
const DEV: u32 = 1;

/// VFS's own `PATH_RET_SYMLINK`: look up the link itself, do not follow it.
const PATH_RET_SYMLINK: u32 = 4;

// The handlers want their request in `m_in`, the receive loop having unpacked
// the path-carrying ones into `cch`/`user_path`. On the host there is no VFS
// and no kernel to copy a name grant through, so these drivers fill in exactly
// what the loop (or a grant copy on the target) would have: the same payload
// layouts VFS's `vfs/request.rs` builds.

/// REQ_LOOKUP of `name` in `dir_ino`; returns `(status, inode)`.
fn lookup(dir_ino: u32, name: &[u8], flags: u32) -> (i32, u32) {
    unsafe {
        let ext2 = glo::ext2_ptr();
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..8].copy_from_slice(&(dir_ino as u64).to_le_bytes());
        raw[8..16].copy_from_slice(&(ROOT_INODE as u64).to_le_bytes());
        raw[16..20].copy_from_slice(&flags.to_le_bytes());
        raw[20..24].copy_from_slice(&((name.len() + 1) as u32).to_le_bytes());
        raw[24..24 + name.len()].copy_from_slice(name);
        raw[24 + name.len()] = 0;

        let r = fs_lookup();
        let raw = &(*ext2).m_out.m_payload.raw;
        (r, u32::from_le_bytes(raw[20..24].try_into().unwrap()))
    }
}

/// The stored link count of `ino`, with the inode released again.
fn nlink(ino: u32) -> u16 {
    unsafe {
        let ip = get_inode(DEV, ino);
        let n = (*ip).i_links_count;
        put_inode(ip);
        n
    }
}

/// Put `name` where the receive loop leaves an entry name, NUL-terminated.
fn set_name(ext2: *mut glo::Ext2Global, name: &[u8], off: usize) {
    unsafe {
        let up = &mut (*ext2).user_path;
        up[off..off + name.len()].copy_from_slice(name);
        up[off + name.len()] = 0;
    }
}

/// REQ_CREATE of `name` in `dir_ino`; returns `(status, inode)`.
fn create(dir_ino: u32, name: &[u8], mode: u16) -> (i32, u32) {
    unsafe {
        let ext2 = glo::ext2_ptr();
        (*ext2).cch[0] = dir_ino as i32;
        (*ext2).cch[1] = mode as i32;
        (*ext2).cch[2] = 0; // uid
        (*ext2).cch[3] = 0; // gid
        (*ext2).caller_uid = SU_UID as u16;
        (*ext2).caller_gid = 0;
        set_name(ext2, name, 0);

        let r = fs_create();
        let raw = &(*ext2).m_out.m_payload.raw;
        (r, u32::from_le_bytes(raw[8..12].try_into().unwrap()))
    }
}

/// REQ_MKDIR of `name` in `dir_ino`. The reply carries no inode, so callers
/// look the new directory up.
fn mkdir(dir_ino: u32, name: &[u8], mode: u16) -> i32 {
    unsafe {
        let ext2 = glo::ext2_ptr();
        (*ext2).cch[0] = dir_ino as i32;
        (*ext2).cch[1] = mode as i32;
        (*ext2).cch[2] = 0;
        (*ext2).cch[3] = 0;
        (*ext2).caller_uid = SU_UID as u16;
        (*ext2).caller_gid = 0;
        set_name(ext2, name, 0);
        fs_mkdir()
    }
}

/// REQ_LINK of `name` for `file_ino` into `dir_ino`.
fn link(dir_ino: u32, file_ino: u32, name: &[u8]) -> i32 {
    unsafe {
        let ext2 = glo::ext2_ptr();
        (*ext2).caller_uid = SU_UID as u16;
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&file_ino.to_le_bytes());
        raw[4..8].copy_from_slice(&dir_ino.to_le_bytes());
        raw[8..12].copy_from_slice(&0i32.to_le_bytes()); // name grant
        raw[16..24].copy_from_slice(&((name.len() + 1) as u64).to_le_bytes());
        set_name(ext2, name, 0);
        fs_link()
    }
}

/// REQ_UNLINK (`rmdir` false) or REQ_RMDIR (`rmdir` true) of `name`.
fn unlink(dir_ino: u32, name: &[u8], rmdir: bool) -> i32 {
    unsafe {
        let ext2 = glo::ext2_ptr();
        (*ext2).req_nr = if rmdir {
            REQ_RMDIR - FS_BASE
        } else {
            REQ_UNLINK - FS_BASE
        };
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&dir_ino.to_le_bytes());
        raw[8..12].copy_from_slice(&0i32.to_le_bytes()); // name grant
        raw[16..24].copy_from_slice(&((name.len() + 1) as u64).to_le_bytes());
        set_name(ext2, name, 0);
        fs_unlink()
    }
}

/// REQ_RENAME of `old_name` in `old_dir` to `new_name` in `new_dir`. The two
/// names get a slot each in `user_path` (`link.rs`'s `NAME_SLOT`).
fn rename(old_dir: u32, old_name: &[u8], new_dir: u32, new_name: &[u8]) -> i32 {
    unsafe {
        let ext2 = glo::ext2_ptr();
        (*ext2).caller_uid = SU_UID as u16;
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&old_dir.to_le_bytes());
        raw[4..8].copy_from_slice(&new_dir.to_le_bytes());
        raw[8..16].copy_from_slice(&((old_name.len() + 1) as u64).to_le_bytes());
        raw[16..24].copy_from_slice(&((new_name.len() + 1) as u64).to_le_bytes());
        raw[24..28].copy_from_slice(&0i32.to_le_bytes()); // old name grant
        raw[28..32].copy_from_slice(&0i32.to_le_bytes()); // new name grant
        set_name(ext2, old_name, 0);
        set_name(ext2, new_name, EXT2_NAME_MAX + 1);
        fs_rename()
    }
}

/// REQ_FTRUNC of `ino`: an `end` of zero truncates to `start`, otherwise the
/// half-open range `[start, end)` is freed.
fn ftrunc(ino: u32, start: i64, end: i64) -> i32 {
    unsafe {
        let ext2 = glo::ext2_ptr();
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&ino.to_le_bytes());
        raw[8..16].copy_from_slice(&start.to_le_bytes());
        raw[16..24].copy_from_slice(&end.to_le_bytes());
        fs_ftrunc()
    }
}

/// REQ_RDLINK of `ino`; returns `(status, bytes the target occupies)`.
fn readlink(ino: u32, mem_size: usize) -> (i32, u64) {
    unsafe {
        let ext2 = glo::ext2_ptr();
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&ino.to_le_bytes());
        raw[8..12].copy_from_slice(&0i32.to_le_bytes()); // buffer grant
        raw[16..24].copy_from_slice(&(mem_size as u64).to_le_bytes());
        let r = fs_rdlink();
        let raw = &(*ext2).m_out.m_payload.raw;
        (r, u64::from_le_bytes(raw[0..8].try_into().unwrap()))
    }
}

#[test]
fn test_read_and_write_on_a_real_ext2_image() {
    unsafe {
        // The image is copied first: `include_bytes!` lands in read-only
        // memory, and the server writes inodes back through the block cache
        // (through the raw pointer it holds, so the binding itself is not
        // mutated here).
        let disk = Vec::from(IMAGE);
        fs::ext2::main::init_server();
        block_io::ram_disk_init(disk.as_ptr(), disk.len());
        libs::libminixfs::cache::lmfs_set_block_io(block_io::ram_disk_io);

        let ext2 = glo::ext2_ptr();
        (*ext2).m_in.m_source = 1;
        (*ext2).m_in.m_payload.m1.m1i1 = DEV as i32;
        assert_eq!(fs_readsuper(), OK, "mount of the fixture failed");

        // read_super switches the cache to the filesystem's block size before
        // it resolves any block number through it.
        assert_eq!(libs::libminixfs::cache::lmfs_fs_block_size(), 1024);

        // VFS's readsuper reply describes the root inode.
        let raw = (*ext2).m_out.m_payload.raw;
        assert_eq!(
            u32::from_le_bytes(raw[12..16].try_into().unwrap()),
            ROOT_INODE,
            "readsuper did not report the root inode"
        );

        let sp = get_super(DEV);
        assert!(!sp.is_null());

        // REQ_LOOKUP for "hello.txt" in the root directory.
        let (r, file_ino) = lookup(ROOT_INODE, b"hello.txt", 0);
        assert_eq!(r, OK, "lookup of hello.txt failed");
        assert!(
            file_ino >= EXT2_GOOD_OLD_FIRST_INO,
            "lookup returned inode {file_ino}"
        );

        // The inode's metadata comes from the on-disk inode table.
        let fip = get_inode(DEV, file_ino);
        assert!(!fip.is_null());
        let st = build_stat(fip).expect("stat of the looked-up inode");
        assert_eq!(st.st_size, 11);
        assert_eq!(st.st_mode & I_TYPE as u32, I_REGULAR as u32);
        assert_eq!(st.st_blksize, 1024);

        // Its data block is mapped: a hole would read back as zeros, so this
        // is what shows the on-disk block pointer was followed.
        assert_ne!(read_map(fip, 0, 0), NO_BLOCK, "file block not mapped");

        // REQ_READ of the whole file. The grant copy is a kernel call and is
        // compiled out on the host, so what is checked here is the block walk
        // and the counts VFS gets back.
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&file_ino.to_le_bytes());
        raw[8..16].copy_from_slice(&0i64.to_le_bytes());
        raw[16..20].copy_from_slice(&0i32.to_le_bytes());
        raw[24..32].copy_from_slice(&11u64.to_le_bytes());
        (*ext2).req_nr = REQ_READ - FS_BASE;
        // The status is the error code (VFS rejects only negatives) and the
        // byte count travels in the reply: a new position of 11 means the walk
        // consumed the whole file.
        assert_eq!(fs_readwrite(), OK, "read reported an error");
        let raw = (*ext2).m_out.m_payload.raw;
        assert_eq!(i64::from_le_bytes(raw[0..8].try_into().unwrap()), 11);
        assert_eq!(u32::from_le_bytes(raw[8..12].try_into().unwrap()), 11);
        put_inode(fip);

        // REQ_GETDENTS on the root directory packs directory entries (each at
        // least 13 bytes of `struct dirent`) for the caller.
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&ROOT_INODE.to_le_bytes());
        raw[8..16].copy_from_slice(&0i64.to_le_bytes());
        raw[16..20].copy_from_slice(&0i32.to_le_bytes());
        assert_eq!(fs_getdents(), OK);
        let raw = (*ext2).m_out.m_payload.raw;
        let packed = i32::from_le_bytes(raw[8..12].try_into().unwrap());
        assert!(packed >= 13, "no directory entries were packed: {packed}");

        // ---- symlinks ----
        //
        // A lookup follows a symlink unless VFS asks for the link itself.
        let (r, target) = lookup(ROOT_INODE, b"short-link", 0);
        assert_eq!(r, OK);
        assert_eq!(target, file_ino, "short-link did not resolve to hello.txt");

        let (r, short_link) = lookup(ROOT_INODE, b"short-link", PATH_RET_SYMLINK);
        assert_eq!(r, OK);
        assert_ne!(short_link, file_ino, "the link itself was not returned");

        // rdlink: a target under 60 bytes is stored in the inode's block
        // array, a longer one in a data block, and both are reported by length.
        let (r, n) = readlink(short_link, 64);
        assert_eq!(r, OK);
        assert_eq!(n, 9, "fast symlink target length");
        let recorded = (*ext2).cch[0];
        assert_eq!(recorded, 9);

        let (r, long_link) = lookup(ROOT_INODE, b"long-link", PATH_RET_SYMLINK);
        assert_eq!(r, OK);
        let (r, n) = readlink(long_link, 1024);
        assert_eq!(r, OK);
        assert_eq!(n, 80, "slow symlink target length");

        // The buffer size is a cap, not a length.
        let (r, n) = readlink(long_link, 5);
        assert_eq!(r, OK);
        assert_eq!(n, 5);

        // ---- write path: allocation, indirection and freeing ----
        //
        // The data itself moves through a grant (a kernel call, compiled out
        // on the host), so what is verifiable here is the allocation and the
        // mapping the write leaves behind.
        let fip = get_inode(DEV, file_ino);
        assert!(!fip.is_null());
        let free_before = (*sp).s_free_blocks_count;

        // Write 3000 bytes from 0: the first block is already in the image,
        // the next two are holes that have to be allocated.
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&file_ino.to_le_bytes());
        raw[8..16].copy_from_slice(&0i64.to_le_bytes());
        raw[16..20].copy_from_slice(&0i32.to_le_bytes());
        raw[24..32].copy_from_slice(&3000u64.to_le_bytes());
        (*ext2).req_nr = REQ_WRITE - FS_BASE;
        assert_eq!(fs_readwrite(), OK, "write reported an error");
        let raw = (*ext2).m_out.m_payload.raw;
        assert_eq!(i64::from_le_bytes(raw[0..8].try_into().unwrap()), 3000);
        assert_eq!((*fip).i_size, 3000, "size was not extended");
        assert_ne!(read_map(fip, 1024, 0), NO_BLOCK, "hole block not allocated");
        assert!(
            (*sp).s_free_blocks_count < free_before,
            "the new blocks did not come from the bitmap: {} -> {}",
            free_before,
            (*sp).s_free_blocks_count
        );

        // Past the 12 direct blocks: this needs the single indirect block.
        let indirect_pos = EXT2_NDIR_BLOCKS as u64 * 1024;
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&file_ino.to_le_bytes());
        raw[8..16].copy_from_slice(&(indirect_pos as i64).to_le_bytes());
        raw[16..20].copy_from_slice(&0i32.to_le_bytes());
        raw[24..32].copy_from_slice(&1024u64.to_le_bytes());
        (*ext2).req_nr = REQ_WRITE - FS_BASE;
        assert_eq!(fs_readwrite(), OK, "indirect write reported an error");
        assert_ne!(
            (*fip).i_block[EXT2_IND_BLOCK],
            NO_BLOCK,
            "no single indirect block was allocated"
        );
        assert_ne!(
            read_map(fip, indirect_pos, 0),
            NO_BLOCK,
            "indirectly mapped block missing"
        );

        // Freeing the indirect block's only entry has to drop the indirect
        // block itself, not just the entry.
        assert_eq!(write_map(fip, indirect_pos, 0, WMAP_FREE as i32), OK);
        assert_eq!(
            (*fip).i_block[EXT2_IND_BLOCK],
            NO_BLOCK,
            "the empty indirect block was kept"
        );
        assert_eq!(read_map(fip, indirect_pos, 0), NO_BLOCK);
        put_inode(fip);

        // ---- ftrunc ----
        //
        // Truncating back to the original 11 bytes has to give the blocks the
        // writes took straight back to the bitmap.
        let free_before = (*sp).s_free_blocks_count;
        let fip = get_inode(DEV, file_ino);
        assert_ne!((*fip).i_size, 11);

        assert_eq!(ftrunc(file_ino, 11, 0), OK);
        assert_eq!((*fip).i_size, 11, "truncate did not cut the size");
        assert_eq!(
            (*fip).i_block[EXT2_IND_BLOCK],
            NO_BLOCK,
            "the indirect block survived the truncate"
        );
        assert_eq!(
            read_map(fip, 1024, 0),
            NO_BLOCK,
            "a freed block is still mapped"
        );
        assert!(
            (*sp).s_free_blocks_count > free_before,
            "no blocks were returned to the bitmap: {} -> {}",
            free_before,
            (*sp).s_free_blocks_count
        );

        // Truncating to zero frees the file's last data block, and growing it
        // again leaves a hole that reads as zeros.
        assert_eq!(ftrunc(file_ino, 0, 0), OK);
        assert_eq!((*fip).i_size, 0);
        assert_eq!(read_map(fip, 0, 0), NO_BLOCK);
        assert_eq!(ftrunc(file_ino, 2048, 0), OK);
        assert_eq!((*fip).i_size, 2048);
        assert_eq!(
            read_map(fip, 0, 0),
            NO_BLOCK,
            "the regrown range is not a hole"
        );
        assert_eq!(ftrunc(file_ino, 0, 0), OK);

        // Freeing a sub-range only drops the blocks it covers.
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&file_ino.to_le_bytes());
        raw[8..16].copy_from_slice(&0i64.to_le_bytes());
        raw[16..20].copy_from_slice(&0i32.to_le_bytes());
        raw[24..32].copy_from_slice(&3000u64.to_le_bytes());
        (*ext2).req_nr = REQ_WRITE - FS_BASE;
        assert_eq!(fs_readwrite(), OK);
        assert_eq!(ftrunc(file_ino, 1024, 2048), OK);
        assert_eq!((*fip).i_size, 3000, "a range free must not set the size");
        assert_eq!(
            read_map(fip, 1024, 0),
            NO_BLOCK,
            "the freed range is still mapped"
        );
        assert_ne!(
            read_map(fip, 2048, 0),
            NO_BLOCK,
            "the range after it was freed too"
        );
        assert_eq!(ftrunc(file_ino, 0, 0), OK);
        put_inode(fip);

        // ---- link / unlink ----
        let fip = get_inode(DEV, file_ino);
        let nlink_before = (*fip).i_links_count;

        assert_eq!(link(ROOT_INODE, file_ino, b"hard.txt"), OK, "link failed");
        assert_eq!(
            (*fip).i_links_count,
            nlink_before + 1,
            "the link count was not raised"
        );
        let (r, ino) = lookup(ROOT_INODE, b"hard.txt", 0);
        assert_eq!(r, OK);
        assert_eq!(ino, file_ino, "the hard link points elsewhere");

        // A second entry for the same name is refused.
        assert_eq!(link(ROOT_INODE, file_ino, b"hard.txt"), EEXIST);

        let r_unlink = unlink(ROOT_INODE, b"hard.txt", false);
        assert_eq!(r_unlink, OK, "unlink failed");
        assert_eq!(
            (*fip).i_links_count,
            nlink_before,
            "the link count was not dropped"
        );
        assert_eq!(lookup(ROOT_INODE, b"hard.txt", 0).0, ENOENT);
        put_inode(fip);

        // ---- create / unlink of the last link ----
        let inodes_before = (*sp).s_free_inodes_count;
        let (r, scratch) = create(ROOT_INODE, b"scratch.txt", 0o644);
        assert_eq!(r, OK, "create failed");
        assert_eq!(
            (*sp).s_free_inodes_count,
            inodes_before - 1,
            "the create did not take an inode"
        );
        assert_eq!(lookup(ROOT_INODE, b"scratch.txt", 0).1, scratch);

        assert_eq!(unlink(ROOT_INODE, b"scratch.txt", false), OK);
        assert_eq!(lookup(ROOT_INODE, b"scratch.txt", 0).0, ENOENT);
        assert_eq!(
            (*sp).s_free_inodes_count,
            inodes_before,
            "the unlinked inode was not freed"
        );

        // ---- mkdir / rmdir ----
        assert_eq!(mkdir(ROOT_INODE, b"scratchdir", 0o755), OK, "mkdir failed");
        let (r, dir_ino) = lookup(ROOT_INODE, b"scratchdir", 0);
        assert_eq!(r, OK);
        let dip = get_inode(DEV, dir_ino);
        assert_eq!(
            (*dip).i_mode & I_TYPE,
            I_DIRECTORY,
            "mkdir made a non-directory"
        );
        put_inode(dip);

        // The new directory carries its own "." and ".." entries.
        assert_eq!(lookup(dir_ino, b".", 0), (OK, dir_ino));
        assert_eq!(lookup(dir_ino, b"..", 0), (OK, ROOT_INODE));

        // An rmdir of a directory that still has entries is refused.
        let (r, _) = create(dir_ino, b"inner", 0o644);
        assert_eq!(r, OK);
        assert_eq!(unlink(ROOT_INODE, b"scratchdir", true), ENOTEMPTY);

        assert_eq!(unlink(dir_ino, b"inner", false), OK);
        let inodes_before = (*sp).s_free_inodes_count;
        assert_eq!(unlink(ROOT_INODE, b"scratchdir", true), OK, "rmdir failed");
        assert_eq!(lookup(ROOT_INODE, b"scratchdir", 0).0, ENOENT);
        assert_eq!(
            (*sp).s_free_inodes_count,
            inodes_before + 1,
            "rmdir did not free the directory inode"
        );

        // ---- rename ----
        //
        // Within one directory, and then across two.
        assert_eq!(
            rename(ROOT_INODE, b"hello.txt", ROOT_INODE, b"hello2.txt"),
            OK
        );
        assert_eq!(lookup(ROOT_INODE, b"hello.txt", 0).0, ENOENT);
        assert_eq!(
            lookup(ROOT_INODE, b"hello2.txt", 0).1,
            file_ino,
            "the entry moved"
        );

        let (r, sub_ino) = lookup(ROOT_INODE, b"sub", 0);
        assert_eq!(r, OK);
        assert_eq!(
            rename(ROOT_INODE, b"hello2.txt", sub_ino, b"moved.txt"),
            OK,
            "rename across directories failed"
        );
        assert_eq!(lookup(ROOT_INODE, b"hello2.txt", 0).0, ENOENT);
        assert_eq!(lookup(sub_ino, b"moved.txt", 0).1, file_ino);

        // Renaming a directory across parents rewrites its ".." and moves the
        // parent link counts with it.
        assert_eq!(mkdir(ROOT_INODE, b"d1", 0o755), OK);
        let (r, d1_ino) = lookup(ROOT_INODE, b"d1", 0);
        assert_eq!(r, OK);
        let sub_links = nlink(sub_ino);

        assert_eq!(
            rename(ROOT_INODE, b"d1", sub_ino, b"d1"),
            OK,
            "dir rename failed"
        );
        assert_eq!(lookup(ROOT_INODE, b"d1", 0).0, ENOENT);
        assert_eq!(lookup(sub_ino, b"d1", 0).1, d1_ino);
        assert_eq!(
            lookup(d1_ino, b"..", 0),
            (OK, sub_ino),
            "\"..\" still points at the old parent"
        );
        assert_eq!(
            nlink(sub_ino),
            sub_links + 1,
            "the new parent did not gain a link"
        );

        // A directory cannot be moved under itself.
        assert_eq!(rename(sub_ino, b"d1", d1_ino, b"d1"), EINVAL);

        // ---- bpeek / flush ----
        //
        // bpeek faults a device's blocks into the cache the VM server shares,
        // and the server has not opted into that cache (nor could it: the
        // fixture's 1024-byte blocks are not a page), so it is refused rather
        // than reported as done. flush must refuse to drop the blocks of the
        // device it is serving.
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&DEV.to_le_bytes());
        raw[8..16].copy_from_slice(&0i64.to_le_bytes());
        raw[24..32].copy_from_slice(&1024u64.to_le_bytes());
        assert_eq!(fs_bpeek(), libs::libminixfs::errors::ENXIO);

        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&DEV.to_le_bytes());
        assert_eq!(
            fs_flush(),
            EBUSY,
            "flush dropped the mounted device's blocks"
        );

        // ---- utime ----
        //
        // ext2 keeps whole seconds; UTIME_OMIT leaves a field alone.
        let fip = get_inode(DEV, file_ino);
        let atime_before = (*fip).i_atime;
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&file_ino.to_le_bytes());
        raw[8..16].copy_from_slice(&111i64.to_le_bytes()); // actime
        raw[16..24].copy_from_slice(&2222i64.to_le_bytes()); // modtime
        raw[24..28].copy_from_slice(&(UTIME_OMIT as i32).to_le_bytes());
        raw[28..32].copy_from_slice(&0i32.to_le_bytes());
        assert_eq!(fs_utime(), OK, "utime failed");
        assert_eq!((*fip).i_atime, atime_before, "UTIME_OMIT touched atime");
        assert_eq!((*fip).i_mtime, 2222, "modtime was not stored");

        // UTIME_NOW marks the field for update instead of storing a time.
        let raw = &mut (*ext2).m_in.m_payload.raw;
        raw[0..4].copy_from_slice(&file_ino.to_le_bytes());
        raw[24..28].copy_from_slice(&(UTIME_NOW as i32).to_le_bytes());
        raw[28..32].copy_from_slice(&(UTIME_OMIT as i32).to_le_bytes());
        assert_eq!(fs_utime(), OK);
        assert_eq!(
            (*fip).i_update,
            ATIME | CTIME,
            "UTIME_NOW did not mark atime"
        );
        put_inode(fip);
    }
}
