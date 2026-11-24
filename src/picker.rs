//! The file chooser window shown for portal requests (see `portal.rs`).
//!
//! It is a normal `Pane` in picker mode (same listing, path bar, sorting and
//! navigation as the main window) with a footer: a file-name field when
//! saving, and Cancel / Open|Save|Select buttons.
//!
//! The answer goes back through `Chooser::reply`. Closing the window any
//! other way drops the sender, which the portal side reads as "cancelled".

use crate::{
    actions::{FocusPath, GoToFolder, GoBack, GoForward, GoHome, GoUp, NewFolder, OpenSelected, Refresh, ToggleHidden},
    components::{
        button::button,
        controls::{checkbox, chip},
        line_edit::{EditOutcome, LineEdit},
        pane::{Pane, PaneEvent},
    },
    portal::{Choice, Chooser, Kind, Outcome},
    theme,
};
use gpui::{prelude::*, *};
use crate::filetype::Filter;
use std::{path::PathBuf, sync::Arc};

// ---- receiving requests -----------------------------------------------------------

/// Empty model whose only job is to own the task that waits for requests.
struct Host;

struct HostHandle(#[allow(dead_code)] Entity<Host>);
impl Global for HostHandle {}

/// Open a chooser window for every request that arrives on `requests`.
pub fn serve(cx: &mut App, requests: async_channel::Receiver<Chooser>) {
    let host = cx.new(|_| Host);
    host.update(cx, |_, cx| {
        cx.spawn(async move |this, cx| {
            while let Ok(request) = requests.recv().await {
                if this.update(cx, |_, cx| open(cx, request)).is_err() {
                    break;
                }
            }
        })
        .detach();
    });
    cx.set_global(HostHandle(host)); // keeps the model (and so the task) alive
}

fn open(cx: &mut App, request: Chooser) {
    let cancel = request.cancel.clone();
    let title: SharedString = if request.title.is_empty() {
        match request.kind {
            Kind::Save { .. } => "Save As".into(),
            Kind::Open { directory: true, .. } => "Select Folder".into(),
            Kind::Open { .. } => "Open File".into(),
        }
    } else {
        request.title.clone().into()
    };
    let bounds = Bounds::centered(None, size(px(820.), px(560.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some(title.clone()), ..Default::default() }),
            // not "filer": lets a compositor rule (niri: open-floating) match choosers only
            app_id: Some("filer-chooser".into()),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Picker::new(request, title, window, cx)),
    );
    match opened {
        // the app withdrew the request (or it ended): close the window if it is still there
        Ok(handle) => cx
            .spawn(async move |cx| {
                let _ = cancel.recv().await;
                handle.update(cx, |_, window, _| window.remove_window()).ok();
            })
            .detach(),
        Err(e) => eprintln!("portal: could not open the chooser window: {e}"),
    }
}

// ---- the window -------------------------------------------------------------------

pub struct Picker {
    pane: Entity<Pane>,
    kind: Kind,
    title: SharedString,
    app: SharedString,
    reply: Option<async_channel::Sender<Outcome>>,
    /// Save: the file name being typed.
    name: LineEdit,
    name_focus: FocusHandle,
    /// The app's file-type filters and the one in force.
    filters: Vec<Arc<Filter>>,
    current_filter: usize,
    /// The app's extra options, with the user's current picks.
    choices: Vec<Choice>,
    /// Keeps the chooser a child of the app's window while it is open.
    #[allow(dead_code)]
    parent: Option<crate::foreign::Parent>,
    /// The file whose name was last copied into the field.
    synced: Option<PathBuf>,
    /// Save: the typed name already exists and the user was warned once.
    overwrite_warned: bool,
    note: Option<SharedString>,
}

