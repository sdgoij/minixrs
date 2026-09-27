//! The core Wayland interface tables.
//!
//! One entry per interface: its name, the version this port speaks, and the
//! signature of each request and event by opcode. The signatures are the
//! published ones (`wayland.xml`), so a decoded message means the same thing to
//! a stock client as to ours. Written by hand; `tests` pins the shapes that the
//! server and client depend on.

use crate::wire::Arg;

/// One protocol interface.
pub struct Interface {
    pub name: &'static str,
    pub version: u32,
    /// Signatures by request opcode.
    pub requests: &'static [&'static str],
    /// Signatures by event opcode.
    pub events: &'static [&'static str],
}

impl Interface {
    /// The signature of request `opcode`, if the interface has one there.
    pub fn request(&self, opcode: u16) -> Option<&'static str> {
        self.requests.get(opcode as usize).copied()
    }

    /// The signature of event `opcode`, if the interface has one there.
    pub fn event(&self, opcode: u16) -> Option<&'static str> {
        self.events.get(opcode as usize).copied()
    }
}

/// `wl_display`: `sync`, `get_registry`; events `error`, `delete_id`.
pub static DISPLAY: Interface = Interface {
    name: "wl_display",
    version: 1,
    requests: &["n", "n"],
    events: &["ous", "u"],
};

/// `wl_registry`: request `bind`; events `global`, `global_remove`.
pub static REGISTRY: Interface = Interface {
    name: "wl_registry",
    version: 1,
    requests: &["usun"],
    events: &["usu", "u"],
};

/// `wl_callback`: event `done`.
pub static CALLBACK: Interface = Interface {
    name: "wl_callback",
    version: 1,
    requests: &[],
    events: &["u"],
};

/// `wl_compositor`: `create_surface`, `create_region`.
pub static COMPOSITOR: Interface = Interface {
    name: "wl_compositor",
    version: 1,
    requests: &["n", "n"],
    events: &[],
};

/// `wl_region`: `destroy`, `add`, `subtract`.
pub static REGION: Interface = Interface {
    name: "wl_region",
    version: 1,
    requests: &["", "iiii", "iiii"],
    events: &[],
};

/// `wl_surface`: `destroy`, `attach`, `damage`, `frame`, `set_opaque_region`,
/// `set_input_region`, `commit`; events `enter`, `leave`.
pub static SURFACE: Interface = Interface {
    name: "wl_surface",
    version: 1,
    requests: &["", "oii", "iiii", "n", "o", "o", ""],
    events: &["o", "o"],
};

/// `wl_shm`: request `create_pool`; event `format`.
pub static SHM: Interface = Interface {
    name: "wl_shm",
    version: 1,
    requests: &["nhi"],
    events: &["u"],
};

/// `wl_shm_pool`: `create_buffer`, `destroy`.
pub static SHM_POOL: Interface = Interface {
    name: "wl_shm_pool",
    version: 1,
    requests: &["niiiiu", ""],
    events: &[],
};

/// `wl_buffer`: request `destroy`; event `release`.
pub static BUFFER: Interface = Interface {
    name: "wl_buffer",
    version: 1,
    requests: &[""],
    events: &[""],
};

/// `wl_output`: events `geometry`, `mode`, `scale`, `done`.
pub static OUTPUT: Interface = Interface {
    name: "wl_output",
    version: 1,
    requests: &[],
    events: &["iiiiissi", "uiii", "i", ""],
};

/// `wl_seat`: `get_pointer`, `get_keyboard`, `get_touch`; events `capabilities`,
/// `name`.
pub static SEAT: Interface = Interface {
    name: "wl_seat",
    version: 1,
    requests: &["n", "n", "n"],
    events: &["u", "s"],
};

/// `wl_pointer`: `set_cursor`, `release`; the input events of 1c.
pub static POINTER: Interface = Interface {
    name: "wl_pointer",
    version: 1,
    requests: &["uoii", ""],
    events: &["uoff", "uo", "uff", "uuuu", "uuf", ""],
};

