//! Properties dialog for one item (type, where, size, dates, owner, permissions) or
//! for a selection (how many items, total size).
//!
//! Folder sizes are added up on a background thread and shown as they grow; closing
//! the dialog cancels the count. For a single item the read / write / execute boxes
//! change its permissions straight away.

use crate::{
    components::{
        controls::{checkbox, chip},
        modal::frame,
    },
    filetype, model,
    props::{self, Stat, Usage},
    theme,
};
use gpui::{prelude::*, *};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

pub enum PropertiesEvent {
    Close,
    /// A permission box was changed: the folder listing it should be refreshed.
    PermissionsChanged(PathBuf),
}

pub struct PropertiesDialog {
    items: Vec<PathBuf>,
    /// What could be read about each item (an item that vanished is left out).
    stats: Vec<Stat>,
    /// "PNG image (image/png)" for one item.
    kind: SharedString,
    usage: Usage,
    /// The size count is still running.
    measuring: bool,
    /// Current permission bits of the single item.
    mode: u32,
    error: Option<SharedString>,
    cancel: Arc<AtomicBool>,
    _tasks: Vec<Task<()>>,
    focus: FocusHandle,
}

impl EventEmitter<PropertiesEvent> for PropertiesDialog {}

impl Focusable for PropertiesDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for PropertiesDialog {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst); // stop counting a big folder
    }
}

/// `12.3 KB (12,345 bytes)`
fn size_text(bytes: u64) -> String {
    if bytes < 1000 {
        format!("{bytes} bytes")
    } else {
        format!("{} ({} bytes)", model::format_size(bytes), props::group_digits(bytes))
    }
}

fn time_text(t: Option<std::time::SystemTime>) -> String {
    let s = model::format_modified(t);
    if s.is_empty() {
        "\u{2014}".into()
    } else {
        s
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl PropertiesDialog {
    pub fn new(items: Vec<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut stats = Vec::new();
        let mut error = None;
        for path in &items {
            match props::stat(path) {
                Ok(s) => stats.push(s),
                Err(e) => {
                    error.get_or_insert_with(|| {
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        SharedString::from(format!("Couldn't read \u{201c}{name}\u{201d}: {e}"))
                    });
                }
            }
        }
        let kind = match stats.as_slice() {
            [s] => {
                let label = model::entry_for(&s.path).map(|e| e.kind).unwrap_or_default();
                match filetype::mime_for(&s.name, s.is_dir) {
                    Some(m) => format!("{label} ({m})"),
                    None => label,
                }
            }
            _ => String::new(),
        };
        // a lone file needs no counting; folders and selections do
        let needs_count = stats.len() > 1 || stats.first().is_some_and(|s| s.is_dir);
        let mut dialog = Self {
            mode: stats.first().map(|s| s.mode).unwrap_or(0),
            usage: Usage::default(),
            measuring: needs_count,
            items: stats.iter().map(|s| s.path.clone()).collect(),
            stats,
            kind: kind.into(),
            error,
            cancel: Arc::new(AtomicBool::new(false)),
            _tasks: Vec::new(),
            focus: cx.focus_handle(),
        };
        if needs_count {
            dialog.start_count(cx);
        }
        dialog
    }

    /// Add up the sizes in the background; a poller copies the running total into the dialog.
    fn start_count(&mut self, cx: &mut Context<Self>) {
        let paths = self.items.clone();
        let cancel = self.cancel.clone();
        let shared = Arc::new(Mutex::new(Usage::default()));
        let finished = Arc::new(AtomicBool::new(false));
        let (shared_bg, finished_bg) = (shared.clone(), finished.clone());

        let work = cx.background_executor().spawn(async move {
            let total = props::measure(&paths, &cancel, &mut |u| {
                if let Ok(mut s) = shared_bg.lock() {
                    *s = u.clone();
                }
            });
            if let Ok(mut s) = shared_bg.lock() {
                *s = total;
            }
            finished_bg.store(true, Ordering::SeqCst);
        });
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let done = finished.load(Ordering::SeqCst);
            let snapshot = shared.lock().map(|s| s.clone()).unwrap_or_default();
            let alive = this
                .update(cx, |d, cx| {
                    d.usage = snapshot;
                    d.measuring = !done;
                    cx.notify();
                })
                .is_ok();
            if done || !alive {
                break;
            }
        });
        self._tasks = vec![work, poll];
    }

    /// Switch one permission box. The change is applied at once; if the system refuses
    /// (not the owner, read-only file system), the box stays as it was.
    fn toggle(&mut self, who: usize, what: usize, cx: &mut Context<Self>) {
        let Some(stat) = self.stats.first().filter(|_| self.stats.len() == 1) else { return };
        let new = props::with_bit(self.mode, who, what, !props::has_bit(self.mode, who, what));
        match props::set_mode(&stat.path, new) {
            Ok(()) => {
                self.mode = new;
                self.error = None;
                cx.emit(PropertiesEvent::PermissionsChanged(stat.path.clone()));
            }
            Err(e) => self.error = Some(format!("Couldn't change permissions: {e}").into()),
        }
        cx.notify();
    }

    fn key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        if matches!(ev.keystroke.key.as_str(), "escape" | "enter") {
            cx.emit(PropertiesEvent::Close);
        }
    }

    /// The "Size" lines, which depend on whether the count has finished.
    fn size_rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        let u = &self.usage;
        let counting = if self.measuring { " \u{2026}" } else { "" };
        match self.stats.as_slice() {
            [s] if !s.is_dir => {
                rows.push(("Size", size_text(s.size)));
            }
            [_] => {
                rows.push(("Size", format!("{}{counting}", size_text(u.bytes))));
                // the folder itself is counted too: it isn't one of its own contents
                let folders = u.folders.saturating_sub(1);
                rows.push((
                    "Contents",
                    format!("{}, {}{counting}", plural(u.files, "file", "files"), plural(folders, "folder", "folders")),
                ));
                rows.push(("On disk", format!("{}{counting}", model::format_size(u.on_disk))));
            }
            _ => {
                rows.push(("Total size", format!("{}{counting}", size_text(u.bytes))));
                rows.push((
                    "Contents",
                    format!("{}, {}{counting}", plural(u.files, "file", "files"), plural(u.folders, "folder", "folders")),
                ));
                rows.push(("On disk", format!("{}{counting}", model::format_size(u.on_disk))));
            }
        }
        if u.unreadable > 0 && !self.measuring {
            rows.push(("Unreadable", plural(u.unreadable, "item", "items")));
        }
        rows
    }

    fn general_rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        match self.stats.as_slice() {
            [] => {}
            [s] => {
                rows.push(("Type", self.kind.to_string()));
                rows.push((
                    "Location",
                    s.parent.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "/".into()),
                ));
                if let (true, Some(target)) = (s.is_symlink, &s.link_target) {
                    let broken = if s.link_broken { " (missing)" } else { "" };
                    rows.push(("Link to", format!("{}{broken}", target.display())));
                }
                rows.extend(self.size_rows());
                rows.push(("Modified", time_text(s.modified)));
                rows.push(("Accessed", time_text(s.accessed)));
                rows.push(("Created", time_text(s.created)));
                rows.push(("Owner", s.owner.clone()));
                rows.push(("Group", s.group.clone()));
            }
            many => {
                let paths: Vec<PathBuf> = many.iter().map(|s| s.path.clone()).collect();
                rows.push(("Selected", plural(many.len() as u64, "item", "items")));
                rows.push((
                    "Location",
                    props::common_parent(&paths)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Several folders".into()),
                ));
                rows.extend(self.size_rows());
            }
        }
        rows
    }
}

