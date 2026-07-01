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
    // getdents via VFS IPC would go here — stub for now
    write_out(dir.as_bytes());
    write_out(b"\n");
    unsafe {
        minix_rt::syscall1(5, fd as u64);
    }
    0
}

/// mkdir — create directory (stub).
pub fn mkdir(args: &[&str]) -> i32 {
    for path in &args[1..] {
        write_err(b"mkdir: ");
        write_err(path.as_bytes());
        write_err(b": not yet implemented\n");
    }
    1
}

/// rm — remove file (stub).
pub fn rm(args: &[&str]) -> i32 {
    for path in &args[1..] {
        write_err(b"rm: ");
        write_err(path.as_bytes());
        write_err(b": not yet implemented\n");
    }
    1
}

/// ln — create hard link (stub).
pub fn ln(_args: &[&str]) -> i32 {
    write_err(b"ln: not yet implemented\n");
    1
}

/// chmod — change file mode (stub).
pub fn chmod(_args: &[&str]) -> i32 {
    write_err(b"chmod: not yet implemented\n");
    1
}

/// chown — change file owner (stub).
pub fn chown(_args: &[&str]) -> i32 {
    write_err(b"chown: not yet implemented\n");
    1
}

/// sync — synchronize cached writes (stub).
pub fn sync(_args: &[&str]) -> i32 {
    write_out(b"sync\n");
    0
}

/// mknod — create device node (stub).
pub fn mknod(_args: &[&str]) -> i32 {
    write_err(b"mknod: not yet implemented\n");
    1
}

/// reboot — reboot the system (stub).
pub fn reboot(_args: &[&str]) -> i32 {
    write_out(b"reboot\n");
    0
}

/// fsck — file system check (stub).
pub fn fsck(_args: &[&str]) -> i32 {
    write_out(b"fsck\n");
    0
}

/// sh — minimal shell (stub).
pub fn sh(_args: &[&str]) -> i32 {
    write_out(b"sh: stub (no PM server yet)\n");
    0
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
    #[ignore = "infinite loop (init never returns)"]
    fn test_init_stub() {
        assert_eq!(init(&["/sbin/init"]), !0); // never returns
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
}
