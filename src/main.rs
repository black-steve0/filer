mod actions;
mod components;
mod dragout;
mod filter;
mod menus;
mod model;
mod settings;
mod settings_view;
mod theme;
mod workspace;

use gpui::*;
use std::path::PathBuf;
use workspace::Workspace;

/// Folders to open, from the command line: `filer .`, `filer ~/a /mnt/b`.
/// Relative paths are resolved against the current directory; a file opens
/// its parent folder; each distinct folder gets its own pane.
fn startup_paths() -> Vec<PathBuf> {
    let cwd = std::env::current_dir().ok();
    let mut out: Vec<PathBuf> = Vec::new();
    for arg in std::env::args_os().skip(1) {
        if arg == "-h" || arg == "--help" {
            println!("usage: filer [PATH]...\n\nOpens each PATH in its own pane (no PATH: the startup folder).");
            std::process::exit(0);
        }
        let given = PathBuf::from(&arg);
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
    out
}

fn main() {
    let paths = startup_paths();
    Application::new().run(move |cx: &mut App| {
        settings::init(cx);
        actions::bind_keys(cx);

        let cfg = settings::get();
        let bounds = Bounds::centered(
            None,
            size(px(cfg.window_w as f32), px(cfg.window_h as f32)),
            cx,
        );
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Files".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                dragout::init(window); // native drag-out to other apps (Wayland)
                let workspace = cx.new(|cx| Workspace::new(paths, cx));
                // give the first pane keyboard focus right away
                workspace.update(cx, |ws, cx| ws.focus_active(window, cx));
                workspace
            },
        )
        .unwrap();

        cx.on_window_closed(|cx| {
            // the Settings window alone doesn't keep the app running
            let only_settings = cx
                .windows()
                .iter()
                .all(|w| w.downcast::<settings_view::SettingsView>().is_some());
            if only_settings {
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });
}
