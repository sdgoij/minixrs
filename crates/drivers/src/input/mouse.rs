//! PS/2 mouse — 3-byte packet decoding.
//!
//! Ported from `pckbd.c` function `kbdaux_process()`.
//!
//! A PS/2 mouse sends 3-byte packets:
//!   Byte 0: YOVFL XOVFL YSIGN XSIGN 1 MBSB MBBT RMB LMB
//!   Byte 1: X delta (signed, 8-bit)
//!   Byte 2: Y delta (signed, 8-bit)

#![allow(clippy::identity_op)]

/// Decoded mouse state from a 3-byte PS/2 packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseState {
    /// Left mouse button pressed.
    pub left: bool,
    /// Right mouse button pressed.
    pub right: bool,
    /// Middle mouse button pressed.
    pub middle: bool,
    /// X-axis movement delta (signed).
    pub delta_x: i32,
    /// Y-axis movement delta (signed).
    pub delta_y: i32,
    /// X overflow flag.
    pub overflow_x: bool,
    /// Y overflow flag.
    pub overflow_y: bool,
}

/// Raw 3-byte PS/2 mouse packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MousePacket {
    pub byte0: u8,
    pub byte1: u8,
    pub byte2: u8,
}

impl MouseState {
    /// Decode a 3-byte PS/2 mouse packet into structured state.
    ///
    /// Returns `None` if the packet is out of sync (bit 3 of byte 0 is not set).
    pub fn decode(packet: &MousePacket) -> Option<Self> {
        // Bit 3 must be set on a valid first byte; otherwise we're out of sync.
        if packet.byte0 & 0x08 == 0 {
            return None;
        }

        let left = (packet.byte0 & 0x01) != 0;
        let right = (packet.byte0 & 0x02) != 0;
        let middle = (packet.byte0 & 0x04) != 0;

        // Sign extension: if sign bit is set, extend to 32-bit signed.
        let mut delta_x = packet.byte1 as i32;
        if packet.byte0 & 0x10 != 0 {
            delta_x |= -256i32; // sign-extend from 8 bits
        }

        let mut delta_y = packet.byte2 as i32;
        if packet.byte0 & 0x20 != 0 {
            delta_y |= -256i32; // sign-extend from 8 bits
        }

        let overflow_x = (packet.byte0 & 0x40) != 0;
        let overflow_y = (packet.byte0 & 0x80) != 0;

        Some(MouseState {
            left,
            right,
            middle,
            delta_x,
            delta_y,
            overflow_x,
            overflow_y,
        })
    }

    /// Return the set of button bit changes relative to a previous state.
    ///
    /// Each entry is `(button_index, is_pressed)` where button_index is 0 (left),
    /// 1 (right), 2 (middle), matching `INPUT_BUTTON_1 + index`.
    pub fn button_changes(&self, prev: &MouseState) -> [(u8, bool); 3] {
        [
            (0, self.left && !prev.left),
            (1, self.right && !prev.right),
            (2, self.middle && !prev.middle),
        ]
    }
}

impl From<[u8; 3]> for MousePacket {
    fn from(bytes: [u8; 3]) -> Self {
        MousePacket {
            byte0: bytes[0],
            byte1: bytes[1],
            byte2: bytes[2],
        }
    }
}

/// Represents a parsed mouse event from an ongoing stream of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEvent {
    /// A complete packet was decoded.
    Packet(MouseState),
    /// Resynchronization needed (invalid byte 0).
    Resync,
    /// Waiting for more bytes (incomplete packet).
    NeedMore,
}

/// PS/2 mouse byte-stream parser (stateful).
///
/// Accumulates 3-byte packets from a stream of bytes and decodes them.
#[derive(Debug, Clone, Copy)]
pub struct MouseParser {
    /// Previously decoded state (for button delta tracking).
    pub prev_state: MouseState,
    /// Internal byte buffer.
    buf: [u8; 3],
    /// Number of bytes accumulated so far.
    count: u8,
}

impl Default for MouseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseParser {
    /// Create a new parser with default (no-buttons-pressed) state.
    pub const fn new() -> Self {
        MouseParser {
            prev_state: MouseState {
                left: false,
                right: false,
                middle: false,
                delta_x: 0,
                delta_y: 0,
                overflow_x: false,
                overflow_y: false,
            },
            buf: [0; 3],
            count: 0,
        }
    }

    /// Feed one byte into the parser.  Returns an event if sufficient data
    /// has been accumulated.
    pub fn feed(&mut self, byte: u8) -> MouseEvent {
        if self.count == 0 && byte & 0x08 == 0 {
            // Out of sync; wait for a valid first byte.
            return MouseEvent::Resync;
        }

        self.buf[self.count as usize] = byte;
        self.count += 1;

        if self.count < 3 {
            return MouseEvent::NeedMore;
        }

        self.count = 0;

        let packet = MousePacket {
            byte0: self.buf[0],
            byte1: self.buf[1],
            byte2: self.buf[2],
        };
        match MouseState::decode(&packet) {
            Some(state) => {
                let event = MouseEvent::Packet(state);
                self.prev_state = state;
                event
            }
            None => MouseEvent::Resync,
        }
    }

