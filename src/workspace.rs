//! Root view: owns the components, wires their events together and handles
//! every app-level command (actions). Components never reference each other.

use crate::{
    actions::*,
    apps,
    components::{
        conflict::{ConflictDialog, ConflictEvent},
        dnd::FileDrag,
        menubar::MenuBar,
        open_with::{OpenWithDialog, OpenWithEvent},
        pane::{Pane, PaneEvent},
        properties::{PropertiesDialog, PropertiesEvent},
        resize::grip,
        sidebar::{Sidebar, SidebarEvent},
        statusbar::StatusBar,
        toolbar::Toolbar,
    },
    dbus::{Request, Show},
    dragout, filetype, model,
    settings::{self, Settings, SettingsChanged},
    theme,
    transfer::{Progress, Transfer, TransferJob},
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
    /// Started by D-Bus activation: the first request from another app takes
    /// over the lone default pane instead of opening a second one.
    adopt_first_request: bool,
    /// A drop that hit a name clash: paused until the dialog below is answered.
    job: Option<TransferJob>,
    dialog: Option<(Entity<ConflictDialog>, Subscription)>,
    /// "Open With" and "Properties" dialogs (at most one dialog of any kind is open).
    open_with: Option<(Entity<OpenWithDialog>, Subscription)>,
    props: Option<(Entity<PropertiesDialog>, Subscription)>,
    /// Give the open dialog keyboard focus on the next render.
    focus_dialog: bool,
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
    /// One pane per folder in `paths` (command-line arguments); none given
    /// means a single pane in the startup folder from Settings.
    pub fn new(mut paths: Vec<PathBuf>, dbus_activated: bool, cx: &mut Context<Self>) -> Self {
        if paths.is_empty() {
            paths.push(settings::get().startup_dir());
        }

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
            adopt_first_request: dbus_activated,
            job: None,
            dialog: None,
            open_with: None,
            props: None,
            focus_dialog: false,
        };
        for path in paths {
            let pane = this.new_pane(path, cx);
            this.panes.push(pane);
        }
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

    /// Another app asked (over D-Bus) to show folders or reveal files, e.g.
    /// Firefox "Open Containing Folder" or Discord "Show in Folder".
    ///
    /// Each target goes to the pane that already shows its folder; if there is
    /// none, a new pane opens next to the active one, so whatever the user is
    /// looking at stays put. The last target ends up active and focused.
    pub fn handle_request(&mut self, req: Request, window: &mut Window, cx: &mut Context<Self>) {
        if req.kind == Show::Properties {
            // "Properties" from another app: just the dialog, over whatever is on screen
            self.show_properties(req.paths, cx);
            window.activate_window();
            crate::activation::activate(window, &req.startup_id);
            return;
        }
        // right after D-Bus activation the only pane is the default one nobody asked for
        let mut adopt = std::mem::take(&mut self.adopt_first_request) && self.panes.len() == 1;
        for path in req.paths {
            // (folder to show, file to select inside it)
            let (dir, select) = match req.kind {
                Show::Folders if path.is_dir() => (path, None),
                _ => match path.parent() {
                    Some(parent) => (parent.to_path_buf(), Some(path)),
                    None => (path, None), // "/" has no parent
                },
            };
            let at = match self.panes.iter().position(|p| p.read(cx).path() == dir.as_path()) {
                Some(i) => i,
                None if std::mem::take(&mut adopt) => {
                    self.panes[0].update(cx, |p, cx| p.navigate(dir, cx));
                    0
                }
                None => self.add_pane(dir, cx),
            };
            if let Some(file) = select {
                self.panes[at].update(cx, |p, cx| p.reveal(file, cx));
            }
            self.set_active(at, cx);
        }
        self.focus_active(window, cx);
        window.activate_window();
        // the caller's xdg-activation token lets the compositor really focus us, not just flag us
        crate::activation::activate(window, &req.startup_id);
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
            PaneEvent::Picked(_) => {} // only panes inside a file chooser emit this
        }
    }

    /// Copy or move `paths` into `dest` (a drop from another split, from the
    /// sidebar, or from another application). When a dropped item has the name
    /// of something already in `dest`, the work pauses and the user is asked
    /// (rename / merge / replace / skip); see `on_conflict`.
    fn transfer(&mut self, paths: Vec<PathBuf>, dest: PathBuf, copy: bool, cx: &mut Context<Self>) {
        if self.modal_open() {
            return; // the previous drop is still waiting for an answer (or another dialog is up)
        }
        let mut job = TransferJob::new(paths, dest, copy);
        let step = job.run();
        self.drive(job, step, cx);
    }

    /// Either open the dialog for the clash the job stopped at, or wrap up.
    fn drive(&mut self, job: TransferJob, step: Progress, cx: &mut Context<Self>) {
        match step {
            Progress::Conflict(conflict) => {
                let dialog = cx.new(|cx| ConflictDialog::new(conflict, cx));
                let sub = cx.subscribe(&dialog, Self::on_conflict);
                self.dialog = Some((dialog, sub));
                self.job = Some(job);
                self.focus_dialog = true;
            }
            Progress::Done(result) => self.finish_transfer(result, cx),
        }
        cx.notify();
    }

    /// The user answered the dialog: carry on with the paused drop.
    fn on_conflict(&mut self, _: Entity<ConflictDialog>, event: &ConflictEvent, cx: &mut Context<Self>) {
        self.dialog = None;
        self.refocus = true; // keyboard focus goes back to the pane
        let Some(mut job) = self.job.take() else {
            cx.notify();
            return;
        };
        match event {
            ConflictEvent::Resolved { choice, apply_to_all } => {
                let step = job.resolve(*choice, *apply_to_all);
                self.drive(job, step, cx);
            }
            ConflictEvent::Cancelled => {
                let result = job.cancel();
                self.finish_transfer(result, cx);
                cx.notify();
            }
        }
    }

    // ---- Open With / Properties ----------------------------------------------

    /// Is any dialog (name clash, Open With, Properties) on screen?
    fn modal_open(&self) -> bool {
        self.dialog.is_some() || self.open_with.is_some() || self.props.is_some()
    }

    /// Keyboard focus of whichever dialog is open.
    fn modal_focus(&self, cx: &App) -> Option<FocusHandle> {
        if let Some((d, _)) = &self.dialog {
            Some(d.focus_handle(cx))
        } else if let Some((d, _)) = &self.open_with {
            Some(d.focus_handle(cx))
        } else {
            self.props.as_ref().map(|(d, _)| d.focus_handle(cx))
        }
    }

    /// "Open With...": look up the installed applications for the selected item (off the
    /// UI thread, it reads a few hundred small files), then show the dialog.
    fn open_with(&mut self, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let Some(path) = self.panes[self.active].read(cx).open_with_target() else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mime = filetype::mime_for(&name, path.is_dir());
        cx.spawn(async move |this, cx| {
            let lookup = cx.background_executor().spawn(async move { apps::lookup(mime.as_deref()) }).await;
            this.update(cx, |this, cx| this.show_open_with(path, lookup, cx)).ok();
        })
        .detach();
    }

    fn show_open_with(&mut self, path: PathBuf, lookup: apps::Lookup, cx: &mut Context<Self>) {
        if self.modal_open() {
            return; // something else came up while the lookup ran
        }
        let dialog = cx.new(|cx| OpenWithDialog::new(path, lookup, cx));
        let sub = cx.subscribe(&dialog, Self::on_open_with);
        self.open_with = Some((dialog, sub));
        self.focus_dialog = true;
        cx.notify();
    }

    fn on_open_with(&mut self, _: Entity<OpenWithDialog>, event: &OpenWithEvent, cx: &mut Context<Self>) {
        self.open_with = None;
        self.refocus = true; // keyboard focus goes back to the pane
        if let OpenWithEvent::Open { app, path, mime, set_default } = event {
            if let Err(msg) = apps::launch(app, std::slice::from_ref(path)) {
                let pane = self.active_pane();
                pane.update(cx, |p, cx| p.set_error(format!("Open With failed: {msg}"), cx));
            }
            if let (true, Some(mime)) = (*set_default, mime.clone()) {
                let id = app.id.clone();
                let name = app.name.clone();
                cx.spawn(async move |this, cx| {
                    let result = cx.background_executor().spawn(async move { apps::set_default(&id, &mime) }).await;
                    if let Err(e) = result {
                        this.update(cx, |this, cx| {
                            let pane = this.active_pane();
                            pane.update(cx, |p, cx| p.set_error(format!("Couldn't make {name} the default: {e}"), cx));
                        })
                        .ok();
                    }
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// Properties of the selection (or of the folder itself when nothing is selected).
    fn properties(&mut self, cx: &mut Context<Self>) {
        let items = self.panes[self.active].read(cx).properties_targets();
        self.show_properties(items, cx);
    }

    fn show_properties(&mut self, items: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.modal_open() || items.is_empty() {
            return;
        }
        let dialog = cx.new(|cx| PropertiesDialog::new(items, cx));
        let sub = cx.subscribe(&dialog, Self::on_properties);
        self.props = Some((dialog, sub));
        self.focus_dialog = true;
        cx.notify();
    }

    fn on_properties(&mut self, _: Entity<PropertiesDialog>, event: &PropertiesEvent, cx: &mut Context<Self>) {
        match event {
            PropertiesEvent::Close => {
                self.props = None;
                self.refocus = true;
            }
            // permissions changed: the listing (Perms column) shows the new value
            PropertiesEvent::PermissionsChanged(path) => {
                if let Some(parent) = path.parent() {
                    let dirs = [parent.to_path_buf()];
                    for pane in &self.panes {
                        pane.update(cx, |p, cx| p.refresh_if_showing(&dirs, cx));
                    }
                }
            }
        }
        cx.notify();
    }

    /// Bring every pane that shows an affected folder up to date and report failures.
    fn finish_transfer(&mut self, result: Transfer, cx: &mut Context<Self>) {
        for pane in &self.panes {
            pane.update(cx, |p, cx| p.refresh_if_showing(&result.touched, cx));
        }
        // report failures in the pane that was dropped on (else the active one);
        // set after the refresh, because a reload clears the pane's message
        if !result.errors.is_empty() {
            let target = self
                .panes
                .iter()
                .find(|p| p.read(cx).path() == result.dest.as_path())
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
        if std::mem::take(&mut self.focus_dialog) {
            if let Some(handle) = self.modal_focus(cx) {
                window.focus(&handle);
            }
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
            .on_action(cx.listener(|this, _: &OpenWith, _, cx| this.open_with(cx)))
            .on_action(cx.listener(|this, _: &ShowProperties, _, cx| this.properties(cx)))
            // (these two are bound for the whole Workspace, so a dialog must ignore them)
            .on_action(cx.listener(|this, _: &FocusPath, window, cx| {
                if this.modal_open() {
                    return;
                }
                let pane = this.active_pane();
                pane.update(cx, |p, cx| p.start_path_edit(window, cx));
            }))
            .on_action(cx.listener(|this, _: &GoToFolder, window, cx| {
                if this.modal_open() {
                    return;
                }
                let pane = this.active_pane();
                pane.update(cx, |p, cx| p.start_jump(window, cx));
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
            // last child = painted on top of everything: the "already exists" dialog
            .children(self.dialog.as_ref().map(|(dialog, _)| dialog.clone()))
            .children(self.open_with.as_ref().map(|(dialog, _)| dialog.clone()))
            .children(self.props.as_ref().map(|(dialog, _)| dialog.clone()))
    }
}