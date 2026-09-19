//! Hand-written wasm processes for the dispatch protocol.
//!
//! Deliberately tiny and straight-line: the point is the mechanism around them.
//! Each exported entry point is one wasm instance's whole life — it runs, blocks
//! inside a syscall, and is resumed by the host when the kernel says so. Nothing
//! here knows it was suspended, which is the property the design rests on.
//!
//! # The syscall ABI is the real one
//!
//! These call `minix_syscall` with the same name and arity as the gate
//! `minix_rt` uses on wasm32, and with the numbers userland actually issues — 46
//! SEND, 47 RECEIVE, 20 GETPID, which index the kernel's syscall table. (An
//! earlier version used the kernel's *internal* IPC verbs, 1 and 2, which are
//! not syscall numbers at all.) When a real server replaces this module, the
//! host does not change: it already speaks this ABI.
//!
//! # Console
//!
//! Printing goes to a host import directly, standing in for the tty path. M3
//! replaces it with the real tty server.

#![no_std]
#![no_main]

unsafe extern "C" {
    /// The kernel's syscall gate. Suspends the caller when the kernel says so,
    /// and does not return until the host has rewound it.
    fn minix_syscall(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64;
    /// One console byte.
    fn host_console_write(byte: u32);
}

/// Syscall numbers, as `minix_std` and the kernel's table define them.
const NR_GETPID: u64 = 20;
const NR_BRK: u64 = 36;
const NR_KERNEL_CALL: u64 = 50;
const SEND_CALL: u64 = 46;
const RECEIVE_CALL: u64 = 47;

/// `SYS_VIRCOPY` (kernel call 15) and the `SELF` endpoint, as the kernel's
/// `system.rs` defines them.
const KERNEL_CALL_VIRCOPY: u64 = 15;
const ENDPOINT_SELF: i32 = 31742;

/// The process's message buffer, where the host lands a delivered message.
static mut MSG: [u8; 64] = [0; 64];

/// Where the host tells this process who it is: `[own_endpoint, peer_endpoint]`.
/// The real kernel hands a process its endpoint at exec; here the host writes it
/// into memory before the first run.
static mut INFO: [u32; 4] = [0; 4];

fn syscall0(nr: u64) -> i64 {
    // SAFETY: the host supplies the gate for every instance it instantiates.
    unsafe { minix_syscall(nr, 0, 0, 0, 0, 0, 0) }
}

fn syscall1(nr: u64, a0: u64) -> i64 {
    // SAFETY: see `syscall0`.
    unsafe { minix_syscall(nr, a0, 0, 0, 0, 0, 0) }
}

fn syscall2(nr: u64, a0: u64, a1: u64) -> i64 {
    // SAFETY: see `syscall0`.
    unsafe { minix_syscall(nr, a0, a1, 0, 0, 0, 0) }
}

fn msg_set32(idx: usize, val: u32) {
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(MSG).cast::<u32>().add(idx), val) }
}

fn msg_get32(idx: usize) -> u32 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(MSG).cast::<u32>().add(idx)) }
}

fn info_get(idx: usize) -> u32 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(INFO).cast::<u32>().add(idx)) }
}

fn print(s: &str) {
    for byte in s.bytes() {
        // SAFETY: the host supplies this import at instantiation.
        unsafe { host_console_write(byte as u32) };
    }
}

fn print_hex(s: &str, value: u64) {
    print(s);
    print("0x");
    for shift in (0..16).rev() {
        let nibble = (value >> (shift * 4)) & 0xF;
        let ch = if nibble < 10 {
            b'0' + nibble as u8
        } else {
            b'a' + (nibble - 10) as u8
        };
        // SAFETY: see `print`.
        unsafe { host_console_write(ch as u32) };
    }
    print("\n");
}

/// Address of the message buffer, so the host can move payload bytes without
/// hardcoding an offset into the module's layout.
#[unsafe(no_mangle)]
pub extern "C" fn msg_ptr() -> u32 {
    core::ptr::addr_of_mut!(MSG) as u32
}

/// Address of the info block, so the host can hand the process its endpoint.
#[unsafe(no_mangle)]
pub extern "C" fn info_ptr() -> u32 {
    core::ptr::addr_of_mut!(INFO) as u32
}

