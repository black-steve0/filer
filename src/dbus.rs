//! `org.freedesktop.FileManager1`: how other apps ask "the file manager" to
//! show something.
//!
//! Firefox ("Open Containing Folder" in Downloads) and Discord / any Electron
//! app (`shell.showItemInFolder`) call one of these methods on the session bus:
//!
//!   ShowFolders(as uris, s startup_id)         open these folders
//!   ShowItems(as uris, s startup_id)           open the parent folder, select the item
//!   ShowItemProperties(as uris, s startup_id)  properties dialog (we just reveal the item)
//!
//! This module owns the bus name on its own thread and turns each call into a
//! `Request` on a channel. `main` drains the channel on gpui's thread and hands
//! each request to the Workspace, so no UI code ever runs on the D-Bus thread.
//!
//! If another file manager already owns the name (Nautilus, Dolphin, ...),
//! we log it and carry on without the service; the app itself works as before.

use crate::uri;
use std::path::PathBuf;

pub const BUS_NAME: &str = "org.freedesktop.FileManager1";
pub const OBJECT_PATH: &str = "/org/freedesktop/FileManager1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    /// Show each path as a folder.
    Folders,
    /// Show each path inside its parent folder, selected.
    Items,
}

/// One call from another application.
#[derive(Debug)]
pub struct Request {
    pub kind: Show,
    /// Absolute paths, already decoded from the `file://` URIs. Never empty.
    pub paths: Vec<PathBuf>,
    /// Startup-notification / xdg-activation token (see `activation.rs`).
    pub startup_id: String,
}

struct FileManager1 {
    tx: async_channel::Sender<Request>,
}

impl FileManager1 {
    /// Decode the URIs and queue them for the UI. Calls with nothing usable
    /// (bad URIs, other hosts, `trash:///`) are dropped silently.
    async fn forward(&self, kind: Show, uris: Vec<String>, startup_id: String) {
        let paths: Vec<PathBuf> = uris.iter().filter_map(|u| uri::to_path(u)).collect();
        if paths.is_empty() {
            eprintln!("dbus: {kind:?}: no usable file:// URI in {uris:?}");
            return;
        }
        let _ = self.tx.send(Request { kind, paths, startup_id }).await;
    }
}

// Rust names become D-Bus names: show_folders -> ShowFolders.
// Only the methods in this block are exported; helpers live in the impl above.
#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager1 {
    async fn show_folders(&self, uris: Vec<String>, startup_id: String) {
        self.forward(Show::Folders, uris, startup_id).await;
    }

    async fn show_items(&self, uris: Vec<String>, startup_id: String) {
        self.forward(Show::Items, uris, startup_id).await;
    }

    async fn show_item_properties(&self, uris: Vec<String>, startup_id: String) {
        // there is no properties dialog yet: selecting the file is the closest thing
        self.forward(Show::Items, uris, startup_id).await;
    }
}

/// Claim the bus name on a background thread. Returns the queue of incoming
/// requests, or `None` if the thread could not even be started.
///
/// Failing to get the name (no session bus, name already owned) is not an
/// error here: the thread logs it and ends, and the queue simply stays empty.
pub fn start() -> Option<async_channel::Receiver<Request>> {
    let (tx, rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new().name("dbus".into()).spawn(move || {
        if let Err(e) = zbus::block_on(serve(tx)) {
            eprintln!("dbus: {BUS_NAME} not available: {e}");
        }
    });
    match spawned {
        Ok(_) => Some(rx),
        Err(e) => {
            eprintln!("dbus: could not start thread: {e}");
            None
        }
    }
}

async fn serve(tx: async_channel::Sender<Request>) -> zbus::Result<()> {
    let _connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, FileManager1 { tx })?
        .build()
        .await?;
    eprintln!("dbus: serving {BUS_NAME}");
    // zbus answers calls from its own executor for as long as the connection
    // lives, so this thread only has to keep it alive.
    std::future::pending::<()>().await;
    Ok(())
}
