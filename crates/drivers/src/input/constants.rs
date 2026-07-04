//! PS/2 keyboard and mouse constants.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/hid/pckbd/pckbd.h` and
//! `.refs/minix-3.3.0/minix/include/minix/input.h`

#![allow(clippy::identity_op)]


/// Keyboard data port (read/write data, read output buffer).
pub const KEYBD: u16 = 0x60;

/// Keyboard command port (write commands to controller).
pub const KB_COMMAND: u16 = 0x64;

/// Keyboard status port (read status from controller).
pub const KB_STATUS: u16 = 0x64;


/// ACK byte sent by keyboard in response to a command.
pub const KB_ACK: u8 = 0xFA;


/// Auxiliary device output buffer full.
pub const KB_AUX_BYTE: u8 = 0x20;

/// Output buffer full (data available to read).
pub const KB_OUT_FULL: u8 = 0x01;

/// Input buffer full (controller not ready to receive).
pub const KB_IN_FULL: u8 = 0x02;


/// Read the controller command byte.
pub const KBC_RD_RAM_CCB: u8 = 0x20;

/// Write the controller command byte.
pub const KBC_WR_RAM_CCB: u8 = 0x60;

/// Disable auxiliary device (mouse).
pub const KBC_DI_AUX: u8 = 0xA7;

/// Enable auxiliary device (mouse).
pub const KBC_EN_AUX: u8 = 0xA8;

/// Disable keyboard interface.
pub const KBC_DI_KBD: u8 = 0xAD;

/// Enable keyboard interface.
pub const KBC_EN_KBD: u8 = 0xAE;


/// Wait this many microseconds for a status update.
pub const KBC_WAIT_TIME: u32 = 100_000;

/// Wait this many microseconds for a result byte.
pub const KBC_READ_TIME: u32 = 1_000_000;

/// Microseconds to delay when polling.
pub const KBC_IN_DELAY: u32 = 7;


/// Output buffer size for data to the keyboard.
pub const KBD_OUT_BUFSZ: usize = 16;

/// Number of scancode entries in the translation tables.
pub const KBD_SCAN_CODES: usize = 0x80;


/// OR this mask to indicate a key release.
pub const SCAN_RELEASE: u8 = 0x80;

/// Scancode for the left Ctrl key.
pub const SCAN_CTRL: u8 = 0x1D;

/// Scancode for NumLock.
pub const SCAN_NUMLOCK: u8 = 0x45;

/// Prefix for extended scancodes (e.g., arrows, Home, End).
pub const SCAN_EXT0: u8 = 0xE0;

/// Prefix for Pause/Break-style extended scancodes.
pub const SCAN_EXT1: u8 = 0xE1;


/// Scroll Lock LED bit.
pub const LED_SCROLL_LOCK: u8 = 0x01;

/// Num Lock LED bit.
pub const LED_NUM_LOCK: u8 = 0x02;

/// Caps Lock LED bit.
pub const LED_CAPS_LOCK: u8 = 0x04;

/// Command to set keyboard LEDs.
pub const LED_CODE: u8 = 0xED;


/// General Desktop page.
pub const INPUT_PAGE_GD: u16 = 0x0001;

/// Keyboard/Keypad page.
pub const INPUT_PAGE_KEY: u16 = 0x0007;

/// LED page.
pub const INPUT_PAGE_LED: u16 = 0x0008;

/// Button page.
pub const INPUT_PAGE_BUTTON: u16 = 0x0009;

/// Consumer page.
pub const INPUT_PAGE_CONS: u16 = 0x000C;


/// Key release event value.
pub const INPUT_RELEASE: i32 = 0;

/// Key press event value.
pub const INPUT_PRESS: i32 = 1;


/// Absolute value (the default).
pub const INPUT_FLAG_ABS: u16 = 0x00;

/// Relative value.
pub const INPUT_FLAG_REL: u16 = 0x04;


/// X-axis movement.
pub const INPUT_GD_X: u16 = 0x0030;

/// Y-axis movement.
pub const INPUT_GD_Y: u16 = 0x0031;

