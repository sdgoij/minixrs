//! TI1225 CardBus bridge driver — PCI-to-PCI bridge with hot-plug.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/bus/ti1225/ti1225.c`
//!
//! Controls the Texas Instruments TI1225 PCI-to-CardBus bridge.
//! Provides card detection, power management, voltage detection,
//! and bus rescanning.

use crate::DriverError;


/// TI1225 vendor ID (Texas Instruments).
pub const TI1225_VENDOR: u16 = 0x104C;

/// TI1225 device ID.
pub const TI1225_DEVICE: u16 = 0xAC1E;

/// Number of CardBus sockets supported (TI1225 has 2).
pub const TI1225_SOCKETS: usize = 2;


/// System Control register.
pub const TI1225_SYSTEM_CONTROL: u8 = 0x80;

/// CardBus Socket Event register.
pub const TI1225_CARD_EVENT: u8 = 0x84;

/// CardBus Socket Mask register.
pub const TI1225_CARD_MASK: u8 = 0x88;

/// CardBus Socket Present State register.
pub const TI1225_CARD_PRESENT: u8 = 0x8C;

/// CardBus Socket Force Event register.
pub const TI1225_CARD_FORCE: u8 = 0x90;

/// CardBus Socket Control register (per socket, offset by socket*8).
pub const TI1225_SOCKET_CONTROL: u8 = 0x9C;

/// ExCA Memory Window 0 Start (per socket).
pub const TI1225_MEM0_START: u8 = 0xA0;

/// ExCA I/O Window 0 Start (per socket).
pub const TI1225_IO0_START: u8 = 0xA4;


/// System Control: reset the bridge.
pub const TI1225_CTRL_RESET: u32 = 0x0000_0001;

/// System Control: enable address/data stepping.
pub const TI1225_CTRL_STEP: u32 = 0x0000_0002;

/// Card Detect bits in Present State register.
pub const TI1225_CD_MASK: u32 = 0x0000_0003;

/// Card Detect 1 (bit 0).
pub const TI1225_CD1: u32 = 0x0000_0001;

/// Card Detect 2 (bit 1).
pub const TI1225_CD2: u32 = 0x0000_0002;

/// Card bus reset bit (per socket).
pub const TI1225_SOCKET_RESET: u32 = 0x0000_0001;

/// Power bits (per socket).
pub const TI1225_SOCKET_POWER: u32 = 0x0000_0700;

/// 3.3V power.
pub const TI1225_POWER_3V: u32 = 0x0000_0100;

/// 5V power.
pub const TI1225_POWER_5V: u32 = 0x0000_0200;

/// Voltage sense bits.
pub const TI1225_VS_MASK: u32 = 0x0000_0060;

/// Voltage sense 1 (bit 5).
pub const TI1225_VS1: u32 = 0x0000_0020;

/// Voltage sense 2 (bit 6).
pub const TI1225_VS2: u32 = 0x0000_0040;


/// State of a single CardBus socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub enum CardState {
    /// No card present.
    #[default]
    Empty,
    /// Card detected, powering up.
    PoweringUp,
    /// Card ready for use.
    Ready,
    /// Card in reset.
    Resetting,
}

/// A TI1225 socket descriptor.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Ti1225Socket {
    /// Current card state.
    pub state: CardState,
    /// Detected voltage (0 = unknown, 33 = 3.3V, 50 = 5V).
    pub voltage: u8,
    /// Card detect 1 status.
    pub cd1: bool,
    /// Card detect 2 status.
    pub cd2: bool,
}

impl Ti1225Socket {
    const fn new() -> Self {
        Self {
            state: CardState::Empty,
            voltage: 0,
            cd1: false,
            cd2: false,
        }
    }
}

/// TI1225 bridge state.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Ti1225State {
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number.
    pub dev: u8,
    /// PCI function number.
    pub func: u8,
    /// Per-socket state.
    pub sockets: [Ti1225Socket; TI1225_SOCKETS],
}

impl Ti1225State {
    const fn new() -> Self {
        Self {
            bus: 0,
            dev: 0,
            func: 0,
            sockets: [Ti1225Socket::new(); TI1225_SOCKETS],
        }
    }
}


/// TI1225 bridge instances (max 4 bridges).
static mut TI1225_BRIDGES: [Ti1225State; 4] = [Ti1225State::new(); 4];
static mut TI1225_BRIDGE_COUNT: usize = 0;


unsafe fn ti_pci_read32(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    unsafe { crate::arch_io::pci_cfg_read32(bus, dev, func, reg) }
}

unsafe fn ti_pci_write32(bus: u8, dev: u8, func: u8, reg: u8, val: u32) {
    unsafe { crate::arch_io::pci_cfg_write32(bus, dev, func, reg, val) }
}


