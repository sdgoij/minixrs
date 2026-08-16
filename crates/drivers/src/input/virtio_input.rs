//! Virtio-input driver (virtio-keyboard) — `linux/virtio_input.h`.
//!
//! Probes device ID 18, allocates the eventq (0) + statusq (1), and
//! exchanges one writable 8-byte `virtio_input_event` buffer per event.
//! The device fills it with Linux input events (type/code/value; EV_KEY
//! plus Linux keycodes), which [`keycode_to_usage`] maps to the HID
//! keyboard usages the input server feeds the window server.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/` virtio conventions;
//! the spec structs come from `linux/virtio_input.h`.

use crate::DriverError;
use crate::bus::virtio;

/// Virtio device ID for virtio-keyboard.
pub const VIRTIO_INPUT_DEVICE_ID: u16 = 0x0012;

/// Queue indexes: eventq receives device → driver events; statusq is for
/// device-status queries (unused by the keyboard path).
const EVENTQ: usize = 0;
const STATUSQ: usize = 1;

/// Linux input event types (`linux/input-event-codes.h`).
const EV_KEY: u16 = 0x01;

/// `virtio_input_config` selectors.
const CFG_EV_BITS: u8 = 0x10;

/// `struct virtio_input_event` — 8 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VirtioInputEvent {
    pub type_: u16,
    pub code: u16,
    pub value: u32,
}

/// The virtio-keyboard backend: one device, a pool of shared event buffers.
///
/// The device queues events and only delivers them at EV_SYN/SYN_REPORT,
/// popping one writable buffer per queued event (a key is press + SYN, so
/// two buffers at minimum). A single buffer made every delivery fail with
/// the device's queue-full path (observed: `virtio_input_queue_full`), so
/// the driver keeps a pool armed and re-submits each buffer after its reap.
pub struct VirtioInput {
    dev: Option<virtio::VirtioDevice>,
    ev_bufs: [[u8; 8]; EV_BUF_COUNT],
    initialized: bool,
}

/// Number of armed 8-byte event buffers. The device batches events and
/// pops one writable buffer per event at each SYN_REPORT (a sendkey is
/// press + SYN + release + SYN = 4 events); if ANY pop fails it undoes
/// the whole batch, so the pool must comfortably cover bursts of rapid
/// keys.
const EV_BUF_COUNT: usize = 32;

impl VirtioInput {
    pub const fn new() -> Self {
        Self {
            dev: None,
            ev_bufs: [[0; 8]; EV_BUF_COUNT],
            initialized: false,
        }
    }

    /// Probe the virtio-keyboard and set up the event queue. A no-op once
    /// initialized; `NotFound` when the device is absent (x86 PS/2 path).
    pub fn init(&mut self) -> Result<(), DriverError> {
        if self.initialized {
            return Ok(());
        }
        let mut dev = virtio::virtio_probe(VIRTIO_INPUT_DEVICE_ID, "virtio-input", &[], 0)
            .map_err(|_| DriverError::NotFound)?;
        virtio::virtio_alloc_queue(&mut dev, EVENTQ).map_err(|_| DriverError::Io)?;
        virtio::virtio_alloc_queue(&mut dev, STATUSQ).map_err(|_| DriverError::Io)?;
        virtio::virtio_device_ready(&mut dev);
        // Arm the whole buffer pool (the vring descriptor carries each
        // buffer's VA; the transport adds phys_delta (VA→PA)).
        for i in 0..EV_BUF_COUNT {
            if submit_event_buf(&self.ev_bufs[i], i, &mut dev).is_err() {
                return Err(DriverError::Io);
            }
        }
        self.dev = Some(dev);
        self.initialized = true;
        Ok(())
    }

