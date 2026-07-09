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
    unsafe {
        let fd: u64 = 1;
        let ptr: u64 = s.as_ptr() as u64;
        let count: u64 = s.len() as u64;
        core::arch::asm!(
            "syscall",
            in("rax") 3u64,  // NR_WRITE
            in("rdi") fd,
            in("rsi") ptr,
            in("rdx") count,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") _,
            options(nostack),
        );
    }
}

pub fn write_err(s: &[u8]) {
    unsafe {
        let fd: u64 = 2;
        let ptr: u64 = s.as_ptr() as u64;
        let count: u64 = s.len() as u64;
        core::arch::asm!(
            "syscall",
            in("rax") 3u64,  // NR_WRITE
            in("rdi") fd,
            in("rsi") ptr,
            in("rdx") count,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("rax") _,
            options(nostack),
        );
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
    let dir = if args.len() > 1 { args[1] } else { "." };
    // Use IPC-based open via minix_std (routes to VFS)
    let fd = match unsafe { minix_std::fs::open(dir, 0, 0) } {
        Ok(fd) => {
            write_out(b"OP:OK fd=");
            let code = fd;
            if code >= 100 {
                write_out(&[b'0' + ((code / 100) % 10) as u8]);
            }
            if code >= 10 {
                write_out(&[b'0' + ((code / 10) % 10) as u8]);
            }
            write_out(&[b'0' + (code % 10) as u8]);
            write_out(b"\r\n");
            fd
        }
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
    // Debug always
    write_out(b"[n=");
    if n >= 100 {
        write_out(&[b'0' + ((n / 100) % 10) as u8]);
    }
    if n >= 10 {
        write_out(&[b'0' + ((n / 10) % 10) as u8]);
    }
    write_out(&[b'0' + (n % 10) as u8]);
    write_out(b" \r\n");
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

/// sh — minimal shell that reads lines from stdin and echoes them.
/// On host builds, just prints a stub message and returns.
pub fn sh(_args: &[&str]) -> i32 {
    #[cfg(not(target_os = "none"))]
    {
        write_out(b"sh: stub (no MINIX syscall ABI on host)\n");
        0
    }
    #[cfg(target_os = "none")]
    {
        write_out(b"# ");
        let mut buf = [0u8; 256];
        loop {
            // Read a line from stdin (fd 0).
            let mut pos = 0usize;
            while pos < buf.len() - 1 {
                let n = minix_rt::read(0, &mut buf[pos..pos + 1]);
                if n <= 0 {
                    break;
                }
                let c = buf[pos];
                // Enter (\r from QEMU terminal) ends the line.
                if c == b'\r' || c == b'\n' {
                    write_out(b"\r\n");
                    break;
                }
                // Backspace (DEL 0x7F or BS 0x08) erases previous char.
                if c == 0x7F || c == 0x08 {
                    if pos > 0 {
                        pos -= 1;
                        write_out(b"\x08 \x08");
                    }
                    continue;
                }
                // Echo printable character and store it.
                write_out(&[c]);
                pos += 1;
            }

            // Parse and execute the command.
            let line_len = pos;
            if line_len > 0 {
                // Convert the line to a &str for splitting.
                let line_str = core::str::from_utf8(&buf[..line_len]).unwrap_or("");

                // Split into tokens by whitespace.
                let mut tokens = [""; 32];
                let mut argc = 0usize;
                for token in line_str.split_whitespace() {
                    if argc < tokens.len() {
                        tokens[argc] = token;
                        argc += 1;
                    }
                }

                if argc > 0 {
                    let cmd = tokens[0];
                    let args = &tokens[..argc];
                    match cmd {
                        "echo" => {
                            let _ = echo(args);
                        }
                        "cat" => {
                            let _ = cat(args);
                        }
                        "cp" => {
                            let _ = cp(args);
                        }
                        "ls" => {
                            let _ = ls(args);
                        }
                        "mkdir" => {
                            let _ = mkdir(args);
                        }
                        "rm" => {
                            let _ = rm(args);
                        }
                        "ln" => {
                            let _ = ln(args);
                        }
                        "chmod" => {
                            let _ = chmod(args);
                        }
                        "chown" => {
                            let _ = chown(args);
                        }
                        "sync" => {
                            let _ = sync(args);
                        }
                        "mknod" => {
                            let _ = mknod(args);
                        }
                        "reboot" => {
                            let _ = reboot(args);
                        }
                        "fsck" => {
                            let _ = fsck(args);
                        }
                        "help" => {
                            write_out(b"available commands: echo cat cp ls mkdir rm ln");
                            write_out(b" chmod chown sync mknod reboot fsck help clear\r\n");
                        }
                        "clear" => {
                            write_out(b"\x1b[H\x1b[2J");
                        }
                        "cd" => {
                            if args.len() < 2 {
                                write_err(b"sh: cd: missing argument\r\n");
                            } else {
                                let path = args[1].as_bytes();
                                let r = minix_rt::chdir(path);
                                write_out(b"[r=");
                                let code = if r >= 0 { r as u64 } else { (-r) as u64 };
                                if code >= 10 {
                                    write_out(&[b'0' + ((code / 10) % 10) as u8]);
                                }
                                write_out(&[b'0' + (code % 10) as u8]);
                                write_out(b"]\r\n");
                                if r < 0 {
                                    write_err(b"sh: cd: ");
                                    write_err(path);
                                    write_err(b": ");
                                    write_err(errstr(r as i32));
                                    write_err(b"\r\n");
                                }
                            }
                        }
                        "exit" => {
                            write_out(b"\r\n");
                            return 0;
                        }
                        _ => {
                            // Try external command via kernel fork/exec.
                            let cmd_bytes = cmd.as_bytes();
                            let mut cmd_path = [0u8; 256];
                            // If the command starts with '/', use it directly.
                            // Otherwise try /bin/<cmd> first, then /sbin/<cmd>.
                            let path_len = if cmd_bytes.starts_with(b"/") {
                                let len = (cmd_bytes.len() + 1).min(cmd_path.len());
                                cmd_path[..len - 1].copy_from_slice(&cmd_bytes[..len - 1]);
                                cmd_path[len - 1] = 0;
                                len
                            } else if 5 + cmd_bytes.len() < cmd_path.len() {
                                cmd_path[..5].copy_from_slice(b"/bin/");
                                cmd_path[5..5 + cmd_bytes.len()].copy_from_slice(cmd_bytes);
                                cmd_path[5 + cmd_bytes.len()] = 0;
                                5 + cmd_bytes.len() + 1
                            } else {
                                0
                            };
                            if path_len > 0 {
                                let pid = minix_rt::fork();
                                if pid < 0 {
                                    write_err(b"sh: fork failed\r\n");
                                } else if pid == 0 {
                                    // Build argv array for the child.
                                    // argv[0] = resolved path (cmd_path)
                                    // argv[1..] = remaining command-line tokens
                                    // argv[N] = null terminator (from zeroed array)
                                    let mut argv_buf: [*const u8; 32] = [core::ptr::null(); 32];
                                    argv_buf[0] = cmd_path.as_ptr() as *const u8;
                                    for i in 1..argc.min(31) {
                                        argv_buf[i] = tokens[i].as_ptr();
                                    }

                                    // Child: try /bin/<cmd> first, or use path directly
                                    let r = unsafe {
                                        minix_rt::exec_replace(
                                            &cmd_path[..path_len],
                                            argv_buf.as_ptr(),
                                        )
                                    };
                                    if r < 0
                                        && !cmd_bytes.starts_with(b"/")
                                        && 6 + cmd_bytes.len() < cmd_path.len()
                                    {
                                        // Try /sbin/<cmd>
                                        cmd_path[..6].copy_from_slice(b"/sbin/");
                                        cmd_path[6..6 + cmd_bytes.len()].copy_from_slice(cmd_bytes);
                                        cmd_path[6 + cmd_bytes.len()] = 0;
                                        let _ = unsafe {
                                            minix_rt::exec_replace(
                                                &cmd_path[..6 + cmd_bytes.len() + 1],
                                                argv_buf.as_ptr(),
                                            )
                                        };
                                    }
                                    write_err(b"sh: ");
                                    write_err(cmd.as_bytes());
                                    write_err(b": not found\r\n");
                                    minix_rt::exit(1);
                                } else {
                                    // Parent: wait for child
                                    let status = minix_rt::waitpid(pid);
                                    if status < 0 {
                                        write_err(b"sh: waitpid failed\r\n");
                                    }
                                }
                            } else {
                                write_err(b"sh: ");
                                write_err(cmd.as_bytes());
                                write_err(b": not found\r\n");
                            }
                        }
                    }
                }
            }

            // Print the next prompt.
            write_out(b"# ");
        }
    }
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

    write_out(b"init: starting shell...\n");
    // Build argv: ["/bin/sh", null]
    #[cfg(target_os = "none")]
    let argv: [*const u8; 2] = [c"/bin/sh".as_ptr() as *const u8, core::ptr::null()];
    #[cfg(target_os = "none")]
    let ret = unsafe { minix_rt::exec_replace(c"/bin/sh".to_bytes_with_nul(), argv.as_ptr()) };
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
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack))
        };
        #[cfg(not(target_arch = "riscv64"))]
        unsafe {
            core::arch::asm!("pause")
        };
        let _ = minix_rt::getpid();
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
