use crate::{components::icon::image_or, theme};
use gpui::{prelude::*, *};

/// Stateless toolbar button. Pass `cx.listener(...)` as the handler.
///
/// Shows `assets/icons/<icon>.{svg,png}` when that image exists; otherwise
/// falls back to the `label` as plain text.
pub fn button(
    id: impl Into<ElementId>,
    icon: &'static str,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(30.))
        .h(px(28.))
        // always 1px so toggling never shifts the layout; navy when ON
        .border_1()
        .border_color(if active { rgb(theme::ACCENT) } else { rgba(0x00000000) })
        .px_2()
        .mx_px()
        .cursor_pointer()
        .text_color(rgb(theme::TEXT))
        // ON = highlighter yellow + navy frame + sunken + bold: unmistakable next to a plain gray button
        .bg(rgb(if active { theme::HILITE } else { theme::MANTLE }))
        .shadow(if active { theme::sunken() } else { theme::raised() })
        .hover(move |s| s.bg(rgb(if active { 0xffff66 } else { 0xd4d4d4 })))
        .when(active, |d| d.font_weight(FontWeight::BOLD))
        .child(image_or(icon, px(16.), move || div().child(label).into_any_element()))
        .on_mouse_down(MouseButton::Left, on_click)
}