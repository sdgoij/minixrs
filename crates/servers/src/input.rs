//! Keyboard + mouse driver server — `/dev/kbd` (major 20).
//!
//! Two backends feed the same HID event ring: the 8042 (x86, IRQ-1 hook
//! via SYS_IRQCTL — the kernel's `kbd_isr_entry` notifies this process on
//! every key event; IRQ-12 hook for the PS/2 mouse) and virtio-input
//! (x86 virtio-mouse, riscv/aarch64 virtio-keyboard — no wired userland
//! IRQ, so the event queue is polled on a periodic SYS_SETALARM). Both
//! drain into the ring, which the window server consumes as a character
//! device. Reads are non-blocking: a read with no pending events returns
//! EAGAIN and the consumer polls (blocking reads and CDEV_SELECT are
//! follow-ons once VFS passes read nonblock flags).

use arch_common::com::{CDEV_CLOSE, CDEV_OPEN, CDEV_READ, is_cdev_rq};
#[cfg(target_os = "minix")]
use drivers::bus::virtio::{virtio_set_devio, virtio_set_phys_delta};
use drivers::input::constants::{
    INPUT_BUTTON_1, INPUT_FLAG_ABS, INPUT_FLAG_REL, INPUT_GD_X, INPUT_GD_Y, INPUT_PAGE_ABS,
    INPUT_PAGE_BUTTON, INPUT_PAGE_GD, INPUT_PAGE_KEY,
};
#[cfg(target_arch = "x86_64")]
use drivers::input::controller::Ps2Controller;
use drivers::input::devio::DevioIo;
use drivers::input::driver::{InputCallbacks, InputDriver};
use drivers::input::virtio_input::{EV_ABS, EV_KEY, EV_REL, VirtioInput, keycode_to_usage};

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

/// Virtio-input backend (virtio-keyboard on riscv/aarch64, virtio-mouse
/// on x86); uninitialized when no virtio-input device is present.
static mut VIRTIO_INPUT: VirtioInput = VirtioInput::new();

/// Whether the 8042 PS/2 controller came up. Only x86 has an I8042;
/// on riscv/aarch64 SYS_DEVIO returns ENOSYS and `devio_hook` reads the
/// request back as the status byte, so draining the absent controller
/// would spin forever.
static mut PS2_READY: bool = false;

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

/// Enqueue one decoded event into the ring (drops when full).
fn push_event(page: u16, code: u16, press: i32) {
    unsafe {
        let next = (EV_TAIL + 1) % EV_QUEUE_LEN;
        if next == EV_HEAD {
            return; // queue full — drop
        }
        EV_QUEUE[EV_TAIL] = InputEvent { page, code, press };
        EV_TAIL = next;
    }
}

impl InputCallbacks for ServerCallbacks {
    fn key_event(&mut self, page: u16, code: u16, press: i32) {
        push_event(page, code, press);
    }

    fn mouse_event(&mut self, page: u16, code: u16, value: i32, _flags: u16) {
        // The page distinguishes the semantics: GD (0x01) carries X/Y
        // deltas (relative), BUTTON (0x09) carries button state 0/1
        // (absolute). Both fit the same {page, code, press} event shape.
        push_event(page, code, value);
    }
}

/// Drain all pending scancodes from the 8042 and decode them into events.
/// Returns true when at least one event was queued (EV_TAIL advanced).
fn drain_keyboard() -> bool {
    if !unsafe { PS2_READY } {
        return false;
    }
    let driver = unsafe { &mut *core::ptr::addr_of_mut!(INPUT_DRIVER) };
    let mut cb = ServerCallbacks;
    let tail = unsafe { EV_TAIL };
    // The 8042 keeps IRQ 1 asserted while data is pending, so the ISR
    // notifies once per batch; drain until the controller reports empty.
    while unsafe { driver.intr_handler::<DevioIo, ServerCallbacks>(&mut cb) } {}
    unsafe { EV_TAIL != tail }
}

/// Drain pending virtio-input events into the HID event ring. The device
/// emits Linux input events: EV_KEY with Linux keycodes (keyboard —
/// `keycode_to_usage` maps them to HID usages; mouse buttons BTN_* map to
/// the BUTTON page), EV_REL X/Y deltas (relative mouse — the GD page),
/// and EV_ABS X/Y positions (absolute tablet — the ABS page, QEMU
/// normalized to 0..0x7FFF). Which device the instance probed decides
/// which codes appear. Returns true when an event was queued.
fn drain_virtio() -> bool {
    let tail = unsafe { EV_TAIL };
    let input = unsafe { &mut *core::ptr::addr_of_mut!(VIRTIO_INPUT) };
    let mut cb = ServerCallbacks;
    input.drain(|type_, code, value| {
        match (type_, code) {
            // Linux BTN_LEFT/RIGHT/MIDDLE = 0x110/0x111/0x112.
            (EV_KEY, 0x110) => cb.mouse_event(
                INPUT_PAGE_BUTTON,
                INPUT_BUTTON_1,
                value as i32,
                INPUT_FLAG_ABS,
            ),
            (EV_KEY, 0x111) => cb.mouse_event(
                INPUT_PAGE_BUTTON,
                INPUT_BUTTON_1 + 1,
                value as i32,
                INPUT_FLAG_ABS,
            ),
            (EV_KEY, 0x112) => cb.mouse_event(
                INPUT_PAGE_BUTTON,
                INPUT_BUTTON_1 + 2,
                value as i32,
                INPUT_FLAG_ABS,
            ),
            // Linux REL_X = 0x00, REL_Y = 0x01.
            (EV_REL, 0x00) => {
                cb.mouse_event(INPUT_PAGE_GD, INPUT_GD_X, value as i32, INPUT_FLAG_REL)
            }
            (EV_REL, 0x01) => {
                cb.mouse_event(INPUT_PAGE_GD, INPUT_GD_Y, value as i32, INPUT_FLAG_REL)
            }
            // Linux ABS_X = 0x00, ABS_Y = 0x01 (absolute tablet; the value
            // is already QEMU-normalized to 0..0x7FFF).
            (EV_ABS, 0x00) => {
                cb.mouse_event(INPUT_PAGE_ABS, INPUT_GD_X, value as i32, INPUT_FLAG_ABS)
            }
            (EV_ABS, 0x01) => {
                cb.mouse_event(INPUT_PAGE_ABS, INPUT_GD_Y, value as i32, INPUT_FLAG_ABS)
            }
            (EV_KEY, _) => {
                if let Some(usage) = keycode_to_usage(code) {
                    cb.key_event(INPUT_PAGE_KEY, usage, value as i32);
                }
            }
            _ => {}
        }
    });
    unsafe { EV_TAIL != tail }
}

