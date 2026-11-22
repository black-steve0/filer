//! Small stateless form widgets for the Settings window, in the same retro
//! bevelled style as the toolbar. Like `button`, they take a click handler
//! (usually `cx.listener(...)`) and hold no state of their own.

use crate::theme;
use gpui::{prelude::*, *};

/// A bevelled option button. `active` = highlighter yellow, sunken, bold
/// (the same look as the toolbar toggles). Used for choices and +/- steppers.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label: SharedString = label.into();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(26.))
        .h(px(24.))
        .px_2()
        .cursor_pointer()
        // always 1px so toggling never shifts the layout
        .border_1()
        .border_color(if active { rgb(theme::ACCENT) } else { rgba(0x00000000) })
        .text_color(rgb(theme::TEXT))
        .bg(rgb(if active { theme::HILITE } else { theme::MANTLE }))
        .shadow(if active { theme::sunken() } else { theme::raised() })
        .hover(move |s| s.bg(rgb(if active { 0xffff66 } else { 0xd4d4d4 })))
        .when(active, |d| d.font_weight(FontWeight::BOLD))
        .child(label)
        .on_mouse_down(MouseButton::Left, on_click)
}

/// Sunken square with a check mark when `checked`.
pub fn checkbox(
    id: impl Into<ElementId>,
    checked: bool,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .w(px(18.))
        .h(px(18.))
        .cursor_pointer()
        .bg(rgb(theme::BG))
        .shadow(theme::sunken())
        .border_1()
        .border_color(rgb(theme::SURFACE0))
        .text_color(rgb(theme::TEXT))
        .font_weight(FontWeight::BOLD)
        .hover(|s| s.bg(rgb(theme::HOVER_ROW)))
        .child(if checked { "\u{2713}" } else { "" })
        .on_mouse_down(MouseButton::Left, on_click)
}

/// `[-]  12 px  [+]`
pub fn stepper(
    id: &'static str,
    value: u32,
    unit: &'static str,
    minus: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    plus: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(chip((id, 0usize), "\u{2212}", false, minus))
        .child(
            div()
                .min_w(px(72.))
                .flex()
                .justify_center()
                .font_weight(FontWeight::BOLD)
                .child(format!("{value}{unit}")),
        )
        .child(chip((id, 1usize), "+", false, plus))
        .into_any_element()
}

/// One setting: label (and optional hint) on the left, its control on the right.
pub fn row(label: &'static str, hint: &'static str, control: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_4()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(rgb(0xe0e0e0))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().font_weight(FontWeight::BOLD).child(label))
                .children((!hint.is_empty()).then(|| {
                    div().text_xs().text_color(rgb(theme::SUBTEXT)).child(hint)
                })),
        )
        .child(div().flex().flex_row().items_center().gap_1().flex_shrink_0().child(control))
        .into_any_element()
}

/// Like `row`, but the control sits below the label and may wrap onto several
/// lines (for long option lists such as date formats).
pub fn wide_row(label: &'static str, hint: &'static str, controls: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(rgb(0xe0e0e0))
        .child(div().font_weight(FontWeight::BOLD).child(label))
        .children((!hint.is_empty()).then(|| {
            div().text_xs().text_color(rgb(theme::SUBTEXT)).child(hint)
        }))
        .child(div().flex().flex_row().flex_wrap().gap_1().children(controls))
        .into_any_element()
}

/// Read-only `label ........ value` line (shortcuts, file paths).
pub fn info_row(label: impl Into<SharedString>, value: impl Into<SharedString>) -> AnyElement {
    let (label, value): (SharedString, SharedString) = (label.into(), value.into());
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_4()
        .px_3()
        .py_1()
        .border_b_1()
        .border_color(rgb(0xe0e0e0))
        .child(div().min_w_0().truncate().child(label))
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(theme::SUBTEXT))
                .child(value),
        )
        .into_any_element()
}

/// A titled panel: navy title bar (like the sidebar's PLACES header) over white rows.
pub fn group(title: &'static str, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .mb_3()
        .bg(rgb(theme::BG))
        .border_1()
        .border_color(rgb(theme::SURFACE0))
        .shadow(theme::sunken())
        .child(
            div()
                .px_2()
                .py_0p5()
                .bg(rgb(theme::TITLE_ACTIVE))
                .text_color(rgb(0xffffff))
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .child(title),
        )
        .children(rows)
        .into_any_element()
}