    /// Reap completed events. `f` is called with `(code, value)` for each
    /// EV_KEY event. Returns the number of events reaped. Each used
    /// descriptor is immediately re-submitted, so the pool stays armed.
    pub fn drain<F: FnMut(u16, u32)>(&mut self, mut f: F) -> usize {
        let Some(dev) = self.dev.as_mut() else {
            return 0;
        };
        let mut n = 0;
        while let Some((token, _len)) = virtio::virtio_from_queue(dev, EVENTQ) {
            if token < EV_BUF_COUNT {
                let ev = VirtioInputEvent {
                    type_: u16::from_le_bytes([self.ev_bufs[token][0], self.ev_bufs[token][1]]),
                    code: u16::from_le_bytes([self.ev_bufs[token][2], self.ev_bufs[token][3]]),
                    value: u32::from_le_bytes([
                        self.ev_bufs[token][4],
                        self.ev_bufs[token][5],
                        self.ev_bufs[token][6],
                        self.ev_bufs[token][7],
                    ]),
                };
                if ev.type_ == EV_KEY {
                    f(ev.code, ev.value);
                }
            }
            n += 1;
            if token >= EV_BUF_COUNT || submit_event_buf(&self.ev_bufs[token], token, dev).is_err()
            {
                // Buffer not re-armed — the next poll re-attempts.
                break;
            }
        }
        n
    }

    /// Whether the backend probed the device and set up its queues.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Read the EV_KEY support bitmap from the device config (`select =
    /// CFG_EV_BITS, subsel = EV_KEY`). Returns `None` if the device is not
    /// initialized.
    pub fn read_ev_key_bits(&self) -> Option<u128> {
        let dev = self.dev.as_ref()?;
        virtio::virtio_swrite8(dev, 0, CFG_EV_BITS);
        virtio::virtio_swrite8(dev, 1, EV_KEY as u8);
        let size = virtio::virtio_sread16(dev, 2);
        let mut bits: u128 = 0;
        let nbytes = (size as usize).min(16);
        for i in 0..nbytes {
            let b = virtio::virtio_sread8(dev, 4 + i as u16) as u128;
            bits |= b << (i * 8);
        }
        Some(bits)
    }
}

impl Default for VirtioInput {
    fn default() -> Self {
        Self::new()
    }
}

/// Add one event buffer to the ring for the next device write. `token` is
/// the buffer's pool index; `virtio_from_queue` returns it on the reap so
/// the drain reads the right buffer.
fn submit_event_buf(
    buf: &[u8; 8],
    token: usize,
    dev: &mut virtio::VirtioDevice,
) -> Result<(), DriverError> {
    let va = buf.as_ptr() as u64;
    virtio::virtio_to_queue(
        dev,
        EVENTQ,
        &[virtio::VirtioPhysBuf {
            addr: va | 1, // writable
            size: 8,
        }],
        token,
    )
    .map_err(|_| DriverError::Io)
}

