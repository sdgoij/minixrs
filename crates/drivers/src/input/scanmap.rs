//! Scancode translation tables — PC PS/2 scancodes → USB HID usage codes.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/hid/pckbd/table.c`

#![allow(clippy::identity_op)]

use crate::input::constants::*;

/// A single entry in a scancode translation table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanmapEntry {
    /// HID usage page (e.g. `INPUT_PAGE_KEY`).
    pub page: u16,
    /// HID usage code within the page (e.g. `INPUT_KEY_A`).
    pub code: u16,
}


/// Normal (unprefixed) scancode → HID translation.
///
/// Indexed by scancode & 0x7F.  Entries not listed are {0, 0} (unmapped).
pub static SCANMAP_NORMAL: [ScanmapEntry; KBD_SCAN_CODES] = {
    let mut tbl = [ScanmapEntry { page: 0, code: 0 }; KBD_SCAN_CODES];

    tbl[0x01] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_ESCAPE,
    };
    tbl[0x02] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_1,
    };
    tbl[0x03] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_2,
    };
    tbl[0x04] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_3,
    };
    tbl[0x05] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_4,
    };
    tbl[0x06] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_5,
    };
    tbl[0x07] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_6,
    };
    tbl[0x08] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_7,
    };
    tbl[0x09] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_8,
    };
    tbl[0x0A] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_9,
    };
    tbl[0x0B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_0,
    };
    tbl[0x0C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_DASH,
    };
    tbl[0x0D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_EQUAL,
    };
    tbl[0x0E] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_BACKSPACE,
    };
    tbl[0x0F] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_TAB,
    };
    tbl[0x10] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_Q,
    };
    tbl[0x11] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_W,
    };
    tbl[0x12] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_E,
    };
    tbl[0x13] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_R,
    };
    tbl[0x14] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_T,
    };
    tbl[0x15] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_Y,
    };
    tbl[0x16] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_U,
    };
    tbl[0x17] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_I,
    };
    tbl[0x18] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_O,
    };
    tbl[0x19] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_P,
    };
    tbl[0x1A] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_OPEN_BRACKET,
    };
    tbl[0x1B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_CLOSE_BRACKET,
    };
    tbl[0x1C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_ENTER,
    };
    tbl[0x1D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LEFT_CTRL,
    };
    tbl[0x1E] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_A,
    };
    tbl[0x1F] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_S,
    };
    tbl[0x20] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_D,
    };
    tbl[0x21] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F,
    };
    tbl[0x22] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_G,
    };
    tbl[0x23] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_H,
    };
    tbl[0x24] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_J,
    };
    tbl[0x25] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_K,
    };
    tbl[0x26] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_L,
    };
    tbl[0x27] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_SEMICOLON,
    };
    tbl[0x28] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_APOSTROPH,
    };
    tbl[0x29] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_GRAVE_ACCENT,
    };
    tbl[0x2A] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LEFT_SHIFT,
    };
    tbl[0x2B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_BACKSLASH,
    };
    tbl[0x2C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_Z,
    };
    tbl[0x2D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_X,
    };
    tbl[0x2E] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_C,
    };
    tbl[0x2F] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_V,
    };
    tbl[0x30] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_B,
    };
    tbl[0x31] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_N,
    };
    tbl[0x32] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_M,
    };
    tbl[0x33] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_COMMA,
    };
    tbl[0x34] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_PERIOD,
    };
    tbl[0x35] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_SLASH,
    };
    tbl[0x36] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_RIGHT_SHIFT,
    };
    tbl[0x37] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_STAR,
    };
    tbl[0x38] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LEFT_ALT,
    };
    tbl[0x39] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_SPACEBAR,
    };
    tbl[0x3A] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_CAPS_LOCK,
    };
    tbl[0x3B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F1,
    };
    tbl[0x3C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F2,
    };
    tbl[0x3D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F3,
    };
    tbl[0x3E] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F4,
    };
    tbl[0x3F] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F5,
    };
    tbl[0x40] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F6,
    };
    tbl[0x41] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F7,
    };
    tbl[0x42] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F8,
    };
    tbl[0x43] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F9,
    };
    tbl[0x44] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F10,
    };
    tbl[0x45] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_NUM_LOCK,
    };
    tbl[0x46] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_SCROLL_LOCK,
    };
    tbl[0x47] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_7,
    };
    tbl[0x48] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_8,
    };
    tbl[0x49] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_9,
    };
    tbl[0x4A] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_DASH,
    };
    tbl[0x4B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_4,
    };
    tbl[0x4C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_5,
    };
    tbl[0x4D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_6,
    };
    tbl[0x4E] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_PLUS,
    };
    tbl[0x4F] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_1,
    };
    tbl[0x50] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_2,
    };
    tbl[0x51] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_3,
    };
    tbl[0x52] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_0,
    };
    tbl[0x53] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_PERIOD,
    };
    tbl[0x54] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_SYSREQ,
    };
    tbl[0x56] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_EUROPE_2,
    };
    tbl[0x57] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F11,
    };
    tbl[0x58] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F12,
    };
    tbl[0x59] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_EQUAL,
    };
    tbl[0x5C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_I10L_6,
    };
    tbl[0x64] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F13,
    };
    tbl[0x65] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F14,
    };
    tbl[0x66] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F15,
    };
    tbl[0x67] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F16,
    };
    tbl[0x68] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F17,
    };
    tbl[0x69] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F18,
    };
    tbl[0x6A] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F19,
    };
    tbl[0x6B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F20,
    };
    tbl[0x6C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F21,
    };
    tbl[0x6D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F22,
    };
    tbl[0x6E] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F23,
    };
    tbl[0x70] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_I10L_2,
    };
    tbl[0x71] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LANG_2,
    };
    tbl[0x72] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LANG_1,
    };
    tbl[0x73] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_I10L_1,
    };
    tbl[0x76] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_F24,
    };
    tbl[0x77] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LANG_4,
    };
    tbl[0x78] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LANG_3,
    };
    tbl[0x79] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_I10L_4,
    };
    tbl[0x7B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_I10L_5,
    };
    tbl[0x7D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_I10L_3,
    };
    tbl[0x7E] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_EQUAL_SIGN,
    };

    tbl
};


