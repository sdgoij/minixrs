//! Phase 4c's gate: wayland-rs's *server runtime* serving a stock wayland-rs client.
//!
//! The server half is a real `Display` with a global (`wl_compositor`) and the `Dispatch` impls
//! that receive what a client asks of it; the client half is `wayland-client`: it connects over
//! the port's `/dev/uds` sockets, reads the registry, binds the global, creates a surface,
//! commits it, and roundtrips. Passing means wayland-server's backend ran on this port —
//! socket I/O, the registry handshake, request dispatch, and the `wl_callback.done` reply that
//! `roundtrip` blocks on.
//!
//! One process, two roles: the listener is bound before the fork so the client cannot connect
//! before it exists, and the parent waits for the child's exit status as well as its own result.
// The C ABI symbols rustix's `libc` backend names (epoll, poll, sendmsg/recvmsg, send) come
// from the port's static C library; rustc only links an rlib the crate references.
use minix_libc as _;

use std::sync::Arc;
use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_compositor as client_compositor, wl_registry, wl_surface as client_surface,
};
use wayland_client::{Connection, Dispatch as ClientDispatch, QueueHandle};

use wayland_server::protocol::{wl_compositor, wl_surface};
use wayland_server::{
    Client, DataInit, Dispatch as ServerDispatch, Display, DisplayHandle, GlobalDispatch, New,
};

/// The name `/dev/uds` knows the socket by (`crates/servers/src/wlserver.rs` uses `/run/wayland-0`).
const SOCKET: &str = "/run/wlgate";
const SERVER_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Server {
    binds: u32,
    surfaces: u32,
    commits: u32,
}

impl GlobalDispatch<wl_compositor::WlCompositor, ()> for Server {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<wl_compositor::WlCompositor>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        state.binds += 1;
        data_init.init(resource, ());
    }
}

impl ServerDispatch<wl_compositor::WlCompositor, ()> for Server {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                state.surfaces += 1;
                println!("wlgate: server create_surface");
                data_init.init(id, ());
            }
            _ => {}
        }
    }
}

impl ServerDispatch<wl_surface::WlSurface, ()> for Server {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &wl_surface::WlSurface,
        request: wl_surface::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_surface::Request::Commit = request {
            state.commits += 1;
            println!("wlgate: server commit");
        }
    }
}

#[derive(Default)]
struct ClientState;

impl ClientDispatch<wl_registry::WlRegistry, GlobalListContents> for ClientState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _contents: &GlobalListContents,
        _conn: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            println!("wlgate: client sees global {name} {interface}");
        }
    }
}

impl ClientDispatch<client_compositor::WlCompositor, ()> for ClientState {
    fn event(
        _state: &mut Self,
        _compositor: &client_compositor::WlCompositor,
        _event: client_compositor::Event,
        _data: &(),
        _conn: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
    }
}

impl ClientDispatch<client_surface::WlSurface, ()> for ClientState {
    fn event(
        _state: &mut Self,
        _surface: &client_surface::WlSurface,
        _event: client_surface::Event,
        _data: &(),
        _conn: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
    }
}

/// The server half: one client, one global, dispatch until the client's requests arrive.
fn server(listener: std::os::minix::net::UnixListener) -> ! {
    // `accept` answers `EAGAIN` until a connection is queued (`.notes/wayland.md` 6.1).
    let stream = loop {
        match listener.accept() {
            Ok((stream, _addr)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                println!("wlgate: server accept failed: {e}");
                minix_rt::exit(1);
            }
        }
    };
    println!("wlgate: server accepted a client");

    let mut display = Display::<Server>::new().expect("wlgate: display");
    display
        .handle()
        .create_global::<Server, wl_compositor::WlCompositor, ()>(4, ());
    if let Err(e) = display.handle().insert_client(stream, Arc::new(())) {
        println!("wlgate: server insert_client failed: {e}");
        minix_rt::exit(1);
    }

    let mut state = Server::default();
    let deadline = Instant::now() + SERVER_DEADLINE;
    while Instant::now() < deadline {
        match display.dispatch_clients(&mut state) {
            Ok(_) => {}
            Err(e) => {
                println!("wlgate: server dispatch stopped: {e}");
                break;
            }
        }
        let _ = display.flush_clients();
        if state.commits > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    println!(
        "wlgate: server binds={} surfaces={} commits={}",
        state.binds, state.surfaces, state.commits
    );
    minix_rt::exit(if state.surfaces > 0 && state.commits > 0 {
        0
    } else {
        1
    });
}

/// The client half: the registry handshake, a bind, a request and a roundtrip.
fn client() -> bool {
    let stream = match std::os::minix::net::UnixStream::connect(SOCKET) {
        Ok(stream) => stream,
        Err(e) => {
            println!("wlgate: client connect failed: {e}");
            return false;
        }
    };
    let conn = match Connection::from_socket(stream) {
        Ok(conn) => conn,
        Err(e) => {
            println!("wlgate: client connection failed: {e:?}");
            return false;
        }
    };
    let (globals, mut queue) = match registry_queue_init::<ClientState>(&conn) {
        Ok(pair) => pair,
        Err(e) => {
            println!("wlgate: client registry init failed: {e:?}");
            return false;
        }
    };
    let handle = queue.handle();
    let mut state = ClientState;

    let compositor: client_compositor::WlCompositor = match globals.bind(&handle, 4..=4, ()) {
        Ok(compositor) => compositor,
        Err(e) => {
            println!("wlgate: client could not bind wl_compositor: {e:?}");
            return false;
        }
    };
    let surface = compositor.create_surface(&handle, ());
    surface.commit();

    if let Err(e) = queue.roundtrip(&mut state) {
        println!("wlgate: client roundtrip failed: {e:?}");
        return false;
    }
    println!("wlgate: client bound wl_compositor, created a surface and committed it");
    true
}

fn main() {
    let listener = match std::os::minix::net::UnixListener::bind(SOCKET) {
        Ok(listener) => listener,
        Err(e) => {
            println!("wlgate: bind {SOCKET} failed: {e}");
            std::process::exit(1);
        }
    };

    let child = minix_rt::fork();
    if child == 0 {
        server(listener);
    }
    if child < 0 {
        println!("wlgate: fork failed");
        std::process::exit(1);
    }

    let client_ok = client();
    let server_status = minix_rt::waitpid_status(child);
    let server_ok = server_status == 0;
    println!("wlgate: client={client_ok} server_exit={server_status}");
    if client_ok && server_ok {
        println!("wlgate: PASS wayland-server served a stock client");
    } else {
        println!("wlgate: FAIL client={client_ok} server_exit={server_status}");
        std::process::exit(1);
    }
}
