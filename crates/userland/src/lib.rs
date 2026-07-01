//! Userland command implementations.
//!
//! Each command is a public function taking `&[&str]` arguments and
//! returning an `i32` exit code. The binary entry points in `src/bin/`
//! just parse argv from the kernel and call these functions.
//!
//! This layout makes every command testable via `#[cfg(test)]`.

#![no_std]

/// Write a byte slice to file descriptor 1 (stdout).
pub fn write_out(s: &[u8]) {
    minix_rt::write(1, s);
}

/// Write a byte slice to file descriptor 2 (stderr).
pub fn write_err(s: &[u8]) {
    minix_rt::write(2, s);
}

/// Convert a null-terminated argv pointer into a slice of string slices.
/// Returns (arg_count, arg_slices) packed into a fixed-size buffer.
pub fn parse_args<'a>(argc: i32, argv: *const *const u8, buf: &'a mut [&str; 64]) -> &'a [&'a str] {
    let count = (argc as usize).min(64).min(buf.len());
    for i in 0..count {
        let ptr = unsafe { argv.add(i).read() };
        let mut len = 0usize;
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        // Store as &str (ASCII-safe for command names)
        buf[i] = unsafe { core::str::from_utf8_unchecked(slice) };
    }
    &buf[..count]
}

/// echo — print arguments to stdout separated by spaces, ending with newline.
pub fn echo(args: &[&str]) -> i32 {
    for (i, arg) in args.iter().enumerate().skip(1) {
        if i > 1 {
            write_out(b" ");
        }
        write_out(arg.as_bytes());
    }
    write_out(b"\n");
    0
}

/// cat — concatenate files and print to stdout.
/// With no arguments, reads from stdin (fd 0).
pub fn cat(args: &[&str]) -> i32 {
    let mut exit_code = 0;
    if args.len() <= 1 {
        // Read from stdin
        let mut buf = [0u8; 8192];
        loop {
            let n = minix_rt::read(0, &mut buf);
            if n <= 0 {
                break;
            }
            write_out(&buf[..n as usize]);
        }
        return 0;
    }

    for path in &args[1..] {
        // NR_OPEN = 4, O_RDONLY = 0
        let fd = unsafe { minix_rt::syscall3(4, path.as_ptr() as u64, path.len() as u64, 0) };
        if fd < 0 {
            write_err(b"cat: ");
            write_err(path.as_bytes());
            write_err(b": cannot open\n");
            exit_code = 1;
            continue;
        }
        let mut buf = [0u8; 8192];
        loop {
            // NR_READ = 2
            let n = unsafe {
                minix_rt::syscall3(2, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64)
            };
            if n <= 0 {
                break;
            }
            write_out(&buf[..n as usize]);
        }
        // NR_CLOSE = 5
        unsafe {
            minix_rt::syscall1(5, fd as u64);
        }
    }
    exit_code
}

/// cp — copy file src to dst.
pub fn cp(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_err(b"cp: missing file arguments\n");
        return 1;
    }
    let src = &args[1];
    let dst = &args[2];

    // Open source O_RDONLY (0)
    let src_fd = unsafe { minix_rt::syscall3(4, src.as_ptr() as u64, src.len() as u64, 0) };
    if src_fd < 0 {
        write_err(b"cp: cannot open ");
        write_err(src.as_bytes());
        write_err(b"\n");
        return 1;
    }

    // Open/create destination O_WRONLY | O_CREAT | O_TRUNC = 0x201
    let dst_fd = unsafe { minix_rt::syscall3(4, dst.as_ptr() as u64, dst.len() as u64, 0x201) };
    if dst_fd < 0 {
        write_err(b"cp: cannot create ");
        write_err(dst.as_bytes());
        write_err(b"\n");
        unsafe {
            minix_rt::syscall1(5, src_fd as u64);
        }
        return 1;
    }

    let mut buf = [0u8; 8192];
    loop {
        let n = unsafe {
            minix_rt::syscall3(2, src_fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64)
        };
        if n <= 0 {
            break;
        }
        // NR_WRITE = 3
        unsafe {
            minix_rt::syscall3(3, dst_fd as u64, buf.as_ptr() as u64, n as u64);
        }
    }

    unsafe {
        minix_rt::syscall1(5, src_fd as u64);
    }
    unsafe {
        minix_rt::syscall1(5, dst_fd as u64);
    }
    0
}