impl Picker {
    fn new(request: Chooser, title: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let start = request
            .folder
            .filter(|p| p.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let multiple = matches!(request.kind, Kind::Open { multiple: true, directory: false });
        let pane = cx.new(|cx| {
            let mut pane = Pane::new(start, cx);
            pane.set_picker();
            if multiple {
                pane.set_multi();
            }
            pane
        });
        pane.update(cx, |p, cx| p.set_active(true, cx));
        // filters only make sense when picking files, not folders
        let filters: Vec<Arc<Filter>> = if matches!(request.kind, Kind::Open { directory: true, .. }) {
            Vec::new()
        } else {
            request.filters.into_iter().map(Arc::new).collect()
        };
        let current_filter = request.current_filter.min(filters.len().saturating_sub(1));
        if let Some(filter) = filters.get(current_filter) {
            let filter = filter.clone();
            pane.update(cx, |p, cx| p.set_type_filter(Some(filter), cx));
        }
        cx.subscribe_in(&pane, window, |this, _, event: &PaneEvent, window, cx| {
            this.on_pane(event, window, cx)
        })
        .detach();

        let name_focus = cx.focus_handle();
        let name = match &request.kind {
            Kind::Save { name } => LineEdit::for_rename(name.clone()),
            Kind::Open { .. } => LineEdit::new(String::new()),
        };
        // saving starts in the name field, opening in the list
        if matches!(request.kind, Kind::Save { .. }) {
            window.focus(&name_focus);
        } else {
            let list = pane.focus_handle(cx);
            window.focus(&list);
        }
        // child of the asking app's window, so compositors keep it on top (and niri floats it)
        let parent = crate::foreign::attach(window, &request.parent_window);
        // ask the compositor to bring the chooser to the front (same call the folder requests use)
        window.activate_window();

        Self {
            pane,
            kind: request.kind,
            title,
            app: request.app_id.into(),
            reply: Some(request.reply),
            name,
            name_focus,
            filters,
            current_filter,
            choices: request.choices,
            parent,
            synced: None,
            overwrite_warned: false,
            note: None,
        }
    }

    // ---- answering ----------------------------------------------------------------

    fn finish(&mut self, paths: Vec<PathBuf>, window: &mut Window) {
        if let Some(reply) = self.reply.take() {
            let chosen = self.choices.iter().map(|c| (c.id.clone(), c.selected.clone())).collect();
            let filter = self.filters.get(self.current_filter).map(|f| (**f).clone());
            let _ = reply.try_send(Outcome::Picked(paths, filter, chosen));
        }
        window.remove_window();
    }

    fn cancel(&mut self, window: &mut Window) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(Outcome::Cancelled);
        }
        window.remove_window();
    }

    fn say(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.note = Some(text.into());
        cx.notify();
    }

    /// The main button (and Enter in the name field).
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.note = None;
        match self.kind.clone() {
            Kind::Open { directory: true, .. } => {
                let dir = {
                    let pane = self.pane.read(cx);
                    pane.selected_dir().unwrap_or_else(|| pane.path().to_path_buf())
                };
                self.finish(vec![dir], window);
            }
            Kind::Open { .. } => {
                let files = self.pane.read(cx).marked_files();
                if !files.is_empty() {
                    self.finish(files, window); // the marked files, or the one selected
                } else if self.pane.read(cx).selected_path().is_some() {
                    // a folder is selected: Enter goes into it
                    self.pane.update(cx, |p, cx| p.open_selected(cx));
                } else {
                    self.say("Select a file first.", cx);
                }
            }
            Kind::Save { .. } => {
                let name = self.name.text.trim().to_string();
                if name.is_empty() || name.contains('/') || name == "." || name == ".." {
                    self.say("Type a file name (no slashes).", cx);
                    return;
                }
                let path = self.pane.read(cx).path().join(&name);
                if path.is_dir() {
                    self.say(format!("\"{name}\" is a folder."), cx);
                } else if path.exists() && !self.overwrite_warned {
                    self.overwrite_warned = true;
                    self.say(format!("\"{name}\" already exists. Click Save again to replace it."), cx);
                } else {
                    self.finish(vec![path], window);
                }
            }
        }
    }

    fn pick_filter(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(filter) = self.filters.get(index).cloned() {
            self.current_filter = index;
            self.pane.update(cx, |p, cx| p.set_type_filter(Some(filter), cx));
            cx.notify();
        }
    }

    fn pick_choice(&mut self, choice: usize, value: String, cx: &mut Context<Self>) {
        if let Some(c) = self.choices.get_mut(choice) {
            c.selected = value;
            cx.notify();
        }
    }

    // ---- events ---------------------------------------------------------------------

    fn on_pane(&mut self, event: &PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Picked(path) => match self.kind {
                Kind::Open { .. } => self.finish(vec![path.clone()], window),
                Kind::Save { .. } => {
                    // double-click on an existing file: take its name, then save over it
                    self.take_name(path);
                    self.confirm(window, cx);
                }
            },
            PaneEvent::Changed => {
                // clicking a file in the list fills the name field
                if matches!(self.kind, Kind::Save { .. }) {
                    let selected = self.pane.read(cx).selected_file();
                    if selected != self.synced {
                        if let Some(path) = &selected {
                            self.take_name(path);
                        }
                        self.synced = selected;
                        cx.notify();
                    }
                }
            }
            PaneEvent::Activated | PaneEvent::CloseRequested | PaneEvent::Dropped { .. } => {}
        }
    }

    fn take_name(&mut self, path: &std::path::Path) {
        if let Some(name) = path.file_name() {
            self.name = LineEdit::new(name.to_string_lossy().into_owned());
            self.overwrite_warned = false;
        }
    }

    fn name_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        match self.name.handle_key(ev, cx) {
            EditOutcome::Submit => self.confirm(window, cx),
            EditOutcome::Cancel => self.cancel(window),
            EditOutcome::Edited => {
                self.overwrite_warned = false;
                self.note = None;
                cx.notify();
            }
            EditOutcome::Ignored => {}
        }
    }
}

