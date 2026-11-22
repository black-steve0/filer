use crate::{actions::*, components::button::button, theme};
use gpui::{prelude::*, *};

/// Buttons just dispatch actions (same commands as the shortcuts and menus).
/// The workspace pushes toggle state in via `set_state`.
/// ("Close pane" lives in each pane's title bar as an X.)
#[derive(Default)]
pub struct Toolbar {
    hidden: bool,
}

impl Toolbar {
    pub fn set_state(&mut self, hidden: bool, cx: &mut Context<Self>) {
        if self.hidden != hidden {
            self.hidden = hidden;
            cx.notify();
        }
    }
}

fn sep() -> impl IntoElement {
    div().w(px(1.)).h(px(16.)).mx_1().bg(rgb(theme::SURFACE0))
}

impl Render for Toolbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hidden = self.hidden;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_3()
            .py_1()
            .bg(rgb(theme::MANTLE))
            .border_b_1()
            .border_color(rgb(theme::SURFACE0))
            .child(button("back", "back", "←", false, cx.listener(|_, _, w, cx| w.dispatch_action(GoBack.boxed_clone(), cx))))
            .child(button("fwd", "forward", "→", false, cx.listener(|_, _, w, cx| w.dispatch_action(GoForward.boxed_clone(), cx))))
            .child(button("up", "up", "↑", false, cx.listener(|_, _, w, cx| w.dispatch_action(GoUp.boxed_clone(), cx))))
            .child(button("home", "home", "Home", false, cx.listener(|_, _, w, cx| w.dispatch_action(GoHome.boxed_clone(), cx))))
            .child(sep())
            .child(button("refresh", "refresh", "Refresh", false, cx.listener(|_, _, w, cx| w.dispatch_action(Refresh.boxed_clone(), cx))))
            .child(button("newfile", "new-file", "New File", false, cx.listener(|_, _, w, cx| w.dispatch_action(NewFile.boxed_clone(), cx))))
            .child(button("newdir", "new-folder", "New Folder", false, cx.listener(|_, _, w, cx| w.dispatch_action(NewFolder.boxed_clone(), cx))))
            .child(sep())
            .child(button("cut", "cut", "Cut", false, cx.listener(|_, _, w, cx| w.dispatch_action(CutFile.boxed_clone(), cx))))
            .child(button("copy", "copy", "Copy", false, cx.listener(|_, _, w, cx| w.dispatch_action(CopyFile.boxed_clone(), cx))))
            .child(button("paste", "paste", "Paste", false, cx.listener(|_, _, w, cx| w.dispatch_action(PasteFile.boxed_clone(), cx))))
            .child(button("find", "find", "Filter", false, cx.listener(|_, _, w, cx| w.dispatch_action(FindInPane.boxed_clone(), cx))))
            .child(div().flex_1())
            .child(button("hidden", "hidden", "Hidden", hidden, cx.listener(|_, _, w, cx| w.dispatch_action(ToggleHidden.boxed_clone(), cx))))
            .child(button("split", "split", "Split", false, cx.listener(|_, _, w, cx| w.dispatch_action(SplitPane.boxed_clone(), cx))))
            .child(sep())
            .child(button("settings", "settings", "Settings", false, cx.listener(|_, _, w, cx| w.dispatch_action(OpenSettings.boxed_clone(), cx))))
    }
}