/// `wl_keyboard`: `release`; the input events of 1c.
pub static KEYBOARD: Interface = Interface {
    name: "wl_keyboard",
    version: 1,
    requests: &[""],
    events: &["uhu", "uoa", "uo", "uuuu", "uuuuu", "ii"],
};

/// Every interface this crate knows, by protocol name.
pub static ALL: &[&Interface] = &[
    &DISPLAY,
    &REGISTRY,
    &CALLBACK,
    &COMPOSITOR,
    &REGION,
    &SURFACE,
    &SHM,
    &SHM_POOL,
    &BUFFER,
    &OUTPUT,
    &SEAT,
    &POINTER,
    &KEYBOARD,
];

/// The interface with this protocol name, or `None`.
pub fn by_name(name: &[u8]) -> Option<&'static Interface> {
    ALL.iter().copied().find(|i| i.name.as_bytes() == name)
}

/// A global the Phase 1 server advertises over `wl_registry`.
pub struct Global {
    /// The registry name the client binds with.
    pub name: u32,
    pub interface: &'static str,
    pub version: u32,
}

/// Which interface an object speaks. Both sides track this: the server to
/// dispatch a request, the client to decode an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Display,
    Registry,
    Callback,
    Compositor,
    Region,
    Surface,
    Shm,
    ShmPool,
    Buffer,
    Output,
    Seat,
    Pointer,
    Keyboard,
}

impl Kind {
    /// The protocol interface this kind speaks.
    pub fn interface(self) -> &'static Interface {
        match self {
            Kind::Display => &DISPLAY,
            Kind::Registry => &REGISTRY,
            Kind::Callback => &CALLBACK,
            Kind::Compositor => &COMPOSITOR,
            Kind::Region => &REGION,
            Kind::Surface => &SURFACE,
            Kind::Shm => &SHM,
            Kind::ShmPool => &SHM_POOL,
            Kind::Buffer => &BUFFER,
            Kind::Output => &OUTPUT,
            Kind::Seat => &SEAT,
            Kind::Pointer => &POINTER,
            Kind::Keyboard => &KEYBOARD,
        }
    }

    /// The kind a protocol interface name denotes, for `wl_registry.bind`.
    pub fn from_name(name: &[u8]) -> Option<Kind> {
        Some(match name {
            b"wl_compositor" => Kind::Compositor,
            b"wl_region" => Kind::Region,
            b"wl_surface" => Kind::Surface,
            b"wl_shm" => Kind::Shm,
            b"wl_shm_pool" => Kind::ShmPool,
            b"wl_buffer" => Kind::Buffer,
            b"wl_output" => Kind::Output,
            b"wl_seat" => Kind::Seat,
            b"wl_pointer" => Kind::Pointer,
            b"wl_keyboard" => Kind::Keyboard,
            _ => return None,
        })
    }
}

/// The globals `/sbin/wlserver` offers. `wl_compositor`, `wl_shm`, one
/// `wl_output` and one `wl_seat` — the factories a Phase 1 client binds.
pub static GLOBALS: &[Global] = &[
    Global {
        name: 1,
        interface: "wl_compositor",
        version: 1,
    },
    Global {
        name: 2,
        interface: "wl_shm",
        version: 1,
    },
    Global {
        name: 3,
        interface: "wl_output",
        version: 1,
    },
    Global {
        name: 4,
        interface: "wl_seat",
        version: 1,
    },
];