/// Minix `struct dirent` — format returned by `getdents`.
/// Layout matches `/usr/include/sys/dirent.h`.
#[repr(C)]
pub struct Dirent {
    pub d_fileno: u64,
    pub d_reclen: u16,
    pub d_namlen: u16,
    pub d_type: u8,
    pub d_name: [u8; 0], // flexible array, accessed via pointer arithmetic
}

const DIRENT_NAME_OFF: usize = 13; // offset of d_name in struct Dirent

/// ls — list directory contents.
pub fn ls(args: &[&str]) -> i32 {
    let dir = if args.len() > 1 { &args[1] } else { "." };
    // Open directory O_RDONLY (0)
    let fd = unsafe { minix_rt::syscall3(4, dir.as_ptr() as u64, dir.len() as u64, 0) };
    if fd < 0 {
        write_err(b"ls: cannot access ");
        write_err(dir.as_bytes());
        write_err(b"\n");
        return 1;
    }
    let mut buf = [0u8; 4096];
    let n = unsafe { minix_rt::syscall3(47, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) };
    if n <= 0 {
        // Fallback: just print the directory name if getdents fails
        write_out(dir.as_bytes());
        write_out(b"\n");
    } else {
        let mut off = 0usize;
        while off < n as usize {
            if off + DIRENT_NAME_OFF > n as usize {
                break;
            }
            let reclen = u16::from_ne_bytes([buf[off + 8], buf[off + 9]]);
            if reclen == 0 || off + reclen as usize > n as usize {
                break;
            }
            let namlen = u16::from_ne_bytes([buf[off + 10], buf[off + 11]]);
            if namlen > 0 && off + DIRENT_NAME_OFF + namlen as usize <= n as usize {
                let name = &buf[off + DIRENT_NAME_OFF..off + DIRENT_NAME_OFF + namlen as usize];
                // Skip . and ..
                if name != b"." && name != b".." {
                    write_out(name);
                    write_out(b"  ");
                }
            }
            off += reclen as usize;
        }
        write_out(b"\n");
    }
    unsafe {
        minix_rt::syscall1(5, fd as u64);
    }
    0
}

/// mkdir — create directories.
pub fn mkdir(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_err(b"mkdir: missing operand\n");
        return 1;
    }
    let mut exit_code = 0;
    for path in &args[1..] {
        let ret = minix_rt::mkdir(path.as_bytes(), 0o755);
        if ret < 0 {
            write_err(b"mkdir: ");
            write_err(path.as_bytes());
            write_err(b": ");
            write_err(errstr(-ret as i32));
            write_err(b"\n");
            exit_code = 1;
        }
    }
    exit_code
}

/// rm — remove files or directories.
pub fn rm(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_err(b"rm: missing operand\n");
        return 1;
    }
    let mut paths_start = 1;
    let mut recursive = false;
    if args.len() > 1 && args[1] == "-r" {
        recursive = true;
        paths_start = 2;
    }
    if paths_start >= args.len() {
        write_err(b"rm: missing operand\n");
        return 1;
    }
    let mut exit_code = 0;
    for path in &args[paths_start..] {
        let ret = if recursive {
            rm_recursive(path.as_bytes())
        } else {
            rm_single(path.as_bytes())
        };
        if ret < 0 {
            write_err(b"rm: ");
            write_err(path.as_bytes());
            write_err(b": ");
            write_err(errstr(-ret as i32));
            write_err(b"\n");
            exit_code = 1;
        }
    }
    exit_code
}

