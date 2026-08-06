//! Userland command implementations.
//!
//! Each command is a public function taking `&[&str]` arguments and
//! returning an `i32` exit code. The binary entry points in `src/bin/`
//! just parse argv from the kernel and call these functions.
//!
//! This layout makes every command testable via `#[cfg(test)]`.

#![no_std]

use core::sync::atomic::{AtomicI32, Ordering};

/// When >= 0, all `write_out` calls are routed through this fd (via VFS)
/// instead of the kernel's serial shortcut on fd 1. Set by the shell's
/// redirect child after `fork`, so it is process-private.
static REDIRECT_FD: AtomicI32 = AtomicI32::new(-1);

/// Route subsequent `write_out` calls to `fd` (>= 0) or back to serial (-1).
#[cfg(target_os = "none")]
pub fn set_redirect_fd(fd: i32) {
    REDIRECT_FD.store(fd, Ordering::Relaxed);
}

/// Write a byte slice to file descriptor 1 (stdout).
pub fn write_out(s: &[u8]) {
    let fd = REDIRECT_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        write_fd(fd, s);
        return;
    }
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "aarch64"
    ))]
    unsafe {
        minix_rt::write(1, s.as_ptr(), s.len());
    }
}

/// echo — print arguments to stdout separated by spaces, ending with newline.
pub fn echo(args: &[&str]) -> i32 {
    echo_fd(args, -1)
}

/// echo with output fd (for redirect).
pub fn echo_fd(args: &[&str], out_fd: i32) -> i32 {
    for (i, arg) in args.iter().enumerate().skip(1) {
        if i > 1 {
            write_fd(out_fd, b" ");
        }
        write_fd(out_fd, arg.as_bytes());
    }
    write_fd(out_fd, b"\n");
    0
}

fn write_fd(fd: i32, s: &[u8]) {
    if fd >= 0 {
        let _ = unsafe { minix_std::fs::write(fd, s) };
    } else {
        write_out(s);
    }
}

pub fn print_dec(mut n: u32) {
    let mut buf = [0u8; 12];
    let mut i = 12;
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    write_err(&buf[i..]);
}

pub fn write_err(s: &[u8]) {
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "aarch64"
    ))]
    unsafe {
        minix_rt::write(2, s.as_ptr(), s.len());
    }
}

/// Convert a null-terminated argv pointer into a slice of string slices.
///
/// # Safety
///
/// `argv` must point to a valid null-terminated array of `argc` string
/// pointers, and each string must be null-terminated.
pub unsafe fn parse_args<'a>(
    argc: i32,
    argv: *const *const u8,
    buf: &'a mut [&str; 64],
) -> &'a [&'a str] {
    let count = (argc as usize).min(64).min(buf.len());
    for (i, slot) in buf.iter_mut().enumerate().take(count) {
        let ptr = unsafe { argv.add(i).read() };
        let mut len = 0usize;
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        *slot = unsafe { core::str::from_utf8_unchecked(slice) };
    }
    &buf[..count]
}

