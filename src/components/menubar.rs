use crate::{
    components::{
        context_menu::{ContextMenu, ContextMenuEvent, MenuEntry},
        icon::image_or,
    },
    menus, theme,
};
use gpui::{prelude::*, *};

const MENUS: &[(&str, fn() -> Vec<MenuEntry>)] = &[
    ("File", menus::file_menu),
    ("Edit", menus::edit_menu),
    ("View", menus::view_menu),
    ("Go", menus::go_menu),
];

/// Top menu bar: File / Edit / View / Go. Hovering another label while a
/// menu is open switches to it, like a native menu bar.
pub struct MenuBar {
    open: Option<usize>,
    dropdown: Option<Entity<ContextMenu>>,
}

impl MenuBar {
    pub fn new() -> Self {
        Self { open: None, dropdown: None }
    }

    fn open_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        let menu = cx.new(|_| ContextMenu::new((MENUS[index].1)()));
        cx.subscribe(&menu, |this, _, _: &ContextMenuEvent, cx| {
            this.open = None;
            this.dropdown = None;
            cx.notify();
        })
        .detach();
        self.open = Some(index);
        self.dropdown = Some(menu);
        cx.notify();
    }
}

impl Render for MenuBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cells: Vec<_> = MENUS
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let is_open = self.open == Some(i);
                let mut cell = div().relative().child(
                    div()
                        .id(("menu-label", i))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .when(is_open, |d| d.bg(rgb(theme::ACCENT)).text_color(rgb(0xffffff)))
                        .hover(|s| s.bg(rgb(theme::ACCENT)).text_color(rgb(0xffffff)))
                        .child(*label)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.open_menu(i, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered && this.open.is_some() && this.open != Some(i) {
                                this.open_menu(i, cx);
                            }
                        })),
                );
                if is_open {
                    if let Some(dropdown) = &self.dropdown {
                        cell = cell.child(
                            deferred(
                                div()
                                    .absolute()
                                    .top(px(30.))
                                    .left_0()
                                    .child(dropdown.clone()),
                            )
                            .with_priority(2),
                        );
                    }
                }
                cell
            })
            .collect();

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .bg(rgb(theme::MANTLE))
            .border_b_1()
            .border_color(rgb(theme::SURFACE0))
            .shadow(theme::raised())
            .text_color(rgb(theme::TEXT))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(theme::ACCENT))
                    .child(image_or("app", px(16.), || div().into_any_element()))
                    .child("Files"),
            )
            .children(cells)
    }
}