/// Remove a single file (or empty directory). Returns 0 on success,
/// or negative errno on failure.
fn rm_single(path: &[u8]) -> i32 {
    let ret = minix_rt::unlink(path);
    if ret >= 0 {
        return 0;
    }
    let err = -ret as i32;
    if err == 21 {
        // EISDIR — try rmdir
        let r = minix_rt::rmdir(path);
        if r >= 0 {
            return 0;
        }
        return r as i32;
    }
    ret as i32
}

/// Recursively remove a directory tree.
fn rm_recursive(path: &[u8]) -> i32 {
    // Try as file first
    let ret = minix_rt::unlink(path);
    if ret >= 0 {
        return 0;
    }
    let err = -ret as i32;
    if err != 21 {
        // Not EISDIR — some other error or already removed
        return ret as i32;
    }

    // Open the directory
    let fd = minix_rt::open(path, 0);
    if fd < 0 {
        return fd as i32;
    }

    // Read and process entries
    let mut buf = [0u8; 4096];
    loop {
        let n =
            unsafe { minix_rt::syscall3(47, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) };
        if n <= 0 {
            break;
        }
        let mut off = 0usize;
        while off < n as usize {
            if off + DIRENT_NAME_OFF > n as usize {
                break;
            }
            let reclen = u16::from_ne_bytes([buf[off + 8], buf[off + 9]]);
            if reclen == 0 || off + reclen as usize > n as usize {
                break;
            }
            let namlen = u16::from_ne_bytes([buf[off + 10], buf[off + 11]]);
            if namlen == 0 || off + DIRENT_NAME_OFF + namlen as usize > n as usize {
                off += reclen as usize;
                continue;
            }
            let name = &buf[off + DIRENT_NAME_OFF..off + DIRENT_NAME_OFF + namlen as usize];
            // Skip . and ..
            if name == b"." || name == b".." {
                off += reclen as usize;
                continue;
            }
            // Build child path: parent + "/" + name
            let mut child = [0u8; 1024];
            let plen = path.len().min(512);
            let nlen = namlen as usize;
            if plen + 1 + nlen > child.len() {
                off += reclen as usize;
                continue;
            }
            child[..plen].copy_from_slice(&path[..plen]);
            child[plen] = b'/';
            child[plen + 1..plen + 1 + nlen].copy_from_slice(name);
            let child_path = &child[..plen + 1 + nlen];

            // Recurse
            let r = rm_recursive(child_path);
            if r < 0 {
                return r;
            }
            off += reclen as usize;
        }
    }

    minix_rt::close(fd as i32);

    // Remove the now-empty directory
    let r = minix_rt::rmdir(path);
    if r < 0 {
        return r as i32;
    }
    0
}

/// ln — create hard links.
pub fn ln(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_err(b"ln: missing operand\n");
        return 1;
    }
    let target = args[1];
    let link_name = args[2];
    let ret = minix_rt::link(target.as_bytes(), link_name.as_bytes());
    if ret < 0 {
        write_err(b"ln: ");
        write_err(errstr(-ret as i32));
        write_err(b"\n");
        return 1;
    }
    0
}

/// chmod — change file mode.
pub fn chmod(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_err(b"chmod: missing operand\n");
        return 1;
    }
    // Parse octal mode (e.g., "755" → 0o755)
    let mode_str = args[1];
    let mode = match u32::from_str_radix(mode_str, 8) {
        Ok(m) if m <= 0o7777 => m,
        _ => {
            write_err(b"chmod: invalid mode: ");
            write_err(mode_str.as_bytes());
            write_err(b"\n");
            return 1;
        }
    };
    let mut exit_code = 0;
    for path in &args[2..] {
        let ret = minix_rt::chmod(path.as_bytes(), mode);
        if ret < 0 {
            write_err(b"chmod: ");
            write_err(path.as_bytes());
            write_err(b": ");
            write_err(errstr(-ret as i32));
            write_err(b"\n");
            exit_code = 1;
        }
    }
    exit_code
}

