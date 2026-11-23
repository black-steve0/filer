//! Root view: owns the components, wires their events together and handles
//! every app-level command (actions). Components never reference each other.

use crate::{
    actions::*,
    components::{
        dnd::FileDrag,
        menubar::MenuBar,
        pane::{Pane, PaneEvent},
        resize::grip,
        sidebar::{Sidebar, SidebarEvent},
        statusbar::StatusBar,
        toolbar::Toolbar,
    },
    dragout, model,
    settings::{self, Settings, SettingsChanged},
    theme,
};
use gpui::{prelude::*, *};
use std::{path::PathBuf, sync::Arc};

/// Drag marker: the grip to the right of pane `.0`.
struct PaneDrag(usize);

/// Narrowest a dragged pane can get.
const MIN_PANE: f32 = 160.;

pub struct Workspace {
    menubar: Entity<MenuBar>,
    toolbar: Entity<Toolbar>,
    sidebar: Entity<Sidebar>,
    status: Entity<StatusBar>,
    panes: Vec<Entity<Pane>>, // any number, at least one
    active: usize,
    /// Set when the focused pane was just closed: re-focus on the next render.
    refocus: bool,
    /// Settings as of the last change, to tell what the user just touched.
    prev: Arc<Settings>,
}

/// Run a no-window method on the active pane when `$action` fires.
macro_rules! on_pane {
    ($cx:expr, $action:ty, $method:ident) => {
        $cx.listener(|this: &mut Workspace, _: &$action, _window, cx| {
            let pane = this.active_pane();
            pane.update(cx, |p, cx| p.$method(cx));
        })
    };
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let start = settings::get().startup_dir();

        let menubar = cx.new(|_| MenuBar::new());
        let toolbar = cx.new(|_| Toolbar::default());
        let sidebar = cx.new(|cx| Sidebar::new(cx));
        let status = cx.new(|_| StatusBar::default());

        cx.subscribe(&sidebar, Self::on_sidebar).detach();
        // every change made in the Settings window lands here
        cx.observe_global::<SettingsChanged>(|this, cx| this.on_settings(cx)).detach();

        let mut this = Self {
            menubar,
            toolbar,
            sidebar,
            status,
            panes: Vec::new(),
            active: 0,
            refocus: false,
            prev: settings::get(),
        };
        let first = this.new_pane(start, cx);
        this.panes.push(first);
        this.set_active(0, cx);
        this
    }

    fn new_pane(&self, path: PathBuf, cx: &mut Context<Self>) -> Entity<Pane> {
        let pane = cx.new(|cx| Pane::new(path, cx));
        cx.subscribe(&pane, Self::on_pane).detach();
        pane
    }

    fn active_pane(&self) -> Entity<Pane> {
        self.panes[self.active].clone()
    }

    pub fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.panes[self.active].focus_handle(cx);
        window.focus(&handle);
    }

    fn set_active(&mut self, index: usize, cx: &mut Context<Self>) {
        self.active = index;
        for (i, pane) in self.panes.iter().enumerate() {
            pane.update(cx, |p, cx| p.set_active(i == index, cx));
        }
        self.refresh_chrome(cx);
    }

    /// Sync toolbar toggles, sidebar highlight and status bar with the active pane.
    fn refresh_chrome(&mut self, cx: &mut Context<Self>) {
        let multi = self.panes.len() > 1;
        let (path, summary, hidden) = {
            let p = self.panes[self.active].read(cx);
            (p.path().to_path_buf(), p.summary(), p.show_hidden())
        };
        self.toolbar.update(cx, |t, cx| t.set_state(hidden, cx));
        // each pane shows its own X button only while there is more than one
        for pane in &self.panes {
            pane.update(cx, |p, cx| p.set_closable(multi, cx));
        }
        self.sidebar.update(cx, |s, cx| s.set_active(path, cx));
        self.status.update(cx, |s, cx| s.set_text(summary, cx));
    }

    // ---- commands that need more than one pane / the window --------------

    /// Open another pane (next to the active one) showing `path`.
    /// Returns its index. Does not change which pane is active.
    fn add_pane(&mut self, path: PathBuf, cx: &mut Context<Self>) -> usize {
        let pane = self.new_pane(path, cx);
        let at = self.active + 1;
        self.panes.insert(at, pane);
        cx.notify();
        at
    }

    fn split_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.panes[self.active].read(cx).path().to_path_buf();
        let at = self.add_pane(path, cx);
        self.set_active(at, cx);
        self.focus_active(window, cx);
    }

    /// Close the pane at `index` (the active one for Ctrl+W / the menu, or
    /// whichever pane's X button was clicked).
    fn close_pane_at(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.panes.len() < 2 || index >= self.panes.len() {
            return;
        }
        self.panes.remove(index);
        // keep the same pane active if it survived, else its left neighbour
        self.active = match self.active.cmp(&index) {
            std::cmp::Ordering::Greater => self.active - 1,
            std::cmp::Ordering::Equal => index.saturating_sub(1).min(self.panes.len() - 1),
            std::cmp::Ordering::Less => self.active,
        };
        self.set_active(self.active, cx);
        self.refocus = true; // keyboard focus is restored on the next render
        cx.notify();
    }

    fn close_pane(&mut self, cx: &mut Context<Self>) {
        self.close_pane_at(self.active, cx);
    }

    fn switch_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() < 2 {
            return;
        }
        let next = (self.active + 1) % self.panes.len();
        self.set_active(next, cx);
        self.focus_active(window, cx);
    }

    fn open_in_other_pane(&mut self, cx: &mut Context<Self>) {
        let dir = {
            let pane = self.active_pane();
            pane.read(cx).selected_dir()
        };
        if let Some(dir) = dir {
            self.open_dir_in_other_pane(dir, cx);
        }
    }

    fn open_dir_in_other_pane(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if self.panes.len() > 1 {
            let other = (self.active + 1) % self.panes.len();
            self.panes[other].update(cx, |p, cx| p.navigate(dir, cx));
        } else {
            self.add_pane(dir, cx);
        }
        self.refresh_chrome(cx);
        cx.notify();
    }

    fn add_place(&mut self, cx: &mut Context<Self>) {
        let path = {
            let pane = self.panes[self.active].read(cx);
            pane.selected_dir().unwrap_or_else(|| pane.path().to_path_buf())
        };
        self.sidebar.update(cx, |s, cx| s.add_place(path, cx));
    }

    // ---- event handlers --------------------------------------------------

    /// Settings changed: bring the panes and the chrome in line. Appearance
    /// values (fonts, bars shown, ...) are read again by `render`.
    fn on_settings(&mut self, cx: &mut Context<Self>) {
        let cur = settings::get();
        let prev = std::mem::replace(&mut self.prev, cur.clone());
        if prev.sidebar_width != cur.sidebar_width {
            self.sidebar.update(cx, |s, cx| s.reset_width(cx));
        }
        for pane in &self.panes {
            pane.update(cx, |p, cx| p.settings_changed(&prev, &cur, cx));
        }
        self.refresh_chrome(cx);
        cx.notify();
    }

    fn on_sidebar(&mut self, _: Entity<Sidebar>, event: &SidebarEvent, cx: &mut Context<Self>) {
        match event {
            SidebarEvent::Navigate(path) => {
                let path = path.clone();
                self.active_pane().update(cx, |p, cx| p.navigate(path, cx));
            }
            SidebarEvent::OpenInOtherPane(path) => self.open_dir_in_other_pane(path.clone(), cx),
            SidebarEvent::OpenTerminal(path) => {
                let path = path.clone();
                self.active_pane().update(cx, |p, cx| p.open_terminal_at(path, cx));
            }
            SidebarEvent::DropInto { paths, dest, copy } => {
                self.transfer(paths.clone(), dest.clone(), *copy, cx);
            }
        }
    }

