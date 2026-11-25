//! "Already exists" dialog for drag and drop: shown over the whole window when a
//! dropped item has the same name as something in the destination folder.
//!
//!   folder:  [Rename] [Merge] [Skip]      file:  [Replace] [Skip]
//!   [x] Apply to all items                       [Cancel]
//!
//! It only collects the answer (`ConflictEvent`); the Workspace feeds it to the
//! running `TransferJob`, which does the actual work.

use crate::{
    components::{
        controls::chip,
        modal::{check_row, frame},
    },
    model, theme,
    transfer::{Choice, Conflict, ConflictKind},
};
use gpui::{prelude::*, *};
use std::path::Path;

pub enum ConflictEvent {
    Resolved { choice: Choice, apply_to_all: bool },
    /// Esc / Cancel: stop the whole drop (what was already done stays done).
    Cancelled,
}

pub struct ConflictDialog {
    conflict: Conflict,
    /// "Folder, modified ..." / "12 KB, modified ..." for the item in the way and the dropped one.
    existing: SharedString,
    incoming: SharedString,
    apply_to_all: bool,
    focus: FocusHandle,
}

impl EventEmitter<ConflictEvent> for ConflictDialog {}

impl Focusable for ConflictDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn describe(path: &Path) -> SharedString {
    match model::entry_for(path) {
        Some(e) => {
            let when = model::format_modified(e.modified);
            if e.is_dir {
                format!("Folder, modified {when}").into()
            } else {
                format!("{}, modified {when}", model::format_size(e.size)).into()
            }
        }
        None => "Not available".into(),
    }
}

impl ConflictDialog {
    pub fn new(conflict: Conflict, cx: &mut Context<Self>) -> Self {
        Self {
            existing: describe(&conflict.existing),
            incoming: describe(&conflict.src),
            conflict,
            apply_to_all: false,
            focus: cx.focus_handle(),
        }
    }

    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        cx.emit(ConflictEvent::Resolved { choice, apply_to_all: self.apply_to_all });
    }

    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        self.apply_to_all = !self.apply_to_all;
        cx.notify();
    }

    fn key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => cx.emit(ConflictEvent::Cancelled),
            "enter" => self.choose(self.conflict.kind.default_choice(), cx),
            "space" => self.toggle_all(cx),
            _ => {}
        }
    }
}

/// `Existing   Folder, modified ...`
fn detail(label: &'static str, text: SharedString) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .child(div().w(px(64.)).flex_shrink_0().font_weight(FontWeight::BOLD).child(label))
        .child(div().min_w_0().truncate().child(text))
}

impl Render for ConflictDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.conflict.kind;
        let default = kind.default_choice();
        let name = self.conflict.name();
        let folder = self.conflict.folder().display().to_string();

        let (title, noun) = match kind {
            ConflictKind::Folder => ("Folder already exists", "A folder"),
            ConflictKind::File => ("File already exists", "A file"),
            ConflictKind::Mismatch => ("Name already in use", "An item"),
        };
        let hint = match kind {
            ConflictKind::Folder => {
                "Rename keeps both folders. Merge puts the contents of the dropped folder into the existing one. Skip leaves it where it is."
            }
            ConflictKind::File => {
                "Replace overwrites the existing file. Skip leaves the dropped file where it is."
            }
            ConflictKind::Mismatch => {
                "One is a file and the other a folder, so they can't be merged or replaced. Rename keeps both."
            }
        };

        let choices = kind.choices().iter().enumerate().map(|(i, &choice)| {
            chip(
                ("conflict-choice", i),
                choice.label(),
                choice == default, // highlighted: this is what Enter does
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.choose(choice, cx);
                    cx.stop_propagation();
                }),
            )
        });

        let rows = vec![
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::BOLD)
                        .child(format!("\u{201c}{name}\u{201d}")),
                )
                .child(div().child(format!("{noun} with this name already exists in")))
                .child(div().truncate().text_color(rgb(theme::SUBTEXT)).child(folder))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .bg(rgb(theme::BG))
                .shadow(theme::sunken())
                .border_1()
                .border_color(rgb(theme::SURFACE0))
                .child(detail("Existing", self.existing.clone()))
                .child(detail("Dropped", self.incoming.clone()))
                .into_any_element(),
            div().text_xs().text_color(rgb(theme::SUBTEXT)).child(hint).into_any_element(),
            check_row(
                "conflict-all",
                "Apply to all items",
                self.apply_to_all,
                cx.listener(|this, _: &MouseDownEvent, _, cx| this.toggle_all(cx)),
            ),
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .mt_1()
                .child(div().flex().flex_row().gap_1().children(choices))
                .child(chip(
                    ("conflict-cancel", 0usize),
                    "Cancel",
                    false,
                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                        cx.emit(ConflictEvent::Cancelled);
                        cx.stop_propagation();
                    }),
                ))
                .into_any_element(),
        ];

        frame(title, 460., rows)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                this.key(ev, cx);
                cx.stop_propagation();
            }))
    }
}
