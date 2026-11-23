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
use workspace::Workspace;

fn main() {
    Application::new().run(|cx: &mut App| {
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
                let workspace = cx.new(|cx| Workspace::new(cx));
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