/// System Power Down.
pub const INPUT_GD_SYSTEM_POWER_DOWN: u16 = 0x0081;

/// System Sleep.
pub const INPUT_GD_SYSTEM_SLEEP: u16 = 0x0082;

/// System Wake Up.
pub const INPUT_GD_SYSTEM_WAKE_UP: u16 = 0x0083;


pub const INPUT_KEY_A: u16 = 0x0004;
pub const INPUT_KEY_B: u16 = 0x0005;
pub const INPUT_KEY_C: u16 = 0x0006;
pub const INPUT_KEY_D: u16 = 0x0007;
pub const INPUT_KEY_E: u16 = 0x0008;
pub const INPUT_KEY_F: u16 = 0x0009;
pub const INPUT_KEY_G: u16 = 0x000A;
pub const INPUT_KEY_H: u16 = 0x000B;
pub const INPUT_KEY_I: u16 = 0x000C;
pub const INPUT_KEY_J: u16 = 0x000D;
pub const INPUT_KEY_K: u16 = 0x000E;
pub const INPUT_KEY_L: u16 = 0x000F;
pub const INPUT_KEY_M: u16 = 0x0010;
pub const INPUT_KEY_N: u16 = 0x0011;
pub const INPUT_KEY_O: u16 = 0x0012;
pub const INPUT_KEY_P: u16 = 0x0013;
pub const INPUT_KEY_Q: u16 = 0x0014;
pub const INPUT_KEY_R: u16 = 0x0015;
pub const INPUT_KEY_S: u16 = 0x0016;
pub const INPUT_KEY_T: u16 = 0x0017;
pub const INPUT_KEY_U: u16 = 0x0018;
pub const INPUT_KEY_V: u16 = 0x0019;
pub const INPUT_KEY_W: u16 = 0x001A;
pub const INPUT_KEY_X: u16 = 0x001B;
pub const INPUT_KEY_Y: u16 = 0x001C;
pub const INPUT_KEY_Z: u16 = 0x001D;

pub const INPUT_KEY_1: u16 = 0x001E;
pub const INPUT_KEY_2: u16 = 0x001F;
pub const INPUT_KEY_3: u16 = 0x0020;
pub const INPUT_KEY_4: u16 = 0x0021;
pub const INPUT_KEY_5: u16 = 0x0022;
pub const INPUT_KEY_6: u16 = 0x0023;
pub const INPUT_KEY_7: u16 = 0x0024;
pub const INPUT_KEY_8: u16 = 0x0025;
pub const INPUT_KEY_9: u16 = 0x0026;
pub const INPUT_KEY_0: u16 = 0x0027;

pub const INPUT_KEY_ENTER: u16 = 0x0028;
pub const INPUT_KEY_ESCAPE: u16 = 0x0029;
pub const INPUT_KEY_BACKSPACE: u16 = 0x002A;
pub const INPUT_KEY_TAB: u16 = 0x002B;
pub const INPUT_KEY_SPACEBAR: u16 = 0x002C;
pub const INPUT_KEY_DASH: u16 = 0x002D;
pub const INPUT_KEY_EQUAL: u16 = 0x002E;
pub const INPUT_KEY_OPEN_BRACKET: u16 = 0x002F;
pub const INPUT_KEY_CLOSE_BRACKET: u16 = 0x0030;
pub const INPUT_KEY_BACKSLASH: u16 = 0x0031;
pub const INPUT_KEY_EUROPE_1: u16 = 0x0032;
pub const INPUT_KEY_SEMICOLON: u16 = 0x0033;
pub const INPUT_KEY_APOSTROPH: u16 = 0x0034;
pub const INPUT_KEY_GRAVE_ACCENT: u16 = 0x0035;
pub const INPUT_KEY_COMMA: u16 = 0x0036;
pub const INPUT_KEY_PERIOD: u16 = 0x0037;
pub const INPUT_KEY_SLASH: u16 = 0x0038;
pub const INPUT_KEY_CAPS_LOCK: u16 = 0x0039;

