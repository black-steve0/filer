use crate::{
    components::{
        context_menu::{ContextMenu, ContextMenuEvent},
        icon::{find_image, image_or},
        line_edit::{EditOutcome, LineEdit},
    },
    menus, settings, theme,
};
use super::resize::grip;
use gpui::{prelude::*, *};
use std::path::PathBuf;

pub enum SidebarEvent {
    Navigate(PathBuf),
    OpenInOtherPane(PathBuf),
    OpenTerminal(PathBuf),
}

/// Colors "Change Color" cycles through (pastel row backgrounds).
const PALETTE: [u32; 9] = [
    0x89b4fa, 0xcba6f7, 0xf9e2af, 0xa6e3a1, 0xf5c2e7, 0xfab387, 0x94e2d5, 0xf38ba8, 0xb4befe,
];

/// Drag marker: the grip on the sidebar's right edge.
struct SidebarDrag;

/// Default width (Settings > Layout > Sidebar width).
fn default_width() -> f32 {
    settings::get().sidebar_width as f32
}

struct Place {
    name: SharedString,
    path: PathBuf,
    color: u32,
}

/// "Places" list. Add a folder with the "+" button or "Add to Places" in a
/// pane's menu; remove one with right-click -> "Remove from Places".
/// The list is saved to `<config dir>/gpui-explorer/places.txt`.
pub struct Sidebar {
    places: Vec<Place>,
    width: f32,
    active: Option<PathBuf>,
    /// Which place the open context menu belongs to.
    context: Option<usize>,
    menu: Option<(Point<Pixels>, Entity<ContextMenu>)>,
    /// Inline rename: (place index, editor).
    renaming: Option<(usize, LineEdit)>,
    rename_focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    _blur: Option<Subscription>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

fn places_file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("gpui-explorer").join("places.txt"))
}

fn default_places() -> Vec<Place> {
    let candidates = [
        ("Home", dirs::home_dir(), 0x89b4fa),
        ("Desktop", dirs::desktop_dir(), 0xcba6f7),
        ("Documents", dirs::document_dir(), 0xf9e2af),
        ("Downloads", dirs::download_dir(), 0xa6e3a1),
        ("Pictures", dirs::picture_dir(), 0xf5c2e7),
        ("Music", dirs::audio_dir(), 0xfab387),
        ("Videos", dirs::video_dir(), 0x94e2d5),
    ];
    let mut places: Vec<Place> = candidates
        .into_iter()
        .filter_map(|(name, path, color)| path.map(|path| Place { name: name.into(), path, color }))
        .collect();
    places.push(Place { name: "Root".into(), path: PathBuf::from("/"), color: theme::OVERLAY });
    places
}

/// One place per line: `<color hex>\t<name>\t<path>`.
fn load_places() -> Option<Vec<Place>> {
    let text = std::fs::read_to_string(places_file()?).ok()?;
    let places = text
        .lines()
        .filter_map(|line| {
            let mut it = line.splitn(3, '\t');
            let color = u32::from_str_radix(it.next()?, 16).ok()?;
            let name = it.next()?;
            let path = it.next()?;
            Some(Place { name: name.to_string().into(), path: PathBuf::from(path), color })
        })
        .collect();
    Some(places)
}