impl Render for Picker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cfg = crate::settings::get();
        window.set_rem_size(px(16. * cfg.font_size as f32 / 12.));

        let save = matches!(self.kind, Kind::Save { .. });
        let multiple = matches!(self.kind, Kind::Open { multiple: true, directory: false });
        // without a warning to show, say how many files are marked
        let status: Option<SharedString> = self.note.clone().or_else(|| {
            let n = self.pane.read(cx).marked_files().len();
            (multiple && n > 1).then(|| format!("{n} files selected").into())
        });
        // one chip per filter of the app, the active one highlighted
        let filter_row = (!self.filters.is_empty()).then(|| {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_1()
                .child("Show:")
                .children(self.filters.iter().enumerate().map(|(i, f)| {
                    chip(
                        ("filter", i),
                        f.name.clone(),
                        i == self.current_filter,
                        cx.listener(move |this, _, _, cx| this.pick_filter(i, cx)),
                    )
                }))
        });
        // the app's extra options: a checkbox, or a row of chips when it offers alternatives
        let choice_rows: Vec<_> = self
            .choices
            .iter()
            .enumerate()
            .map(|(ci, c)| {
                let row = div().flex().flex_row().flex_wrap().items_center().gap_1();
                if c.options.is_empty() {
                    let on = c.selected == "true";
                    row.child(checkbox(("choice", ci * 1000), on, cx.listener(move |this, _, _, cx| {
                        this.pick_choice(ci, (!on).to_string(), cx)
                    })))
                    .child(c.label.clone())
                } else {
                    row.child(format!("{}:", c.label)).children(c.options.iter().enumerate().map(|(oi, (id, label))| {
                        let id = id.clone();
                        chip(
                            ("choice", ci * 1000 + oi + 1),
                            label.clone(),
                            *id == c.selected,
                            cx.listener(move |this, _, _, cx| this.pick_choice(ci, id.clone(), cx)),
                        )
                    }))
                }
            })
            .collect();
        let ok_label = match self.kind {
            Kind::Save { .. } => "Save",
            Kind::Open { directory: true, .. } => "Select",
            Kind::Open { .. } => "Open",
        };

        let name_field = save.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child("Name:")
                .child(
                    div()
                        .id("save-name")
                        .key_context("TextInput")
                        .track_focus(&self.name_focus)
                        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                            this.name_key(ev, window, cx)
                        }))
                        .flex_1()
                        .px_2()
                        .py_1()
                        .bg(rgb(theme::BG))
                        .border_1()
                        .border_color(rgb(theme::ACCENT))
                        .shadow(theme::sunken())
                        .overflow_hidden()
                        .child(self.name.render()),
                )
        });

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .bg(rgb(theme::TITLE_ACTIVE))
            .text_color(rgb(0xffffff))
            .font_weight(FontWeight::BOLD)
            .child(self.title.clone())
            .children((!self.app.is_empty()).then(|| {
                div()
                    .font_weight(FontWeight::NORMAL)
                    .text_xs()
                    .child(format!("for {}", self.app))
            }));

        div()
            // "Workspace" turns on the same app shortcuts as the main window (Ctrl+H, F5, Alt+Left, ...)
            .key_context("Workspace Picker")
            .on_action(cx.listener(|this, _: &OpenSelected, window, cx| this.confirm(window, cx)))
            // the list's navigation shortcuts (the main window handles these in Workspace)
            .on_action(cx.listener(|this, _: &GoUp, _, cx| this.pane.update(cx, |p, cx| p.up(cx))))
            .on_action(cx.listener(|this, _: &GoBack, _, cx| this.pane.update(cx, |p, cx| p.back(cx))))
            .on_action(cx.listener(|this, _: &GoForward, _, cx| this.pane.update(cx, |p, cx| p.forward(cx))))
            .on_action(cx.listener(|this, _: &GoHome, _, cx| this.pane.update(cx, |p, cx| p.go_home(cx))))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.pane.update(cx, |p, cx| p.refresh(cx))))
            .on_action(cx.listener(|this, _: &ToggleHidden, _, cx| {
                this.pane.update(cx, |p, cx| p.toggle_hidden(cx))
            }))
            .on_action(cx.listener(|this, _: &FocusPath, window, cx| {
                this.pane.update(cx, |p, cx| p.start_path_edit(window, cx))
            }))
            .on_action(cx.listener(|this, _: &GoToFolder, window, cx| {
                this.pane.update(cx, |p, cx| p.start_jump(window, cx))
            }))
            .on_action(cx.listener(|this, _: &NewFolder, window, cx| {
                this.pane.update(cx, |p, cx| p.new_folder(window, cx))
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::CRUST))
            .text_color(rgb(theme::TEXT))
            .font_family(cfg.font_family.clone())
            .text_size(px(cfg.font_size as f32))
            .child(header)
            .child(div().flex_1().min_h_0().flex().flex_row().p_2().child(self.pane.clone()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .bg(rgb(theme::MANTLE))
                    .border_t_1()
                    .border_color(rgb(theme::SURFACE0))
                    .children(name_field)
                    .children(filter_row)
                    .children(choice_rows)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(rgb(if self.note.is_some() { theme::ERROR } else { theme::SUBTEXT }))
                                    .children(status),
                            )
                            .child(button(
                                "picker-cancel",
                                "picker-cancel",
                                "Cancel",
                                false,
                                cx.listener(|this, _, window, _| this.cancel(window)),
                            ))
                            .child(button(
                                "picker-ok",
                                "picker-ok",
                                ok_label,
                                true,
                                cx.listener(|this, _, window, cx| this.confirm(window, cx)),
                            )),
                    ),
            )
    }
}