/// Extended (0xE0-prefixed) scancode → HID translation.
///
/// Indexed by the byte following 0xE0, masked with 0x7F.
pub static SCANMAP_ESCAPED: [ScanmapEntry; KBD_SCAN_CODES] = {
    let mut tbl = [ScanmapEntry { page: 0, code: 0 }; KBD_SCAN_CODES];

    tbl[0x10] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_SCAN_PREVIOUS_TRACK,
    };
    tbl[0x19] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_SCAN_NEXT_TRACK,
    };
    tbl[0x1C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_ENTER,
    };
    tbl[0x1D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_RIGHT_CTRL,
    };
    tbl[0x20] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_MUTE,
    };
    tbl[0x21] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AL_CALCULATOR,
    };
    tbl[0x22] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_PLAY_PAUSE,
    };
    tbl[0x24] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_STOP,
    };
    tbl[0x2E] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_VOLUME_DOWN,
    };
    tbl[0x30] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_VOLUME_UP,
    };
    tbl[0x32] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AC_HOME,
    };
    tbl[0x35] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_KP_SLASH,
    };
    tbl[0x37] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_PRINT_SCREEN,
    };
    tbl[0x38] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_RIGHT_ALT,
    };
    tbl[0x46] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_PAUSE,
    };
    tbl[0x47] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_HOME,
    };
    tbl[0x48] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_UP_ARROW,
    };
    tbl[0x49] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_PAGE_UP,
    };
    tbl[0x4B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LEFT_ARROW,
    };
    tbl[0x4D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_RIGHT_ARROW,
    };
    tbl[0x4F] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_END,
    };
    tbl[0x50] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_DOWN_ARROW,
    };
    tbl[0x51] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_PAGE_DOWN,
    };
    tbl[0x52] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_INSERT,
    };
    tbl[0x53] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_DELETE,
    };
    tbl[0x5B] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_LEFT_GUI,
    };
    tbl[0x5C] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_RIGHT_GUI,
    };
    tbl[0x5D] = ScanmapEntry {
        page: INPUT_PAGE_KEY,
        code: INPUT_KEY_APPLICATION,
    };
    tbl[0x5E] = ScanmapEntry {
        page: INPUT_PAGE_GD,
        code: INPUT_GD_SYSTEM_POWER_DOWN,
    };
    tbl[0x5F] = ScanmapEntry {
        page: INPUT_PAGE_GD,
        code: INPUT_GD_SYSTEM_SLEEP,
    };
    tbl[0x63] = ScanmapEntry {
        page: INPUT_PAGE_GD,
        code: INPUT_GD_SYSTEM_WAKE_UP,
    };
    tbl[0x65] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AC_SEARCH,
    };
    tbl[0x66] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AC_BOOKMARKS,
    };
    tbl[0x67] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AC_REFRESH,
    };
    tbl[0x68] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AC_STOP,
    };
    tbl[0x69] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AC_FORWARD,
    };
    tbl[0x6A] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AC_BACK,
    };
    tbl[0x6B] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AL_LOCAL_BROWSER,
    };
    tbl[0x6C] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AL_EMAIL_READER,
    };
    tbl[0x6D] = ScanmapEntry {
        page: INPUT_PAGE_CONS,
        code: INPUT_CONS_AL_MEDIA_SELECT,
    };

    tbl
};