/// cat — concatenate files and print to stdout.
/// With no arguments, reads from stdin (fd 0).
pub fn cat(args: &[&str]) -> i32 {
    let mut exit_code = 0;
    if args.len() <= 1 {
        // Read from stdin. A pipe read returns EAGAIN (-11) when the pipe is
        // empty but a writer is still open (this port's pipes don't suspend
        // readers yet); retry so we wait for the writer's data instead of
        // treating a transient empty pipe as EOF.
        let mut buf = [0u8; 8192];
        loop {
            let n = minix_rt::read(0, &mut buf);
            if n == -11 {
                continue;
            }
            if n <= 0 {
                break;
            }
            write_out(&buf[..n as usize]);
        }
        return 0;
    }

    for path in &args[1..] {
        let fd = match unsafe { minix_std::fs::open(path, minix_std::fs::O_RDONLY, 0) } {
            Ok(fd) => fd,
            Err(_) => {
                write_err(b"cat: ");
                write_err(path.as_bytes());
                write_err(b": cannot open\n");
                exit_code = 1;
                continue;
            }
        };
        let mut buf = [0u8; 8192];
        while let Ok(n) = unsafe { minix_std::fs::read(fd, &mut buf) } {
            if n <= 0 {
                break;
            }
            write_out(&buf[..n as usize]);
        }
        let _ = minix_std::fs::close(fd);
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

    // Open/create destination O_WRONLY | O_CREAT | O_TRUNC.
    // O_WRONLY=1, O_CREAT=0o100=0x40, O_TRUNC=0o1000=0x200 → 0x241.
    let dst_flags = minix_std::fs::O_WRONLY | minix_std::fs::O_CREAT | minix_std::fs::O_TRUNC;
    let dst_fd =
        unsafe { minix_rt::syscall3(4, dst.as_ptr() as u64, dst.len() as u64, dst_flags as u64) };
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
        let w = unsafe { minix_rt::syscall3(3, dst_fd as u64, buf.as_ptr() as u64, n as u64) };
        if w < 0 {
            write_err(b"cp: write failed\n");
            unsafe {
                minix_rt::syscall1(5, src_fd as u64);
            }
            unsafe {
                minix_rt::syscall1(5, dst_fd as u64);
            }
            return 1;
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
    let dir = if args.len() > 1 { args[1] } else { "." };
    // Use IPC-based open via minix_std (routes to VFS)
    let fd = match unsafe { minix_std::fs::open(dir, 0, 0) } {
        Ok(fd) => fd,
        Err(e) => {
            write_err(b"ls: cannot access ");
            write_err(dir.as_bytes());
            write_err(b": err=");
            let code = e.0;
            if code >= 100 {
                write_err(&[b'0' + ((code / 100) % 10) as u8]);
            }
            if code >= 10 {
                write_err(&[b'0' + ((code / 10) % 10) as u8]);
            }
            write_err(&[b'0' + (code % 10) as u8]);
            write_err(b"\r\n");
            return 1;
        }
    };
    let mut buf = [0u8; 4096];
    let n = minix_std::fs::getdents(fd, &mut buf).unwrap_or(0);
    if n <= 0 {
        // Fallback: just print the directory name if getdents fails
        write_out(dir.as_bytes());
        write_out(b"\r\n");
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
    let _ = minix_std::fs::close(fd);
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
        let ret = minix_std::fs::mkdir(path, 0o755);
        if let Err(e) = ret {
            write_err(b"mkdir: ");
            write_err(path.as_bytes());
            write_err(b": err=");
            // Convert negative MINIX errno to positive
            let pos = if e.0 < 0 { -e.0 } else { e.0 } as u32;
            write_err(&[b'0' + (pos / 10) as u8, b'0' + (pos % 10) as u8]);
            write_err(b" ");
            write_err(errstr(pos as i32));
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
            write_err(errstr(-ret));
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
    let err = -ret;
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
    let err = -ret;
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
            unsafe { minix_rt::syscall3(57, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) };
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

/// sync — flush cached filesystem writes to disk.
pub fn sync(_args: &[&str]) -> i32 {
    #[cfg(target_os = "none")]
    {
        // Send VFS_SYNC to VFS, which forwards REQ_SYNC to each mounted
        // filesystem; MFS then writes dirty inodes and blocks to the device.
        let mut msg = [0u8; 64];
        msg[0..4].copy_from_slice(&minix_rt::VFS_PROC_NR.to_le_bytes());
        msg[4..8].copy_from_slice(&minix_rt::VFS_SYNC.to_le_bytes());
        let ret = unsafe {
            minix_rt::syscall2(
                minix_rt::SENDREC_CALL,
                minix_rt::VFS_PROC_NR as u64,
                msg.as_mut_ptr() as u64,
            )
        };
        if ret < 0 {
            write_err(b"sync: ");
            write_err(errstr(-ret as i32));
            write_err(b"\n");
            return 1;
        }
        // Reply status is at bytes 4-7 (m_type set by VFS's reply()).
        let status = i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
        if status < 0 {
            write_err(b"sync: ");
            write_err(errstr(-status));
            write_err(b"\n");
            return 1;
        }
    }
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

/// Parse a signed decimal integer, or return `None`.
pub fn parse_i32(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (neg, digits) = match bytes[0] {
        b'-' => (true, &bytes[1..]),
        b'+' => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (b - b'0') as i64;
    }
    let v = if neg { -v } else { v };
    i32::try_from(v).ok()
}

/// kill — send a signal to a process: `kill pid [sig]`, default SIGTERM.
///
/// The kill-termination path is PM_KILL → do_kill → sig_proc → sig_proc_exit
/// → zombie → parent reap (SIGNALS.md Phase 3). Negative pids (process
/// groups) are a follow-up.
pub fn kill(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_err(b"usage: kill pid [sig]\r\n");
        return 1;
    }
    let Some(pid) = parse_i32(args[1]) else {
        write_err(b"kill: invalid pid\r\n");
        return 1;
    };
    let sig = if args.len() >= 3 {
        match parse_i32(args[2]) {
            Some(s) => s,
            None => {
                write_err(b"kill: invalid signal\r\n");
                return 1;
            }
        }
    } else {
        minix_std::time::SIGTERM
    };
    match minix_std::time::kill(pid, sig) {
        Ok(()) => 0,
        Err(e) => {
            write_err(b"kill: ");
            write_err(errstr(e.0));
            write_err(b"\r\n");
            1
        }
    }
}

pub mod shell;
pub use shell::sh;

/// Return a human-readable error string for a POSIX error code.
pub fn errstr(err: i32) -> &'static [u8] {
    match err {
        1 => b"Operation not permitted",
        2 => b"No such file or directory",
        3 => b"No such process",
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

    write_out(b"init: starting shell...\n");

    // Route stdio through VFS → tty: open /dev/console, dup2 it onto
    // 0/1/2, and mark the fds VFS-owned so the kernel forwards reads and
    // writes to VFS (and the tty) instead of the serial ring / direct
    // UART. The p_fd_vfs flags and the VFS filps survive fork/exec, so
    // the shell and its children inherit tty-backed stdio (TTY.md 1C.1).
    let mut console_ok = false;
    let fd = minix_rt::open(b"/dev/console", 0o2); // O_RDWR
    if fd >= 0 {
        let fd = fd as i32;
        if minix_std::fs::dup2(fd, 0).is_ok()
            && minix_std::fs::dup2(fd, 1).is_ok()
            && minix_std::fs::dup2(fd, 2).is_ok()
        {
            unsafe {
                minix_rt::set_fd_vfs(0, 1);
                minix_rt::set_fd_vfs(1, 1);
                minix_rt::set_fd_vfs(2, 1);
            }
            console_ok = true;
        }
    }
    if !console_ok {
        // The kernel guards the direct fd-0 serial path to tty only, so a
        // shell without VFS stdio would be unable to read input. Hang
        // loudly rather than boot a broken shell.
        write_err(b"init: /dev/console setup failed - hanging\n");
        loop {
            #[cfg(any(target_arch = "riscv64", target_arch = "aarch64"))]
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack))
            };
            #[cfg(not(any(target_arch = "riscv64", target_arch = "aarch64")))]
            unsafe {
                core::arch::asm!("pause")
            };
        }
    }

    // Build argv: ["/bin/sh", null]
    #[cfg(target_os = "none")]
    let argv: [*const u8; 2] = [c"/bin/sh".as_ptr() as *const u8, core::ptr::null()];
    #[cfg(target_os = "none")]
    let ret = unsafe {
        minix_rt::execve(
            c"/bin/sh".as_ptr() as *const u8,
            c"/bin/sh".to_bytes_with_nul().len(),
            argv.as_ptr(),
            core::ptr::null(),
        ) as i64
    };
    #[cfg(not(target_os = "none"))]
    let ret = -38i64; // ENOSYS on host
    // If exec fails, print error and loop.
    write_err(b"init: exec failed: err=");
    let err = -ret as i32;
    if err >= 10 {
        write_out(&[b'0' + (err / 10) as u8]);
    }
    write_out(&[b'0' + (err % 10) as u8]);
    write_out(b"\n");
    loop {
        #[cfg(any(target_arch = "riscv64", target_arch = "aarch64"))]
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack))
        };
        #[cfg(not(any(target_arch = "riscv64", target_arch = "aarch64")))]
        unsafe {
            core::arch::asm!("pause")
        };
        let _ = minix_rt::getpid();
    }
}

