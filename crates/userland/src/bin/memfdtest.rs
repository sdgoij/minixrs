//! `memfd` smoke test — `/bin/memfdtest`.
//!
//! Exercises the anonymous shared-memory object a `wl_shm` pool is made of:
//! create it, size it with `ftruncate`, map it `MAP_SHARED` **twice**, and check
//! that one mapping sees what the other wrote; then fork and check the same
//! across address spaces; then check that `read`/`write` through the descriptor
//! and the mappings agree — the object *is* its frames, so they must. A plain
//! read-only view is forked as a baseline (it must survive), then the mapping is
//! `mprotect`ed read-only and the protection is checked to be *enforced*, to
//! survive a fork, to be reversible, and to be correct for a split sub-range.
//!
//! Expect `memfdtest: OK` and exit 0. Each step has its own failure code.

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

const SIZE: usize = 8192;

/// Unwrap a call, reporting `code` and `msg` on failure.
fn step<T>(r: Result<T, minix_std::MinixErr>, code: i32, msg: &[u8]) -> Result<T, i32> {
    r.map_err(|_| {
        userland::write_err(msg);
        code
    })
}

/// Child-side check: attempt `write` and report whether we survived. Used by the
/// protection steps, where surviving is the failure.
fn try_write_and_exit(p: *mut u8, value: u8) -> ! {
    unsafe {
        core::ptr::write_volatile(p, value);
    }
    minix_std::process::exit(0)
}