/// Linux keycode → HID keyboard usage (page 0x07) for the keys the
/// window server can render (`usage_to_ascii` + the shift usages).
/// QEMU's virtio-keyboard emits Linux input keycodes (`KEY_A` = 30 etc.),
/// which differ from the HID usages the PS/2 path produces.
pub fn keycode_to_usage(code: u16) -> Option<u16> {
    let usage = match code {
        // Letters: KEY_A=30 .. KEY_Z=44 (non-contiguous Linux codes).
        30 => 0x04,
        48 => 0x05,
        46 => 0x06,
        32 => 0x07,
        18 => 0x08,
        33 => 0x09,
        34 => 0x0A,
        35 => 0x0B,
        23 => 0x0C,
        36 => 0x0D,
        37 => 0x0E,
        38 => 0x0F,
        50 => 0x10,
        49 => 0x11,
        24 => 0x12,
        25 => 0x13,
        16 => 0x14,
        19 => 0x15,
        31 => 0x16,
        20 => 0x17,
        22 => 0x18,
        47 => 0x19,
        17 => 0x1A,
        45 => 0x1B,
        21 => 0x1C,
        44 => 0x1D,
        // Digits: KEY_1=2 .. KEY_0=11.
        2 => 0x1E,
        3 => 0x1F,
        4 => 0x20,
        5 => 0x21,
        6 => 0x22,
        7 => 0x23,
        8 => 0x24,
        9 => 0x25,
        10 => 0x26,
        11 => 0x27,
        // Controls the wserver renders.
        28 => 0x28, // enter
        14 => 0x2A, // backspace
        57 => 0x2C, // space
        // Punctuation (wserver `usage_to_ascii`).
        12 => 0x2D, // -
        13 => 0x2E, // =
        26 => 0x2F, // [
        27 => 0x30, // ]
        43 => 0x31, // backslash
        39 => 0x33, // ;
        40 => 0x34, // '
        41 => 0x35, // `
        51 => 0x36, // ,
        52 => 0x37, // .
        53 => 0x38, // /
        // Shift/ctrl (the wserver tracks 0xE1/0xE5 shift, 0xE0/0xE4 ctrl).
        42 => 0xE1, // left shift
        54 => 0xE5, // right shift
        29 => 0xE0, // left ctrl
        97 => 0xE4, // right ctrl
        // Arrows (the wterm client turns these into escape sequences).
        103 => 0x52, // up
        108 => 0x51, // down
        105 => 0x50, // left
        106 => 0x4F, // right
        _ => return None,
    };
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_struct_layout() {
        assert_eq!(core::mem::size_of::<VirtioInputEvent>(), 8);
        let ev = VirtioInputEvent {
            type_: 0x0102,
            code: 0x1234,
            value: 0xCAFE_BEEF,
        };
        let b = unsafe { core::slice::from_raw_parts(&ev as *const _ as *const u8, 8) };
        assert_eq!(&b[0..2], &[0x02, 0x01]);
        assert_eq!(&b[2..4], &[0x34, 0x12]);
        assert_eq!(&b[4..8], &[0xEF, 0xBE, 0xFE, 0xCA]);
    }

    #[test]
    fn device_id_and_queues() {
        assert_eq!(VIRTIO_INPUT_DEVICE_ID, 0x0012);
        assert_eq!(EVENTQ, 0);
        assert_eq!(STATUSQ, 1);
    }

    #[test]
    fn letters_map_to_a_z_usages() {
        // KEY_A=30 → usage 0x04 ... KEY_Z=44 → usage 0x1D.
        let keycodes = [
            30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
            17, 45, 21, 44,
        ];
        for (i, &kc) in keycodes.iter().enumerate() {
            assert_eq!(keycode_to_usage(kc), Some(0x04 + i as u16), "keycode {kc}");
        }
    }

    #[test]
    fn digits_map_to_1_0_usages() {
        // KEY_1=2 .. KEY_9=10 → usage 0x1E..0x26, KEY_0=11 → 0x27.
        for i in 0..9u16 {
            assert_eq!(keycode_to_usage(2 + i), Some(0x1E + i));
        }
        assert_eq!(keycode_to_usage(11), Some(0x27)); // 0
    }

    #[test]
    fn controls_and_shift_map() {
        assert_eq!(keycode_to_usage(28), Some(0x28)); // enter
        assert_eq!(keycode_to_usage(14), Some(0x2A)); // backspace
        assert_eq!(keycode_to_usage(57), Some(0x2C)); // space
        assert_eq!(keycode_to_usage(42), Some(0xE1)); // left shift
        assert_eq!(keycode_to_usage(54), Some(0xE5)); // right shift
        assert_eq!(keycode_to_usage(29), Some(0xE0)); // left ctrl
        assert_eq!(keycode_to_usage(97), Some(0xE4)); // right ctrl
        assert_eq!(keycode_to_usage(103), Some(0x52)); // up
        assert_eq!(keycode_to_usage(108), Some(0x51)); // down
        assert_eq!(keycode_to_usage(105), Some(0x50)); // left
        assert_eq!(keycode_to_usage(106), Some(0x4F)); // right
        assert_eq!(keycode_to_usage(12), Some(0x2D)); // -
        assert_eq!(keycode_to_usage(39), Some(0x33)); // ;
        assert_eq!(keycode_to_usage(53), Some(0x38)); // /
    }

    #[test]
    fn unmapped_keys_return_none() {
        assert!(keycode_to_usage(1).is_none()); // esc
        assert!(keycode_to_usage(15).is_none()); // tab
        assert!(keycode_to_usage(100).is_none()); // alt right
        assert!(keycode_to_usage(0xFFFF).is_none());
    }
}