/// chown — change file owner.
pub fn chown(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_err(b"chown: missing operand\n");
        return 1;
    }
    // Parse owner:group (e.g., "100:100")
    let owner_str = args[1];
    let (owner, group) = if let Some(colon) = owner_str.as_bytes().iter().position(|&c| c == b':') {
        let owner_part = core::str::from_utf8(&owner_str.as_bytes()[..colon]).unwrap_or("0");
        let group_part = core::str::from_utf8(&owner_str.as_bytes()[colon + 1..]).unwrap_or("0");
        let uid: i32 = owner_part.parse().unwrap_or(0);
        let gid: i32 = group_part.parse().unwrap_or(0);
        (uid, gid)
    } else {
        let uid: i32 = owner_str.parse().unwrap_or(0);
        (uid, -1)
    };
    let mut exit_code = 0;
    for path in &args[2..] {
        let ret = minix_rt::chown(path.as_bytes(), owner, group);
        if ret < 0 {
            write_err(b"chown: ");
            write_err(path.as_bytes());
            write_err(b": ");
            write_err(errstr(-ret as i32));
            write_err(b"\n");
            exit_code = 1;
        }
    }
    exit_code
}

/// sync — synchronize cached writes.
pub fn sync(_args: &[&str]) -> i32 {
    write_out(b"sync\n");
    0
}

/// mknod — create a device node.
pub fn mknod(args: &[&str]) -> i32 {
    if args.len() < 4 {
        write_err(b"mknod: missing operand\n");
        return 1;
    }
    let path = args[1];
    let mode_str = args[2];
    let dev_str = args[3];
    let mode = match u32::from_str_radix(mode_str, 8) {
        Ok(m) if m <= 0o7777 => m,
        _ => {
            write_err(b"mknod: invalid mode\n");
            return 1;
        }
    };
    let dev: u64 = dev_str.parse().unwrap_or(0);
    let ret = minix_rt::mknod(path.as_bytes(), mode, dev);
    if ret < 0 {
        write_err(b"mknod: ");
        write_err(path.as_bytes());
        write_err(b": ");
        write_err(errstr(-ret as i32));
        write_err(b"\n");
        return 1;
    }
    0
}

/// reboot — reboot the system.
pub fn reboot(_args: &[&str]) -> i32 {
    write_out(b"reboot\n");
    0
}

/// fsck — file system check.
pub fn fsck(_args: &[&str]) -> i32 {
    write_out(b"fsck\n");
    0
}

/// sh — minimal shell.
pub fn sh(_args: &[&str]) -> i32 {
    write_out(b"sh: waiting for PM server...\n");
    0
}

/// Return a human-readable error string for a POSIX error code.
pub fn errstr(err: i32) -> &'static [u8] {
    match err {
        1 => b"Operation not permitted",
        2 => b"No such file or directory",
        5 => b"I/O error",
        9 => b"Bad file descriptor",
        12 => b"Cannot allocate memory",
        13 => b"Permission denied",
        14 => b"Bad address",
        17 => b"File exists",
        20 => b"Not a directory",
        21 => b"Is a directory",
        38 => b"Function not implemented",
        _ => b"Unknown error",
    }
}

