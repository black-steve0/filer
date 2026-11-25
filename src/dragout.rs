//! Native drag-out for Wayland (tested target: niri).
//!
//! gpui can receive drops but cannot start a drag that leaves the window.
//! So we do it ourselves, on the *same* Wayland connection gpui uses:
//!
//!  1. `init` attaches to gpui's `wl_display` (via raw-window-handle), opens a
//!     private event queue and binds our own `wl_seat` / `wl_pointer` /
//!     `wl_data_device_manager`. Our pointer exists only to learn the serial of
//!     the latest button press, which `start_drag` requires and gpui hides.
//!  2. When a row is dragged past the window edge, the Workspace calls `start`.
//!     We create a `wl_data_source` offering `text/uri-list` and call
//!     `wl_data_device.start_drag`. The compositor now owns the drag and the
//!     browser (or whatever is under the cursor) receives a normal file drop.
//!  3. A small thread dispatches our queue (gpui does the socket reading) so
//!     the compositor's `send` request gets answered with the file URIs.
//!
//! Everything lives on one worker thread so the event queue never crosses threads.

use gpui::Window;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::{
    io::Write,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::{DndAction, WlDataDeviceManager},
        wl_data_offer::{self, WlDataOffer},
        wl_data_source::{self, WlDataSource},
        wl_pointer::{self, WlPointer},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_surface::WlSurface,
    },
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};

/// How long a drag may stay "active" without the compositor finishing it.
const DRAG_TIMEOUT: Duration = Duration::from_secs(60);

struct Shared {
    /// Serial of the most recent pointer-button press on any of our surfaces.
    serial: Option<u32>,
    /// A drag we started is still running.
    active: bool,
    started: Instant,
    /// `text/uri-list` body written to whoever asks for the data.
    payload: String,
}

struct State {
    shared: Arc<Mutex<Shared>>,
    pointer: Option<WlPointer>,
}

struct Ctx {
    conn: Connection,
    qh: QueueHandle<State>,
    shared: Arc<Mutex<Shared>>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
    surface: WlSurface,
}

static CTX: OnceLock<Result<Ctx, String>> = OnceLock::new();
static DISABLED: AtomicBool = AtomicBool::new(false);

/// Set up native drag-out. Call once, right after the window is created.
/// Failure is not fatal: `available()` just stays false.
pub fn init(window: &Window) {
    if CTX.get().is_some() {
        return;
    }
    let _ = CTX.set(setup(window));
    if let Some(Err(e)) = CTX.get() {
        eprintln!("drag-out disabled: {e}");
    }
}

/// True when `start` can be tried.
pub fn available() -> bool {
    !DISABLED.load(Ordering::Relaxed) && matches!(CTX.get(), Some(Ok(_)))
}

fn setup(window: &Window) -> Result<Ctx, String> {
    let RawDisplayHandle::Wayland(d) = HasDisplayHandle::display_handle(window).map_err(|e| e.to_string())?.as_raw()
    else {
        return Err("not running on Wayland".into());
    };
    let RawWindowHandle::Wayland(s) = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?.as_raw()
    else {
        return Err("not running on Wayland".into());
    };
    // raw pointers aren't Send; hand them to the worker as integers
    let display = d.display.as_ptr() as usize;
    let surface = s.surface.as_ptr() as usize;

    let (tx, rx) = mpsc::channel::<Result<Ctx, String>>();
    std::thread::Builder::new()
        .name("dragout".into())
        .spawn(move || worker(display, surface, tx))
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(3))
        .map_err(|_| "timed out talking to the compositor".to_string())?
}