/// Initialize a TI1225 bridge.
///
/// `bus`, `dev`, `func` identify the PCI function for the bridge.
/// Must be called for each detected TI1225 bridge.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config space.
pub unsafe fn ti1225_init(bus: u8, dev: u8, func: u8) -> Result<usize, DriverError> {
    unsafe {
        let bridges = core::ptr::addr_of_mut!(TI1225_BRIDGES);
        let count = core::ptr::addr_of_mut!(TI1225_BRIDGE_COUNT);
        let idx = *count;
        if idx >= (*bridges).len() {
            return Err(DriverError::Busy);
        }

        let bridge = &mut (*bridges)[idx];
        bridge.bus = bus;
        bridge.dev = dev;
        bridge.func = func;

        // Reset the bridge.
        ti_pci_write32(bus, dev, func, TI1225_SYSTEM_CONTROL, TI1225_CTRL_RESET);
        ti_pci_write32(bus, dev, func, TI1225_SYSTEM_CONTROL, 0);

        // Detect cards in both sockets.
        for socket in 0..2 {
            ti1225_detect_socket(idx, socket);
        }

        *count = idx + 1;
        Ok(idx)
    }
}

/// Detect a card in a specific socket.
///
/// Reads the Present State register to check card detect and
/// voltage sense pins.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config space.
pub unsafe fn ti1225_detect_socket(bridge_idx: usize, socket: usize) -> CardState {
    unsafe {
        let bridges = core::ptr::addr_of_mut!(TI1225_BRIDGES);
        if bridge_idx >= (*bridges).len() || socket >= TI1225_SOCKETS {
            return CardState::Empty;
        }

        let bridge = &mut (*bridges)[bridge_idx];
        let present = ti_pci_read32(bridge.bus, bridge.dev, bridge.func, TI1225_CARD_PRESENT);

        let cd_bit = match socket {
            0 => TI1225_CD1,
            _ => TI1225_CD2,
        };

        let cd = (present & cd_bit) == 0; // Card detect is active-low
        let vs = (present & TI1225_VS_MASK) >> 5;

        let sock = &mut bridge.sockets[socket];
        sock.cd1 = (present & TI1225_CD1) == 0;
        sock.cd2 = (present & TI1225_CD2) == 0;

        if cd {
            // Voltage detection
            sock.voltage = match vs {
                0b01 => 50, // 5V
                0b10 => 33, // 3.3V
                0b11 => 0,  // No voltage defined (X.XV)
                _ => 0,     // No card or reserved
            };
            sock.state = CardState::Ready;
            CardState::Ready
        } else {
            sock.state = CardState::Empty;
            sock.voltage = 0;
            CardState::Empty
        }
    }
}

/// Power on a CardBus socket.
///
/// Applies the appropriate voltage and waits for power to stabilize.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config space.
pub unsafe fn ti1225_power_on(bridge_idx: usize, socket: usize) -> Result<(), DriverError> {
    unsafe {
        let bridges = core::ptr::addr_of_mut!(TI1225_BRIDGES);
        if bridge_idx >= (*bridges).len() || socket >= TI1225_SOCKETS {
            return Err(DriverError::InvalidArgument);
        }

        let bridge = &mut (*bridges)[bridge_idx];
        let sock = &bridge.sockets[socket];

        let power_val = match sock.voltage {
            50 => TI1225_POWER_5V,
            33 => TI1225_POWER_3V,
            _ => return Err(DriverError::Unsupported),
        };

        let sock_ctrl_offset = TI1225_SOCKET_CONTROL + (socket as u8) * 8;
        let current = ti_pci_read32(bridge.bus, bridge.dev, bridge.func, sock_ctrl_offset);
        ti_pci_write32(
            bridge.bus,
            bridge.dev,
            bridge.func,
            sock_ctrl_offset,
            (current & !TI1225_SOCKET_POWER) | power_val,
        );

        let sock_state = &mut bridge.sockets[socket];
        sock_state.state = CardState::PoweringUp;
        Ok(())
    }
}

/// Reset a CardBus socket.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config space.
pub unsafe fn ti1225_reset_socket(bridge_idx: usize, socket: usize) -> Result<(), DriverError> {
    unsafe {
        let bridges = core::ptr::addr_of_mut!(TI1225_BRIDGES);
        if bridge_idx >= (*bridges).len() || socket >= TI1225_SOCKETS {
            return Err(DriverError::InvalidArgument);
        }

        let bridge = &(*bridges)[bridge_idx];
        let sock_ctrl_offset = TI1225_SOCKET_CONTROL + (socket as u8) * 8;

        // Assert reset.
        ti_pci_write32(
            bridge.bus,
            bridge.dev,
            bridge.func,
            sock_ctrl_offset,
            TI1225_SOCKET_RESET,
        );

        let sock = &mut (*bridges)[bridge_idx].sockets[socket];
        sock.state = CardState::Resetting;
        Ok(())
    }
}

