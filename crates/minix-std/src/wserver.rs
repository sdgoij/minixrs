//! Window-server client protocol (K5): the `wdemo` client talks to the
//! `wserver` boot proc (endpoint 18) with sendrec. Messages use the m2
//! layout (m_type @ byte 4, payload @ byte 8): m2i1 @ 8, m2i2 @ 12,
//! m2i3 @ 16, m2l1 @ 24, m2l2 @ 32, m2l3 @ 40.
//!
//! Requests and their replies:
//! - `ws_create` — {x, y, w, h, title ptr/len} → wid in reply m2i1.
//! - `ws_text` — {wid, row, col, ch} → 0.
//! - `ws_fill` — {wid, x0, y0, x1, y1, color} → 0.
//! - `ws_close` — {wid} → 0.
//! - `ws_input` — {wid}: no immediate reply; when a key routes to the
//!   window, the server sends `WS_KEY` (char in m2l1) and the client's
//!   blocked sendrec completes with it.
//! - `ws_cursor` — {wid, row, col} → 0: the inverse-video block cell.

/// Window-server request base.
pub const WS_BASE: u32 = 0x0D00;
pub const WS_CREATE: u32 = WS_BASE;
pub const WS_TEXT: u32 = WS_BASE + 1;
pub const WS_FILL: u32 = WS_BASE + 2;
pub const WS_CLOSE: u32 = WS_BASE + 3;
pub const WS_INPUT: u32 = WS_BASE + 4;
pub const WS_KEY: u32 = WS_BASE + 5;
pub const WS_FLUSH: u32 = WS_BASE + 6;
pub const WS_CURSOR: u32 = WS_BASE + 7;

// Absolute message-byte offsets (m2 layout).
const OFF_TYPE: usize = 4;
const OFF_M2_I1: usize = 8;
const OFF_M2_I2: usize = 12;
const OFF_M2_I3: usize = 16;
const OFF_M2_L1: usize = 24;
const OFF_M2_L2: usize = 32;
const OFF_M2_L3: usize = 40;

fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_le_bytes());
}

fn msg_set_u64(msg: &mut [u8; 64], off: usize, val: u64) {
    msg[off..off + 8].copy_from_slice(&val.to_le_bytes());
}

fn msg_set_u8(msg: &mut [u8; 64], off: usize, val: u8) {
    msg[off] = val;
}

/// Build a WS_CREATE request. The title travels by vircopy from the
/// client's buffer (`title_ptr` + `title_len`).
pub fn ws_create(x: i32, y: i32, w: i32, h: i32, title_ptr: u64, title_len: i32) -> [u8; 64] {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_TYPE, WS_CREATE as i32);
    msg_set_i32(&mut msg, OFF_M2_I1, x);
    msg_set_i32(&mut msg, OFF_M2_I2, y);
    msg_set_i32(&mut msg, OFF_M2_I3, w);
    msg_set_u64(&mut msg, OFF_M2_L1, h as u64);
    msg_set_u64(&mut msg, OFF_M2_L2, title_ptr);
    msg_set_u64(&mut msg, OFF_M2_L3, title_len as u64);
    msg
}

/// Build a WS_TEXT request (one char at a body cell).
pub fn ws_text(wid: i32, row: i32, col: i32, ch: u8) -> [u8; 64] {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_TYPE, WS_TEXT as i32);
    msg_set_i32(&mut msg, OFF_M2_I1, wid);
    msg_set_i32(&mut msg, OFF_M2_I2, row);
    msg_set_i32(&mut msg, OFF_M2_I3, col);
    msg_set_u8(&mut msg, OFF_M2_L1, ch);
    msg
}

/// Build a WS_FILL request (window-local pixel rect + XRGB color).
pub fn ws_fill(wid: i32, x0: i32, y0: i32, x1: i32, y1: i32, color: u32) -> [u8; 64] {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_TYPE, WS_FILL as i32);
    msg_set_i32(&mut msg, OFF_M2_I1, wid);
    msg_set_i32(&mut msg, OFF_M2_I2, x0);
    msg_set_i32(&mut msg, OFF_M2_I3, y0);
    msg_set_u64(&mut msg, OFF_M2_L1, x1 as u64);
    msg_set_u64(&mut msg, OFF_M2_L2, y1 as u64);
    msg_set_u64(&mut msg, OFF_M2_L3, color as u64);
    msg
}

/// Build a WS_CLOSE request.
/// Build a WS_CLOSE request: remove the window.
pub fn ws_close(wid: i32) -> [u8; 64] {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_TYPE, WS_CLOSE as i32);
    msg_set_i32(&mut msg, OFF_M2_I1, wid);
    msg
}

