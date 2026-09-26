//! One userland program, as its own wasm module.
//!
//! This is the shape `exec` instantiates (`ARCH_WASM32.md` §7.2): the module *is* the
//! program, and the export at the bottom of this file is what the host calls in place of
//! `_start`. It is a separate crate from `wasm-servers` because the two are arranged
//! opposite ways round. A server is one module with an entry per server — all ten run the
//! same binary, because the kernel reserved their slots at boot and nothing chooses between
//! them. A program is the other way round: what exec was handed is a *path*, so the module
//! is chosen from the outside and one module per program is the arrangement step 4 of the
//! M7a plan (module bytes out of the filesystem) will need. Starting with exactly one
//! program keeps that decision visible instead of hiding it behind a dispatcher.
//!
//! # What the entry owns, and why the module rather than the host
//!
//! Two regions are module-owned and exported by address. The Asyncify scratch area is
//! exported for the reason `wasm-servers` exports it: Asyncify must be applied to this
//! module too, its buffer has to live where the program will never touch, and an overflow
//! corrupts memory silently rather than trapping — so the host is told where the region is
//! rather than deriving it from a linker default.
//!
//! The second region is the **argv area**, which is where the host writes the arguments of
//! the process it is creating. The layout is part of this module's ABI, not the host's:
//!
//! ```text
//! base + 0                u32  argc
//! base + 4                u32  argc pointers, each an absolute address in this instance
//! base + 4 + 4 * argc          the argument strings, NUL-terminated, in order
//! ```
//!
//! §7.2's step 3 is "the host copies argv into the new instance's memory and the entry reads
//! it", and this is that with the offsets decided on one side only. The entry then hands the
//! pointer array to `userland::parse_args`, which is what every `userland/src/bin/*.rs`
//! calls on the shipping arches — so a program reads its arguments through one code path on
//! all four targets rather than the wasm one growing a second.
//!
//! # The blob is host-written, so it is checked before it is read
//!
//! `parse_args` scans each argument for its NUL with no upper bound, which is correct for a
//! kernel that validated the pointers and wrong for a blob crossing the host boundary: one
//! misplaced pointer would walk out of the area and trap somewhere unrelated, reporting a
//! host mistake as a fault in the program. The entry therefore validates structure first
//! ([`blob_is_sane`]) and refuses loudly, which is the shape a host bug should have here.

#![no_std]
#![no_main]

// Region the host uses for the Asyncify data buffer and its stack.
//
// Argued exactly as `wasm-servers` argues it: the buffer goes at the module's own
// end-of-statics, because a reserved static can be laid out inside the port's
// bump-heap window, where an allocator overwrites it. The host owns the size.
unsafe extern "C" {
    // lld's end-of-static-data symbol. Only its address is ever taken.
    static __heap_base: u8;
}

/// Address of the scratch region, so the host does not have to guess.
#[unsafe(no_mangle)]
pub extern "C" fn asyncify_scratch_ptr() -> u32 {
    let end = core::ptr::addr_of!(__heap_base) as usize;
    ((end + 15) & !15) as u32
}

/// How much room the host has for the argv blob. Two orders of magnitude more than the
/// longest argument list this port passes, because the cost of running out is a refusal at
/// process creation and the cost of the room is 512 bytes of a module's linear memory.
const ARGV_AREA_SIZE: usize = 512;

/// The most arguments the entry will accept.
///
/// Not this module's own choice: it is the width of the buffer `userland::parse_args`
/// requires, so accepting more would mean reading a blob this program cannot present.
const ARGV_MAX: usize = 64;

#[repr(C, align(8))]
struct ArgvArea([u8; ARGV_AREA_SIZE]);

static mut ARGV_AREA: ArgvArea = ArgvArea([0; ARGV_AREA_SIZE]);

