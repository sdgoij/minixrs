//! Real MINIX servers, one exported entry point each.
//!
//! Every export here is a thin `extern "C"` shim over the server's own
//! `*_server_main` — there is no stand-in behaviour and nothing is skipped. The
//! shims exist for two mechanical reasons: a wasm module can only export a
//! function, and the servers' `src/bin/*.rs` are `#![no_main]` binaries, which a
//! `cdylib`-only target cannot build.
//!
//! # Why reaching a main loop is the thing to check
//!
//! DS, RS and PM each run their own init and then enter `loop { RECEIVE }`, and
//! none of them needs another server to get there. The host therefore reads "it
//! finished init and reached its main loop" off the syscall trace, without asking
//! the servers to print anything on this port's behalf and without adding a hook
//! to them. The trace has to be read as a *tail* rather than a first syscall: RS
//! now registers its grant table and hands DS the process table before it
//! receives anything, so its first syscall is that work, and a server that blocks
//! in RECEIVE from `ANY` is what "it is in its loop" means for all three.
//!
//! What that does *not* prove is that they can talk to each other, which is what
//! the DS client at the end of this file is for: it gives DS a real request to
//! answer, so the client-server path (`ds_publish`/`ds_retrieve`) runs for the
//! first time on this port. It needs `sys_vircopy`, because DS reads the key out
//! of the client's memory and on this port only the host can reach it (§5.1).
//!
//! And DS can only *accept* that request from a process it can name, which is
//! where RS's part comes in: RS hands DS its public process table as a grant in
//! an `RS_INIT` message (`servers/rs.rs`), DS maps every entry in use, and a
//! process that then announces itself with `rs_up` gets a label of its own. The
//! second client here is the control — same key, same protocol, no announcement —
//! so the refusal an unlabelled publisher gets stays measured rather than assumed.
//!
//! That handshake closes in both directions, for the two services RS asks: DS
//! answers once it has copied the rproctab and PM answers from its own loop, and
//! `minix_rs_is_active` lets the host ask *RS* whether it consumed each answer. The
//! effect of an answer is a flag inside RS rather than a copy, so no amount of trace
//! reading can show it.

#![no_std]
#![no_main]

// Region the host uses for the Asyncify data buffer and its stack.
//
// The buffer has to live where no allocator ever writes, and a reserved static
// does not qualify: this port's direct bump heap is a fixed window at `HEAP_BASE`
// (2 MiB here), while the linker places `.bss` wherever it likes. This module's
// statics run well past that window, and a build whose scratch landed at 0x294000
// — *inside* the block cache's own range — had MFS allocate its first file read
// over the scratch and trap on the next unwind. The linker's `__heap_base` is the
// first byte past the whole static image, so a buffer there is out of every
// allocator's reach by construction.
//
// The host owns the buffer's size — 64 KiB, which the fork spike's fitted 36.1
// bytes per frame makes ~1800 frames of stack, far past any server's deepest
// block. It is generous on purpose, because an Asyncify overflow corrupts
// silently rather than trapping.
unsafe extern "C" {
    // lld's end-of-static-data symbol, defined for every module with linear
    // memory. Only its address is ever taken.
    static __heap_base: u8;
}

/// Address of the scratch region, so the host does not have to guess.
///
/// At the module's own end-of-statics, rounded up to the 16-byte alignment the
/// Asyncify struct the host writes requires.
#[unsafe(no_mangle)]
pub extern "C" fn asyncify_scratch_ptr() -> u32 {
    let end = core::ptr::addr_of!(__heap_base) as usize;
    ((end + 15) & !15) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn minix_server_ds() -> i32 {
    servers::ds::ds_server_main();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn minix_server_rs() -> i32 {
    servers::rs::rs_server_main();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn minix_server_pm() -> i32 {
    servers::pm::pm_server_main();
    0
}

/// The RAM disk block driver. Its device 0 is the boot filesystem image, which the
/// host copies into this instance at `RAMDISK_IMAGE_VA` before calling this — the
/// wasm stand-in for the kernel mapping the image on the hardware arches.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_ramdisk() -> i32 {
    servers::ramdisk::ramdisk_server_main();
    0
}

/// The virtual memory server. Every process that calls `brk()` depends on it, so it
/// is spawned before the filesystem servers whose allocators grow a heap.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_vm() -> i32 {
    servers::vm::vm_main();
    0
}

/// The MinixFS file server. Its root device is the RAM disk instance, reached over
/// BDEV.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_mfs() -> i32 {
    fs::mfs::main::mfs_main();
    0
}

