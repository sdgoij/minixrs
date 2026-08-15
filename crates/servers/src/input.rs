//! PS/2 keyboard driver server — `/dev/kbd` (major 20).
//!
//! Registers an IRQ-1 hook via SYS_IRQCTL (the kernel's `kbd_isr_entry`
//! notifies this process on every key event), drains the 8042 scancode
//! stream through SYS_DEVIO, decodes scancodes to HID events with the
//! input crate's state machine, and serves the decoded events as a
//! character device. Reads are non-blocking: a read with no pending
//! events returns EAGAIN and the consumer polls (blocking reads and
//! CDEV_SELECT are follow-ons once VFS passes read nonblock flags).

use arch_common::com::{CDEV_CLOSE, CDEV_OPEN, CDEV_READ, is_cdev_rq};
use drivers::input::controller::Ps2Controller;
use drivers::input::devio::DevioIo;
use drivers::input::driver::{InputCallbacks, InputDriver};

/// A decoded input event: HID usage page, usage code, and press value.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InputEvent {
    pub page: u16,
    pub code: u16,
    pub press: i32,
}

const EV_QUEUE_LEN: usize = 64;
const EAGAIN: i32 = -11;

/// Decode state for the keyboard scancode stream.
static mut INPUT_DRIVER: InputDriver = InputDriver::new();

/// Consumer endpoint (the window server) notified when events queue; -1 =
/// none. Registered via `INPUT_REG_CONSUMER`.
static mut CONSUMER_EP: i32 = -1;

/// Bounded event ring (dropping when full — a keyboard repeat outruns
/// the consumer rather than blocking the IRQ path).
static mut EV_QUEUE: [InputEvent; EV_QUEUE_LEN] = [InputEvent {
    page: 0,
    code: 0,
    press: 0,
}; EV_QUEUE_LEN];
static mut EV_HEAD: usize = 0;
static mut EV_TAIL: usize = 0;

/// Scratch space for the inline read reply (48-byte payload).
static mut EV_SCRATCH: [u8; 48] = [0; 48];

/// [`InputCallbacks`] implementation that queues decoded events.
struct ServerCallbacks;

impl InputCallbacks for ServerCallbacks {
    fn key_event(&mut self, page: u16, code: u16, press: i32) {
        unsafe {
            let next = (EV_TAIL + 1) % EV_QUEUE_LEN;
            if next == EV_HEAD {
                return; // queue full — drop
            }
            EV_QUEUE[EV_TAIL] = InputEvent { page, code, press };
            EV_TAIL = next;
        }
    }

    fn mouse_event(&mut self, _page: u16, _code: u16, _value: i32, _flags: u16) {}
}

/// Drain all pending scancodes from the 8042 and decode them into events.
/// Returns true when at least one event was queued (EV_TAIL advanced).
fn drain_keyboard() -> bool {
    let driver = unsafe { &mut *core::ptr::addr_of_mut!(INPUT_DRIVER) };
    let mut cb = ServerCallbacks;
    let tail = unsafe { EV_TAIL };
    // The 8042 keeps IRQ 1 asserted while data is pending, so the ISR
    // notifies once per batch; drain until the controller reports empty.
    while unsafe { driver.intr_handler::<DevioIo, ServerCallbacks>(&mut cb) } {}
    unsafe { EV_TAIL != tail }
}

/// Pop up to `count` bytes of events (8 bytes each) into `buf`.
fn pop_events(buf: &mut [u8], count: usize) -> usize {
    unsafe {
        let mut n = 0;
        while n + 8 <= count && EV_HEAD != EV_TAIL {
            let ev = EV_QUEUE[EV_HEAD];
            EV_HEAD = (EV_HEAD + 1) % EV_QUEUE_LEN;
            buf[n..n + 2].copy_from_slice(&ev.page.to_le_bytes());
            buf[n + 2..n + 4].copy_from_slice(&ev.code.to_le_bytes());
            buf[n + 4..n + 8].copy_from_slice(&ev.press.to_le_bytes());
            n += 8;
        }
        n
    }
}

/// Port-I/O hook: route every access through SYS_DEVIO (userland drivers
/// have no direct port access). The request/port/value live at
/// payload[0..12]; the result comes back in payload[0..4].
///
/// # Safety
///
/// Called from the devio hook registry; safe as long as the request is a
/// valid SYS_DEVIO shape.
#[cfg(target_os = "minix")]
fn devio_hook(request: u32, port: u16, value: u32) -> u32 {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&request.to_ne_bytes());
    msg[12..16].copy_from_slice(&(port as u32).to_ne_bytes());
    msg[16..20].copy_from_slice(&value.to_ne_bytes());
    // SYS_DEVIO is kernel_call 21 (kernel_call adds KERNEL_CALL itself).
    minix_rt::kernel_call(21, &mut msg);
    u32::from_ne_bytes(msg[8..12].try_into().unwrap_or([0u8; 4]))
}

