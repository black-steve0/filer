//! Breadcrumb path display that turns into a text field when you click the
//! empty part of the bar (or press Ctrl+L). Enter navigates, Esc cancels.
//!
//! The same field is also the "Go to folder" box (Ctrl+P): it opens empty and
//! focused, lists the 5 best matching folders while you type (`jump::search` for
//! words, `jump::complete_path` once you type a path), Up/Down choose, Tab
//! completes, Enter goes there and hands the keyboard back to the file list.

use crate::{
    components::{
        icon::image_or,
        line_edit::{EditOutcome, LineEdit},
    },
    jump, theme,
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
    /// Opened with Ctrl+P: starts empty and searches folders by name.
    jump: bool,
    /// The folders offered under the field, best first, and the highlighted one.
    suggestions: Vec<PathBuf>,
    chosen: Option<usize>,
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
    jump::expand(raw)
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

fn address_label(text: &'static str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .flex_shrink_0()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(theme::TEXT))
        .child(image_or("location", px(14.), || div().into_any_element()))
        .child(text)
}

impl PathBar {
    /// `return_focus` is where keyboard focus goes when editing ends.
    pub fn new(path: PathBuf, return_focus: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self {
            path,
            edit: None,
            error: None,
            jump: false,
            suggestions: Vec::new(),
            chosen: None,
            focus: cx.focus_handle(),
            return_focus,
            _blur: None,
        }
    }

    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = path;
        self.edit = None;
        self.error = None;
        self.suggestions.clear();
        cx.notify();
    }

    pub fn start_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin(false, window, cx);
    }

    /// Ctrl+P: an empty box that searches folders; the keyboard is in it at once.
    pub fn start_jump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin(true, window, cx);
    }

    fn begin(&mut self, jump_mode: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = if jump_mode { String::new() } else { self.path.to_string_lossy().into_owned() };
        self.edit = Some(LineEdit::new(text));
        self.jump = jump_mode;
        self.error = None;
        self.suggestions.clear();
        self.chosen = None;
        if jump_mode {
            jump::refresh_if_stale();
            self.refresh_suggestions();
        }
        window.focus(&self.focus);
        self._blur = Some(cx.on_blur(&self.focus, window, |this, _, cx| {
            if this.edit.is_some() {
                this.edit = None;
                this.error = None;
                this.suggestions.clear();
                cx.notify();
            }
        }));
        cx.notify();
    }

    /// Recompute the folders listed under the field from what is typed.
    fn refresh_suggestions(&mut self) {
        let Some(edit) = &self.edit else { return };
        let typed = edit.text.trim();
        let path_mode = jump::looks_like_path(typed);
        self.suggestions = if path_mode {
            jump::complete_path(typed, 5)
        } else {
            jump::search(typed, &self.path, 5)
        };
        // searching: the best match is already chosen, so Enter just goes;
        // typing a path: nothing is chosen until you press Down (Enter goes to what you typed)
        self.chosen = (!path_mode && !self.suggestions.is_empty()).then_some(0);
    }

    /// Up / Down in the list of folders.
    fn move_choice(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.suggestions.len();
        if len == 0 {
            return;
        }
        self.chosen = match (self.chosen, delta) {
            (None, d) if d > 0 => Some(0),
            (None, _) => Some(len - 1),
            (Some(0), d) if d < 0 && jump::looks_like_path(self.edit.as_ref().map_or("", |e| e.text.trim())) => None,
            (Some(i), d) => Some((i as isize + d).rem_euclid(len as isize) as usize),
        };
        cx.notify();
    }

    /// Tab: put the chosen (or best) folder into the field and keep typing.
    fn complete(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.chosen.or(Some(0)).and_then(|i| self.suggestions.get(i)).cloned() else { return };
        let mut text = jump::display(&target);
        if !text.ends_with('/') {
            text.push('/');
        }
        if let Some(edit) = self.edit.as_mut() {
            edit.set_text(text);
        }
        self.error = None;
        self.refresh_suggestions();
        cx.notify();
    }

    /// Go to `target`: close the box, hand the keyboard back to the file list.
    fn go_to(&mut self, target: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(window, cx);
        cx.emit(PathBarEvent::Navigate(target));
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit = None;
        self.error = None;
        self.suggestions.clear();
        window.focus(&self.return_focus);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.edit else { return };
        let text = edit.text.trim().to_string();
        let typed = expand(&text);
        // 1. a folder you picked with Up/Down (or the best match while searching)
        // 2. what you typed, when that is a folder
        // 3. the first folder that completes what you typed
        let target = self
            .chosen
            .and_then(|i| self.suggestions.get(i).cloned())
            .or_else(|| (!text.is_empty() && typed.is_dir()).then(|| typed.clone()))
            .or_else(|| self.suggestions.first().cloned());
        match target {
            Some(target) => self.go_to(target, window, cx),
            None if text.is_empty() => self.finish(window, cx),
            None => {
                let msg = if typed.exists() { "Not a folder" } else { "No such folder" };
                self.error = Some(msg.into());
                cx.notify();
            }
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if self.edit.is_none() {
            return;
        }
        let m = &ev.keystroke.modifiers;
        if !m.control && !m.alt && !m.platform {
            match ev.keystroke.key.as_str() {
                "down" => return self.move_choice(1, cx),
                "up" => return self.move_choice(-1, cx),
                "tab" => return if m.shift { self.move_choice(-1, cx) } else { self.complete(cx) },
                _ => {}
            }
        }
        let Some(edit) = self.edit.as_mut() else { return };
        match edit.handle_key(ev, cx) {
            EditOutcome::Submit => self.submit(window, cx),
            EditOutcome::Cancel => self.finish(window, cx),
            EditOutcome::Edited => {
                self.error = None;
                self.refresh_suggestions();
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

            let hint = if self.jump {
                "Enter go  Tab complete  Up/Down choose  Esc cancel"
            } else {
                ""
            };
            let bar = shell()
                .child(address_label(if self.jump { "Go to:" } else { "Address:" }))
                .child(editor)
                .children(
                    self.error
                        .clone()
                        .map(|e| div().ml_2().text_xs().text_color(rgb(theme::ERROR)).child(e)),
                )
                .children((!hint.is_empty() && self.error.is_none()).then(|| {
                    div().flex_shrink_0().text_xs().text_color(rgb(theme::OVERLAY)).child(hint)
                }));

            // the folders that match, under the field (drawn above the file list)
            let typed_empty = edit.text.trim().is_empty();
            let rows: Vec<AnyElement> = self
                .suggestions
                .iter()
                .enumerate()
                .map(|(i, folder)| {
                    let target = folder.clone();
                    let name = folder
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "/".into());
                    let parent = folder.parent().map(jump::display).unwrap_or_default();
                    let on = self.chosen == Some(i);
                    div()
                        .id(("suggestion", i))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .cursor_pointer()
                        .text_color(rgb(theme::TEXT))
                        .when(on, |d| d.bg(rgb(theme::HILITE)))
                        .when(!on, |d| d.hover(|s| s.bg(rgb(theme::HOVER_ROW))))
                        .child(div().font_weight(FontWeight::BOLD).flex_shrink_0().child(name))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(theme::SUBTEXT))
                                .child(parent),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.go_to(target.clone(), window, cx);
                            }),
                        )
                        .into_any_element()
                })
                .collect();
            let message = if rows.is_empty() && !typed_empty {
                Some("No matching folders")
            } else if rows.is_empty() && self.jump {
                Some("Type part of a folder name")
            } else {
                None
            };

            return div()
                .relative()
                .child(bar)
                .when(!rows.is_empty() || message.is_some(), |d| {
                    d.child(deferred(
                        div()
                            .absolute()
                            .top(px(34.))
                            .left(px(8.))
                            .w(px(520.))
                            .max_w_full()
                            .flex()
                            .flex_col()
                            .occlude()
                            .bg(rgb(theme::BG))
                            .border_1()
                            .border_color(rgb(theme::ACCENT))
                            .shadow(theme::raised())
                            .children(rows)
                            .children(message.map(|m| {
                                div().px_2().py_0p5().text_xs().text_color(rgb(theme::OVERLAY)).child(m)
                            })),
                    ))
                })
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

        shell().child(address_label("Address:")).child(field).into_any_element()
    }
}