/// Build a WS_CURSOR request: place the inverse-video block cursor at a
/// body cell. Redraws are deferred to WS_FLUSH like WS_TEXT.
pub fn ws_cursor(wid: i32, row: i32, col: i32) -> [u8; 64] {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_TYPE, WS_CURSOR as i32);
    msg_set_i32(&mut msg, OFF_M2_I1, wid);
    msg_set_i32(&mut msg, OFF_M2_I2, row);
    msg_set_i32(&mut msg, OFF_M2_I3, col);
    msg
}

/// Build a WS_INPUT request: block until a key routes to window `wid`.
pub fn ws_input(wid: i32) -> [u8; 64] {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_TYPE, WS_INPUT as i32);
    msg_set_i32(&mut msg, OFF_M2_I1, wid);
    msg
}

/// Build a WS_FLUSH request: repaint the desktop with the buffered
/// WS_TEXT/WS_FILL updates (redraws are deferred until this arrives).
pub fn ws_flush() -> [u8; 64] {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_TYPE, WS_FLUSH as i32);
    msg
}

/// Reply status of a window-server request (the reply's m_type).
pub fn ws_reply_status(msg: &[u8; 64]) -> i32 {
    i32::from_le_bytes(msg[OFF_TYPE..OFF_TYPE + 4].try_into().unwrap_or([0; 4]))
}

/// Window id from a WS_CREATE reply (m2i1).
pub fn ws_reply_wid(msg: &[u8; 64]) -> i32 {
    i32::from_le_bytes(
        msg[OFF_M2_I1..OFF_M2_I1 + 4]
            .try_into()
            .unwrap_or([0xFF; 4]),
    )
}

/// Routed key char from a WS_KEY delivery (m2l1).
pub fn ws_key_char(msg: &[u8; 64]) -> u8 {
    msg[OFF_M2_L1]
}

/// HID usage code of the routed key (m2l2) — lets clients distinguish
/// special keys (e.g. arrows, which carry no ASCII char) from plain chars.
pub fn ws_key_usage(msg: &[u8; 64]) -> u16 {
    u16::from_le_bytes([msg[OFF_M2_L2], msg[OFF_M2_L2 + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_constants() {
        assert_eq!(WS_BASE, 0x0D00);
        assert_eq!(WS_CREATE, 0x0D00);
        assert_eq!(WS_TEXT, 0x0D01);
        assert_eq!(WS_FILL, 0x0D02);
        assert_eq!(WS_CLOSE, 0x0D03);
        assert_eq!(WS_INPUT, 0x0D04);
        assert_eq!(WS_KEY, 0x0D05);
        assert_eq!(WS_FLUSH, 0x0D06);
        assert_eq!(WS_CURSOR, 0x0D07);
    }

    #[test]
    fn test_ws_create_layout() {
        let msg = ws_create(40, 40, 320, 200, 0x1234, 4);
        assert_eq!(ws_reply_status(&msg), 0x0D00); // m_type at byte 4
        assert_eq!(i32::from_le_bytes(msg[8..12].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(msg[12..16].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(msg[16..20].try_into().unwrap()), 320);
        assert_eq!(u64::from_le_bytes(msg[24..32].try_into().unwrap()), 200);
        assert_eq!(u64::from_le_bytes(msg[32..40].try_into().unwrap()), 0x1234);
        assert_eq!(u64::from_le_bytes(msg[40..48].try_into().unwrap()), 4);
    }

    #[test]
    fn test_ws_cursor_layout() {
        let msg = ws_cursor(3, 4, 5);
        assert_eq!(ws_reply_status(&msg), 0x0D07);
        assert_eq!(i32::from_le_bytes(msg[8..12].try_into().unwrap()), 3);
        assert_eq!(i32::from_le_bytes(msg[12..16].try_into().unwrap()), 4);
        assert_eq!(i32::from_le_bytes(msg[16..20].try_into().unwrap()), 5);
    }

    #[test]
    fn test_ws_text_and_key_layout() {
        let msg = ws_text(1, 2, 3, b'a');
        assert_eq!(ws_reply_status(&msg), 0x0D01);
        assert_eq!(i32::from_le_bytes(msg[8..12].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(msg[12..16].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(msg[16..20].try_into().unwrap()), 3);
        assert_eq!(msg[24], b'a');
        assert_eq!(ws_key_char(&msg), b'a');
    }

    #[test]
    fn test_ws_reply_parsing() {
        let mut reply = [0u8; 64];
        reply[4..8].copy_from_slice(&0i32.to_le_bytes());
        reply[8..12].copy_from_slice(&2i32.to_le_bytes());
        assert_eq!(ws_reply_status(&reply), 0);
        assert_eq!(ws_reply_wid(&reply), 2);
    }
}
