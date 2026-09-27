//! Keyboard and pointer translation for `wl_seat` (Phase 1c).
//!
//! The input server speaks HID records (page, usage, value); Wayland speaks evdev
//! keycodes and xkb-style modifier masks. This module is the translation between
//! them, kept pure so the layout rules are host-testable and the server loop stays
//! a loop.
//!
//! Phase 1 sends `wl_keyboard.key` as an evdev keycode with **no keymap event**
//! (`WAYLAND.md` §6.6): the in-house client interprets it with the US table here,
//! and a real XKB keymap — which needs `xkbcommon` — is a later port item.

/// `wl_keyboard.key` / `wl_pointer.button` state: released.
pub const STATE_RELEASED: u32 = 0;
/// `wl_keyboard.key` / `wl_pointer.button` state: pressed.
pub const STATE_PRESSED: u32 = 1;

// `wl_keyboard.modifiers`'s masks. These are the xkb state bits, so a US layout
// uses shift, caps, ctrl, mod1 (alt), mod2 (num) and mod4 (logo).

/// Shift.
pub const MOD_SHIFT: u32 = 1 << 0;
/// Caps lock.
pub const MOD_CAPS: u32 = 1 << 1;
/// Control.
pub const MOD_CTRL: u32 = 1 << 2;
/// Mod1 — alt here.
pub const MOD_ALT: u32 = 1 << 3;
/// Mod2 — num lock here.
pub const MOD_NUM: u32 = 1 << 4;
/// Mod4 — the logo/super key here.
pub const MOD_LOGO: u32 = 1 << 6;

// The HID usages this module names (keyboard page 0x07). The input server's
// backends all decode into these, whatever produced them.
pub const USAGE_CAPS_LOCK: u16 = 0x39;
pub const USAGE_NUM_LOCK: u16 = 0x53;
pub const USAGE_LEFT_CTRL: u16 = 0xE0;
pub const USAGE_LEFT_SHIFT: u16 = 0xE1;
pub const USAGE_LEFT_ALT: u16 = 0xE2;
pub const USAGE_LEFT_GUI: u16 = 0xE3;
pub const USAGE_RIGHT_CTRL: u16 = 0xE4;
pub const USAGE_RIGHT_SHIFT: u16 = 0xE5;
pub const USAGE_RIGHT_ALT: u16 = 0xE6;
pub const USAGE_RIGHT_GUI: u16 = 0xE7;

/// evdev button codes, which is what `wl_pointer.button` carries.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

/// A HID keyboard usage as its evdev keycode, or `None` for one Phase 1 does not
/// translate.
///
/// The mapping is the standard `hid-input` one, and it is **not** the order the
/// usages are in: HID lists the letters alphabetically while evdev lists them in
/// QWERTY order, so `'a'` (usage 0x04) is keycode 30 and `'q'` (0x14) is 16.
pub fn hid_to_evdev(usage: u16) -> Option<u8> {
    Some(match usage {
        // Letters.
        0x04 => 30, // a
        0x05 => 48, // b
        0x06 => 46, // c
        0x07 => 32, // d
        0x08 => 18, // e
        0x09 => 33, // f
        0x0A => 34, // g
        0x0B => 35, // h
        0x0C => 23, // i
        0x0D => 36, // j
        0x0E => 37, // k
        0x0F => 38, // l
        0x10 => 50, // m
        0x11 => 49, // n
        0x12 => 24, // o
        0x13 => 25, // p
        0x14 => 16, // q
        0x15 => 19, // r
        0x16 => 31, // s
        0x17 => 20, // t
        0x18 => 22, // u
        0x19 => 47, // v
        0x1A => 17, // w
        0x1B => 45, // x
        0x1C => 21, // y
        0x1D => 44, // z
        // The digit row.
        0x1E => 2,  // 1
        0x1F => 3,  // 2
        0x20 => 4,  // 3
        0x21 => 5,  // 4
        0x22 => 6,  // 5
        0x23 => 7,  // 6
        0x24 => 8,  // 7
        0x25 => 9,  // 8
        0x26 => 10, // 9
        0x27 => 11, // 0
        // Controls.
        0x28 => 28, // enter
        0x29 => 1,  // escape
        0x2A => 14, // backspace
        0x2B => 15, // tab
        0x2C => 57, // space
        // Punctuation.
        0x2D => 12, // -
        0x2E => 13, // =
        0x2F => 26, // [
        0x30 => 27, // ]
        0x31 => 43, // backslash
        0x33 => 39, // ;
        0x34 => 40, // '
        0x35 => 41, // `
        0x36 => 51, // ,
        0x37 => 52, // .
        0x38 => 53, // /
        0x39 => 58, // caps lock
        // Arrows.
        0x4F => 106, // right
        0x50 => 105, // left
        0x51 => 108, // down
        0x52 => 103, // up
        0x53 => 69,  // num lock
        // Modifiers.
        0xE0 => 29,  // left ctrl
        0xE1 => 42,  // left shift
        0xE2 => 56,  // left alt
        0xE3 => 125, // left gui
        0xE4 => 97,  // right ctrl
        0xE5 => 54,  // right shift
        0xE6 => 100, // right alt
        0xE7 => 126, // right gui
        _ => return None,
    })
}

