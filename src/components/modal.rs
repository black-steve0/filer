//! The window every dialog shares: a dimmed backdrop that covers the whole app and
//! swallows the mouse, with a bevelled panel and navy title bar in the middle.
//!
//! A dialog's `render` builds its content, wraps it with `frame`, and adds its own
//! `.track_focus(..)` and `.on_key_down(..)` to the result.

use crate::{components::controls::checkbox, theme};
use gpui::{prelude::*, *};
use std::rc::Rc;

/// Backdrop + panel. `rows` are stacked inside the panel with a small gap.
///
/// The backdrop `occlude`s the mouse, so the panes behind can't be used while a
/// dialog is open. `key_context("TextInput")` switches the pane/app shortcuts off,
/// the same way it does while a text field is focused.
pub fn frame(title: impl Into<SharedString>, width: f32, rows: Vec<AnyElement>) -> Div {
    let panel = div()
        .flex()
        .flex_col()
        .w(px(width))
        .max_w_full()
        .max_h_full()
        .bg(rgb(theme::MANTLE))
        .border_1()
        .border_color(rgb(0x000000))
        .shadow(theme::raised())
        .text_color(rgb(theme::TEXT))
        .child(
            div()
                .px_2()
                .py_0p5()
                .bg(rgb(theme::TITLE_ACTIVE))
                .text_color(rgb(0xffffff))
                .font_weight(FontWeight::BOLD)
                .child(title.into()),
        )
        // scrolls when the window is too small for the content
        .child(
            div()
                .id("modal-body")
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .min_h_0()
                .overflow_y_scroll()
                .children(rows),
        );

    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::alpha(0x000000, 0x66))
        .key_context("TextInput")
        .child(panel)
}

/// A check box with a clickable label. The two are separate hit areas, so one click
/// never toggles twice.
pub fn check_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    checked: bool,
    on_toggle: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_toggle = Rc::new(on_toggle);
    let on_label = on_toggle.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(checkbox(id, checked, move |ev, window, cx| {
            on_toggle(ev, window, cx);
            cx.stop_propagation();
        }))
        .child(
            div()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                    on_label(ev, window, cx);
                    cx.stop_propagation();
                })
                .child(label.into()),
        )
        .into_any_element()
}