/// The virtual file system server. Its `sef_cb_init_fresh` runs `mount_root`, which
/// asks MFS for the root superblock over IPC, and MFS asks the RAM disk instance for
/// the block — so reaching this server's main loop means the whole chain ran.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_vfs() -> i32 {
    // SAFETY: called exactly once, from this instance's only thread, which is the
    // contract `vfs_main` asks for.
    unsafe { servers::vfs::main::vfs_main() };
    0
}

/// The virtio block driver, spawned with **no device attached** on purpose.
///
/// `mount_root` names `virtio_blk` as its preferred root driver and then asks that
/// driver whether it has a device (`bdev_has_device`, a BDEV OPEN). There is no virtio
/// transport on wasm, so without this instance nothing answers and the probe blocks on
/// an absent peer forever — MFS waiting inside it, VFS waiting on MFS, and the RAM disk
/// never asked for a block. Spawned, it answers `EIO` because `virtio_blk_open` finds no
/// device, which is true, and `bdev_driver_root` takes the ramdisk fallback it already
/// has. Faithful rather than a stub: a driver that is present and reports nothing
/// attached. `PORTING_PLAN.md` finding 25.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_virtio_blk() -> i32 {
    servers::virtio_blk::virtio_blk_server_main();
    0
}

/// The virtio-net DL server, whose hardware on this arch is the host's link (M6).
///
/// `net` drives it: `DL_CONF` for the MAC, then `DL_READV_S`/`DL_WRITEV_S` per packet — the protocol
/// `libnetdriver` defines and the one the arches' QEMU device answers. What is the host's here is
/// only the *wire*: `virtio_net_probe` asks the link for a MAC instead of reading a device config,
/// and a transmit is a frame handed over rather than a descriptor the device completes
/// (`crates/drivers/src/network/virtio_net.rs`). The frames themselves are the guest's on both.
///
/// It has to be spawned for `net` to start at all: that server's first act is `DL_CONF`, a SENDREC
/// that blocks until this instance answers it. A system with `net` and no `virtio_net` is a net
/// server that never reaches its loop.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_virtio_net() -> i32 {
    servers::virtio_net::virtio_net_server_main();
    0
}

/// The net server: ARP and ICMP behind `/dev/ip`, and the UDP and TCP sockets behind `/dev/udp` and
/// `/dev/tcp` (M6).
///
/// The device nodes are already in the boot image (`boot-image`'s `DEVICES` table: major 14, minors
/// 0, 1 and 2), so this instance is what an `open("/dev/ip")` resolves to — VFS routes by major, and
/// without an instance the open blocks on a peer that never answers. Its own first acts are
/// `register_grants` and `dl_conf` to `virtio_net`, which is why that instance is spawned as well.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_net() -> i32 {
    servers::net::net_server_main();
    0
}

/// The tty server: the console, the RS-232 lines and the pty pairs, all behind
/// VFS's device layer.
///
/// `/dev/console` is a char-device node in the boot image (major 5, minor 0 —
/// `boot-image`'s `DEVICES` table), and `userland::init` opens it to route its stdio
/// through VFS. What an open resolves to is the dmap entry for major 5, so this
/// instance is the other half of that: without it nothing answers `CDEV_OPEN` for
/// the console and init's setup fails into a spin.
///
/// Its own init is not passive — it registers the console with devman
/// (`devman_add_device("tty0", 0)`), which costs a grant table (`SYS_SETGRANT`) and
/// a copy of the registration blob out of this instance, so reaching the main loop
/// below means the grant and the seam worked.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_tty() -> i32 {
    servers::tty::tty_server_main();
    0
}

/// The device manager, whose VTreeFS the `/devices` mount point names.
///
/// VFS's init mounts it *after* the root filesystem — `mount_devman`, whose own comment
/// says it "blocks until devman starts, like mount_root/MFS". Without this instance VFS
/// stops there on an absent peer, which is the same shape as the `virtio_blk` probe
/// above (`PORTING_PLAN.md` finding 25), so it is answered the same way: by being there.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_devman() -> i32 {
    servers::devman::devman_server_main();
    0
}

/// The framebuffer server: `/dev/fb` (major 19, minor 0 in the boot image).
///
/// On wasm its backend is the host's canvas rather than a device on a bus, so the surface is
/// this instance's own memory, the mode comes from the host, and a flush hands the pixels over
/// - see `drivers::video::fb::CanvasArch` and `ARCH_WASM32.md` §11 (M5a). A host with no display
/// leaves it a driver that found no device, which is why it still reaches its main loop either
/// way: the boot's device map already names this slot, so a client's `CDEV_OPEN` has to be
/// answered by something.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_fb() -> i32 {
    servers::fb::fb_server_main();
    0
}