/// The modifier state a `wl_keyboard.modifiers` event reports.
///
/// Each side of a modifier pair is tracked **separately**: a single flag per
/// modifier would clear shift when the *other* shift's release arrived, which is
/// what a lost press or an autorepeat looks like. Locks (`caps`, `num`) toggle on
/// the *press* and ignore the release; the others follow the key's own state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    lshift: bool,
    rshift: bool,
    lctrl: bool,
    rctrl: bool,
    lalt: bool,
    ralt: bool,
    llogo: bool,
    rlogo: bool,
    caps: bool,
    num: bool,
}

impl Modifiers {
    /// Apply one key transition. Returns whether a modifier changed, so a caller
    /// knows whether a `wl_keyboard.modifiers` event is due.
    pub fn apply(&mut self, usage: u16, pressed: bool) -> bool {
        match usage {
            USAGE_LEFT_SHIFT => Self::set(&mut self.lshift, pressed),
            USAGE_RIGHT_SHIFT => Self::set(&mut self.rshift, pressed),
            USAGE_LEFT_CTRL => Self::set(&mut self.lctrl, pressed),
            USAGE_RIGHT_CTRL => Self::set(&mut self.rctrl, pressed),
            USAGE_LEFT_ALT => Self::set(&mut self.lalt, pressed),
            USAGE_RIGHT_ALT => Self::set(&mut self.ralt, pressed),
            USAGE_LEFT_GUI => Self::set(&mut self.llogo, pressed),
            USAGE_RIGHT_GUI => Self::set(&mut self.rlogo, pressed),
            USAGE_CAPS_LOCK => Self::toggle(&mut self.caps, pressed),
            USAGE_NUM_LOCK => Self::toggle(&mut self.num, pressed),
            _ => false,
        }
    }

    fn set(slot: &mut bool, pressed: bool) -> bool {
        if *slot == pressed {
            return false;
        }
        *slot = pressed;
        true
    }

    fn toggle(slot: &mut bool, pressed: bool) -> bool {
        if !pressed {
            return false;
        }
        *slot = !*slot;
        true
    }

    /// The `depressed` mask: modifiers held right now, either side.
    pub fn depressed(&self) -> u32 {
        let mut m = 0;
        if self.lshift || self.rshift {
            m |= MOD_SHIFT;
        }
        if self.lctrl || self.rctrl {
            m |= MOD_CTRL;
        }
        if self.lalt || self.ralt {
            m |= MOD_ALT;
        }
        if self.llogo || self.rlogo {
            m |= MOD_LOGO;
        }
        m
    }

    /// The `locked` mask: the toggles that are on.
    pub fn locked(&self) -> u32 {
        let mut m = 0;
        if self.caps {
            m |= MOD_CAPS;
        }
        if self.num {
            m |= MOD_NUM;
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_map_in_qwerty_order_not_hid_order() {
        assert_eq!(hid_to_evdev(0x04), Some(30)); // a
        assert_eq!(hid_to_evdev(0x14), Some(16)); // q
        assert_eq!(hid_to_evdev(0x1D), Some(44)); // z
        // HID order is alphabetical, so consecutive usages are not consecutive
        // keycodes.
        assert_ne!(
            hid_to_evdev(0x04).unwrap() as i32 + 1,
            hid_to_evdev(0x05).unwrap() as i32
        );
    }

    #[test]
    fn digit_row_and_controls_map() {
        assert_eq!(hid_to_evdev(0x1E), Some(2)); // 1
        assert_eq!(hid_to_evdev(0x27), Some(11)); // 0
        assert_eq!(hid_to_evdev(0x2C), Some(57)); // space
        assert_eq!(hid_to_evdev(0x28), Some(28)); // enter
        assert_eq!(hid_to_evdev(0x52), Some(103)); // up
    }

    #[test]
    fn an_unmapped_usage_is_not_translated() {
        assert_eq!(hid_to_evdev(0x00), None);
        assert_eq!(hid_to_evdev(0xFFFF), None);
    }

    #[test]
    fn either_side_of_a_modifier_pair_holds_it() {
        let mut m = Modifiers::default();
        assert!(m.apply(USAGE_LEFT_SHIFT, true));
        assert_eq!(m.depressed(), MOD_SHIFT);
        assert!(!m.apply(USAGE_LEFT_SHIFT, true), "already held");
        // Releasing the *other* shift while left is still down must not clear it.
        assert!(!m.apply(USAGE_RIGHT_SHIFT, false));
        assert_eq!(m.depressed(), MOD_SHIFT);
    }

    #[test]
    fn caps_toggles_on_press_and_ignores_release() {
        let mut m = Modifiers::default();
        assert!(m.apply(USAGE_CAPS_LOCK, true));
        assert_eq!(m.locked(), MOD_CAPS);
        assert!(!m.apply(USAGE_CAPS_LOCK, false), "release does nothing");
        assert_eq!(m.locked(), MOD_CAPS);
        assert!(m.apply(USAGE_CAPS_LOCK, true));
        assert_eq!(m.locked(), 0);
    }

    #[test]
    fn a_non_modifier_key_changes_nothing() {
        let mut m = Modifiers::default();
        assert!(!m.apply(0x04, true));
        assert_eq!(m.depressed(), 0);
        assert_eq!(m.locked(), 0);
    }

    #[test]
    fn masks_combine() {
        let mut m = Modifiers::default();
        m.apply(USAGE_LEFT_CTRL, true);
        m.apply(USAGE_LEFT_ALT, true);
        assert_eq!(m.depressed(), MOD_CTRL | MOD_ALT);
    }
}