/// Where the host writes the argv blob: `argc`, then the pointer array, then the strings.
///
/// Exported rather than the offset being written down on both sides. The host has no way to
/// name a static, and a hardcoded address in JavaScript would be a second copy of the
/// module's layout that nothing keeps in step with this file.
#[unsafe(no_mangle)]
pub extern "C" fn argv_area_ptr() -> u32 {
    core::ptr::addr_of_mut!(ARGV_AREA) as u32
}

fn read_u32(addr: u32) -> u32 {
    // SAFETY: the caller has established `addr` is inside the argv area, which is this
    // instance's own linear memory. A volatile read because the host wrote it, so nothing
    // in this translation unit may assume anything about the byte.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn read_u8(addr: u32) -> u8 {
    // SAFETY: as `read_u32` — the address is inside the argv area.
    unsafe { core::ptr::read_volatile(addr as *const u8) }
}

/// Whether the blob at `base` is safe to hand to `userland::parse_args`.
///
/// That is: a count this program can present, a pointer array that fits in the area, and
/// every pointer naming a NUL-terminated string that also lies inside it. Checked as
/// structure rather than as content — what the arguments *say* is the program's business,
/// and the harness pins it by reading what the program printed.
fn blob_is_sane(base: u32, argc: u32) -> bool {
    let end = base + ARGV_AREA_SIZE as u32;
    if argc == 0 || argc > ARGV_MAX as u32 {
        return false;
    }
    if base + 4 + 4 * argc > end {
        return false;
    }
    for i in 0..argc {
        let start = read_u32(base + 4 + 4 * i);
        if start < base || start >= end {
            return false;
        }
        let mut p = start;
        while p < end && read_u8(p) != 0 {
            p += 1;
        }
        // Ran to the end of the area without meeting a terminator, so `parse_args` would
        // scan past it.
        if p == end {
            return false;
        }
    }
    true
}

/// The smallest program that needs `fork` (`ARCH_WASM32.md` §11, M7b step 5a).
///
/// It forks once, and the two instances say which one they are: the child prints the pid it was
/// given and the 0 `fork` returned to it, the parent prints the pid `fork` returned and then
/// reaps the child. One fork, no exec, no loop — a shell forks repeatedly and the shell is 5b's
/// subject, while this is the chain underneath it: PM's table copy, VM's address-space clone
/// (the host's memory copy on this port), the kernel's `Proc` and the schedule that makes the
/// child runnable, and the two ends of one `fork` disagreeing about what it returned.
///
/// Every line is finished without a blocking syscall inside it, so the parent and the child —
/// both writing to the same console — cannot interleave half-finished lines.
fn forktest() -> i32 {
    let pid = minix_rt::fork();
    if pid < 0 {
        userland::write_out(b"forktest: fork failed\n");
        return 1;
    }

    // Asked *after* the fork on purpose: the child's memory is a copy of the parent's at the
    // fork point, so a pid read before it would be the parent's in both instances.
    let me = minix_rt::getpid();
    if me < 0 {
        userland::write_out(b"forktest: getpid failed\n");
        return 1;
    }

    if pid == 0 {
        userland::write_out(b"forktest: child pid=");
        userland::print_dec(me as u32);
        userland::write_out(b" fork=0\n");
        return 0;
    }

    userland::write_out(b"forktest: parent pid=");
    userland::print_dec(me as u32);
    userland::write_out(b" fork=");
    userland::print_dec(pid as u32);
    userland::write_out(b"\n");

    // Blocking: if the child has not exited yet this is where the parent stops, and PM's
    // `tell_parent` is what resumes it — the first time a wasm process has died with a parent
    // waiting. Which of the two happens first is the dispatcher's decision, so the exit code
    // below is the evidence rather than the order.
    let (reaped, status) = minix_rt::waitpid(pid, 0);
    if reaped < 0 {
        userland::write_out(b"forktest: waitpid failed\n");
        return 1;
    }
    userland::write_out(b"forktest: parent reaped pid=");
    userland::print_dec(reaped as u32);
    userland::write_out(b" status=");
    if status < 0 {
        userland::write_out(b"-");
        userland::print_dec(status.unsigned_abs());
    } else {
        userland::print_dec(status as u32);
    }
    userland::write_out(b"\n");
    0
}