/// Process A: offers a payload to B and blocks until B takes it.
#[unsafe(no_mangle)]
pub extern "C" fn proc_a() -> i32 {
    // A non-IPC call, to show the gate routes the whole table and not just the
    // two IPC numbers this harness happens to need.
    if syscall0(NR_GETPID) >= 0 {
        print("A: getpid routed\n");
    } else {
        print("A: getpid unrouted\n");
    }

    // Slot 1 is m_type — the field a MINIX message carries its payload in.
    msg_set32(1, 0x1234);
    print("A: sending to B\n");

    // Blocks here. The kernel dequeues A and the host unwinds this stack; when
    // B receives, the host rewinds and this returns the syscall's result.
    let reply = syscall2(SEND_CALL, info_get(1) as u64, msg_ptr() as u64);

    print("A: unblocked\n");
    reply as i32
}

/// Process B: receives from A, which completes the rendezvous.
#[unsafe(no_mangle)]
pub extern "C" fn proc_b() -> i32 {
    print("B: waiting for A\n");

    let source = syscall2(RECEIVE_CALL, info_get(1) as u64, msg_ptr() as u64);

    // The two halves arrive by different routes, and the checks are separate so
    // a failure says which one broke. The payload was carried by the host,
    // because no part of the kernel can read another instance's memory;
    // m_source was the kernel's own record of who sent the message.
    if msg_get32(1) == 0x1234 {
        print("B: payload intact\n");
    } else {
        print("B: payload wrong\n");
    }
    if msg_get32(0) as i64 == source {
        print("B: m_source agrees with the syscall result\n");
    } else {
        print("B: m_source disagrees\n");
    }
    source as i32
}

/// `brk` values the process reports, so the host's checks do not have to parse
/// the console. The host cannot know the heap base — the kernel derives its
/// window from the HAL's constant and the process has to ask.
static mut HEAP_REPORT: [u64; 4] = [0; 4];

/// Address of the heap report, for the same reason as `info_ptr`.
#[unsafe(no_mangle)]
pub extern "C" fn heap_report_ptr() -> u32 {
    core::ptr::addr_of_mut!(HEAP_REPORT) as u32
}

fn report(idx: usize, value: u64) {
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(HEAP_REPORT).cast::<u64>().add(idx),
            value,
        )
    };
}

/// Process C: asks the kernel for heap and then proves the memory is real.
///
/// The instance is instantiated with less memory than the heap needs, so the
/// byte written below can only land if the host grew the linear memory in
/// response to the kernel accepting the request. That is the whole point: on
/// this port `memory.grow` is the pager, and neither the kernel nor the
/// process can reach it.
#[unsafe(no_mangle)]
pub extern "C" fn proc_heap() -> i32 {
    // Report 0: where the break starts. The kernel's answer is the HAL's heap
    // base, which is the value `minix_rt::HEAP_BASE` has to agree with.
    let base = syscall0(NR_BRK);
    report(0, base as u64);
    print_hex("C: brk(0) = ", base as u64);

    // Report 1: the break we ask for, past the kernel's pre-mapped window and
    // past this instance's initial memory.
    const TARGET: u64 = 0x0021_8000;
    let got = syscall1(NR_BRK, TARGET);
    report(1, got as u64);
    print_hex("C: brk(new) = ", got as u64);

    // Only touch memory the kernel actually granted. Writing unconditionally
    // would turn a rejected brk into a trap, which says nothing about why.
    let usable = got == TARGET as i64;
    report(2, usable as u64);
    if usable {
        let probe = (TARGET - 1) as *mut u8;
        // SAFETY: the kernel granted up to TARGET and the host must have grown
        // the instance's memory to cover it before returning.
        unsafe { core::ptr::write_volatile(probe, 0x5A) };
        let back = unsafe { core::ptr::read_volatile(probe) };
        report(3, back as u64);
        if back == 0x5A {
            print("C: heap byte survived\n");
        } else {
            print("C: heap byte corrupted\n");
        }
    } else {
        print("C: brk rejected\n");
    }

    // A request outside the window must be refused. Doing this last keeps the
    // refused value from affecting the granted one above.
    let refused = syscall1(NR_BRK, 0x0100_0000);
    if refused == -12 {
        print("C: out-of-window brk refused\n");
    } else {
        print_hex("C: out-of-window brk NOT refused: ", refused as u64);
    }

    0
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

/// Destination buffer for the `SYS_VIRCOPY` pair, separate from `MSG` on purpose:
/// the sender's message delivery also lands in the receiver's `MSG`, so a copy
/// checked there could not be told apart from the message that woke the reader.
static mut COPY_BUF: [u8; 64] = [0; 64];

/// Address of the copy destination, so both instances of this module agree on
/// it without either hardcoding the other's layout.
#[unsafe(no_mangle)]
pub extern "C" fn copy_buf_ptr() -> u32 {
    core::ptr::addr_of_mut!(COPY_BUF) as u32
}

/// `[copied byte read back, vircopy return, message arrived]`.
static mut COPY_REPORT: [u64; 4] = [0; 4];

#[unsafe(no_mangle)]
pub extern "C" fn copy_report_ptr() -> u32 {
    core::ptr::addr_of_mut!(COPY_REPORT) as u32
}

fn copy_report(idx: usize, value: u64) {
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(COPY_REPORT).cast::<u64>().add(idx),
            value,
        )
    };
}