/// init — first userspace process.
pub fn init(_args: &[&str]) -> i32 {
    // Print boot banner
    write_out(b"init: booting MINIX/Rust\n");
    write_out(b"init: pid=");
    let pid = minix_rt::getpid();
    // Simple decimal print for PID
    if pid >= 100 {
        write_out(&[b'0' + (pid / 100) as u8]);
    }
    if pid >= 10 {
        write_out(&[b'0' + ((pid / 10) % 10) as u8]);
    }
    write_out(&[b'0' + (pid % 10) as u8]);
    write_out(b"\n");

    // Loop forever — fork/exec/waitpid come when PM is live
    loop {
        unsafe { core::arch::asm!("pause") };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_echo() {
        assert_eq!(echo(&["echo", "hello"]), 0);
        assert_eq!(echo(&["echo"]), 0);
    }

    #[test]
    #[ignore = "requires MINIX syscall ABI (stdin read via NR_READ=2)"]
    fn test_cat_no_args() {
        assert_eq!(cat(&["cat"]), 0);
    }

    #[test]
    fn test_mkdir_no_args() {
        assert_eq!(mkdir(&["mkdir"]), 1);
    }

    #[test]
    fn test_rm_no_args() {
        assert_eq!(rm(&["rm"]), 1);
    }

    #[test]
    fn test_ln_no_args() {
        assert_eq!(ln(&["ln"]), 1);
    }

    #[test]
    #[ignore = "requires MINIX syscall ABI (link via NR_LINK=43)"]
    fn test_ln_two_args() {
        assert_eq!(ln(&["ln", "a", "b"]), 1);
    }

    #[test]
    fn test_chmod_no_args() {
        assert_eq!(chmod(&["chmod"]), 1);
    }

    #[test]
    fn test_chmod_invalid_mode() {
        assert_eq!(chmod(&["chmod", "invalid", "file"]), 1);
    }

    #[test]
    #[ignore = "requires MINIX syscall ABI (chmod via NR_CHMOD=44)"]
    fn test_chmod_two_args() {
        assert_eq!(chmod(&["chmod", "755", "file"]), 1);
    }

    #[test]
    fn test_chown_no_args() {
        assert_eq!(chown(&["chown"]), 1);
    }

    #[test]
    #[ignore = "requires MINIX syscall ABI (chown via NR_CHOWN=45)"]
    fn test_chown_two_args() {
        assert_eq!(chown(&["chown", "100", "file"]), 1);
    }

    #[test]
    fn test_mknod_no_args() {
        assert_eq!(mknod(&["mknod"]), 1);
    }

    #[test]
    fn test_mknod_invalid_mode() {
        assert_eq!(mknod(&["mknod", "dev", "invalid", "0"]), 1);
    }

    #[test]
    fn test_sync_stub() {
        assert_eq!(sync(&["sync"]), 0);
    }

    #[test]
    fn test_reboot_stub() {
        assert_eq!(reboot(&["reboot"]), 0);
    }

    #[test]
    fn test_fsck_stub() {
        assert_eq!(fsck(&["fsck"]), 0);
    }

    #[test]
    fn test_sh_stub() {
        assert_eq!(sh(&["sh"]), 0);
    }

    #[test]
    #[ignore = "infinite loop (init never returns)"]
    fn test_init_stub() {
        assert_eq!(init(&["/sbin/init"]), !0);
    }

    #[test]
    fn test_errstr_known_codes() {
        assert_eq!(errstr(1), b"Operation not permitted");
        assert_eq!(errstr(2), b"No such file or directory");
        assert_eq!(errstr(5), b"I/O error");
        assert_eq!(errstr(9), b"Bad file descriptor");
        assert_eq!(errstr(12), b"Cannot allocate memory");
        assert_eq!(errstr(13), b"Permission denied");
        assert_eq!(errstr(14), b"Bad address");
        assert_eq!(errstr(17), b"File exists");
        assert_eq!(errstr(20), b"Not a directory");
        assert_eq!(errstr(21), b"Is a directory");
        assert_eq!(errstr(38), b"Function not implemented");
    }

    #[test]
    fn test_errstr_unknown_code() {
        assert_eq!(errstr(99), b"Unknown error");
        assert_eq!(errstr(0), b"Unknown error");
    }

    #[test]
    fn test_errstr_negative() {
        assert_eq!(errstr(-1), b"Unknown error");
    }
}