/// Query this process's VA→PA image translation offset (SYS_GETINFO
/// GET_PHYS_DELTA) and hand it to the virtio transport so the vring base
/// and descriptor addresses are programmed as guest-physical addresses
/// (same pattern as the virtio-blk/net/fb servers; without it the
/// virtio-keyboard DMA targets raw VAs and the device's writes land in
/// non-RAM).
#[cfg(target_os = "minix")]
fn init_phys_delta() {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&arch_common::com::GET_PHYS_DELTA.to_ne_bytes());
    minix_rt::kernel_call(26, &mut msg); // SYS_GETINFO
    let delta = i64::from_ne_bytes(msg[0..8].try_into().unwrap_or([0u8; 8]));
    virtio_set_phys_delta(delta);
}

/// Arm the periodic virtio-input poll: SYS_SETALARM (kernel call 24) with
/// a short relative expiry. The kernel notifies via CLOCK on expiry; the
/// notify branch re-arms. The virtio-keyboard has no wired userland IRQ on
/// riscv/aarch64, so the event queue is polled on a tick.
#[cfg(target_os = "minix")]
fn arm_virtio_poll() {
    let input = unsafe { &*core::ptr::addr_of!(VIRTIO_INPUT) };
    if !input.is_initialized() {
        return;
    }
    let mut msg = [0u8; 64];
    // SYS_SETALARM payload: exp_time u64 @8, abs_time i32 @24 (bytes 0-7
    // are the call number/source, clobbered by the kernel dispatch).
    msg[8..16].copy_from_slice(&5u64.to_ne_bytes()); // exp_time: 5 ticks
    msg[24..28].copy_from_slice(&0i32.to_ne_bytes()); // relative
    let _ = minix_rt::kernel_call(24, &mut msg); // SYS_SETALARM
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

/// Register an IRQ-12 hook (SYS_IRQCTL) so the kernel notifies this
/// process on every PS/2 mouse (aux channel) interrupt. The 8042's
/// keyboard ISR drains whatever the controller holds, but mouse packets
/// arrive on their own line (IRQ 12), which needs its own hook.
#[cfg(target_os = "minix")]
fn register_mouse_irq() {
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&arch_common::com::IRQ_SETPOLICY.to_ne_bytes());
    msg[12..16].copy_from_slice(&12i32.to_ne_bytes()); // IRQ 12
    msg[16..20].copy_from_slice(&arch_common::com::IRQ_REENABLE.to_ne_bytes());
    msg[20..24].copy_from_slice(&2i32.to_ne_bytes()); // notify_id
    let _ = minix_rt::kernel_call(19, &mut msg); // SYS_IRQCTL
}

/// Main loop: receive kernel notifications (keyboard IRQs) and CDEV
/// requests for `/dev/kbd`.
pub fn input_server_main() {
    #[cfg(target_os = "minix")]
    {
        const ANY: i32 = 0x0000_ffff;

        drivers::input::devio::input_set_devio(devio_hook);
        // The virtio transport routes PCI config reads through the same
        // SYS_DEVIO hook (without it the x86 pci_probe reads return 0 and
        // the device is never found; the MMIO transports don't use it).
        virtio_set_devio(devio_hook);

        // The virtio transport needs this process's VA→PA delta to program
        // guest-physical addresses (init_phys_delta queries SYS_GETINFO).
        init_phys_delta();

        // Initialize the 8042 (enables the keyboard and mouse interrupts).
        // The I8042 only exists on x86; on riscv/aarch64 the probe would
        // spin through the SYS_DEVIO wait timeouts (port I/O returns ENOSYS
        // there, so every status read looks "output full" and `wait_ready`
        // burns its full 100k-iteration budget).
        #[cfg(target_arch = "x86_64")]
        unsafe {
            PS2_READY = Ps2Controller::init::<DevioIo>().is_ok()
        };
        register_irq();
        register_mouse_irq();

        // Try the virtio-input backend (virtio-keyboard on riscv/aarch64,
        // virtio-mouse on x86). Polled on an alarm when present; the PS/2
        // path stays the x86 keyboard source.
        let virtio_input = unsafe { &mut *core::ptr::addr_of_mut!(VIRTIO_INPUT) };
        if virtio_input.init().is_ok() {
            unsafe {
                minix_rt::write(1, b"input: virtio-input ready\n".as_ptr(), 26);
            }
            arm_virtio_poll();
        }

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
                let had_ps2 = drain_keyboard();
                let had_virtio = drain_virtio();
                // The alarm poll is one-shot; re-arm for the next tick.
                arm_virtio_poll();
                if had_ps2 || had_virtio {
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