fn buf_get32(buf: *const u8) -> u32 {
    unsafe { core::ptr::read_volatile(buf.cast::<u32>()) }
}

/// Scratch for the grant pair: the kernel reads +0 as one `CpGrant` and the bytes
/// that grant covers live at +48.
///
/// Deliberately not `COPY_BUF`: the kernel is handed this address, so it must not
/// move, and the grant check must not depend on which other checks have run. The
/// `repr(C)` field is what puts the table at the struct's own address, which is
/// the only address exported.
#[repr(C, align(8))]
struct GrantScratch {
    bytes: [u8; 64],
}
static mut GRANT_SCRATCH: GrantScratch = GrantScratch { bytes: [0; 64] };

#[unsafe(no_mangle)]
pub extern "C" fn grant_scratch_ptr() -> u32 {
    core::ptr::addr_of_mut!(GRANT_SCRATCH) as u32
}

/// Read the payload word of a message-shaped buffer — index 1, i.e. byte 4, which
/// is `m_type` in a MINIX message and where this module keeps its payload. The
/// destination check has to use the same index the source wrote.
fn buf_payload(buf: *const u8) -> u32 {
    unsafe { core::ptr::read_volatile(buf.add(4).cast::<u32>()) }
}

/// Process C: asks the kernel to copy its own buffer into another instance's.
///
/// This is the shortest path that exercises `SYS_VIRCOPY` on this port: the
/// kernel-call vector entry, the endpoint resolution, and the cross-address-space
/// copy. On an arch with no page tables the last of those is the HAL seam, and it
/// is the same operation DS uses to read a client's key — so this is the cheapest
/// way to know that path works before a server depends on it.
#[unsafe(no_mangle)]
pub extern "C" fn proc_copy_src() -> i32 {
    msg_set32(1, 0x0C0FFEE0);

    // The message layout is the kernel's `COPY_*_OFF` set: src addr @8,
    // dst endpoint @16, dst addr @24, bytes @32, flags @40, src endpoint @48.
    // Both addresses are this module's, because both instances are this module.
    let mut m = [0u8; 64];
    let buf = msg_ptr() as u64;
    let dst = copy_buf_ptr() as u64;
    m[8..16].copy_from_slice(&buf.to_ne_bytes());
    m[16..20].copy_from_slice(&info_get(1).to_ne_bytes());
    m[24..32].copy_from_slice(&dst.to_ne_bytes());
    m[32..40].copy_from_slice(&64u64.to_ne_bytes());
    m[40..44].copy_from_slice(&0i32.to_ne_bytes());
    m[48..52].copy_from_slice(&ENDPOINT_SELF.to_ne_bytes());

    let r = syscall2(NR_KERNEL_CALL, KERNEL_CALL_VIRCOPY, m.as_ptr() as u64);
    copy_report(1, r as u64);
    print("C: vircopy issued\n");

    // Wake D. This also delivers a message into D's `MSG`, which is why the copy
    // was aimed at `COPY_BUF` instead.
    msg_set32(1, 0x00B0A0D0);
    let _ = syscall2(SEND_CALL, info_get(1) as u64, msg_ptr() as u64);
    print("C: sent\n");
    0
}

/// Process D: receives from C, then looks at its own memory to see whether the
/// copy the kernel was asked to make arrived.
#[unsafe(no_mangle)]
pub extern "C" fn proc_copy_dst() -> i32 {
    print("D: waiting\n");
    let src = syscall2(RECEIVE_CALL, 0x0000ffff, msg_ptr() as u64);
    copy_report(2, src as u64);

    let copied = buf_payload(copy_buf_ptr() as *const u8);
    copy_report(0, copied as u64);
    if copied == 0x0C0FFEE0 {
        print("D: copy arrived\n");
    } else {
        print("D: copy missing\n");
    }
    0
}
