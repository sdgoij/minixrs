//! Input driver — unifies PS/2 keyboard and mouse into a single driver.
//!
//! Ported from `pckbd.c` — wraps keyboard scan, mouse packet parsing,
//! and PS/2 controller I/O into a struct-based driver interface.

#![allow(clippy::identity_op)]

use crate::input::constants::*;
use crate::input::controller::{ControllerResult, IoBackend, Ps2Controller};
use crate::input::keyboard::{KbdEvent, KeyboardState};
use crate::input::mouse::{MouseEvent, MouseParser, MouseState};

/// Callbacks invoked by the input driver when events are decoded.
pub trait InputCallbacks {
    /// Called when a key press or release event is decoded.
    fn key_event(&mut self, page: u16, code: u16, press: i32);
    /// Called when a mouse button or movement event is decoded.
    fn mouse_event(&mut self, page: u16, code: u16, value: i32, flags: u16);
}

/// A no-op callback implementation that discards all events.
pub struct NullCallbacks;

impl InputCallbacks for NullCallbacks {
    fn key_event(&mut self, _page: u16, _code: u16, _press: i32) {}
    fn mouse_event(&mut self, _page: u16, _code: u16, _value: i32, _flags: u16) {}
}

/// Unified PS/2 keyboard and mouse driver.
///
/// Owns all state needed to decode scancode and mouse byte streams and
/// dispatches decoded events through a callback interface.
#[derive(Debug, Clone, Copy)]
pub struct InputDriver {
    /// Keyboard scancode state machine.
    pub keyboard: KeyboardState,
    /// Mouse byte-stream parser.
    pub mouse: MouseParser,
    /// Previous mouse button state (for detecting button changes).
    pub prev_mouse_buttons: u8,
}

impl InputDriver {
    /// Create a new input driver in the default state.
    pub const fn new() -> Self {
        InputDriver {
            keyboard: KeyboardState::new(),
            mouse: MouseParser::new(),
            prev_mouse_buttons: 0,
        }
    }

    /// Initialize the PS/2 controller hardware.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.  Should be called once at startup.
    pub unsafe fn init<IO: IoBackend>() -> ControllerResult<()> {
        unsafe { Ps2Controller::init::<IO>() }
    }

    /// Set the keyboard LEDs.
    ///
    /// `led_mask` is a combination of `LED_SCROLL_LOCK`, `LED_NUM_LOCK`,
    /// and `LED_CAPS_LOCK`.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn set_leds<IO: IoBackend>(led_mask: u8) -> ControllerResult<()> {
        unsafe { Ps2Controller::set_leds::<IO>(led_mask) }
    }

    /// Enable or disable Colemak layout mapping.
    pub fn set_colemak(&mut self, enabled: bool) {
        self.keyboard.colemak_enabled = enabled;
    }

    /// Handle a single interrupt by reading from the PS/2 controller.
    ///
    /// Returns `true` if data was available and processed.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.  Should be called from the interrupt handler.
    pub unsafe fn intr_handler<IO: IoBackend, C: InputCallbacks>(
        &mut self,
        callbacks: &mut C,
    ) -> bool {
        let Some((data, is_aux)) = (unsafe { Ps2Controller::scan_keyboard::<IO>() }) else {
            return false;
        };

        if is_aux {
            self.process_mouse_byte::<C>(data, callbacks);
        } else {
            self.process_kbd_byte::<C>(data, callbacks);
        }

        true
    }

    /// Process a keyboard scancode byte through the state machine.
    fn process_kbd_byte<C: InputCallbacks>(&mut self, scode: u8, callbacks: &mut C) {
        match self.keyboard.process(scode) {
            KbdEvent::Event(evt) => {
                callbacks.key_event(evt.page, evt.code, evt.press);
            }
            KbdEvent::Prefix | KbdEvent::Consumed | KbdEvent::Unmapped => {}
        }
    }

    /// Process a mouse data byte through the packet parser.
    fn process_mouse_byte<C: InputCallbacks>(&mut self, byte: u8, callbacks: &mut C) {
        match self.mouse.feed(byte) {
            MouseEvent::Packet(state) => {
                self.dispatch_mouse_state::<C>(&state, callbacks);
            }
            MouseEvent::Resync | MouseEvent::NeedMore => {}
        }
    }

    /// Dispatch HID events for a decoded mouse state.
    fn dispatch_mouse_state<C: InputCallbacks>(&mut self, state: &MouseState, callbacks: &mut C) {
        // Button states indexed: 0=left, 1=right, 2=middle
        let cur_buttons = [state.left as u8, state.right as u8, state.middle as u8];

        for (i, &cur_bit) in cur_buttons.iter().enumerate() {
            let mask = 1u8 << i;
            let prev_bit = (self.prev_mouse_buttons >> i) & 1;

            if cur_bit != prev_bit {
                self.prev_mouse_buttons ^= mask;
                callbacks.mouse_event(
                    INPUT_PAGE_BUTTON,
                    INPUT_BUTTON_1 + i as u16,
                    cur_bit as i32,
                    INPUT_FLAG_ABS,
                );
            }
        }

        // X movement
        if state.delta_x != 0 {
            callbacks.mouse_event(INPUT_PAGE_GD, INPUT_GD_X, state.delta_x, INPUT_FLAG_REL);
        }

        // Y movement
        if state.delta_y != 0 {
            callbacks.mouse_event(INPUT_PAGE_GD, INPUT_GD_Y, state.delta_y, INPUT_FLAG_REL);
        }
    }
}