/// Colemak remapping table: maps a QWERTY HID code to a Colemak HID code.
///
/// Only the keys that differ between QWERTY and Colemak are listed.
/// Keys not listed are left unchanged (use original mapping).
const COLEMAK_LETTER_MAP: [(u16, u16); 14] = [
    (INPUT_KEY_E, INPUT_KEY_F),
    (INPUT_KEY_R, INPUT_KEY_P),
    (INPUT_KEY_T, INPUT_KEY_G),
    (INPUT_KEY_Y, INPUT_KEY_J),
    (INPUT_KEY_U, INPUT_KEY_L),
    (INPUT_KEY_I, INPUT_KEY_U),
    (INPUT_KEY_O, INPUT_KEY_Y),
    (INPUT_KEY_P, INPUT_KEY_SEMICOLON),
    (INPUT_KEY_OPEN_BRACKET, INPUT_KEY_K),
    (INPUT_KEY_CLOSE_BRACKET, INPUT_KEY_X),
    (INPUT_KEY_S, INPUT_KEY_R),
    (INPUT_KEY_D, INPUT_KEY_S),
    (INPUT_KEY_F, INPUT_KEY_T),
    (INPUT_KEY_G, INPUT_KEY_D),
];

/// Apply Colemak remapping to a HID key code.
///
/// If the given code matches a QWERTY key that differs in Colemak, returns the
/// Colemak HID code.  Otherwise returns the code unchanged.
pub const fn apply_colemak(code: u16) -> u16 {
    let mut i = 0;
    while i < COLEMAK_LETTER_MAP.len() {
        if COLEMAK_LETTER_MAP[i].0 == code {
            return COLEMAK_LETTER_MAP[i].1;
        }
        i += 1;
    }
    code
}

/// Translate a normal (unprefixed) scancode to a HID (page, code) pair.
///
/// `index` must be in range 0..0x80 (only the low 7 bits matter).
pub fn lookup_normal(index: u8) -> ScanmapEntry {
    let idx = (index & 0x7F) as usize;
    if idx < KBD_SCAN_CODES {
        SCANMAP_NORMAL[idx]
    } else {
        ScanmapEntry { page: 0, code: 0 }
    }
}