/// ping — send an ICMP echo request to an IPv4 address via `/dev/ip` and
/// report whether the host replied.
///
/// The net server's `/dev/ip` write protocol takes a compact 8-byte
/// request: `dst_ip[4] id[2] seq[2]` (big-endian). The read returns the
/// matching echo reply as a raw IP datagram.
pub fn ping(args: &[&str]) -> i32 {
    let target = if args.len() > 1 { args[1] } else { "10.0.2.2" };
    let ip = match parse_ipv4(target) {
        Some(ip) => ip,
        None => {
            write_err(b"ping: bad address\n");
            return -1;
        }
    };

    // open /dev/ip (chardev major 14) read-write.
    let fd = minix_rt::open(b"/dev/ip", 0o2); // O_RDWR
    if fd < 0 {
        write_err(b"ping: cannot open /dev/ip\n");
        return fd as i32;
    }
    let fd = fd as i32;

    // 8-byte request: dst_ip[4] id[2] seq[2] (big-endian). The ICMP
    // identifier is our process ID (masked to 16 bits), so concurrent
    // ping invocations are distinguishable — matching real ping
    // behavior. The sequence starts at 1 for this session.
    let mut req = [0u8; 8];
    req[0..4].copy_from_slice(&ip);
    req[4..6].copy_from_slice(&(minix_rt::getpid() as u16).to_be_bytes());
    req[6..8].copy_from_slice(&1u16.to_be_bytes());

    let w = unsafe { minix_rt::write(fd, req.as_ptr(), req.len()) };
    if w != req.len() as i64 {
        write_err(b"ping: write failed\n");
        return -5;
    }

    // Read the reply (IP + ICMP echo reply, ~28 bytes).
    let mut rbuf = [0u8; 64];
    let n = minix_rt::read(fd, &mut rbuf);
    if n <= 0 {
        write_err(b"ping: no reply\n");
        return -5;
    }
    let n = n as usize;

    write_out(b"ping: ");
    write_out(target.as_bytes());
    write_out(b" alive");
    // ICMP is at IP header (20 bytes, no options); type@20, id@24, seq@26.
    if n >= 28 && rbuf[20] == 0 {
        // type 0 = echo reply
        write_out(b" (reply id=");
        print_dec(u16::from_be_bytes([rbuf[24], rbuf[25]]) as u32);
        write_out(b" seq=");
        print_dec(u16::from_be_bytes([rbuf[26], rbuf[27]]) as u32);
        write_out(b")");
    }
    write_out(b"\n");
    0
}

