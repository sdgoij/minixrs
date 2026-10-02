#![no_std]
#![no_main]

//! Measures whether a user VA the process was never given is reachable.
//!
//! The invariant is that a VA in the user window which no region covers must fault. If it does
//! not, the address space is handing the process a mapping the kernel built for itself - the
//! low-GB alias `KNOWN_ISSUES.md` item 16 recorded as unaudited (fixed 2026-10-02). That item is why this probe
//! exists: on aarch64 `create_low_gb_pmd_table` used to fill the window with 2 MB blocks whose
//! `AP[2:1]` is `EL0_RW` and whose physical address is `win_base + ((va - user_low) % win_size)`,
//! aliasing the kernel's physical allocator into every process's user window. RISC-V's fresh
//! root does not copy the window's entry at all and x86's identity map stops at the user base,
//! so this is a per-arch measurement rather than a shared one.
//!
//! Each VA is probed in a child, because the fault a missing mapping raises kills the process:
//! the child dies with `SIGSEGV` and the parent reads that as the refusal. The read is safe
//! whatever it hits. The write *stores back the byte it just read*, so finding a page writable
//! cannot change it - a sentinel would corrupt what the alias points at, and for the kernel's
//! allocator that is the one thing this probe must not do.
//!
//! The VAs sit in the band between the loader and the brk heap, which every arch's layout leaves
//! empty: above x86's DSO/loader region (80..128 MiB) and below the heap base (512 MiB). The
//! summary line is what a gate asserts, so one expectation holds on every arch once the
//! invariant holds (`reachable=0`); the line per VA is the diagnostic when it does not.

/// Host-only panic handler - required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// Probed VAs: the band no arch's layout maps (above the loader, below the brk heap), plus one
/// *below* the image base, which probes the other half of the same window - the NULL page and the
/// gap under the image, which `create_low_gb_pmd_table` used to fill with a block descriptor carrying
/// physical address 0 rather than leaving it invalid. aarch64 identity-maps the virtio MMIO window
/// at 0x0a00_0000 for an EL0 driver, so that is avoided.
const VAS: [u64; 4] = [0x0010_0000, 0x0900_0000, 0x1200_0000, 0x1f00_0000];

/// What a child found before it was killed, or did not.
#[derive(Clone, Copy, PartialEq)]
enum Reach {
    /// The read survived: the VA is mapped and EL0-accessible.
    Read,
    /// The read survived but the store did not: mapped, read-only to EL0.
    ReadOnly,
    /// The read was refused: nothing is mapped there.
    Unmapped,
}

fn hex(buf: &mut [u8], at: &mut usize, v: u64) {
    buf[*at] = b'0';
    buf[*at + 1] = b'x';
    *at += 2;
    let mut started = false;
    for i in (0..16).rev() {
        let d = ((v >> (i * 4)) & 0xF) as u8;
        if d != 0 {
            started = true;
        }
        if started || i == 0 {
            buf[*at] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
            *at += 1;
        }
    }
}

fn push(buf: &mut [u8], at: &mut usize, s: &[u8]) {
    for &b in s {
        buf[*at] = b;
        *at += 1;
    }
}

/// Run `body` in a child and report whether it survived. The child prints nothing, so a faulter
/// cannot leave output the harness could read as this step's evidence.
fn survives(body: impl FnOnce()) -> bool {
    match unsafe { minix_std::process::fork() } {
        Ok(0) => {
            body();
            minix_std::process::exit(0);
        }
        Ok(pid) => match minix_std::process::waitpid(pid, 0) {
            Ok((_, status)) => status == 0,
            Err(_) => false,
        },
        Err(_) => false,
    }
}

fn reach(va: u64) -> Reach {
    // The read alone, in its own child: a store fault would otherwise be indistinguishable from
    // a read fault. Its result is dropped in a statement rather than left as the closure's tail
    // expression, which would make `read_volatile`'s type parameter `()` and demand a `*const ()`.
    if !survives(move || {
        unsafe { core::ptr::read_volatile(va as *const u8) };
    }) {
        return Reach::Unmapped;
    }
    let read = unsafe { core::ptr::read_volatile(va as *const u8) };
    if survives(move || {
        unsafe { core::ptr::write_volatile(va as *mut u8, read) };
    }) {
        Reach::Read
    } else {
        Reach::ReadOnly
    }
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let mut reachable = 0u32;
    for va in VAS {
        let r = reach(va);
        if r != Reach::Unmapped {
            reachable += 1;
        }
        let mut line = [0u8; 96];
        let mut at = 0usize;
        push(&mut line, &mut at, b"winprobe: va=");
        hex(&mut line, &mut at, va);
        push(
            &mut line,
            &mut at,
            match r {
                Reach::Read => b" read=ok write=ok",
                Reach::ReadOnly => b" read=ok write=err",
                Reach::Unmapped => b" read=err write=err",
            },
        );
        push(&mut line, &mut at, b"\n");
        userland::write_out(&line[..at]);
    }

    let mut summary = [0u8; 64];
    let mut at = 0usize;
    push(&mut summary, &mut at, b"winprobe: probed=");
    summary[at] = b'0' + (VAS.len() % 10) as u8;
    at += 1;
    push(&mut summary, &mut at, b" reachable=");
    if reachable >= 10 {
        summary[at] = b'0' + ((reachable / 10) % 10) as u8;
        at += 1;
    }
    summary[at] = b'0' + (reachable % 10) as u8;
    at += 1;
    push(&mut summary, &mut at, b"\n");
    userland::write_out(&summary[..at]);
    0
}
