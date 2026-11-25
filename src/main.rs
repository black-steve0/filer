mod actions;
mod activation;
mod apps;
mod components;
mod dbus;
mod dragout;
mod filetype;
mod jump;
mod foreign;
mod filter;
mod menus;
mod model;
mod picker;
mod portal;
mod props;
mod settings;
mod settings_view;
mod theme;
mod transfer;
mod uri;
mod workspace;

use gpui::*;
use std::path::PathBuf;
use workspace::Workspace;

/// What the command line asked for.
struct Args {
    /// Folders to open, one pane each (empty: the startup folder).
    paths: Vec<PathBuf>,
    /// Started by the session bus because another app called FileManager1
    /// (see `dist/`). The first request then replaces the default pane
    /// instead of opening a second one next to it.
    dbus_activated: bool,
    /// Started by the portal (`--portal-backend`): no main window, just serve
    /// file choosers and exit when the last one closes.
    portal_only: bool,
}

/// Folders to open, from the command line: `filer .`, `filer ~/a /mnt/b`,
/// `filer file:///tmp`. Relative paths are resolved against the current
/// directory; a file opens its parent folder; each distinct folder gets its
/// own pane.
fn parse_args() -> Args {
    let cwd = std::env::current_dir().ok();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut dbus_activated = false;
    let mut portal_only = false;
    for arg in std::env::args_os().skip(1) {
        if arg == "-h" || arg == "--help" {
            println!("usage: filer [PATH|URI]...\n\nOpens each PATH in its own pane (no PATH: the startup folder).");
            std::process::exit(0);
        }
        if arg == "--dbus-activated" {
            dbus_activated = true;
            continue;
        }
        if arg == "--portal-backend" {
            portal_only = true;
            continue;
        }
        // launchers may hand over file:// URIs (Exec=... %U)
        let given = match arg.to_str().and_then(|s| s.starts_with("file:").then(|| uri::to_path(s)).flatten()) {
            Some(p) => p,
            None => PathBuf::from(&arg),
        };
        let full = match (&cwd, given.is_absolute()) {
            (Some(cwd), false) => cwd.join(&given),
            _ => given.clone(),
        };
        let full = std::fs::canonicalize(&full).unwrap_or(full); // tidies "." and ".."
        let dir = if full.is_dir() {
            full
        } else if full.is_file() {
            match full.parent() {
                Some(p) => p.to_path_buf(),
                None => continue,
            }
        } else {
            eprintln!("filer: {}: no such file or directory", given.display());
            continue;
        };
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    Args { paths: out, dbus_activated, portal_only }
}

fn main() {
    let Args { paths, dbus_activated, portal_only } = parse_args();
    Application::new().run(move |cx: &mut App| {
        settings::init(cx);
        actions::bind_keys(cx);
        jump::start(); // index folders in the background for the Ctrl+P jump box

        if !portal_only {
            let cfg = settings::get();
            let bounds = Bounds::centered(
                None,
                size(px(cfg.window_w as f32), px(cfg.window_h as f32)),
                cx,
            );
            let main_window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        titlebar: Some(TitlebarOptions {
                            title: Some("Files".into()),
                            ..Default::default()
                        }),
                        // lets the compositor match the window to dist/filer.desktop (icon, taskbar)
                        app_id: Some("filer".into()),
                        ..Default::default()
                    },
                    |window, cx| {
                        dragout::init(window); // native drag-out to other apps (Wayland)
                        let workspace = cx.new(|cx| Workspace::new(paths, dbus_activated, cx));
                        // give the first pane keyboard focus right away
                        workspace.update(cx, |ws, cx| ws.focus_active(window, cx));
                        workspace
                    },
                )
                .unwrap();

            // "Show in folder" from Firefox, Discord, ... arrives as D-Bus calls on
            // another thread; run each one on this thread against the main window
            if let Some(requests) = dbus::start() {
                cx.spawn(async move |cx| {
                    while let Ok(req) = requests.recv().await {
                        if main_window
                            .update(cx, |ws, window, cx| ws.handle_request(req, window, cx))
                            .is_err()
                        {
                            break; // main window is gone
                        }
                    }
                })
                .detach();
            }
        }

        // "Open file" / "Save as" dialogs of other apps (xdg-desktop-portal FileChooser):
        // each request opens a chooser window
        if let Some(requests) = portal::start() {
            picker::serve(cx, requests);
        }

        // How long to wait after the last window closed before quitting. A chooser sends its
        // answer just before it closes and the portal thread still has to put it on the bus, so
        // never quit at once. The portal backend (started by D-Bus for one dialog) stays a while,
        // which also makes the next dialog open instantly.
        let linger = std::time::Duration::from_millis(if portal_only { 20_000 } else { 600 });
        cx.on_window_closed(move |cx| {
            // the Settings window alone doesn't keep the app running
            let only_settings = |cx: &App| {
                cx.windows().iter().all(|w| w.downcast::<settings_view::SettingsView>().is_some())
            };
            if only_settings(cx) {
                // check again after the wait: a new chooser may have opened meanwhile
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(linger).await;
                    let _ = cx.update(|cx| {
                        if only_settings(cx) {
                            cx.quit();
                        }
                    });
                })
                .detach();
            }
        })
        .detach();

        cx.activate(true);
    });
}