    /// Reset the parser to initial state.
    pub fn reset(&mut self) {
        self.count = 0;
        self.prev_state = MouseState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mouse_packet_no_movement() {
        // Byte0: bit3 set, no buttons, no sign
        let packet = MousePacket {
            byte0: 0x08,
            byte1: 0x00,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert!(!state.left);
        assert!(!state.right);
        assert!(!state.middle);
        assert_eq!(state.delta_x, 0);
        assert_eq!(state.delta_y, 0);
        assert!(!state.overflow_x);
        assert!(!state.overflow_y);
    }

    #[test]
    fn test_mouse_left_button() {
        let packet = MousePacket {
            byte0: 0x09,
            byte1: 0x00,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert!(state.left);
        assert!(!state.right);
        assert!(!state.middle);
    }

    #[test]
    fn test_mouse_right_button() {
        let packet = MousePacket {
            byte0: 0x0A,
            byte1: 0x00,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert!(state.right);
        assert!(!state.left);
        assert!(!state.middle);
    }

    #[test]
    fn test_mouse_middle_button() {
        let packet = MousePacket {
            byte0: 0x0C,
            byte1: 0x00,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert!(state.middle);
        assert!(!state.left);
        assert!(!state.right);
    }

    #[test]
    fn test_mouse_all_buttons() {
        let packet = MousePacket {
            byte0: 0x0F,
            byte1: 0x00,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert!(state.left);
        assert!(state.right);
        assert!(state.middle);
    }

    #[test]
    fn test_mouse_positive_delta_x() {
        let packet = MousePacket {
            byte0: 0x08,
            byte1: 0x10,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert_eq!(state.delta_x, 16);
        assert_eq!(state.delta_y, 0);
    }

    #[test]
    fn test_mouse_negative_delta_x() {
        let packet = MousePacket {
            byte0: 0x18,
            byte1: 0xF0,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert_eq!(state.delta_x, -16);
        assert_eq!(state.delta_y, 0);
    }

    #[test]
    fn test_mouse_negative_delta_y() {
        let packet = MousePacket {
            byte0: 0x28,
            byte1: 0x00,
            byte2: 0xFE,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert_eq!(state.delta_y, -2);
        assert_eq!(state.delta_x, 0);
    }

    #[test]
    fn test_mouse_signed_extension() {
        // XSIGN=1, byte1=0xFF → -1
        let packet = MousePacket {
            byte0: 0x18,
            byte1: 0xFF,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert_eq!(state.delta_x, -1);
    }

    #[test]
    fn test_mouse_out_of_sync() {
        // Bit3 not set → invalid
        let packet = MousePacket {
            byte0: 0x00,
            byte1: 0x00,
            byte2: 0x00,
        };
        assert!(MouseState::decode(&packet).is_none());

        let packet = MousePacket {
            byte0: 0x07,
            byte1: 0x00,
            byte2: 0x00,
        };
        assert!(MouseState::decode(&packet).is_none());
    }

    #[test]
    fn test_mouse_overflow() {
        let packet = MousePacket {
            byte0: 0xC8,
            byte1: 0x00,
            byte2: 0x00,
        };
        let state = MouseState::decode(&packet).unwrap();
        assert!(state.overflow_x);
        assert!(state.overflow_y);
    }

    #[test]
    fn test_mouse_packet_from_array() {
        let packet = MousePacket::from([0x08, 0x01, 0xFF]);
        assert_eq!(packet.byte0, 0x08);
        assert_eq!(packet.byte1, 0x01);
        assert_eq!(packet.byte2, 0xFF);
    }

    #[test]
    fn test_mouse_parser_full_packet() {
        let mut parser = MouseParser::new();
        assert_eq!(parser.count, 0);

        // Feed valid 3-byte packet
        let e1 = parser.feed(0x08);
        assert_eq!(e1, MouseEvent::NeedMore);
        let e2 = parser.feed(0x00);
        assert_eq!(e2, MouseEvent::NeedMore);
        let e3 = parser.feed(0x00);
        assert!(matches!(e3, MouseEvent::Packet(_)));
    }

    #[test]
    fn test_mouse_parser_resync() {
        let mut parser = MouseParser::new();

        // A byte without bit3 should trigger resync when count is 0
        let e = parser.feed(0x00);
        assert_eq!(e, MouseEvent::Resync);
        // Still at count 0 after resync
        assert_eq!(parser.count, 0);
    }

    #[test]
    fn test_mouse_parser_button_delta() {
        let mut parser = MouseParser::new();

        // No buttons pressed
        let _ = parser.feed(0x08);
        let _ = parser.feed(0x00);
        let _ = parser.feed(0x00);
        assert!(!parser.prev_state.left);
        assert!(!parser.prev_state.right);
        assert!(!parser.prev_state.middle);

        // Now left button pressed
        let _ = parser.feed(0x09);
        let _ = parser.feed(0x00);
        let _ = parser.feed(0x00);
        assert!(parser.prev_state.left);
    }

    #[test]
    fn test_mouse_parser_reset() {
        let mut parser = MouseParser::new();
        parser.count = 2;
        parser.prev_state.left = true;

        parser.reset();
        assert_eq!(parser.count, 0);
        assert!(!parser.prev_state.left);
    }

    #[test]
    fn test_button_changes() {
        let prev = MouseState {
            left: false,
            right: false,
            middle: false,
            ..MouseState::default()
        };
        let cur = MouseState {
            left: true,
            right: false,
            middle: true,
            ..MouseState::default()
        };
        let changes = cur.button_changes(&prev);
        assert_eq!(changes[0], (0, true)); // left pressed
        assert_eq!(changes[1], (1, false)); // right not pressed
        assert_eq!(changes[2], (2, true)); // middle pressed
    }
}
