//! Hand-written wasm processes for the M2 dispatch protocol.
//!
//! These are deliberately tiny and straight-line: the point of M2 is the
//! mechanism around them, not the programs. Each exported entry point is one
//! wasm instance's whole life — it runs, blocks inside `host_syscall`, and is
//! resumed by the host when the kernel says so. Nothing here is aware that it
//! was ever suspended, which is the property the design rests on (§4.2).
//!
//! # Console
//!
//! Printing goes to a host import directly, standing in for the tty path. M3
//! replaces it with the real tty server; doing it this way here keeps M2's
//! failure modes confined to the dispatch protocol.

#![no_std]
#![no_main]

unsafe extern "C" {
    /// Enter the kernel. Returns the syscall result, and may suspend the caller
    /// before it ever returns — the host drives the unwind.
    fn host_syscall(nr: u32, dst: u32) -> i32;
    /// One console byte.
    fn host_console_write(byte: u32);
}

/// MINIX IPC syscall numbers, as `kernel::ipc` defines them.
const SEND: u32 = 0x01;
const RECEIVE: u32 = 0x02;

/// The process's message buffer. The kernel cannot read it (there is no address
/// translation), so the host carries the bytes between instances; this buffer is
/// where they land.
static mut MSG: [u8; 64] = [0; 64];

/// Where the host tells this process who it is: `[own_endpoint, peer_endpoint]`.
/// The real kernel hands a process its endpoint at exec; here the host writes it
/// into memory before the first run.
static mut INFO: [u32; 4] = [0; 4];

fn msg_set(idx: usize, val: u8) {
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(MSG).cast::<u8>().add(idx), val) }
}

fn msg_get(idx: usize) -> u8 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(MSG).cast::<u8>().add(idx)) }
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
    msg_set(0, 0xAB);
    print("A: sending to B\n");

    // Blocks here. The kernel dequeues A and the host unwinds this stack; when
    // B receives, the host rewinds and this returns the syscall's result.
    // SAFETY: the host supplies this import at instantiation.
    let reply = unsafe { host_syscall(SEND, info_get(1)) };

    print("A: unblocked\n");
    reply
}

/// Process B: receives from A, which completes the rendezvous.
#[unsafe(no_mangle)]
pub extern "C" fn proc_b() -> i32 {
    print("B: waiting for A\n");

    // SAFETY: the host supplies this import at instantiation.
    let source = unsafe { host_syscall(RECEIVE, info_get(1)) };

    // A's payload had to cross instance boundaries to get here, and no part of
    // the kernel could have moved it.
    if msg_get(0) == 0xAB {
        print("B: got A's payload\n");
    } else {
        print("B: payload did not arrive\n");
    }
    source
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}