pub const INPUT_KEY_F1: u16 = 0x003A;
pub const INPUT_KEY_F2: u16 = 0x003B;
pub const INPUT_KEY_F3: u16 = 0x003C;
pub const INPUT_KEY_F4: u16 = 0x003D;
pub const INPUT_KEY_F5: u16 = 0x003E;
pub const INPUT_KEY_F6: u16 = 0x003F;
pub const INPUT_KEY_F7: u16 = 0x0040;
pub const INPUT_KEY_F8: u16 = 0x0041;
pub const INPUT_KEY_F9: u16 = 0x0042;
pub const INPUT_KEY_F10: u16 = 0x0043;
pub const INPUT_KEY_F11: u16 = 0x0044;
pub const INPUT_KEY_F12: u16 = 0x0045;

pub const INPUT_KEY_PRINT_SCREEN: u16 = 0x0046;
pub const INPUT_KEY_SCROLL_LOCK: u16 = 0x0047;
pub const INPUT_KEY_PAUSE: u16 = 0x0048;
pub const INPUT_KEY_INSERT: u16 = 0x0049;
pub const INPUT_KEY_HOME: u16 = 0x004A;
pub const INPUT_KEY_PAGE_UP: u16 = 0x004B;
pub const INPUT_KEY_DELETE: u16 = 0x004C;
pub const INPUT_KEY_END: u16 = 0x004D;
pub const INPUT_KEY_PAGE_DOWN: u16 = 0x004E;
pub const INPUT_KEY_RIGHT_ARROW: u16 = 0x004F;
pub const INPUT_KEY_LEFT_ARROW: u16 = 0x0050;
pub const INPUT_KEY_DOWN_ARROW: u16 = 0x0051;
pub const INPUT_KEY_UP_ARROW: u16 = 0x0052;
pub const INPUT_KEY_NUM_LOCK: u16 = 0x0053;

pub const INPUT_KEY_KP_SLASH: u16 = 0x0054;
pub const INPUT_KEY_KP_STAR: u16 = 0x0055;
pub const INPUT_KEY_KP_DASH: u16 = 0x0056;
pub const INPUT_KEY_KP_PLUS: u16 = 0x0057;
pub const INPUT_KEY_KP_ENTER: u16 = 0x0058;
pub const INPUT_KEY_KP_1: u16 = 0x0059;
pub const INPUT_KEY_KP_2: u16 = 0x005A;
pub const INPUT_KEY_KP_3: u16 = 0x005B;
pub const INPUT_KEY_KP_4: u16 = 0x005C;
pub const INPUT_KEY_KP_5: u16 = 0x005D;
pub const INPUT_KEY_KP_6: u16 = 0x005E;
pub const INPUT_KEY_KP_7: u16 = 0x005F;
pub const INPUT_KEY_KP_8: u16 = 0x0060;
pub const INPUT_KEY_KP_9: u16 = 0x0061;
pub const INPUT_KEY_KP_0: u16 = 0x0062;
pub const INPUT_KEY_KP_PERIOD: u16 = 0x0063;

pub const INPUT_KEY_EUROPE_2: u16 = 0x0064;
pub const INPUT_KEY_APPLICATION: u16 = 0x0065;
pub const INPUT_KEY_POWER: u16 = 0x0066;
pub const INPUT_KEY_KP_EQUAL: u16 = 0x0067;

pub const INPUT_KEY_F13: u16 = 0x0068;
pub const INPUT_KEY_F14: u16 = 0x0069;
pub const INPUT_KEY_F15: u16 = 0x006A;
pub const INPUT_KEY_F16: u16 = 0x006B;
pub const INPUT_KEY_F17: u16 = 0x006C;
pub const INPUT_KEY_F18: u16 = 0x006D;
pub const INPUT_KEY_F19: u16 = 0x006E;
pub const INPUT_KEY_F20: u16 = 0x006F;
pub const INPUT_KEY_F21: u16 = 0x0070;
pub const INPUT_KEY_F22: u16 = 0x0071;
pub const INPUT_KEY_F23: u16 = 0x0072;
pub const INPUT_KEY_F24: u16 = 0x0073;

