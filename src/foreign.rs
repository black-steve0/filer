//! Make a chooser window a child of the app that asked for it (Wayland).
//!
//! The portal gives us the caller's window as `wayland:HANDLE`, a handle the
//! app exported with xdg-foreign v2. We import it and ask the compositor to
//! make our chooser window a child of that toplevel (`set_parent_of`).
//! Compositors then keep the dialog above its parent and, on niri, float it
//! instead of tiling it.
//!
//! Like `dragout.rs`, this attaches to gpui's own `wl_display`, with a private
//! event queue on a small thread. The thread lives as long as the returned
//! `Parent`; dropping it destroys the import. Every failure is only logged: the
//! chooser then simply opens as an ordinary window.

use gpui::Window;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::{sync::mpsc, time::Duration};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry::WlRegistry, wl_surface::WlSurface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::xdg::foreign::zv2::client::{
    zxdg_imported_v2::{self, ZxdgImportedV2},
    zxdg_importer_v2::{self, ZxdgImporterV2},
};

/// Keeps the parent relation alive. Dropping it ends the worker thread.
pub struct Parent {
    _stop: mpsc::Sender<()>,
}

/// Parent `window` to the app window named by the portal's `parent_window`
/// string. `None` when there is nothing to do (empty, `x11:...`, not Wayland)
/// or it did not work.
pub fn attach(window: &Window, parent_window: &str) -> Option<Parent> {
    if std::env::var_os("FILER_NO_PARENT").is_some() {
        return None; // escape hatch: choosers open as plain windows
    }
    let handle = parent_window.strip_prefix("wayland:")?.to_string();
    if handle.is_empty() {
        return None;
    }
    // `Window` has its own `window_handle()` (a gpui handle), so name the trait
    let display_handle = HasDisplayHandle::display_handle(window).ok()?;
    let window_handle = HasWindowHandle::window_handle(window).ok()?;
    let RawDisplayHandle::Wayland(d) = display_handle.as_raw() else { return None };
    let RawWindowHandle::Wayland(s) = window_handle.as_raw() else { return None };
    // raw pointers aren't Send; hand them to the worker as integers
    let (display, surface) = (d.display.as_ptr() as usize, s.surface.as_ptr() as usize);

    let (stop, stopped) = mpsc::channel::<()>();
    let (done, result) = mpsc::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new()
        .name("foreign".into())
        .spawn(move || worker(display, surface, handle, stopped, done));
    if let Err(e) = spawned {
        eprintln!("foreign: could not start thread: {e}");
        return None;
    }
    match result.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Some(Parent { _stop: stop }),
        Ok(Err(e)) => {
            eprintln!("foreign: chooser not attached to the app window: {e}");
            None
        }
        Err(_) => {
            eprintln!("foreign: timed out talking to the compositor");
            None
        }
    }
}

struct State {
    /// The compositor refused or dropped the imported handle.
    gone: bool,
}

fn worker(
    display: usize,
    surface: usize,
    handle: String,
    stopped: mpsc::Receiver<()>,
    done: mpsc::Sender<Result<(), String>>,
) {
    let mut state = State { gone: false };
    let set_up = (|| -> Result<_, String> {
        let backend = unsafe { Backend::from_foreign_display(display as *mut _) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let importer: ZxdgImporterV2 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| format!("the compositor has no xdg-foreign v2: {e}"))?;
        let imported = importer.import_toplevel(handle, &qh, ());

        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface as *mut _) }
            .map_err(|e| e.to_string())?;
        let surface = WlSurface::from_id(&conn, id).map_err(|e| e.to_string())?;

        // a bad handle is answered with `destroyed` right away
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        if state.gone {
            return Err("the app's window handle is not valid (any more)".into());
        }
        imported.set_parent_of(&surface);
        conn.flush().map_err(|e| e.to_string())?;
        Ok((conn, queue, importer, imported))
    })();

    let (conn, mut queue, importer, imported) = match set_up {
        Ok(v) => {
            let _ = done.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = done.send(Err(e));
            return;
        }
    };

    // gpui reads the socket; keep our queue drained until the chooser closes
    while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(Duration::from_millis(200)) {
        if queue.dispatch_pending(&mut state).is_err() {
            break;
        }
    }
    imported.destroy();
    importer.destroy();
    let _ = conn.flush();
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

impl Dispatch<ZxdgImporterV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZxdgImporterV2,
        _: zxdg_importer_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgImportedV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgImportedV2,
        event: zxdg_imported_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_imported_v2::Event::Destroyed = event {
            state.gone = true;
        }
    }
}