/// Register an IRQ-1 hook (SYS_IRQCTL) so the kernel notifies this
/// process on every keyboard interrupt.
#[cfg(target_os = "minix")]
fn register_irq() {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&arch_common::com::IRQ_SETPOLICY.to_ne_bytes());
    msg[12..16].copy_from_slice(&1i32.to_ne_bytes()); // IRQ 1
    msg[16..20].copy_from_slice(&arch_common::com::IRQ_REENABLE.to_ne_bytes());
    msg[20..24].copy_from_slice(&1i32.to_ne_bytes()); // notify_id
    let _ = minix_rt::kernel_call(19, &mut msg); // SYS_IRQCTL
}

/// Main loop: receive kernel notifications (keyboard IRQs) and CDEV
/// requests for `/dev/kbd`.
pub fn input_server_main() {
    #[cfg(target_os = "minix")]
    {
        const ANY: i32 = 0x0000_ffff;

        drivers::input::devio::input_set_devio(devio_hook);

        // Initialize the 8042 (enables the keyboard and mouse interrupts).
        let _ = unsafe { Ps2Controller::init::<DevioIo>() };
        register_irq();

        loop {
            let mut msg = arch_common::ipc::Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };
            let src = unsafe {
                minix_rt::syscall2(
                    minix_rt::RECEIVE_CALL,
                    ANY as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                )
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;
            let call_type = msg.m_type as u32;

            // Kernel notification: a keyboard IRQ fired.
            let is_notify =
                (msg.m_type as u32).wrapping_sub(arch_common::com::NOTIFY_MESSAGE) < 0x100;
            if is_notify {
                if drain_keyboard() {
                    // Wake the registered consumer (window server) so it
                    // routes the queued keys without polling.
                    let consumer = unsafe { CONSUMER_EP };
                    if consumer >= 0 {
                        let mut notify = arch_common::ipc::Message {
                            m_source: 0,
                            m_type: arch_common::com::NOTIFY_MESSAGE as i32,
                            m_payload: unsafe { core::mem::zeroed() },
                        };
                        unsafe {
                            let r = minix_rt::syscall2(
                                minix_rt::SENDNB_CALL,
                                consumer as u64,
                                &mut notify as *mut arch_common::ipc::Message as u64,
                            );
                            if r < 0 {
                                minix_rt::write(2, b"input: notify consumer failed\n".as_ptr(), 30);
                            }
                        }
                    }
                }
                continue;
            }

            // Direct registration request (not a CDEV message): the sender
            // becomes the keyboard-event consumer.
            if call_type == arch_common::com::INPUT_REG_CONSUMER {
                unsafe {
                    CONSUMER_EP = src_ep;
                }
                msg.m_type = 0;
                unsafe {
                    minix_rt::syscall2(
                        minix_rt::SEND_CALL,
                        src_ep as u64,
                        &mut msg as *mut arch_common::ipc::Message as u64,
                    );
                }
                continue;
            }

            let result = if is_cdev_rq(call_type) {
                unsafe { handle_cdev_request(&mut msg, src_ep, call_type) }
            } else {
                -38 // ENOSYS
            };
            msg.m_type = result;
            unsafe {
                minix_rt::syscall2(
                    minix_rt::SEND_CALL,
                    src_ep as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                );
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // Host stub — the server loop cannot run outside the MINIX target.
    }
}

/// Dispatch a CDEV request to the input driver.
///
/// # Safety
///
/// `msg` must point to a valid received message.
unsafe fn handle_cdev_request(
    msg: &mut arch_common::ipc::Message,
    _who_e: i32,
    call_type: u32,
) -> i32 {
    // Standard CDEV message layout (m2 fields):
    //   m2_i1 = minor (+0), m2_i2 = flags (+4), m2_i3 = grant (+8)
    //   m2_l1 = position (+16), m2_l2 = count (+24), m2_l3 = inline data (+32)
    match call_type {
        CDEV_OPEN => {
            let access = unsafe { msg.m_payload.m2.m2i2 };
            access
        }
        CDEV_CLOSE => 0,
        CDEV_READ => {
            let count = unsafe { msg.m_payload.m2.m2l2 as usize };
            let n = count.min(48);
            let dst = unsafe { &mut *core::ptr::addr_of_mut!(EV_SCRATCH) };
            let got = pop_events(&mut dst[..n], n);
            if got > 0 {
                unsafe {
                    msg.m_payload.raw[..got].copy_from_slice(&dst[..got]);
                }
                got as i32
            } else {
                EAGAIN
            }
        }
        _ => -38, // ENOSYS
    }
}