/// The input server: `/dev/kbd` (major 20) — the keyboard and the pointer as events.
///
/// On wasm its third backend is the host's own records (M5c, `ARCH_WASM32.md` §11), which is why the
/// instance has to exist at all: nothing on this port reads an 8042 or probes for a virtio-input, and
/// the desktop has no other way to be told that the reader moved the mouse. It boots after `fb` and
/// before `wserver` because that is the kernel's own `BOOT_IMAGE` order, and the order is load-bearing
/// in one direction: `wserver` registers itself as the input server's consumer while it attaches, so
/// the server it registers with has to be there to answer.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_input() -> i32 {
    servers::input::input_server_main();
    0
}

/// Events the **input server's** ring is holding. Call it on that instance.
///
/// The check that a host record became a guest event reads this rather than the host's queue: what
/// the host queued is the input, and "the guest took it" is a claim about this ring. Zero is the
/// ordinary answer at rest — the driver drains its ring into the consumer as it goes.
#[unsafe(no_mangle)]
pub extern "C" fn minix_input_events_queued() -> i32 {
    servers::input::queued_events()
}

/// The window server: the compositor, on `/dev/fb`.
///
/// On wasm it composes the desktop into a surface of its own — there is no device memory to map and
/// no page tables to map one through — and hands each frame to `/dev/fb` as a single datagram write,
/// which the fb driver copies into the surface it owns and presents when the compositor flushes
/// (M5b, `ARCH_WASM32.md` §11). It boots before `tty` on purpose: the console's window is created at
/// tty's init, and a console with no window server behind it has nowhere to put its cells.
#[unsafe(no_mangle)]
pub extern "C" fn minix_server_wserver() -> i32 {
    servers::wserver::wserver_main();
    0
}

/// The window server's pointer position as `x << 16 | y`, as **that instance** has it.
///
/// The one claim about M5c that no other reading can make: the host's queue is the input and the
/// input server's ring is the delivery, while this says the desktop *moved* — which is the whole
/// point of a pointer record. Both coordinates are non-negative and inside the desktop (the driver
/// clamps them), so a packed pair has no sign to lose.
#[unsafe(no_mangle)]
pub extern "C" fn minix_wserver_pointer() -> u32 {
    let (x, y) = servers::wserver::pointer_position();
    ((x as u32) << 16) | (y as u32 & 0xffff)
}

/// The console's cell grid, row-major, 80x24, as **the tty server's model** computed it.
///
/// Call it on the tty instance: the grid is that server's `console` module state, and every instance
/// carries all the servers' code, so an untouched one answers a pointer into cells that were never
/// written. What this is for is the check that the console's cells are the ones the guest composed —
/// the alternative is inferring them from the bytes the host saw on the console, which is the claim
/// M5b is making. See `console.rs`.
#[unsafe(no_mangle)]
pub extern "C" fn minix_console_grid_ptr() -> u32 {
    servers::console::report_ptr()
}

/// The console screen's cursor cell, as `row << 16 | col`.
#[unsafe(no_mangle)]
pub extern "C" fn minix_console_cursor() -> u32 {
    servers::console::report_cursor()
}

/// Device 0's base address in bytes, as **the RAM disk instance** computed it.
///
/// The host put the image somewhere and the server sized the device from the
/// image's own superblock, so the harness must read the server's answer rather than
/// its own constant — reading the host's back would only restate it. Call it on the
/// RAM disk instance: every instance carries all the servers' code, and an
/// untouched one answers `0`.
#[unsafe(no_mangle)]
pub extern "C" fn minix_ramdisk_device_base() -> u32 {
    servers::ramdisk::device_geometry().0 as u32
}

/// Device 0's size in bytes, as **the RAM disk instance** computed it.
#[unsafe(no_mangle)]
pub extern "C" fn minix_ramdisk_device_size() -> u32 {
    servers::ramdisk::device_geometry().1 as u32
}

