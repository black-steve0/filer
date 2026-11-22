//! A list of commands. Used for both the right-click menu and the menu-bar
//! dropdowns. Choosing an item dispatches its action; the owner just listens
//! for `Dismiss` to close the menu.

use crate::{components::icon::image_or, theme};
use gpui::*;

pub enum MenuEntry {
    Item {
        /// `assets/icons/<icon>.{svg,png}`; blank space is kept when missing.
        icon: &'static str,
        label: &'static str,
        shortcut: &'static str,
        action: Box<dyn Action>,
    },
    Separator,
}

pub enum ContextMenuEvent {
    Dismiss,
}

pub struct ContextMenu {
    entries: Vec<MenuEntry>,
}

impl EventEmitter<ContextMenuEvent> for ContextMenu {}

impl ContextMenu {
    pub fn new(entries: Vec<MenuEntry>) -> Self {
        Self { entries }
    }
}

/// 14px icon slot. Always takes the same width so labels stay aligned.
fn menu_icon(name: &'static str) -> AnyElement {
    let blank = || div().w(px(14.)).h(px(14.)).flex_shrink_0().into_any_element();
    if name.is_empty() {
        blank()
    } else {
        image_or(name, px(14.), blank)
    }
}

impl Render for ContextMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| match entry {
                MenuEntry::Separator => div()
                    .h(px(1.))
                    .my_1()
                    .bg(rgb(theme::SURFACE0))
                    .into_any_element(),
                MenuEntry::Item { icon, label, shortcut, .. } => div()
                    .id(("menu-item", i))
                    .group(format!("menu-item-{i}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::ACCENT)).text_color(rgb(0xffffff)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(menu_icon(*icon))
                            // group_hover: label and shortcut turn white with the row
                            .child(
                                div()
                                    .group_hover(format!("menu-item-{i}"), |s| {
                                        s.text_color(rgb(0xffffff))
                                    })
                                    .child(*label),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .group_hover(format!("menu-item-{i}"), |s| s.text_color(rgb(0xffffff)))
                            .child(*shortcut),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if let Some(MenuEntry::Item { action, .. }) = this.entries.get(i) {
                                window.dispatch_action(action.boxed_clone(), cx);
                            }
                            cx.emit(ContextMenuEvent::Dismiss);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element(),
            })
            .collect();

        div()
            .occlude()
            .min_w(px(230.))
            .p_1()
            .bg(rgb(theme::MANTLE))
            .border_1()
            .border_color(rgb(0x000000))
            .shadow(theme::raised())
            .text_color(rgb(theme::TEXT))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ContextMenuEvent::Dismiss)))
            .children(rows)
    }
}