fn run() -> Result<(), i32> {
    // ---- create and size the object ----
    let fd = step(
        minix_std::fs::memfd_create(0),
        1,
        b"memfdtest: memfd_create failed\n",
    )?;
    step(
        minix_std::fs::truncate(fd, SIZE as i64),
        2,
        b"memfdtest: ftruncate failed\n",
    )?;
    let st = step(minix_std::fs::fstat(fd), 3, b"memfdtest: fstat failed\n")?;
    if st.st_size != SIZE as i64 {
        userland::write_err(b"memfdtest: ftruncate did not take\n");
        return Err(4);
    }

    // ---- two writable shared mappings of one object ----
    let a = unsafe {
        minix_std::vmem::mmap(
            core::ptr::null_mut(),
            SIZE,
            minix_std::vmem::PROT_READ | minix_std::vmem::PROT_WRITE,
            minix_std::vmem::MAP_SHARED,
            fd,
            0,
        )
    };
    if a == minix_std::vmem::MAP_FAILED {
        userland::write_err(b"memfdtest: first mmap failed\n");
        return Err(5);
    }
    let b = unsafe {
        minix_std::vmem::mmap(
            core::ptr::null_mut(),
            SIZE,
            minix_std::vmem::PROT_READ | minix_std::vmem::PROT_WRITE,
            minix_std::vmem::MAP_SHARED,
            fd,
            0,
        )
    };
    if b == minix_std::vmem::MAP_FAILED {
        userland::write_err(b"memfdtest: second mmap failed\n");
        return Err(6);
    }

    // Write a pattern through the first mapping and read it back through the
    // second. Distinct addresses, one object: only shared frames can do this.
    for i in 0..SIZE {
        unsafe {
            core::ptr::write_volatile(a.add(i), (i as u8) ^ 0x5A);
        }
    }
    for i in 0..SIZE {
        if unsafe { core::ptr::read_volatile(b.add(i)) } != (i as u8) ^ 0x5A {
            userland::write_err(b"memfdtest: the two mappings are not one object\n");
            return Err(7);
        }
    }

    // ---- the descriptor and the mappings are one object too ----
    // A `write` through the fd must land in the frames the mappings see...
    step(
        minix_std::fs::lseek(fd, 0, minix_std::fs::SEEK_SET),
        8,
        b"memfdtest: lseek failed\n",
    )?;
    step(
        unsafe { minix_std::fs::write(fd, b"ZZZZ") },
        9,
        b"memfdtest: write failed\n",
    )?;
    for (i, want) in b"ZZZZ".iter().enumerate() {
        if unsafe { core::ptr::read_volatile(a.add(i)) } != *want {
            userland::write_err(b"memfdtest: write() is not visible to a mapping\n");
            return Err(10);
        }
    }
    // ...and a `read` must see what a mapping wrote.
    step(
        minix_std::fs::lseek(fd, 4, minix_std::fs::SEEK_SET),
        11,
        b"memfdtest: lseek failed\n",
    )?;
    let mut got = [0u8; 4];
    let n = step(
        unsafe { minix_std::fs::read(fd, &mut got) },
        12,
        b"memfdtest: read failed\n",
    )?;
    let mut want = [0u8; 4];
    for (k, w) in want.iter_mut().enumerate() {
        *w = ((4 + k) as u8) ^ 0x5A;
    }
    if n != 4 || got != want {
        userland::write_err(b"memfdtest: a mapping's bytes are not visible to read()\n");
        return Err(13);
    }

    // ---- across address spaces ----
    // Touch the pages before forking so the child inherits them COW-protected and
    // its store has to re-enable writability on the shared frame rather than copy
    // it (`VR_SHARED`'s COW exception); a copy would hide the marker below.
    let _ = unsafe { core::ptr::read_volatile(a) };
    let pid = match unsafe { minix_std::process::fork() } {
        Ok(pid) => pid,
        Err(_) => {
            userland::write_err(b"memfdtest: fork failed\n");
            return Err(14);
        }
    };
    if pid == 0 {
        const MARKER: u8 = 0xA5;
        unsafe {
            core::ptr::write_volatile(b, MARKER);
        }
        minix_std::process::exit(0);
    }
    let (_, status) = match minix_std::process::waitpid(pid, 0) {
        Ok(w) => w,
        Err(_) => {
            userland::write_err(b"memfdtest: waitpid failed\n");
            return Err(15);
        }
    };
    if status != 0 {
        userland::write_err(b"memfdtest: the child failed\n");
        return Err(16);
    }
    if unsafe { core::ptr::read_volatile(a) } != 0xA5 {
        userland::write_err(b"memfdtest: the child's write is not visible to the parent\n");
        return Err(17);
    }
    // And the child's write must have reached the object itself, not a COW copy.
    step(
        minix_std::fs::lseek(fd, 0, minix_std::fs::SEEK_SET),
        18,
        b"memfdtest: lseek failed\n",
    )?;
    let mut one = [0u8; 1];
    step(
        unsafe { minix_std::fs::read(fd, &mut one) },
        19,
        b"memfdtest: read failed\n",
    )?;
    if one[0] != 0xA5 {
        userland::write_err(b"memfdtest: the child wrote a private copy\n");
        return Err(20);
    }

    // ---- a plain read-only view, forked, with no `mprotect` anywhere ----
    // This is the baseline the protection steps are read against: a shared frame
    // held by a read-only mapping must survive a fork whose child exits.
    let c = unsafe {
        minix_std::vmem::mmap(
            core::ptr::null_mut(),
            SIZE,
            minix_std::vmem::PROT_READ,
            minix_std::vmem::MAP_SHARED,
            fd,
            0,
        )
    };
    if c == minix_std::vmem::MAP_FAILED {
        userland::write_err(b"memfdtest: read-only mmap failed\n");
        return Err(21);
    }
    let before_c = unsafe { core::ptr::read_volatile(c) };
    let pid = match unsafe { minix_std::process::fork() } {
        Ok(pid) => pid,
        Err(_) => {
            userland::write_err(b"memfdtest: fork failed (control)\n");
            return Err(22);
        }
    };
    if pid == 0 {
        minix_std::process::exit(0);
    }
    let _ = minix_std::process::waitpid(pid, 0);
    let after_c = unsafe { core::ptr::read_volatile(c) };
    if before_c != 0xA5 || after_c != 0xA5 {
        userland::write_err(b"memfdtest: a read-only view did not survive fork\n");
        return Err(23);
    }

    // ---- protecting the mapping ----
    // What a JIT and a `wl_shm` client do once a buffer is written: take write
    // permission away. The mapping holds the child's 0xA5 marker.
    if unsafe { minix_std::vmem::mprotect(a, SIZE, minix_std::vmem::PROT_READ) }.is_err() {
        userland::write_err(b"memfdtest: mprotect failed\n");
        return Err(24);
    }
    // A read is still allowed...
    if unsafe { core::ptr::read_volatile(a) } != 0xA5 {
        userland::write_err(b"memfdtest: mprotect lost the data\n");
        return Err(25);
    }
    // ...and a write must now kill the writer. The child is what gets killed, so
    // this process can go on to check that the object is untouched.
    let pid = match unsafe { minix_std::process::fork() } {
        Ok(pid) => pid,
        Err(_) => {
            userland::write_err(b"memfdtest: fork failed after mprotect\n");
            return Err(26);
        }
    };
    if pid == 0 {
        try_write_and_exit(a, 0x44);
    }
    let (_, status) = match minix_std::process::waitpid(pid, 0) {
        Ok(w) => w,
        Err(_) => {
            userland::write_err(b"memfdtest: waitpid failed after mprotect\n");
            return Err(27);
        }
    };
    let after_fork = unsafe { core::ptr::read_volatile(a) };
    if status == 0 {
        userland::write_err(b"memfdtest: a write to a read-only mapping succeeded\n");
        return Err(28);
    }
    if after_fork != 0xA5 {
        userland::write_err(b"memfdtest: the protected frame did not survive fork\n");
        return Err(29);
    }
    step(
        minix_std::fs::lseek(fd, 0, minix_std::fs::SEEK_SET),
        30,
        b"memfdtest: lseek failed\n",
    )?;
    let mut one = [0u8; 1];
    step(
        unsafe { minix_std::fs::read(fd, &mut one) },
        31,
        b"memfdtest: read failed\n",
    )?;
    if one[0] != 0xA5 {
        userland::write_err(b"memfdtest: the object byte is now ");
        userland::print_dec(one[0] as u32);
        userland::write_err(b" (wanted 165)\n");
        return Err(32);
    }

    // Give write permission back: the protection is reversible, and what is
    // written then must reach the object.
    let rw = minix_std::vmem::PROT_READ | minix_std::vmem::PROT_WRITE;
    if unsafe { minix_std::vmem::mprotect(a, SIZE, rw) }.is_err() {
        userland::write_err(b"memfdtest: restoring write permission failed\n");
        return Err(33);
    }
    unsafe {
        core::ptr::write_volatile(a, 0x77);
    }
    step(
        minix_std::fs::lseek(fd, 0, minix_std::fs::SEEK_SET),
        34,
        b"memfdtest: lseek failed\n",
    )?;
    step(
        unsafe { minix_std::fs::read(fd, &mut one) },
        35,
        b"memfdtest: read failed\n",
    )?;
    if one[0] != 0x77 {
        userland::write_err(b"memfdtest: a write after mprotect did not reach the object\n");
        return Err(36);
    }

    // A *sub-range* protection has to split the region it lands inside: protect
    // only the second page, then the first page must still take a write while the
    // second one kills its writer.
    if unsafe { minix_std::vmem::mprotect(a.add(4096), 4096, minix_std::vmem::PROT_READ) }.is_err()
    {
        userland::write_err(b"memfdtest: range mprotect failed\n");
        return Err(37);
    }
    unsafe {
        core::ptr::write_volatile(a, 0x22);
    }
    let page1 = unsafe { core::ptr::read_volatile(a.add(4096)) };
    let pid = match unsafe { minix_std::process::fork() } {
        Ok(pid) => pid,
        Err(_) => {
            userland::write_err(b"memfdtest: fork failed after range mprotect\n");
            return Err(38);
        }
    };
    if pid == 0 {
        try_write_and_exit(unsafe { a.add(4096) }, 0x44);
    }
    let (_, status) = match minix_std::process::waitpid(pid, 0) {
        Ok(w) => w,
        Err(_) => {
            userland::write_err(b"memfdtest: waitpid failed after range mprotect\n");
            return Err(39);
        }
    };
    if status == 0 {
        userland::write_err(b"memfdtest: a write past the protected range succeeded\n");
        return Err(40);
    }
    if unsafe { core::ptr::read_volatile(a) } != 0x22
        || unsafe { core::ptr::read_volatile(a.add(4096)) } != page1
    {
        userland::write_err(b"memfdtest: the protected range covered the wrong pages\n");
        return Err(41);
    }

    let _ = unsafe { minix_std::vmem::munmap(a, SIZE) };
    let _ = unsafe { minix_std::vmem::munmap(b, SIZE) };
    let _ = unsafe { minix_std::vmem::munmap(c, SIZE) };
    let _ = minix_std::fs::close(fd);
    Ok(())
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    match run() {
        Ok(()) => {
            userland::write_out(b"memfdtest: OK\n");
            0
        }
        Err(code) => code,
    }
}