impl Sidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            places: load_places().unwrap_or_else(default_places),
            width: default_width(),
            active: None,
            context: None,
            menu: None,
            renaming: None,
            rename_focus: cx.focus_handle(),
            return_focus: None,
            _blur: None,
        }
    }

    fn save(&self) {
        let Some(file) = places_file() else { return };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text: String = self
            .places
            .iter()
            .map(|p| format!("{:06x}\t{}\t{}\n", p.color, p.name, p.path.to_string_lossy()))
            .collect();
        let _ = std::fs::write(file, text);
    }

    pub fn reset_width(&mut self, cx: &mut Context<Self>) {
        self.width = default_width();
        cx.notify();
    }

    pub fn set_active(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.active = Some(path);
        cx.notify();
    }

    pub fn add_place(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !path.is_dir() || self.places.iter().any(|p| p.path == path) {
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let color = PALETTE[self.places.len() % PALETTE.len()];
        self.places.push(Place { name: name.into(), path, color });
        self.save();
        cx.notify();
    }

    /// Remove the place whose context menu was opened last.
    pub fn remove_context_place(&mut self, cx: &mut Context<Self>) {
        if let Some(i) = self.context.take() {
            if i < self.places.len() {
                self.places.remove(i);
                self.save();
            }
        }
        self.menu = None;
        cx.notify();
    }

    /// Path of the place whose context menu was opened last.
    fn context_path(&self) -> Option<PathBuf> {
        self.context.and_then(|i| self.places.get(i)).map(|p| p.path.clone())
    }

    pub fn context_open(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.context_path() {
            cx.emit(SidebarEvent::Navigate(p));
        }
    }

    pub fn context_open_in_other_pane(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.context_path() {
            cx.emit(SidebarEvent::OpenInOtherPane(p));
        }
    }

    pub fn context_open_terminal(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.context_path() {
            cx.emit(SidebarEvent::OpenTerminal(p));
        }
    }

    pub fn context_copy_path(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.context_path() {
            cx.write_to_clipboard(ClipboardItem::new_string(p.to_string_lossy().into_owned()));
        }
    }

    pub fn context_change_color(&mut self, cx: &mut Context<Self>) {
        if let Some(place) = self.context.and_then(|i| self.places.get_mut(i)) {
            let next = PALETTE.iter().position(|c| *c == place.color).map_or(0, |i| i + 1);
            place.color = PALETTE[next % PALETTE.len()];
            self.save();
            cx.notify();
        }
    }

    pub fn context_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(i) = self.context {
            let j = i as isize + delta;
            if j >= 0 && (j as usize) < self.places.len() {
                self.places.swap(i, j as usize);
                self.context = Some(j as usize);
                self.save();
                cx.notify();
            }
        }
    }

    /// Start editing the name of the place that was right-clicked.
    /// `return_focus` gets the keyboard back when editing ends.
    pub fn context_rename(
        &mut self,
        return_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(i) = self.context else { return };
        let Some(name) = self.places.get(i).map(|p| p.name.to_string()) else { return };
        self.menu = None;
        self.renaming = Some((i, LineEdit::new(name)));
        self.return_focus = Some(return_focus);
        window.focus(&self.rename_focus);
        self._blur = Some(cx.on_blur(&self.rename_focus, window, |this, _, cx| {
            // clicked elsewhere: drop the edit
            if this.renaming.take().is_some() {
                cx.notify();
            }
        }));
        cx.notify();
    }

    fn finish_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.renaming = None;
        if let Some(f) = self.return_focus.take() {
            window.focus(&f);
        }
        cx.notify();
    }

    fn rename_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some((index, edit)) = self.renaming.as_mut() else { return };
        let index = *index;
        match edit.handle_key(ev, cx) {
            EditOutcome::Submit => {
                let name = edit.text.trim().to_string();
                if !name.is_empty() {
                    if let Some(p) = self.places.get_mut(index) {
                        p.name = name.into();
                    }
                    self.save();
                }
                self.finish_rename(window, cx);
            }
            EditOutcome::Cancel => self.finish_rename(window, cx),
            EditOutcome::Edited => cx.notify(),
            EditOutcome::Ignored => {}
        }
    }

    fn open_menu(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let menu = cx.new(|_| ContextMenu::new(menus::place_menu()));
        cx.subscribe(&menu, |this: &mut Sidebar, _, _: &ContextMenuEvent, cx| {
            this.menu = None;
            cx.notify();
        })
        .detach();
        self.context = Some(index);
        self.menu = Some((position, menu));
        cx.notify();
    }
}