// Request opcodes.
pub mod display_req {
    pub const SYNC: u16 = 0;
    pub const GET_REGISTRY: u16 = 1;
}
pub mod registry_req {
    pub const BIND: u16 = 0;
}
pub mod compositor_req {
    pub const CREATE_SURFACE: u16 = 0;
    pub const CREATE_REGION: u16 = 1;
}
pub mod region_req {
    pub const DESTROY: u16 = 0;
    pub const ADD: u16 = 1;
    pub const SUBTRACT: u16 = 2;
}
pub mod surface_req {
    pub const DESTROY: u16 = 0;
    pub const ATTACH: u16 = 1;
    pub const DAMAGE: u16 = 2;
    pub const FRAME: u16 = 3;
    pub const SET_OPAQUE_REGION: u16 = 4;
    pub const SET_INPUT_REGION: u16 = 5;
    pub const COMMIT: u16 = 6;
}
pub mod shm_req {
    pub const CREATE_POOL: u16 = 0;
}
pub mod shm_pool_req {
    pub const CREATE_BUFFER: u16 = 0;
    pub const DESTROY: u16 = 1;
}
pub mod buffer_req {
    pub const DESTROY: u16 = 0;
}
pub mod seat_req {
    pub const GET_POINTER: u16 = 0;
    pub const GET_KEYBOARD: u16 = 1;
    pub const GET_TOUCH: u16 = 2;
}
pub mod pointer_req {
    pub const SET_CURSOR: u16 = 0;
    pub const RELEASE: u16 = 1;
}
pub mod keyboard_req {
    pub const RELEASE: u16 = 0;
}

// Event opcodes.
pub mod display_ev {
    pub const ERROR: u16 = 0;
    pub const DELETE_ID: u16 = 1;
}
pub mod registry_ev {
    pub const GLOBAL: u16 = 0;
    pub const GLOBAL_REMOVE: u16 = 1;
}
pub mod callback_ev {
    pub const DONE: u16 = 0;
}
pub mod surface_ev {
    pub const ENTER: u16 = 0;
    pub const LEAVE: u16 = 1;
}
pub mod shm_ev {
    pub const FORMAT: u16 = 0;
}
pub mod buffer_ev {
    pub const RELEASE: u16 = 0;
}
pub mod output_ev {
    pub const GEOMETRY: u16 = 0;
    pub const MODE: u16 = 1;
    pub const SCALE: u16 = 2;
    pub const DONE: u16 = 3;
}
pub mod seat_ev {
    pub const CAPABILITIES: u16 = 0;
    pub const NAME: u16 = 1;
}
pub mod pointer_ev {
    pub const ENTER: u16 = 0;
    pub const LEAVE: u16 = 1;
    pub const MOTION: u16 = 2;
    pub const BUTTON: u16 = 3;
    pub const AXIS: u16 = 4;
    pub const FRAME: u16 = 5;
}
pub mod keyboard_ev {
    pub const KEYMAP: u16 = 0;
    pub const ENTER: u16 = 1;
    pub const LEAVE: u16 = 2;
    pub const KEY: u16 = 3;
    pub const MODIFIERS: u16 = 4;
    pub const REPEAT_INFO: u16 = 5;
}

// Enums.
/// `wl_shm.format`.
pub const WL_SHM_FORMAT_ARGB8888: u32 = 0;
pub const WL_SHM_FORMAT_XRGB8888: u32 = 1;
/// `wl_shm.error`.
pub const WL_SHM_ERROR_INVALID_FORMAT: u32 = 0;
pub const WL_SHM_ERROR_INVALID_STRIDE: u32 = 1;
pub const WL_SHM_ERROR_INVALID_FD: u32 = 2;
/// `wl_seat.capability`.
pub const WL_SEAT_CAPABILITY_POINTER: u32 = 1;
pub const WL_SEAT_CAPABILITY_KEYBOARD: u32 = 2;
pub const WL_SEAT_CAPABILITY_TOUCH: u32 = 4;
/// `wl_output.subpixel`.
pub const WL_OUTPUT_SUBPIXEL_UNKNOWN: i32 = 0;
/// `wl_output.transform`.
pub const WL_OUTPUT_TRANSFORM_NORMAL: i32 = 0;
/// `wl_output.mode` flags.
pub const WL_OUTPUT_MODE_CURRENT: u32 = 1;
pub const WL_OUTPUT_MODE_PREFERRED: u32 = 2;
/// `wl_display.error`.
pub const WL_DISPLAY_ERROR_INVALID_OBJECT: u32 = 1;
pub const WL_DISPLAY_ERROR_INVALID_METHOD: u32 = 2;
pub const WL_DISPLAY_ERROR_NO_MEMORY: u32 = 3;

