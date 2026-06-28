//! PS/2 keyboard — scancode translation, modifier tracking.
//!
//! Ported from `pckbd.c` function `kbd_process()`.
//!
//! Handles multi-byte scancodes (0xE0, 0xE1 prefixes), tracks modifier keys,
//! and translates scancodes to HID usage codes via the scanmap tables.

#![allow(clippy::identity_op)]

use crate::input::constants::*;
use crate::input::scanmap;

/// Translates a single PS/2 scancode byte into a (page, code, press) triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranslatedEvent {
    /// HID usage page.
    pub page: u16,
    /// HID usage code.
    pub code: u16,
    /// `INPUT_PRESS` or `INPUT_RELEASE`.
    pub press: i32,
}

/// Decoded scancode result from the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdEvent {
    /// One HID event was produced.
    Event(TranslatedEvent),
    /// The byte was consumed as a prefix (waiting for more).
    Prefix,
    /// The byte was consumed by a multi-byte sequence.
    Consumed,
    /// No mapping found for this scancode.
    Unmapped,
}

/// Keyboard state machine for translating PS/2 scancode streams.
///
/// Tracks a small state for multi-byte scancode sequences and maintains
/// the current modifier key state (shift, ctrl, alt).
#[derive(Debug, Clone, Copy)]
pub struct KeyboardState {
    /// Internal state: 0 = normal, 1 = after 0xE0, 2 = after 0xE1, 3 = after 0xE1+0x1D
    pub state: u8,
    /// Whether Colemak layout remapping is enabled.
    pub colemak_enabled: bool,
}

impl KeyboardState {
    /// Create a new keyboard state in the default (normal) mode.
    pub const fn new() -> Self {
        KeyboardState {
            state: 0,
            colemak_enabled: false,
        }
    }

    /// Feed a scancode byte into the state machine.
    ///
    /// Returns the decoded event.  Multiple calls may be needed for multi-byte
    /// scancode sequences.
    pub fn process(&mut self, scode: u8) -> KbdEvent {
        let press = if scode & SCAN_RELEASE == 0 {
            INPUT_PRESS
        } else {
            INPUT_RELEASE
        };
        let index = scode & !SCAN_RELEASE;

        match self.state {
            1 => {
                // After 0xE0 prefix — use escaped table
                let entry = scanmap::lookup_escaped(index);
                self.state = 0;
                if entry.page == 0 && entry.code == 0 {
                    return KbdEvent::Unmapped;
                }
                KbdEvent::Event(self.make_event(entry.page, entry.code, press))
            }
            2 => {
                // After 0xE1 prefix (Pause/Break sequence)
                self.state = if index == SCAN_CTRL { 3 } else { 0 };
                KbdEvent::Consumed
            }
            3 => {
                // After 0xE1 0x1D — check for NumLock scancode (Pause)
                self.state = 0;
                if index == SCAN_NUMLOCK {
                    return KbdEvent::Event(self.make_event(
                        INPUT_PAGE_KEY,
                        INPUT_KEY_PAUSE,
                        press,
                    ));
                }
                // Fall through to normal lookup
                self.state = 0;
                let entry = scanmap::lookup_normal(index);
                if entry.page == 0 && entry.code == 0 {
                    return KbdEvent::Unmapped;
                }
                KbdEvent::Event(self.make_event(entry.page, entry.code, press))
            }
            _ => {
                // Normal state
                match scode {
                    SCAN_EXT0 => {
                        self.state = 1;
                        return KbdEvent::Prefix;
                    }
                    SCAN_EXT1 => {
                        self.state = 2;
                        return KbdEvent::Prefix;
                    }
                    _ => {}
                }
                let entry = scanmap::lookup_normal(index);
                if entry.page == 0 && entry.code == 0 {
                    return KbdEvent::Unmapped;
                }
                KbdEvent::Event(self.make_event(entry.page, entry.code, press))
            }
        }
    }