/// `assets/icons/place-<name>.{svg,png}` (e.g. `place-home`). No dot any
/// more: without an image the row's colored background is the marker.
fn place_icon(name: &str) -> Option<AnyElement> {
    let key = format!("place-{}", name.to_lowercase());
    find_image(&key).map(|_| image_or(&key, px(16.), || div().w(px(16.)).into_any_element()))
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items: Vec<_> = self
            .places
            .iter()
            .enumerate()
            .map(|(i, place)| {
                let target = place.path.clone();
                let selected = self.active.as_ref() == Some(&place.path);
                let color = place.color;
                let editing = matches!(&self.renaming, Some((ri, _)) if *ri == i);

                let label: AnyElement = match &self.renaming {
                    Some((ri, edit)) if *ri == i => div()
                        .id("place-rename")
                        .key_context("TextInput")
                        .track_focus(&self.rename_focus)
                        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                            this.rename_key(ev, window, cx)
                        }))
                        .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.stop_propagation()))
                        .on_mouse_down(MouseButton::Right, cx.listener(|_, _, _, cx| cx.stop_propagation()))
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .px_1()
                        .bg(rgb(theme::BG))
                        .border_1()
                        .border_color(rgb(theme::ACCENT))
                        .text_color(rgb(theme::TEXT))
                        .child(edit.render())
                        .into_any_element(),
                    _ => div()
                        .min_w_0()
                        .truncate()
                        .child(place.name.clone())
                        .into_any_element(),
                };

                div()
                    .id(("place", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .text_color(rgb(if selected { theme::TEXT } else { theme::LINK }))
                    .when(!editing, |d| d.underline())
                    // the place's color is its row background: faint at rest,
                    // stronger on hover, solid-ish when it is the open folder
                    .bg(theme::alpha(color, if selected { 0xd0 } else { 0x55 }))
                    // always 1px so selecting never shifts the layout
                    .border_1()
                    .border_color(if selected {
                        rgb(theme::ACCENT)
                    } else {
                        rgba(0x00000000)
                    })
                    .when(selected, |d| d.font_weight(FontWeight::BOLD))
                    .when(!selected, |d| {
                        d.hover(move |s| {
                            s.bg(theme::alpha(color, 0x99)).text_color(rgb(theme::LINK_HOVER))
                        })
                    })
                    .children(place_icon(&place.name))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(SidebarEvent::Navigate(target.clone()))
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.open_menu(i, ev.position, cx);
                            cx.stop_propagation();
                        }),
                    )
            })
            .collect();

        let menu = self.menu.as_ref().map(|(position, entity)| {
            deferred(
                anchored()
                    .position(*position)
                    .anchor(Corner::TopLeft)
                    .snap_to_window()
                    .child(entity.clone()),
            )
            .with_priority(1)
        });

        div()
            .flex()
            .flex_col()
            .relative()
            .w(px(self.width))
            .h_full()
            .on_drag_move::<SidebarDrag>(cx.listener(
                |this, ev: &DragMoveEvent<SidebarDrag>, _, cx| {
                    let w = f32::from(ev.event.position.x) - f32::from(ev.bounds.left());
                    this.width = w.clamp(90., 500.);
                    cx.notify();
                },
            ))
            .flex_shrink_0()
            .p_2()
            .gap_0p5()
            .bg(rgb(theme::BG))
            .border_1()
            .border_color(rgb(theme::SURFACE0))
            .shadow(theme::sunken())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_0p5()
                    .mb_1()
                    .bg(rgb(theme::TITLE_ACTIVE))
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(0xffffff))
                    .child("PLACES")
                    .child(
                        // "+" adds the active pane's current (or selected) folder
                        div()
                            .id("add-place")
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(18.))
                            .h(px(18.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(theme::MANTLE)).text_color(rgb(theme::TEXT)))
                            .child(image_or("add-place", px(14.), || {
                                div().child("+").into_any_element()
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _, window, cx| {
                                    window.dispatch_action(
                                        crate::actions::AddPlace.boxed_clone(),
                                        cx,
                                    );
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .id("places-list")
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(items),
            )
            // resize grip on the right edge
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .child(grip("sidebar-grip", SidebarDrag)),
            )
            .children(menu)
    }
}