pub const INPUT_KEY_EQUAL_SIGN: u16 = 0x007E;

pub const INPUT_KEY_I10L_1: u16 = 0x0076;
pub const INPUT_KEY_I10L_2: u16 = 0x0077;
pub const INPUT_KEY_I10L_3: u16 = 0x0078;
pub const INPUT_KEY_I10L_4: u16 = 0x0079;
pub const INPUT_KEY_I10L_5: u16 = 0x007A;
pub const INPUT_KEY_I10L_6: u16 = 0x007B;

pub const INPUT_KEY_LANG_1: u16 = 0x0084;
pub const INPUT_KEY_LANG_2: u16 = 0x0085;
pub const INPUT_KEY_LANG_3: u16 = 0x0086;
pub const INPUT_KEY_LANG_4: u16 = 0x0087;
pub const INPUT_KEY_LANG_5: u16 = 0x0088;

pub const INPUT_KEY_SYSREQ: u16 = 0x008C;


pub const INPUT_KEY_LEFT_CTRL: u16 = 0x00E0;
pub const INPUT_KEY_LEFT_SHIFT: u16 = 0x00E1;
pub const INPUT_KEY_LEFT_ALT: u16 = 0x00E2;
pub const INPUT_KEY_LEFT_GUI: u16 = 0x00E3;
pub const INPUT_KEY_RIGHT_CTRL: u16 = 0x00E4;
pub const INPUT_KEY_RIGHT_SHIFT: u16 = 0x00E5;
pub const INPUT_KEY_RIGHT_ALT: u16 = 0x00E6;
pub const INPUT_KEY_RIGHT_GUI: u16 = 0x00E7;


pub const INPUT_LED_NUMLOCK: u16 = 0x0001;
pub const INPUT_LED_CAPSLOCK: u16 = 0x0002;
pub const INPUT_LED_SCROLLLOCK: u16 = 0x0003;


pub const INPUT_BUTTON_1: u16 = 0x0001;


pub const INPUT_CONS_SCAN_NEXT_TRACK: u16 = 0x00B5;
pub const INPUT_CONS_SCAN_PREVIOUS_TRACK: u16 = 0x00B6;
pub const INPUT_CONS_STOP: u16 = 0x00B7;

pub const INPUT_CONS_PLAY_PAUSE: u16 = 0x00CD;

pub const INPUT_CONS_MUTE: u16 = 0x00E2;

pub const INPUT_CONS_VOLUME_UP: u16 = 0x00E9;
pub const INPUT_CONS_VOLUME_DOWN: u16 = 0x00EA;

pub const INPUT_CONS_AL_MEDIA_SELECT: u16 = 0x0183;

pub const INPUT_CONS_AL_EMAIL_READER: u16 = 0x018A;

pub const INPUT_CONS_AL_CALCULATOR: u16 = 0x0192;

pub const INPUT_CONS_AL_LOCAL_BROWSER: u16 = 0x0194;

pub const INPUT_CONS_AC_SEARCH: u16 = 0x0221;
pub const INPUT_CONS_AC_GO_TO: u16 = 0x0222;
pub const INPUT_CONS_AC_HOME: u16 = 0x0223;
pub const INPUT_CONS_AC_BACK: u16 = 0x0224;
pub const INPUT_CONS_AC_FORWARD: u16 = 0x0225;
pub const INPUT_CONS_AC_STOP: u16 = 0x0226;
pub const INPUT_CONS_AC_REFRESH: u16 = 0x0227;

pub const INPUT_CONS_AC_BOOKMARKS: u16 = 0x022A;


/// Keyboard device flag.
pub const INPUT_DEV_KBD: u8 = 0x01;