/// Owns the event queue for the life of the app.
fn worker(display: usize, surface: usize, tx: mpsc::Sender<Result<Ctx, String>>) {
    let shared = Arc::new(Mutex::new(Shared {
        serial: None,
        active: false,
        started: Instant::now(),
        payload: String::new(),
    }));
    let mut state = State { shared: shared.clone(), pointer: None };

    let built = (|| -> Result<_, String> {
        // attach to gpui's connection instead of opening a second one
        let backend = unsafe { Backend::from_foreign_display(display as *mut _) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();

        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).map_err(|e| format!("no wl_seat: {e}"))?;
        let manager: WlDataDeviceManager =
            globals.bind(&qh, 1..=3, ()).map_err(|e| format!("no wl_data_device_manager: {e}"))?;
        let device = manager.get_data_device(&seat, &qh, ());

        // the seat announces its pointer; a second round trip delivers our wl_pointer
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        if state.pointer.is_none() {
            return Err("seat has no pointer".into());
        }

        // gpui's wl_surface, wrapped so it can be passed as the drag origin
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface as *mut _) }
            .map_err(|e| e.to_string())?;
        let surface = WlSurface::from_id(&conn, id).map_err(|e| e.to_string())?;

        Ok((Ctx { conn, qh, shared: shared.clone(), manager, device, surface }, queue))
    })();

    let (ctx, mut queue) = match built {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };
    let conn = ctx.conn.clone();
    let _ = tx.send(Ok(ctx));

    // gpui's own loop reads the socket; events for our objects pile up in our
    // queue until we dispatch them, so do that here. Faster while a drag runs.
    loop {
        if queue.dispatch_pending(&mut state).is_err() {
            break;
        }
        let _ = conn.flush();
        let active = {
            let mut s = shared.lock().unwrap();
            if s.active && s.started.elapsed() > DRAG_TIMEOUT {
                s.active = false;
            }
            s.active
        };
        std::thread::sleep(Duration::from_millis(if active { 8 } else { 40 }));
    }
}

/// Start a compositor-level drag carrying `paths`. Call while the mouse button
/// is still held (the Workspace does, when a row is dragged outside the window).
pub fn start(paths: &[PathBuf]) -> Result<(), String> {
    let ctx = match CTX.get() {
        Some(Ok(c)) => c,
        Some(Err(e)) => return Err(e.clone()),
        None => return Err("drag-out was not initialised".into()),
    };
    if paths.is_empty() {
        return Err("nothing to drag".into());
    }
    let result = (|| {
        let serial = {
            let mut s = ctx.shared.lock().unwrap();
            if s.active {
                return Err("a drag is already running".to_string());
            }
            let serial = s.serial.ok_or("no button press seen yet")?;
            s.payload = uri_list(paths);
            s.active = true;
            s.started = Instant::now();
            serial
        };
        let source = ctx.manager.create_data_source(&ctx.qh, ctx.shared.clone());
        source.offer("text/uri-list".into());
        if ctx.manager.version() >= 3 {
            source.set_actions(DndAction::Copy);
        }
        ctx.device.start_drag(Some(&source), &ctx.surface, None, serial);
        ctx.conn.flush().map_err(|e| e.to_string())
    })();
    if result.is_err() {
        DISABLED.store(true, Ordering::Relaxed); // don't retry on every mouse move
        if let Ok(mut s) = ctx.shared.lock() {
            s.active = false;
        }
    }
    result
}

/// `file:///percent/encoded/path\r\n` for every path.
fn uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        out.push_str("file://");
        for &b in p.as_os_str().as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out.push_str("\r\n");
    }
    out
}

// ---- protocol event handlers ---------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            serial,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            if let Ok(mut s) = state.shared.lock() {
                s.serial = Some(serial);
            }
        }
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: wayland_client::protocol::wl_data_device_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        _: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    // the device announces clipboard / drag offers as new objects; they must be
    // given a handler even though we ignore them
    wayland_client::event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        _: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataSource, Arc<Mutex<Shared>>> for State {
    fn event(
        _: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        shared: &Arc<Mutex<Shared>>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // the target wants the data: write the URI list into its pipe
            wl_data_source::Event::Send { mime_type, fd } => {
                if mime_type == "text/uri-list" {
                    let payload = shared.lock().map(|s| s.payload.clone()).unwrap_or_default();
                    let mut file = std::fs::File::from(fd);
                    let _ = file.write_all(payload.as_bytes());
                } // dropping `file` / `fd` closes the pipe
            }
            // dropped somewhere that took it, or abandoned
            wl_data_source::Event::DndFinished | wl_data_source::Event::Cancelled => {
                source.destroy();
                if let Ok(mut s) = shared.lock() {
                    s.active = false;
                }
            }
            _ => {}
        }
    }
}