/// `Label      value`, value cut short with an ellipsis when it doesn't fit.
fn prop_row(label: &'static str, value: String) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .px_2()
        .py_0p5()
        .child(div().w(px(78.)).flex_shrink_0().font_weight(FontWeight::BOLD).child(label))
        .child(div().min_w_0().truncate().child(value))
        .into_any_element()
}

impl Render for PropertiesDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.stats.as_slice() {
            [s] => format!("{} Properties", s.name),
            _ => "Properties".to_string(),
        };
        let heading = match self.stats.as_slice() {
            [s] => s.name.clone(),
            [] => String::new(),
            many => plural(many.len() as u64, "item", "items"),
        };

        let mut body: Vec<AnyElement> = Vec::new();
        if !heading.is_empty() {
            body.push(div().truncate().font_weight(FontWeight::BOLD).child(heading).into_any_element());
        }
        let rows = self.general_rows();
        if !rows.is_empty() {
            body.push(
                div()
                    .flex()
                    .flex_col()
                    .py_1()
                    .bg(rgb(theme::BG))
                    .shadow(theme::sunken())
                    .border_1()
                    .border_color(rgb(theme::SURFACE0))
                    .children(rows.into_iter().map(|(label, value)| prop_row(label, value)))
                    .into_any_element(),
            );
        }

        // permissions: only for a single item
        if let [stat] = self.stats.as_slice() {
            if !stat.link_broken {
                let mode = self.mode;
                let cell = || div().w(px(64.)).flex().justify_center();
                let header = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(div().w(px(64.)))
                    .children(props::WHAT.iter().map(|w| cell().text_xs().font_weight(FontWeight::BOLD).child(*w)));
                let grid = props::WHO.iter().enumerate().map(|(who, label)| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .child(div().w(px(64.)).child(*label))
                        .children((0..3).map(|what| {
                            cell().child(checkbox(
                                ("perm", who * 3 + what),
                                props::has_bit(mode, who, what),
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.toggle(who, what, cx);
                                    cx.stop_propagation();
                                }),
                            ))
                        }))
                });
                body.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .bg(rgb(theme::BG))
                        .shadow(theme::sunken())
                        .border_1()
                        .border_color(rgb(theme::SURFACE0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_between()
                                .child(div().font_weight(FontWeight::BOLD).child("Permissions"))
                                .child(div().text_color(rgb(theme::SUBTEXT)).child(format!(
                                    "{}  ({})",
                                    props::rwx(mode),
                                    props::octal(mode)
                                ))),
                        )
                        .child(header)
                        .children(grid)
                        .into_any_element(),
                );
            }
        }

        if let Some(err) = &self.error {
            body.push(div().text_color(rgb(theme::ERROR)).child(err.clone()).into_any_element());
        }
        body.push(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .mt_1()
                .child(chip(
                    ("properties-close", 0usize),
                    "Close",
                    true,
                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                        cx.emit(PropertiesEvent::Close);
                        cx.stop_propagation();
                    }),
                ))
                .into_any_element(),
        );

        frame(title, 440., body)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                this.key(ev, cx);
                cx.stop_propagation();
            }))
    }
}