/// Mouse device flag.
pub const INPUT_DEV_MOUSE: u8 = 0x02;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_io_port_addresses() {
        assert_eq!(KEYBD, 0x60);
        assert_eq!(KB_COMMAND, 0x64);
        assert_eq!(KB_STATUS, 0x64);
    }

    #[test]
    fn test_controller_commands() {
        assert_eq!(KBC_RD_RAM_CCB, 0x20);
        assert_eq!(KBC_WR_RAM_CCB, 0x60);
        assert_eq!(KBC_DI_AUX, 0xA7);
        assert_eq!(KBC_EN_AUX, 0xA8);
        assert_eq!(KBC_DI_KBD, 0xAD);
        assert_eq!(KBC_EN_KBD, 0xAE);
    }

    #[test]
    fn test_status_bits() {
        assert_eq!(KB_AUX_BYTE, 0x20);
        assert_eq!(KB_OUT_FULL, 0x01);
        assert_eq!(KB_IN_FULL, 0x02);
    }

    #[test]
    fn test_scancode_constants() {
        assert_eq!(SCAN_RELEASE, 0x80);
        assert_eq!(SCAN_CTRL, 0x1D);
        assert_eq!(SCAN_NUMLOCK, 0x45);
        assert_eq!(SCAN_EXT0, 0xE0);
        assert_eq!(SCAN_EXT1, 0xE1);
    }

    #[test]
    fn test_led_flags() {
        assert_eq!(LED_SCROLL_LOCK, 0x01);
        assert_eq!(LED_NUM_LOCK, 0x02);
        assert_eq!(LED_CAPS_LOCK, 0x04);
        assert_eq!(LED_CODE, 0xED);
    }

    #[test]
    fn test_led_combinations() {
        // All three LEDs can be OR'd together
        let all = LED_SCROLL_LOCK | LED_NUM_LOCK | LED_CAPS_LOCK;
        assert_eq!(all, 0x07);
        // Scroll + Num
        assert_eq!(LED_SCROLL_LOCK | LED_NUM_LOCK, 0x03);
        // Num + Caps
        assert_eq!(LED_NUM_LOCK | LED_CAPS_LOCK, 0x06);
    }

    #[test]
    fn test_hid_pages() {
        assert_eq!(INPUT_PAGE_GD, 0x0001);
        assert_eq!(INPUT_PAGE_KEY, 0x0007);
        assert_eq!(INPUT_PAGE_LED, 0x0008);
        assert_eq!(INPUT_PAGE_BUTTON, 0x0009);
        assert_eq!(INPUT_PAGE_CONS, 0x000C);
    }

    #[test]
    fn test_event_values() {
        assert_eq!(INPUT_RELEASE, 0);
        assert_eq!(INPUT_PRESS, 1);
    }

    #[test]
    fn test_event_flags() {
        assert_eq!(INPUT_FLAG_ABS, 0x00);
        assert_eq!(INPUT_FLAG_REL, 0x04);
    }

    #[test]
    fn test_modifier_keys() {
        assert_eq!(INPUT_KEY_LEFT_CTRL, 0x00E0);
        assert_eq!(INPUT_KEY_LEFT_SHIFT, 0x00E1);
        assert_eq!(INPUT_KEY_LEFT_ALT, 0x00E2);
        assert_eq!(INPUT_KEY_LEFT_GUI, 0x00E3);
        assert_eq!(INPUT_KEY_RIGHT_CTRL, 0x00E4);
        assert_eq!(INPUT_KEY_RIGHT_SHIFT, 0x00E5);
        assert_eq!(INPUT_KEY_RIGHT_ALT, 0x00E6);
        assert_eq!(INPUT_KEY_RIGHT_GUI, 0x00E7);
    }

    #[test]
    fn test_keyboard_buffer_size() {
        assert_eq!(KBD_OUT_BUFSZ, 16);
        assert_eq!(KBD_SCAN_CODES, 0x80);
    }

    #[test]
    fn test_kb_ack() {
        assert_eq!(KB_ACK, 0xFA);
    }

    #[test]
    fn test_timing_constants() {
        assert_eq!(KBC_WAIT_TIME, 100_000);
        assert_eq!(KBC_READ_TIME, 1_000_000);
        assert_eq!(KBC_IN_DELAY, 7);
    }
}