    fn make_event(&self, page: u16, mut code: u16, press: i32) -> TranslatedEvent {
        if self.colemak_enabled && page == INPUT_PAGE_KEY {
            code = scanmap::apply_colemak(code);
        }
        TranslatedEvent { page, code, press }
    }
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_key_press() {
        let mut kbd = KeyboardState::new();
        // Scancode 0x1E = 'A' press (bit7 clear = press)
        let event = kbd.process(0x1E);
        assert_eq!(
            event,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_A,
                press: INPUT_PRESS,
            })
        );
    }

    #[test]
    fn test_normal_key_release() {
        let mut kbd = KeyboardState::new();
        // Scancode 0x9E = 'A' release (bit7 set = release)
        let event = kbd.process(0x9E);
        assert_eq!(
            event,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_A,
                press: INPUT_RELEASE,
            })
        );
    }

    #[test]
    fn test_ext0_prefix() {
        let mut kbd = KeyboardState::new();
        // 0xE0 prefix
        let e1 = kbd.process(SCAN_EXT0);
        assert_eq!(e1, KbdEvent::Prefix);
        assert_eq!(kbd.state, 1);

        // 0x48 = Up arrow
        let e2 = kbd.process(0x48);
        assert_eq!(
            e2,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_UP_ARROW,
                press: INPUT_PRESS,
            })
        );
        assert_eq!(kbd.state, 0);
    }

    #[test]
    fn test_ext0_release() {
        let mut kbd = KeyboardState::new();
        let _ = kbd.process(SCAN_EXT0);
        // 0xC8 = Up arrow release (0x48 | 0x80)
        let e = kbd.process(0xC8);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_UP_ARROW,
                press: INPUT_RELEASE,
            })
        );
    }

    #[test]
    fn test_ext1_pause_sequence() {
        let mut kbd = KeyboardState::new();
        // Pause = 0xE1, 0x1D, 0x45
        assert_eq!(kbd.process(SCAN_EXT1), KbdEvent::Prefix);
        assert_eq!(kbd.state, 2);

        assert_eq!(kbd.process(0x1D), KbdEvent::Consumed); // enters state 3
        assert_eq!(kbd.state, 3);

        let e = kbd.process(SCAN_NUMLOCK);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_PAUSE,
                press: INPUT_PRESS,
            })
        );
        assert_eq!(kbd.state, 0);
    }

    #[test]
    fn test_ext1_non_pause_fallthrough() {
        let mut kbd = KeyboardState::new();
        let _ = kbd.process(SCAN_EXT1);
        let _ = kbd.process(0x1D);
        // Non-NumLock after E1+1D falls through to normal table
        let e = kbd.process(0x1E); // 'A'
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_A,
                press: INPUT_PRESS,
            })
        );
    }

    #[test]
    fn test_modifier_left_shift() {
        let mut kbd = KeyboardState::new();
        // Press left shift
        let e = kbd.process(0x2A);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_LEFT_SHIFT,
                press: INPUT_PRESS,
            })
        );

        // Release left shift
        let e = kbd.process(0xAA);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_LEFT_SHIFT,
                press: INPUT_RELEASE,
            })
        );
    }

    #[test]
    fn test_modifier_right_shift() {
        let mut kbd = KeyboardState::new();
        let e = kbd.process(0x36); // Right shift press
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_RIGHT_SHIFT,
                press: INPUT_PRESS,
            })
        );
    }

    #[test]
    fn test_modifier_left_ctrl() {
        let mut kbd = KeyboardState::new();
        let e = kbd.process(0x1D);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_LEFT_CTRL,
                press: INPUT_PRESS,
            })
        );
    }

    #[test]
    fn test_modifier_right_ctrl_via_ext0() {
        let mut kbd = KeyboardState::new();
        let _ = kbd.process(SCAN_EXT0);
        let e = kbd.process(0x1D);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_RIGHT_CTRL,
                press: INPUT_PRESS,
            })
        );
    }

    #[test]
    fn test_modifier_left_alt() {
        let mut kbd = KeyboardState::new();
        let e = kbd.process(0x38);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_LEFT_ALT,
                press: INPUT_PRESS,
            })
        );
    }

    #[test]
    fn test_unmapped_scancode() {
        let mut kbd = KeyboardState::new();
        let e = kbd.process(0x00); // unmapped
        assert_eq!(e, KbdEvent::Unmapped);
    }

    #[test]
    fn test_unmapped_escaped_scancode() {
        let mut kbd = KeyboardState::new();
        let _ = kbd.process(SCAN_EXT0);
        let e = kbd.process(0x01); // unmapped in escaped table
        assert_eq!(e, KbdEvent::Unmapped);
    }

    #[test]
    fn test_colemak_letter_remap() {
        let mut kbd = KeyboardState::new();
        kbd.colemak_enabled = true;

        // E scancode 0x12 → Colemak: F
        let e = kbd.process(0x12);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_F,
                press: INPUT_PRESS,
            })
        );

        // A scancode 0x1E → Colemak: A (unchanged)
        let e = kbd.process(0x1E);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_A,
                press: INPUT_PRESS,
            })
        );
    }

    #[test]
    fn test_colemak_does_not_affect_non_letters() {
        let mut kbd = KeyboardState::new();
        kbd.colemak_enabled = true;

        // F1 stays F1
        let e = kbd.process(0x3B);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_F1,
                press: INPUT_PRESS,
            })
        );
    }

    #[test]
    fn test_keyboard_state_default() {
        let kbd = KeyboardState::new();
        assert_eq!(kbd.state, 0);
        assert!(!kbd.colemak_enabled);
    }

    #[test]
    fn test_release_bit_masking() {
        let mut kbd = KeyboardState::new();
        // 0x9E = 0x1E | 0x80 → A release
        let e = kbd.process(0x9E);
        assert_eq!(
            e,
            KbdEvent::Event(TranslatedEvent {
                page: INPUT_PAGE_KEY,
                code: INPUT_KEY_A,
                press: INPUT_RELEASE,
            })
        );
    }
}