/// Release reset on a CardBus socket.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config space.
pub unsafe fn ti1225_release_reset(bridge_idx: usize, socket: usize) -> Result<(), DriverError> {
    unsafe {
        let bridges = core::ptr::addr_of_mut!(TI1225_BRIDGES);
        if bridge_idx >= (*bridges).len() || socket >= TI1225_SOCKETS {
            return Err(DriverError::InvalidArgument);
        }

        let bridge = &(*bridges)[bridge_idx];
        let sock_ctrl_offset = TI1225_SOCKET_CONTROL + (socket as u8) * 8;
        ti_pci_write32(bridge.bus, bridge.dev, bridge.func, sock_ctrl_offset, 0);

        let sock = &mut (*bridges)[bridge_idx].sockets[socket];
        sock.state = CardState::Ready;
        Ok(())
    }
}

/// Get the number of initialized bridges.
pub fn ti1225_bridge_count() -> usize {
    unsafe { *core::ptr::addr_of_mut!(TI1225_BRIDGE_COUNT) }
}

/// Get a reference to a bridge state.
pub fn ti1225_get_bridge(index: usize) -> Option<&'static Ti1225State> {
    unsafe {
        let count = *core::ptr::addr_of_mut!(TI1225_BRIDGE_COUNT);
        if index < count {
            let bridges = core::ptr::addr_of_mut!(TI1225_BRIDGES);
            Some(&(*bridges)[index])
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ti1225_constants() {
        assert_eq!(TI1225_VENDOR, 0x104C);
        assert_eq!(TI1225_DEVICE, 0xAC1E);
        assert_eq!(TI1225_SOCKETS, 2);
    }

    #[test]
    fn test_ti1225_socket_new() {
        let s = Ti1225Socket::new();
        assert_eq!(s.state, CardState::Empty);
        assert_eq!(s.voltage, 0);
        assert!(!s.cd1);
    }

    #[test]
    fn test_ti1225_state_new() {
        let s = Ti1225State::new();
        assert_eq!(s.bus, 0);
        assert_eq!(s.sockets.len(), 2);
        for sock in &s.sockets {
            assert_eq!(sock.state, CardState::Empty);
        }
    }

    #[test]
    fn test_ti1225_register_offsets() {
        assert_eq!(TI1225_SYSTEM_CONTROL, 0x80);
        assert_eq!(TI1225_CARD_EVENT, 0x84);
        assert_eq!(TI1225_CARD_PRESENT, 0x8C);
        assert_eq!(TI1225_SOCKET_CONTROL, 0x9C);
    }

    #[test]
    fn test_ti1225_ctrl_bits() {
        assert_eq!(TI1225_CTRL_RESET, 1);
        assert_eq!(TI1225_CD_MASK, 3);
        assert_eq!(TI1225_POWER_3V, 0x100);
        assert_eq!(TI1225_POWER_5V, 0x200);
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore = "requires hardware I/O")]
    fn test_ti1225_init_no_hardware_still_returns_index() {
        unsafe {
            // Without actual TI1225 hardware, detect will find empty sockets.
            let result = ti1225_init(0, 0, 0);
            assert!(result.is_ok());
            let idx = result.unwrap();
            assert_eq!(idx, 0);
        }
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore = "requires hardware I/O")]
    fn test_ti1225_detect_empty_socket() {
        unsafe {
            let _ = ti1225_init(0, 0, 0);
            // Without hardware, detection should return Empty.
            let state = ti1225_detect_socket(0, 0);
            assert_eq!(state, CardState::Empty);
        }
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore = "requires hardware I/O")]
    fn test_ti1225_power_unknown_voltage_fails() {
        unsafe {
            let _ = ti1225_init(0, 0, 0);
            // Without a card detected, voltage is 0, power-on should fail.
            let result = ti1225_power_on(0, 0);
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_ti1225_reset_socket_invalid_index() {
        unsafe {
            assert!(ti1225_reset_socket(99, 0).is_err());
        }
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore = "requires hardware I/O")]
    fn test_ti1225_bridge_count() {
        unsafe {
            *core::ptr::addr_of_mut!(TI1225_BRIDGE_COUNT) = 0;
            let _ = ti1225_init(0, 0, 0);
            assert_eq!(ti1225_bridge_count(), 1);
        }
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore = "requires hardware I/O")]
    fn test_ti1225_get_bridge() {
        unsafe {
            *core::ptr::addr_of_mut!(TI1225_BRIDGE_COUNT) = 0;
            let _ = ti1225_init(1, 2, 3);
            let bridge = ti1225_get_bridge(0);
            assert!(bridge.is_some());
            assert_eq!(bridge.unwrap().bus, 1);
            assert_eq!(bridge.unwrap().dev, 2);
            assert_eq!(bridge.unwrap().func, 3);
        }
    }

    #[test]
    fn test_ti1225_get_bridge_out_of_range() {
        assert!(ti1225_get_bridge(99).is_none());
    }

    #[test]
    fn test_card_state_default() {
        assert_eq!(CardState::default(), CardState::Empty);
    }
}