/// Whether `endpoint`'s slot has left `RS_INITIALIZING` — read it from the **RS
/// instance**.
///
/// The init-complete reply's effect is a state change inside RS, not a byte that
/// crosses the host boundary, so this is the only way the harness can check that
/// `do_init_ready` actually consumed the answer rather than merely receiving it.
/// Each instance carries all three servers' code, so calling this on the wrong
/// instance reads an untouched table and answers `0`.
#[unsafe(no_mangle)]
pub extern "C" fn minix_rs_is_active(endpoint: i32) -> i32 {
    // SAFETY: single-threaded instance, and `is_active` only reads this
    // instance's own process table.
    if unsafe { servers::rs::is_active(endpoint) } {
        1
    } else {
        0
    }
}

/// How many devices devman has registered under its device root — read it from the
/// **devman** instance.
///
/// The driver that registers is the tty server, and whether its `ADD` was *accepted*
/// is devman's own state rather than anything the host can watch cross, so the answer
/// has to come from devman. Asking any other instance reads an empty table and
/// answers `0`.
#[unsafe(no_mangle)]
pub extern "C" fn minix_devman_device_count() -> i32 {
    servers::devman::devman_device_count()
}

// ------------------------------------------------------------------ DS client

/// What the client observed, so the host can read it without a console.
///
/// `[publish status, retrieve status, retrieved value, rs_up status,
/// unsolicited-init status]` — the first two are errnos (negated), the third the
/// value the store handed back, the fourth the status of announcing this process
/// to RS (which is what makes the first one possible), and the fifth the status of
/// claiming to be initialised without having been asked. Nothing in this module
/// declares a console import, so a report in memory is the only way the client's
/// result can reach the host; the M2 harness established the pattern.
static mut DS_REPORT: [i64; 5] = [0; 5];

/// Address of the report, so the host does not have to parse the module layout.
#[unsafe(no_mangle)]
pub extern "C" fn ds_report_ptr() -> u32 {
    core::ptr::addr_of_mut!(DS_REPORT) as u32
}

fn ds_report(idx: usize, value: i64) {
    // SAFETY: single-threaded instance, and the host reads the report only after
    // the entry point has returned or blocked. `addr_of_mut!` avoids taking a
    // reference to the mutable static.
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(DS_REPORT).cast::<i64>().add(idx),
            value,
        )
    };
}

/// A real DS client: announce itself to RS, publish a value, then read it back.
///
/// Deliberately `minix_util`'s clients rather than hand-rolled messages, so what
/// runs is the real protocol against the real servers. The key is a `const` slice
/// in this instance's rodata, and its address is what DS is handed — so the value
/// can only come back if DS's `sys_vircopy` read it out of *this* instance, which
/// is a copy no part of the kernel can make here.
///
/// The `rs_up` call is what makes the publish possible at all: DS seeds its label
/// table from RS's public process table and RS publishes each service it
/// registers, so a process DS cannot name is refused every write. Announcing
/// first, with the label RS then publishes, is the whole of what authorises this
/// client.
#[unsafe(no_mangle)]
pub extern "C" fn minix_ds_client() -> i32 {
    const KEY: &[u8] = b"seam.key";
    const VALUE: u32 = 0x2a;

    ds_report(
        3,
        match minix_util::rs::rs_up(b"seamclient") {
            Ok(()) => 0,
            Err(e) => -(e.0 as i64),
        },
    );

    ds_report(
        0,
        match minix_util::ds::ds_publish_u32(KEY, VALUE) {
            Ok(()) => 0,
            Err(e) => -(e.0 as i64),
        },
    );

    match minix_util::ds::ds_retrieve_u32(KEY) {
        Ok(value) => {
            ds_report(1, 0);
            ds_report(2, value as i64);
        }
        Err(e) => {
            ds_report(1, -(e.0 as i64));
            ds_report(2, -1);
        }
    }

    // The other direction of the same message: `rs_up` made this process known to
    // RS, and it still may not report itself initialised — RS puts a slot into
    // `RS_INITIALIZING` only when *it* asks, so an unsolicited `RS_INIT` is
    // refused. Without that, any client could mark itself ready.
    ds_report(
        4,
        match minix_util::rs::rs_init_ready(0) {
            Ok(()) => 0,
            Err(e) => -(e.0 as i64),
        },
    );

    0
}

/// A client that never announces itself to RS, so DS has no label for it.
///
/// This is the behaviour the seeding must *not* change: the label table is what
/// authorises a writer, and a process that skipped `rs_up` is not in it, so its
/// publish is refused as EPERM while a labelled one's succeeds. It writes the
/// same key, so a mistaken acceptance would be visible as a changed value rather
/// than as a missing entry.
#[unsafe(no_mangle)]
pub extern "C" fn minix_ds_client_unregistered() -> i32 {
    const KEY: &[u8] = b"seam.key";

    ds_report(
        0,
        match minix_util::ds::ds_publish_u32(KEY, 0x99) {
            Ok(()) => 0,
            Err(e) => -(e.0 as i64),
        },
    );

    0
}