/// Decode a request's arguments, using the interface's own signature.
pub fn decode_request<'a>(
    iface: &Interface,
    opcode: u16,
    body: &'a [u8],
    out: &mut [Arg<'a>],
) -> Result<usize, crate::wire::WireError> {
    decode_sig(iface.request(opcode), body, out)
}

/// Decode an event's arguments, using the interface's own signature.
pub fn decode_event<'a>(
    iface: &Interface,
    opcode: u16,
    body: &'a [u8],
    out: &mut [Arg<'a>],
) -> Result<usize, crate::wire::WireError> {
    decode_sig(iface.event(opcode), body, out)
}

/// Decode `body` against a signature. Either side's decode ends the same way:
/// the signature must exist and the body must be consumed exactly.
fn decode_sig<'a>(
    sig: Option<&'static str>,
    body: &'a [u8],
    out: &mut [Arg<'a>],
) -> Result<usize, crate::wire::WireError> {
    let sig = sig.ok_or(crate::wire::WireError::BadSignature)?;
    let mut r = crate::wire::Reader::new(body);
    let n = crate::wire::decode(sig, &mut r, out)?;
    // The body must be consumed exactly; trailing bytes mean the signature and
    // the sender disagree.
    if !r.is_done() {
        return Err(crate::wire::WireError::ArgOverrun);
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve() {
        assert_eq!(
            by_name(b"wl_compositor").map(|i| i.name),
            Some("wl_compositor")
        );
        assert_eq!(by_name(b"wl_shm").map(|i| i.name), Some("wl_shm"));
        assert!(by_name(b"wl_nonexistent").is_none());
    }

    #[test]
    fn display_request_shapes() {
        assert_eq!(DISPLAY.request(display_req::SYNC), Some("n"));
        assert_eq!(DISPLAY.request(display_req::GET_REGISTRY), Some("n"));
        assert_eq!(DISPLAY.request(9), None);
        assert_eq!(DISPLAY.event(display_ev::ERROR), Some("ous"));
    }

    #[test]
    fn registry_bind_signature_arg_count() {
        // `usun` is four arguments; the server relies on this when it decodes a
        // bind to learn the interface name and the new object id.
        assert_eq!(REGISTRY.request(registry_req::BIND), Some("usun"));
        assert_eq!(REGISTRY.events.len(), 2);
    }

    #[test]
    fn shm_pool_create_buffer_shape() {
        assert_eq!(
            SHM_POOL.request(shm_pool_req::CREATE_BUFFER),
            Some("niiiiu")
        );
        assert_eq!(SHM.request(shm_req::CREATE_POOL), Some("nhi"));
    }

    #[test]
    fn surface_request_count_matches_opcodes() {
        assert_eq!(SURFACE.requests.len(), 7);
        assert_eq!(SURFACE.request(surface_req::COMMIT), Some(""));
        assert_eq!(SURFACE.request(surface_req::ATTACH), Some("oii"));
    }

    #[test]
    fn globals_are_the_four_factories() {
        let names: [&str; 4] = [
            GLOBALS[0].interface,
            GLOBALS[1].interface,
            GLOBALS[2].interface,
            GLOBALS[3].interface,
        ];
        assert_eq!(names, ["wl_compositor", "wl_shm", "wl_output", "wl_seat"]);
        // Every advertised interface must exist in the table.
        for g in GLOBALS {
            assert!(by_name(g.interface.as_bytes()).is_some(), "{}", g.interface);
        }
    }
}