fn on_pane(&mut self, pane: Entity<Pane>, event: &PaneEvent, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Activated => {
                if let Some(i) = self.panes.iter().position(|p| *p == pane) {
                    if i != self.active {
                        self.set_active(i, cx);
                    }
                }
            }
            PaneEvent::CloseRequested => {
                if let Some(i) = self.panes.iter().position(|p| *p == pane) {
                    self.close_pane_at(i, cx);
                }
            }
            PaneEvent::Changed => {
                if self.panes[self.active] == pane {
                    self.refresh_chrome(cx);
                }
            }
            PaneEvent::Dropped { paths, dest, copy } => {
                self.transfer(paths.clone(), dest.clone(), *copy, cx);
            }
        }
    }

    /// Copy or move `paths` into `dest` (a drop from another split, from the
    /// sidebar, or from another application), then bring every pane that
    /// shows an affected folder up to date.
    fn transfer(&mut self, paths: Vec<PathBuf>, dest: PathBuf, copy: bool, cx: &mut Context<Self>) {
        let result = model::transfer(&paths, &dest, copy);
        for pane in &self.panes {
            pane.update(cx, |p, cx| p.refresh_if_showing(&result.touched, cx));
        }
        // report failures in the pane that was dropped on (else the active one);
        // set after the refresh, because a reload clears the pane's message
        if !result.errors.is_empty() {
            let target = self
                .panes
                .iter()
                .find(|p| p.read(cx).path() == dest.as_path())
                .cloned()
                .unwrap_or_else(|| self.active_pane());
            let msg = format!("Drop failed: {}", result.errors.join("; "));
            target.update(cx, |p, cx| p.set_error(msg, cx));
        }
        self.refresh_chrome(cx);
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.refocus) {
            self.focus_active(window, cx);
        }

        // Settings > Appearance > Font size: rem-based sizes (text-xs, spacing) scale with it
        let cfg = settings::get();
        window.set_rem_size(px(16. * cfg.font_size as f32 / 12.));

        // Each pane sits in a wrapper that owns its width; between two panes
        // there is a grip. Dragging it sets the left pane's width to
        // "mouse x minus the wrapper's left edge".
        let last = self.panes.len().saturating_sub(1);
        let mut pane_els: Vec<AnyElement> = Vec::new();
        for (i, pane) in self.panes.iter().enumerate() {
            let width = pane.read(cx).width();
            let wrap = div().id(("pane-wrap", i)).flex().h_full().on_drag_move::<PaneDrag>(
                cx.listener(move |this, ev: &DragMoveEvent<PaneDrag>, _, cx| {
                    if ev.drag(&*cx).0 != i {
                        return;
                    }
                    let w = (f32::from(ev.event.position.x) - f32::from(ev.bounds.left()))
                        .max(MIN_PANE);
                    if let Some(p) = this.panes.get(i) {
                        p.update(cx, |p, cx| p.set_width(Some(w), cx));
                    }
                }),
            );
            let wrap = match width {
                Some(w) => wrap.w(px(w)).min_w(px(MIN_PANE)).flex_shrink_0(),
                None => wrap.flex_1().min_w(px(720.)), // room for all the columns
            };
            pane_els.push(wrap.child(pane.clone()).into_any_element());
            if i < last {
                pane_els.push(
                    div()
                        .flex()
                        .justify_center()
                        .w(px(8.))
                        .h_full()
                        .flex_shrink_0()
                        .child(grip(("pane-grip", i), PaneDrag(i)))
                        .into_any_element(),
                );
            }
        }
        div()
            .key_context("Workspace")
            // navigation
            .on_action(on_pane!(cx, GoBack, back))
            .on_action(on_pane!(cx, GoForward, forward))
            .on_action(on_pane!(cx, GoUp, up))
            .on_action(on_pane!(cx, GoHome, go_home))
            // view
            .on_action(on_pane!(cx, Refresh, refresh))
            .on_action(on_pane!(cx, ToggleHidden, toggle_hidden))
            .on_action(cx.listener(|this, _: &SplitPane, window, cx| this.split_pane(window, cx)))
            .on_action(cx.listener(|this, _: &ClosePane, _, cx| this.close_pane(cx)))
            .on_action(cx.listener(|this, _: &AddPlace, _, cx| this.add_place(cx)))
            .on_action(cx.listener(|this, _: &RemovePlace, _, cx| {
                this.sidebar.update(cx, |s, cx| s.remove_context_place(cx));
            }))
            .on_action(cx.listener(|this, _: &SwitchPane, window, cx| this.switch_pane(window, cx)))
            // file operations
            .on_action(on_pane!(cx, OpenSelected, open_selected))
            .on_action(on_pane!(cx, CopyPath, copy_path))
            .on_action(on_pane!(cx, MoveToTrash, trash_selected))
            .on_action(on_pane!(cx, OpenTerminal, open_terminal))
            .on_action(on_pane!(cx, OpenTerminalInSelected, open_terminal_selected))
            .on_action(on_pane!(cx, Duplicate, duplicate))
            .on_action(on_pane!(cx, DragOut, drag_out))
            .on_action(on_pane!(cx, ExtractArchive, extract_selected))
            .on_action(cx.listener(|this, _: &CompressZip, _, cx| {
                this.active_pane().update(cx, |p, cx| p.compress_selected(model::Archive::Zip, cx));
            }))
            .on_action(cx.listener(|this, _: &CompressTarGz, _, cx| {
                this.active_pane().update(cx, |p, cx| p.compress_selected(model::Archive::TarGz, cx));
            }))
            // a row dragged past the window edge becomes a real compositor drag,
            // so it can be dropped into a browser or any other app
            .on_drag_move::<FileDrag>(cx.listener(|this, ev: &DragMoveEvent<FileDrag>, window, cx| {
                if !dragout::available() || ev.bounds.contains(&ev.event.position) {
                    return;
                }
                let paths = ev.drag(&*cx).paths.clone();
                match dragout::start(&paths) {
                    // the compositor owns the pointer now: gpui will never see the
                    // button release, so end its in-app drag here
                    Ok(()) => {
                        cx.stop_active_drag(window);
                    }
                    Err(msg) => {
                        let pane = this.active_pane();
                        pane.update(cx, |p, cx| p.set_error(format!("Drag out failed: {msg}"), cx));
                    }
                }
            }))
            .on_action(on_pane!(cx, ToggleColType, toggle_type))
            .on_action(on_pane!(cx, ToggleColSize, toggle_size))
            .on_action(on_pane!(cx, ToggleColModified, toggle_modified))
            .on_action(on_pane!(cx, ToggleColPerms, toggle_perms))
            .on_action(cx.listener(|this, _: &ResetLayout, _, cx| {
                for pane in &this.panes {
                    pane.update(cx, |p, cx| p.reset_layout(cx));
                }
                this.sidebar.update(cx, |s, cx| s.reset_width(cx));
                cx.notify();
            }))
            // sidebar "Places" context menu
            .on_action(cx.listener(|this, _: &PlaceOpen, _, cx| {
                this.sidebar.update(cx, |s, cx| s.context_open(cx));
            }))
            .on_action(cx.listener(|this, _: &PlaceOpenInOtherPane, _, cx| {
                this.sidebar.update(cx, |s, cx| s.context_open_in_other_pane(cx));
            }))
            .on_action(cx.listener(|this, _: &PlaceOpenTerminal, _, cx| {
                this.sidebar.update(cx, |s, cx| s.context_open_terminal(cx));
            }))
            .on_action(cx.listener(|this, _: &PlaceCopyPath, _, cx| {
                this.sidebar.update(cx, |s, cx| s.context_copy_path(cx));
            }))
            .on_action(cx.listener(|this, _: &PlaceChangeColor, _, cx| {
                this.sidebar.update(cx, |s, cx| s.context_change_color(cx));
            }))
            .on_action(cx.listener(|this, _: &PlaceMoveUp, _, cx| {
                this.sidebar.update(cx, |s, cx| s.context_move(-1, cx));
            }))
            .on_action(cx.listener(|this, _: &PlaceMoveDown, _, cx| {
                this.sidebar.update(cx, |s, cx| s.context_move(1, cx));
            }))
            .on_action(cx.listener(|this, _: &PlaceRename, window, cx| {
                let back = this.panes[this.active].focus_handle(cx);
                this.sidebar.update(cx, |s, cx| s.context_rename(back, window, cx));
            }))
            .on_action(on_pane!(cx, CopyFile, copy_file))
            .on_action(on_pane!(cx, CutFile, cut_file))
            .on_action(on_pane!(cx, PasteFile, paste_file))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| {
                let pane = this.active_pane();
                pane.update(cx, |p, cx| p.new_file(window, cx));
            }))
            .on_action(cx.listener(|this, _: &FindInPane, window, cx| {
                let pane = this.active_pane();
                pane.update(cx, |p, cx| p.start_filter(window, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenInOtherPane, _, cx| this.open_in_other_pane(cx)))
            .on_action(cx.listener(|this, _: &NewFolder, window, cx| {
                let pane = this.active_pane();
                pane.update(cx, |p, cx| p.new_folder(window, cx));
            }))
            .on_action(cx.listener(|this, _: &StartRename, window, cx| {
                let pane = this.active_pane();
                pane.update(cx, |p, cx| p.start_rename(window, cx));
            }))
            .on_action(cx.listener(|this, _: &FocusPath, window, cx| {
                let pane = this.active_pane();
                pane.update(cx, |p, cx| p.start_path_edit(window, cx));
            }))
            .on_action(cx.listener(|_, _: &QuitApp, _, cx| cx.quit()))
            .on_action(cx.listener(|_, _: &OpenSettings, _, cx| crate::settings_view::open(cx)))
            // layout
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::CRUST))
            .text_color(rgb(theme::TEXT))
            .font_family(cfg.font_family.clone())
            .text_size(px(cfg.font_size as f32))
            .children(cfg.show_menubar.then(|| self.menubar.clone()))
            .children(cfg.show_toolbar.then(|| self.toolbar.clone()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .p_2()
                    .gap_2()
                    .children(cfg.show_sidebar.then(|| self.sidebar.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_row()
                            .id("panes")
                            .overflow_x_scroll()
                            .children(pane_els),
                    ),
            )
            .children(cfg.show_statusbar.then(|| self.status.clone()))
    }
}