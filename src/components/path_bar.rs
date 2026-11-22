//! Breadcrumb path display that turns into a text field when you click the
//! empty part of the bar (or press Ctrl+L). Enter navigates, Esc cancels.

use crate::{
    components::{
        icon::image_or,
        line_edit::{EditOutcome, LineEdit},
    },
    theme,
};
use gpui::{prelude::*, *};
use std::path::{Component, PathBuf};

pub enum PathBarEvent {
    Navigate(PathBuf),
    Activated,
}

pub struct PathBar {
    path: PathBuf,
    edit: Option<LineEdit>,
    error: Option<SharedString>,
    focus: FocusHandle,
    return_focus: FocusHandle,
    _blur: Option<Subscription>,
}

impl EventEmitter<PathBarEvent> for PathBar {}

impl Focusable for PathBar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn expand(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn shell() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .overflow_hidden()
        .gap_2()
        .px_2()
        .py_1()
        .bg(rgb(theme::MANTLE))
        .border_b_1()
        .border_color(rgb(theme::SURFACE0))
}

fn address_label() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .flex_shrink_0()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(theme::TEXT))
        .child(image_or("location", px(14.), || div().into_any_element()))
        .child("Address:")
}

impl PathBar {
    /// `return_focus` is where keyboard focus goes when editing ends.
    pub fn new(path: PathBuf, return_focus: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self {
            path,
            edit: None,
            error: None,
            focus: cx.focus_handle(),
            return_focus,
            _blur: None,
        }
    }

    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = path;
        self.edit = None;
        self.error = None;
        cx.notify();
    }

    pub fn start_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit = Some(LineEdit::new(self.path.to_string_lossy().into_owned()));
        self.error = None;
        window.focus(&self.focus);
        self._blur = Some(cx.on_blur(&self.focus, window, |this, _, cx| {
            if this.edit.is_some() {
                this.edit = None;
                this.error = None;
                cx.notify();
            }
        }));
        cx.notify();
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit = None;
        self.error = None;
        window.focus(&self.return_focus);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.edit else { return };
        let target = expand(edit.text.trim());
        if target.is_dir() {
            self.finish(window, cx);
            cx.emit(PathBarEvent::Navigate(target));
        } else {
            let msg = if target.exists() { "Not a folder" } else { "No such folder" };
            self.error = Some(msg.into());
            cx.notify();
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(edit) = self.edit.as_mut() else { return };
        match edit.handle_key(ev, cx) {
            EditOutcome::Submit => self.submit(window, cx),
            EditOutcome::Cancel => self.finish(window, cx),
            EditOutcome::Edited => {
                self.error = None;
                cx.notify();
            }
            EditOutcome::Ignored => {}
        }
    }

    fn crumbs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let comps: Vec<_> = self.path.components().collect();
        let total = comps.len();
        let mut acc = PathBuf::new();
        let mut out: Vec<AnyElement> = Vec::new();

        for (i, comp) in comps.iter().enumerate() {
            acc.push(comp.as_os_str());
            let label = match comp {
                Component::RootDir => "/".to_string(),
                c => c.as_os_str().to_string_lossy().into_owned(),
            };
            let target = acc.clone();
            let last = i + 1 == total;

            out.push(
                div()
                    .id(("crumb", i))
                    .px_1()
                    .cursor_pointer()
                    .text_color(rgb(theme::LINK))
                    .underline()
                    .when(last, |d| d.font_weight(FontWeight::BOLD))
                    .hover(|s| s.text_color(rgb(theme::LINK_HOVER)))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(PathBarEvent::Navigate(target.clone()))
                        }),
                    )
                    .into_any_element(),
            );
            if !last {
                out.push(div().text_color(rgb(theme::OVERLAY)).child("»").into_any_element());
            }
        }
        out
    }
}

impl Render for PathBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(edit) = &self.edit {
            let editor = div()
                .id("path-edit")
                .key_context("TextInput")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    this.on_key(ev, window, cx)
                }))
                .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.stop_propagation()))
                .flex_1()
                .px_2()
                .py_1()
                .bg(rgb(theme::BG))
                .border_1()
                .border_color(rgb(theme::ACCENT))
                .shadow(theme::sunken())
                .overflow_hidden()
                .child(edit.render());

            return shell()
                .child(address_label())
                .child(editor)
                .children(
                    self.error
                        .clone()
                        .map(|e| div().ml_2().text_xs().text_color(rgb(theme::ERROR)).child(e)),
                )
                .into_any_element();
        }

        let fill = div()
            .id("path-fill")
            .flex_1()
            .min_w(px(24.))
            .h(px(22.))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    cx.emit(PathBarEvent::Activated);
                    this.start_edit(window, cx);
                }),
            );

        let field = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .items_center()
            .overflow_hidden()
            .px_1()
            .bg(rgb(theme::BG))
            .shadow(theme::sunken())
            .children(self.crumbs(cx))
            .child(fill);

        shell().child(address_label()).child(field).into_any_element()
    }
}