/// Translate an 0xE0-prefixed scancode to a HID (page, code) pair.
///
/// `index` must be in range 0..0x80 (only the low 7 bits matter).
pub fn lookup_escaped(index: u8) -> ScanmapEntry {
    let idx = (index & 0x7F) as usize;
    if idx < KBD_SCAN_CODES {
        SCANMAP_ESCAPED[idx]
    } else {
        ScanmapEntry { page: 0, code: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_table_size() {
        assert_eq!(SCANMAP_NORMAL.len(), 0x80);
        assert_eq!(SCANMAP_ESCAPED.len(), 0x80);
    }

    #[test]
    fn test_normal_table_specific_keys() {
        // Escape at scancode 0x01
        assert_eq!(
            lookup_normal(0x01),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_ESCAPE
            }
        );
        // 'A' at scancode 0x1E
        assert_eq!(
            lookup_normal(0x1E),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_A
            }
        );
        // Spacebar at scancode 0x39
        assert_eq!(
            lookup_normal(0x39),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_SPACEBAR
            }
        );
        // Left shift at scancode 0x2A
        assert_eq!(
            lookup_normal(0x2A),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_LEFT_SHIFT
            }
        );
        // Enter at scancode 0x1C
        assert_eq!(
            lookup_normal(0x1C),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_ENTER
            }
        );
    }

    #[test]
    fn test_normal_table_function_keys() {
        assert_eq!(
            lookup_normal(0x3B),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_F1
            }
        );
        assert_eq!(
            lookup_normal(0x44),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_F10
            }
        );
        assert_eq!(
            lookup_normal(0x58),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_F12
            }
        );
    }

    #[test]
    fn test_normal_table_numpad() {
        assert_eq!(
            lookup_normal(0x37),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_KP_STAR
            }
        );
        assert_eq!(
            lookup_normal(0x4A),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_KP_DASH
            }
        );
        assert_eq!(
            lookup_normal(0x4E),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_KP_PLUS
            }
        );
    }

    #[test]
    fn test_normal_table_unmapped_entry() {
        // 0x00 should be unmapped
        assert_eq!(lookup_normal(0x00), ScanmapEntry { page: 0, code: 0 });
        // 0x55 is not in the table
        assert_eq!(lookup_normal(0x55), ScanmapEntry { page: 0, code: 0 });
    }

    #[test]
    fn test_escaped_table_specific_keys() {
        // Up arrow at 0xE0 0x48
        assert_eq!(
            lookup_escaped(0x48),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_UP_ARROW
            }
        );
        // Right Ctrl at 0xE0 0x1D
        assert_eq!(
            lookup_escaped(0x1D),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_RIGHT_CTRL
            }
        );
        // Home at 0xE0 0x47
        assert_eq!(
            lookup_escaped(0x47),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_HOME
            }
        );
    }

    #[test]
    fn test_escaped_table_unmapped_entry() {
        // 0x00 should be unmapped
        assert_eq!(lookup_escaped(0x00), ScanmapEntry { page: 0, code: 0 });
    }

    #[test]
    fn test_escaped_keypad_enter() {
        assert_eq!(
            lookup_escaped(0x1C),
            ScanmapEntry {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_KP_ENTER
            }
        );
    }

    #[test]
    fn test_escaped_consumer_keys() {
        // Mute at 0xE0 0x20
        assert_eq!(
            lookup_escaped(0x20),
            ScanmapEntry {
                page: INPUT_PAGE_CONS,
                code: INPUT_CONS_MUTE
            }
        );
        // Volume Up at 0xE0 0x30
        assert_eq!(
            lookup_escaped(0x30),
            ScanmapEntry {
                page: INPUT_PAGE_CONS,
                code: INPUT_CONS_VOLUME_UP
            }
        );
    }

    #[test]
    fn test_colemak_letter_mapping() {
        // A stays A
        assert_eq!(apply_colemak(INPUT_KEY_A), INPUT_KEY_A);
        // W stays W (home row)
        assert_eq!(apply_colemak(INPUT_KEY_W), INPUT_KEY_W);
        // E → F
        assert_eq!(apply_colemak(INPUT_KEY_E), INPUT_KEY_F);
        // R → P
        assert_eq!(apply_colemak(INPUT_KEY_R), INPUT_KEY_P);
        // T → G
        assert_eq!(apply_colemak(INPUT_KEY_T), INPUT_KEY_G);
        // Y → J
        assert_eq!(apply_colemak(INPUT_KEY_Y), INPUT_KEY_J);
    }

    #[test]
    fn test_colemak_does_not_affect_non_letters() {
        // Non-letter codes pass through unchanged
        assert_eq!(apply_colemak(INPUT_KEY_SPACEBAR), INPUT_KEY_SPACEBAR);
        assert_eq!(apply_colemak(INPUT_KEY_ENTER), INPUT_KEY_ENTER);
        assert_eq!(apply_colemak(INPUT_KEY_ESCAPE), INPUT_KEY_ESCAPE);
        assert_eq!(apply_colemak(INPUT_KEY_LEFT_SHIFT), INPUT_KEY_LEFT_SHIFT);
        assert_eq!(apply_colemak(INPUT_KEY_1), INPUT_KEY_1);
    }

    #[test]
    fn test_colemak_numpad_unchanged() {
        assert_eq!(apply_colemak(INPUT_KEY_KP_1), INPUT_KEY_KP_1);
        assert_eq!(apply_colemak(INPUT_KEY_KP_STAR), INPUT_KEY_KP_STAR);
    }
}