/// The entry the host calls to run this program in a slot.
///
/// `argv` is the pointer array's address, so the module can tell a host that filled in the
/// blob at an offset of its own invention from one that used the layout above: the two
/// disagree and the process is refused rather than reading a different set of bytes.
///
/// It never returns. A program's lifetime ends in `SYS_EXIT` so that PM's half of a process
/// lifecycle runs — the kernel marks the process SIGNALED | SIG_PENDING | SLOT_FREE, queues
/// the exit for PM to read with `GETKSIG`, and notifies PM as the signal manager — and
/// `minix_rt::exit` then traps, because this target has no return path out of an entry
/// point and a spin would hang the host instead of ending the run.
///
/// Nothing here sets `p_fd_vfs`, and nothing opens a console: this process has no descriptors
/// at all yet, which is exactly the state `init` is in before it opens `/dev/console`. So
/// every write below leaves through the kernel's console shortcut, and the kernel reads the
/// bytes out of *this* instance through the copy seam (§5.1) — which is the fact the harness
/// checks, because finding 29 is what a write that transfers `count = 0` looks like when it
/// reports success.
#[unsafe(no_mangle)]
pub extern "C" fn minix_program_main(argc: i32, argv: u32) {
    let base = argv_area_ptr();
    if argc < 0 || argv != base + 4 || !blob_is_sane(base, argc as u32) {
        userland::write_err(b"program: malformed argv blob\r\n");
        minix_rt::exit(126);
    }

    let mut buf = [""; ARGV_MAX];
    // SAFETY: `blob_is_sane` established the contract `parse_args` documents — `argc`
    // pointers inside the area, each naming a NUL-terminated string inside it.
    let args = unsafe { userland::parse_args(argc, argv as *const *const u8, &mut buf) };

    // Dispatch on argv[0], the way a multi-call program does — and the reason a *module* can
    // hold more than one program while the host's registry still maps one path to one module
    // (§7.2's step 4): the path chooses the module, argv[0] chooses the program in it. A program
    // reached by `exec` gets the path the caller typed, which is why the arms name both a path
    // and a bare name: `/bin/echo` is what the shell's `run_external` builds and hands to
    // `execve` (shell.rs), while `echo` and `forktest` are what the harness passes when it
    // instantiates this module itself for a slot.
    let rc = match args.first().copied() {
        Some("echo") | Some("/bin/echo") => userland::echo(args),
        // The first arm whose work is the filesystem rather than its own stdio: `cat` opens the path
        // it is given, which on this target is a file in the boot image, so the read comes back
        // through VFS and MFS rather than out of the console. It is what reads back what the smoke
        // scenario's `>` wrote, and the only arm here that a *second* process had to have made a
        // file for.
        Some("cat") | Some("/bin/cat") => userland::cat(args),
        Some("/bin/sh") | Some("sh") => userland::sh(args),
        Some("forktest") | Some("/bin/forktest") => forktest(),
        // The first program in this module whose work is not the console's: `ping` opens `/dev/ip`
        // and the net server behind it drives the host's link (M6), so this is the arm that says
        // the module can hold a *client* of a device rather than a handler of its own stdio.
        Some("ping") | Some("/bin/ping") => userland::ping(args),
        Some(name) => {
            userland::write_err(b"program: no such command: ");
            userland::write_err(name.as_bytes());
            userland::write_err(b"\r\n");
            127
        }
        // Unreachable while `blob_is_sane` refuses an empty blob, and handled anyway:
        // indexing `args[0]` here would be a panic on wasm, which is a trap the harness
        // would report as this program faulting.
        None => {
            userland::write_err(b"program: empty argv\r\n");
            127
        }
    };

    minix_rt::exit(rc);
}