// -------------------------------------------------------------------- INIT

/// Write `n` in decimal into the tail of `out`, returning the digits.
fn decimal(mut n: u32, out: &mut [u8; 10]) -> &[u8] {
    let mut i = out.len();
    loop {
        i -= 1;
        out[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &out[i..]
}

/// Print a signed decimal, for the report lines that carry a status.
fn write_i32(v: i32) {
    let mut out = [0u8; 12];
    let mut n = v.unsigned_abs();
    let mut i = out.len();
    loop {
        i -= 1;
        out[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if v < 0 {
        i -= 1;
        out[i] = b'-';
    }
    userland::write_out(&out[i..]);
}

/// Sentinel for a report entry the run never reached, so a step that did not happen
/// cannot be mistaken for one that succeeded with a zero status.
const INIT_NOT_REACHED: i64 = -4096;

/// Facts about INIT's console setup for the host to assert numerically, rather than
/// parse out of the console: `[open status, dup2 status, VFS-routed write status]`,
/// where a negative status is a negated errno and the write's is a byte count.
static mut INIT_REPORT: [i64; 3] = [INIT_NOT_REACHED; 3];

/// Address of the report, so the host does not have to parse the module layout.
#[unsafe(no_mangle)]
pub extern "C" fn init_report_ptr() -> u32 {
    core::ptr::addr_of_mut!(INIT_REPORT) as u32
}

fn init_report(idx: usize, value: i64) {
    // SAFETY: single-threaded instance, and the host reads the report only after the
    // entry point has blocked or exited. `addr_of_mut!` avoids taking a reference to the
    // mutable static.
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(INIT_REPORT).cast::<i64>().add(idx),
            value,
        )
    };
}

/// The first *user* process: INIT, in the slot `BOOT_IMAGE` already reserves for
/// it.
///
/// Every instance before this one has been a server, and a server is a particular
/// kind of process: a privilege slot of its own, `SYS_PROC` set, `SRV_T` traps,
/// every kernel call allowed, and a main loop that never ends. INIT is the other
/// kind. `proc_init` links it to the *shared* USER privilege slot instead
/// (`s_proc_nr == NONE`, `USR_T` traps, an empty kernel-call mask, `s_ipc_to`
/// holding only the services an ordinary user may call), and its lifetime ends.
/// What was missing was an instance to put in that slot; the harness asks the
/// kernel which kind it made with `minix_proc_kind` rather than trusting the slot
/// number.
///
/// Both of the things it does are cross-boundary. Its output leaves through
/// `sys_write_handler`'s console shortcut rather than through VFS: INIT has not
/// dup2'd a redirect onto fd 1, so `p_fd_vfs` is 0, and the kernel reads the bytes
/// out of *this* instance through the copy seam (§5.1) and emits them itself. And
/// `getpid` is not a kernel syscall on this port — `minix-rt` reaches PM with
/// `PM_GETPID` — so the pid it prints is PM's answer, which INIT can only have got
/// because the shared USER privilege slot lets an ordinary user send to PM. No
/// write from any instance reached the console before findings 28 and 29, so this
/// is also the first process that could have shown the difference.
///
/// It walks the console chain, which is the whole of M3e: `userland::init`'s own next
/// steps, in its order — open `/dev/console`, dup2 the fd onto 0..2, then tell the kernel
/// those fds are VFS's. From the `set_fd_vfs` on, fd 1 writes leave the kernel's console
/// shortcut and take the long way: the kernel forwards them to VFS, VFS vircopies them
/// out of *this* instance into a `CDEV_WRITE` message, and the tty writes the bytes with
/// its own `write(1)`. `/dev/console` is major 5 in the boot image and VFS's dmap maps
/// major 5 to the tty instance, so the open needs nothing else — which is exactly what
/// had not been established. The dup2 has to come before the flag: forwarding fd 1 to
/// VFS asks VFS for *fd 1's* filp, and with nothing dup2'd onto fd 1 there is no filp and
/// the answer is EBADF.
///
/// It ends by exec'ing `/bin/sh`, which is M7a: on this port exec is module instantiation
/// (§7.2), so the shell arrives as a wasm module the host instantiates *into this slot*,
/// carrying the stdio above with it because the process is the same process — the same
/// fds, the same `p_fd_vfs`, the same VFS filps. This instance then ceases to exist, which
/// is what the harness checks.
#[unsafe(no_mangle)]
pub extern "C" fn minix_init() {
    userland::write_out(b"init: booting MINIX/Rust\r\n");
    // The pid line in one write, and `getpid` *before* it: the console is shared, and a line whose
    // pieces straddle a blocking syscall can be split by whoever else runs while it waits. `getpid`
    // is a SENDREC to PM, so this is exactly such a wait — and when the input server joined the boot
    // (M5c), the desktop's own `ready` line landed in the middle of this one.
    let mut digits = [0u8; 10];
    let pid = decimal(minix_rt::getpid() as u32, &mut digits);
    let mut line = [0u8; 32];
    let mut n = 0;
    for b in b"init: pid=" {
        line[n] = *b;
        n += 1;
    }
    for b in pid {
        line[n] = *b;
        n += 1;
    }
    line[n] = b'\r';
    line[n + 1] = b'\n';
    userland::write_out(&line[..n + 2]);

    // Every line above this one came out through the kernel's console shortcut, which is
    // what serves fd 1 until a process says otherwise. These are the first writes in the
    // port that have to reach a char driver.
    let fd = minix_rt::open(b"/dev/console", 0o2); // O_RDWR
    init_report(0, fd);
    userland::write_out(b"init: open(/dev/console) -> ");
    write_i32(fd as i32);
    userland::write_out(b"\r\n");
    if fd < 0 {
        userland::write_out(b"init: no console, exiting\r\n");
        minix_rt::exit(1);
    }
    let fd = fd as i32;

    let mut dup_err = 0i32;
    for slot in 0..3 {
        if let Err(e) = minix_std::fs::dup2(fd, slot) {
            dup_err = e.0;
        }
    }
    init_report(1, -dup_err as i64);
    userland::write_out(b"init: dup2 onto 0..2 -> ");
    write_i32(-dup_err);
    userland::write_out(b"\r\n");
    if dup_err != 0 {
        userland::write_out(b"init: stdio not routed, exiting\r\n");
        minix_rt::exit(1);
    }

    // SAFETY: this instance has one thread, and these are its own descriptors. The kernel
    // reads this flag to decide whether fd 1's writes take the shortcut or go to VFS.
    unsafe {
        minix_rt::set_fd_vfs(0, 1);
        minix_rt::set_fd_vfs(1, 1);
        minix_rt::set_fd_vfs(2, 1);
    }

    // From here on every line is evidence about the chain rather than a report about the
    // process: it can only appear on the console if VFS routed it to the tty and the tty
    // wrote it. The explicit `write` is what puts a number in the report; `write_out`
    // after it is the same call a shell would make.
    const VFS_LINE: &[u8] = b"init: stdio is VFS-routed\r\n";
    let n = unsafe { minix_rt::write(1, VFS_LINE.as_ptr(), VFS_LINE.len()) };
    init_report(2, n);
    userland::write_out(b"init: VFS-routed write -> ");
    write_i32(n as i32);
    userland::write_out(b"\r\n");

    // init's last step is `exec("/bin/sh")`, and this is where M7a earns its keep: the
    // shell is a *module* the host instantiates into this slot, not a call to
    // `userland::sh` in place. The call is `userland::init`'s, line for line — the same
    // path, the same argv, the same PM→VFS chain — and what differs is only the answer:
    // the module the host has for `/bin/sh` is wasm rather than the ELF the boot image
    // happens to carry at that path. Getting there means the frame built here crosses two
    // instance boundaries (this one to VFS by `sys_vircopy`, VFS's to the kernel) before
    // the kernel parses it back into argv, which is the port's copy seam doing the work a
    // shared address space does on the other arches.
    let argv: [*const u8; 2] = [c"/bin/sh".as_ptr() as *const u8, core::ptr::null()];
    let ret = unsafe {
        minix_rt::execve(
            c"/bin/sh".as_ptr() as *const u8,
            c"/bin/sh".to_bytes_with_nul().len(),
            argv.as_ptr(),
            core::ptr::null(),
        )
    };

    // Only a failure reaches here, and on failure the image was not replaced — so this is
    // init's own error path, not the shell's, and the report is about the exec. It exits
    // rather than spinning: `userland::init` loops on `getpid` here, which on this target
    // hangs the host rather than failing (finding 12).
    userland::write_out(b"init: exec failed: err=");
    write_i32(ret);
    userland::write_out(b"\r\n");
    minix_rt::exit(1);
}
