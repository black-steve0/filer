//! "Open With" dialog: pick an installed application to open one file or folder.
//!
//!   ┌ Open With ─────────────────────────┐
//!   │ photo.png                          │
//!   │ PNG image (image/png)              │
//!   │ ┌────────────────────────────────┐ │
//!   │ │ Image Viewer           default │ │   click = select, double-click = open
//!   │ │ GIMP                           │ │   Up / Down / Enter / Esc work too
//!   │ └────────────────────────────────┘ │
//!   │ [ ] Show all applications          │
//!   │ [ ] Always use this application... │
//!   │                    [Open] [Cancel] │
//!   └────────────────────────────────────┘
//!
//! It only collects the answer (`OpenWithEvent`); the Workspace starts the program.

use crate::{
    apps::{DesktopApp, Lookup},
    components::{
        controls::chip,
        modal::{check_row, frame},
    },
    model, theme,
};
use gpui::{prelude::*, *};
use std::path::PathBuf;

pub enum OpenWithEvent {
    Open {
        app: DesktopApp,
        path: PathBuf,
        mime: Option<String>,
        /// "Always use this application": make it the default for `mime`.
        set_default: bool,
    },
    Cancelled,
}

pub struct OpenWithDialog {
    path: PathBuf,
    name: SharedString,
    /// "PNG image (image/png)"
    kind: SharedString,
    lookup: Lookup,
    /// List every installed application instead of only those that handle the type.
    show_all: bool,
    selected: usize,
    always: bool,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl EventEmitter<OpenWithEvent> for OpenWithDialog {}

impl Focusable for OpenWithDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl OpenWithDialog {
    pub fn new(path: PathBuf, lookup: Lookup, cx: &mut Context<Self>) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let kind = match (model::entry_for(&path).map(|e| e.kind), &lookup.mime) {
            (Some(k), Some(m)) => format!("{k} ({m})"),
            (Some(k), None) => k,
            (None, Some(m)) => m.clone(),
            (None, None) => String::new(),
        };
        Self {
            path,
            name: name.into(),
            kind: kind.into(),
            // nothing registered for this type: the only useful list is everything
            show_all: lookup.matching.is_empty(),
            lookup,
            selected: 0,
            always: false,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        }
    }

    /// The rows currently shown.
    fn list(&self) -> &[DesktopApp] {
        if self.show_all {
            &self.lookup.all
        } else {
            &self.lookup.matching
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let last = self.list().len().saturating_sub(1);
        self.selected = index.min(last);
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        let Some(app) = self.list().get(self.selected).cloned() else { return };
        cx.emit(OpenWithEvent::Open {
            app,
            path: self.path.clone(),
            set_default: self.always && self.lookup.mime.is_some(),
            mime: self.lookup.mime.clone(),
        });
    }

    fn toggle_show_all(&mut self, cx: &mut Context<Self>) {
        // keep the same application selected when it is in the other list too
        let keep = self.list().get(self.selected).map(|a| a.id.clone());
        self.show_all = !self.show_all;
        let index = keep.and_then(|id| self.list().iter().position(|a| a.id == id)).unwrap_or(0);
        self.select(index, cx);
    }

    fn key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => cx.emit(OpenWithEvent::Cancelled),
            "enter" => self.open(cx),
            "up" => self.select(self.selected.saturating_sub(1), cx),
            "down" => self.select(self.selected + 1, cx),
            "home" => self.select(0, cx),
            "end" => self.select(usize::MAX, cx),
            _ => {}
        }
    }
}

impl Render for OpenWithDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected;
        let default_id = self.lookup.default_id.clone();
        let list = self.list();

        let rows: Vec<AnyElement> = list
            .iter()
            .enumerate()
            .map(|(i, app)| {
                let on = i == selected;
                let is_default = default_id.as_deref() == Some(app.id.as_str());
                div()
                    .id(("open-with-app", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(on, |d| d.bg(rgb(theme::ACCENT)).text_color(rgb(0xffffff)))
                    .when(!on, |d| d.hover(|s| s.bg(rgb(theme::HOVER_ROW))))
                    .child(div().min_w_0().truncate().child(app.name.clone()))
                    .when(is_default, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(rgb(if on { 0xffffff } else { theme::SUBTEXT }))
                                .child("default"),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.select(i, cx);
                            if ev.click_count >= 2 {
                                this.open(cx); // double-click
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
            })
            .collect();

        let empty = rows.is_empty();
        let has_matches = !self.lookup.matching.is_empty();
        let hint: Option<&str> = if empty {
            Some("No applications were found.")
        } else if !has_matches {
            Some("No application is registered for this type of file. All installed applications are listed.")
        } else {
            None
        };

        let mut body: Vec<AnyElement> = vec![
            div()
                .flex()
                .flex_col()
                .child(div().truncate().font_weight(FontWeight::BOLD).child(self.name.clone()))
                .child(div().truncate().text_color(rgb(theme::SUBTEXT)).child(self.kind.clone()))
                .into_any_element(),
            div()
                .id("open-with-list")
                .track_scroll(&self.scroll)
                .overflow_y_scroll()
                .h(px(190.))
                .bg(rgb(theme::BG))
                .shadow(theme::sunken())
                .border_1()
                .border_color(rgb(theme::SURFACE0))
                .children(rows)
                .into_any_element(),
        ];
        if let Some(hint) = hint {
            body.push(div().text_xs().text_color(rgb(theme::SUBTEXT)).child(hint).into_any_element());
        }
        if has_matches {
            body.push(check_row(
                "open-with-all",
                "Show all applications",
                self.show_all,
                cx.listener(|this, _: &MouseDownEvent, _, cx| this.toggle_show_all(cx)),
            ));
        }
        if self.lookup.mime.is_some() && !empty {
            body.push(check_row(
                "open-with-always",
                "Always use this application for this type of file",
                self.always,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.always = !this.always;
                    cx.notify();
                }),
            ));
        }
        body.push(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_1()
                .mt_1()
                .child(chip(
                    ("open-with-open", 0usize),
                    "Open",
                    true, // highlighted: this is what Enter does
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.open(cx);
                        cx.stop_propagation();
                    }),
                ))
                .child(chip(
                    ("open-with-cancel", 0usize),
                    "Cancel",
                    false,
                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                        cx.emit(OpenWithEvent::Cancelled);
                        cx.stop_propagation();
                    }),
                ))
                .into_any_element(),
        );

        frame("Open With", 420., body)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                this.key(ev, cx);
                cx.stop_propagation();
            }))
    }
}
