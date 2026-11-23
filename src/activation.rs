//! Bring our window to the front when another app asks us to show something.
//!
//! `FileManager1` calls carry a `startup_id`. On Wayland that is an
//! xdg-activation token the caller got from the compositor (it proves the
//! request follows a click in the caller). We hand it back to the compositor
//! with `xdg_activation_v1.activate`, which is what lets niri focus our window
//! instead of only marking it "urgent".
//!
//! Like `foreign.rs` and `dragout.rs` it uses gpui's own `wl_display`, with a
//! private event queue on a short-lived thread. Any failure is only logged;
//! the plain `window.activate_window()` call stays as the fallback.

use gpui::Window;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry::WlRegistry, wl_surface::WlSurface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::XdgActivationTokenV1, xdg_activation_v1::XdgActivationV1,
};

/// Ask the compositor to focus `window` using `token`. Does nothing for an
/// empty token (or a non-Wayland session) and never blocks the caller.
pub fn activate(window: &Window, token: &str) {
    let token = token.trim();
    if token.is_empty() {
        return;
    }
    // `Window` has its own `window_handle()` (a gpui handle), so name the trait
    let Ok(display_handle) = HasDisplayHandle::display_handle(window) else { return };
    let Ok(window_handle) = HasWindowHandle::window_handle(window) else { return };
    let RawDisplayHandle::Wayland(d) = display_handle.as_raw() else { return };
    let RawWindowHandle::Wayland(s) = window_handle.as_raw() else { return };
    // raw pointers aren't Send; hand them to the thread as integers
    let (display, surface) = (d.display.as_ptr() as usize, s.surface.as_ptr() as usize);
    let token = token.to_string();

    let spawned = std::thread::Builder::new()
        .name("activation".into())
        .spawn(move || {
            if let Err(e) = run(display, surface, token) {
                eprintln!("activation: {e}");
            }
        });
    if let Err(e) = spawned {
        eprintln!("activation: could not start thread: {e}");
    }
}

struct State;

fn run(display: usize, surface: usize, token: String) -> Result<(), String> {
    let backend = unsafe { Backend::from_foreign_display(display as *mut _) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let activation: XdgActivationV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("the compositor has no xdg-activation: {e}"))?;

    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface as *mut _) }
        .map_err(|e| e.to_string())?;
    let surface = WlSurface::from_id(&conn, id).map_err(|e| e.to_string())?;

    activation.activate(token, &surface);
    conn.flush().map_err(|e| e.to_string())?;
    queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    activation.destroy();
    conn.flush().map_err(|e| e.to_string())
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgActivationV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &XdgActivationV1,
        _: <XdgActivationV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// we never create a token ourselves, but the generated code wants the type known
impl Dispatch<XdgActivationTokenV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &XdgActivationTokenV1,
        _: <XdgActivationTokenV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