/// Parse "a.b.c.d" into four bytes; None on failure.
fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut part = 0usize;
    let mut val = 0u32;
    let mut digits = false;
    for &b in s.as_bytes() {
        match b {
            b'.' => {
                if !digits || val > 255 || part >= 3 {
                    return None;
                }
                out[part] = val as u8;
                part += 1;
                val = 0;
                digits = false;
            }
            b'0'..=b'9' => {
                digits = true;
                val = val * 10 + (b - b'0') as u32;
                if val > 255 {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if !digits || val > 255 || part != 3 {
        return None;
    }
    out[3] = val as u8;
    Some(out)
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
    fn test_parse_i32() {
        assert_eq!(parse_i32("0"), Some(0));
        assert_eq!(parse_i32("123"), Some(123));
        assert_eq!(parse_i32("-9"), Some(-9));
        assert_eq!(parse_i32("+7"), Some(7));
        assert_eq!(parse_i32(""), None);
        assert_eq!(parse_i32("-"), None);
        assert_eq!(parse_i32("12a"), None);
        assert_eq!(parse_i32("999999999999"), None); // overflows i32
    }

    #[test]
    fn test_kill_usage_and_bad_args() {
        // Host runs: the syscall path returns ENOSYS, so only arg
        // validation is pinned here.
        assert_eq!(kill(&["kill"]), 1); // usage
        assert_eq!(kill(&["kill", "abc"]), 1); // invalid pid
        assert_eq!(kill(&["kill", "1", "xyz"]), 1); // invalid signal
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