impl Default for InputDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCallbacks {
        key_events: core::cell::Cell<u32>,
        mouse_events: core::cell::Cell<u32>,
        last_page: core::cell::Cell<u16>,
        last_code: core::cell::Cell<u16>,
        last_value: core::cell::Cell<i32>,
    }

    impl TestCallbacks {
        const fn new() -> Self {
            TestCallbacks {
                key_events: core::cell::Cell::new(0),
                mouse_events: core::cell::Cell::new(0),
                last_page: core::cell::Cell::new(0),
                last_code: core::cell::Cell::new(0),
                last_value: core::cell::Cell::new(0),
            }
        }
    }

    impl InputCallbacks for TestCallbacks {
        fn key_event(&mut self, page: u16, code: u16, press: i32) {
            self.key_events.set(self.key_events.get() + 1);
            self.last_page.set(page);
            self.last_code.set(code);
            self.last_value.set(press);
        }

        fn mouse_event(&mut self, page: u16, code: u16, value: i32, _flags: u16) {
            self.mouse_events.set(self.mouse_events.get() + 1);
            self.last_page.set(page);
            self.last_code.set(code);
            self.last_value.set(value);
        }
    }

    #[test]
    fn test_driver_process_key_press() {
        let mut driver = InputDriver::new();
        let mut cb = TestCallbacks::new();

        // Scancode 0x1E = 'A' press
        driver.process_kbd_byte::<TestCallbacks>(0x1E, &mut cb);
        assert_eq!(cb.key_events.get(), 1);
        assert_eq!(cb.last_page.get(), INPUT_PAGE_KEY);
        assert_eq!(cb.last_code.get(), INPUT_KEY_A);
        assert_eq!(cb.last_value.get(), INPUT_PRESS);
    }

    #[test]
    fn test_driver_process_key_release() {
        let mut driver = InputDriver::new();
        let mut cb = TestCallbacks::new();

        driver.process_kbd_byte::<TestCallbacks>(0x9E, &mut cb);
        assert_eq!(cb.key_events.get(), 1);
        assert_eq!(cb.last_code.get(), INPUT_KEY_A);
        assert_eq!(cb.last_value.get(), INPUT_RELEASE);
    }

    #[test]
    fn test_driver_process_ext0_key() {
        let mut driver = InputDriver::new();
        let mut cb = TestCallbacks::new();

        driver.process_kbd_byte::<TestCallbacks>(SCAN_EXT0, &mut cb);
        assert_eq!(cb.key_events.get(), 0); // prefix, no event

        driver.process_kbd_byte::<TestCallbacks>(0x48, &mut cb);
        assert_eq!(cb.key_events.get(), 1);
        assert_eq!(cb.last_code.get(), INPUT_KEY_UP_ARROW);
    }

    #[test]
    fn test_driver_mouse_byte_accumulation() {
        let mut driver = InputDriver::new();
        let mut cb = TestCallbacks::new();

        // Feed three bytes of a valid mouse packet (no movement, no buttons)
        driver.process_mouse_byte::<TestCallbacks>(0x08, &mut cb);
        driver.process_mouse_byte::<TestCallbacks>(0x00, &mut cb);
        driver.process_mouse_byte::<TestCallbacks>(0x00, &mut cb);
        // No events expected (no changes from default state)
        assert_eq!(cb.mouse_events.get(), 0);
    }

    #[test]
    fn test_driver_mouse_button_event() {
        let mut driver = InputDriver::new();
        let mut cb = TestCallbacks::new();

        // Left button pressed
        driver.process_mouse_byte::<TestCallbacks>(0x09, &mut cb);
        driver.process_mouse_byte::<TestCallbacks>(0x00, &mut cb);
        driver.process_mouse_byte::<TestCallbacks>(0x00, &mut cb);

        // Should have 1 button event
        assert_eq!(cb.mouse_events.get(), 1);
        assert_eq!(cb.last_page.get(), INPUT_PAGE_BUTTON);
        assert_eq!(cb.last_code.get(), INPUT_BUTTON_1);
        assert_eq!(cb.last_value.get(), 1);
    }

    #[test]
    fn test_driver_mouse_movement_event() {
        let mut driver = InputDriver::new();
        let mut cb = TestCallbacks::new();

        // X movement +10
        driver.process_mouse_byte::<TestCallbacks>(0x08, &mut cb);
        driver.process_mouse_byte::<TestCallbacks>(10, &mut cb);
        driver.process_mouse_byte::<TestCallbacks>(0x00, &mut cb);

        // Should have 1 movement event
        assert_eq!(cb.mouse_events.get(), 1);
        assert_eq!(cb.last_page.get(), INPUT_PAGE_GD);
        assert_eq!(cb.last_code.get(), INPUT_GD_X);
        assert_eq!(cb.last_value.get(), 10);
    }

    #[test]
    fn test_driver_colemak() {
        let mut driver = InputDriver::new();
        driver.set_colemak(true);
        assert!(driver.keyboard.colemak_enabled);

        let mut cb = TestCallbacks::new();

        // E scancode 0x12 → Colemak: F
        driver.process_kbd_byte::<TestCallbacks>(0x12, &mut cb);
        assert_eq!(cb.last_code.get(), INPUT_KEY_F);
    }

    #[test]
    fn test_driver_new() {
        let driver = InputDriver::new();
        assert!(!driver.keyboard.colemak_enabled);
        assert_eq!(driver.keyboard.state, 0);
        assert_eq!(driver.prev_mouse_buttons, 0);
    }

    #[test]
    fn test_null_callbacks_no_panic() {
        let mut driver = InputDriver::new();
        let mut null = NullCallbacks;

        // Should not panic
        driver.process_kbd_byte::<NullCallbacks>(0x1E, &mut null);
        driver.process_kbd_byte::<NullCallbacks>(0x9E, &mut null);
    }
}
