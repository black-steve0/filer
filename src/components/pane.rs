use crate::{
    actions::*,
    components::{
        context_menu::{ContextMenu, ContextMenuEvent},
        dnd::{self, DragPreview, FileDrag},
        icon::{icon, image_or},
        line_edit::{EditOutcome, LineEdit},
        path_bar::{PathBar, PathBarEvent},
        resize::grip,
    },
    menus,
    filter::{KindFilter, Query},
    model::{self, Archive, Entry, SortKey},
    settings::{self, ClickMode, Settings},
    theme,
};
use gpui::{prelude::*, *};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Row height (Settings > Appearance). Every row must be the same size for the virtual list.
fn row_height() -> f32 {
    settings::get().row_height as f32
}

/// Icon slot in front of the name (fixed, so images and "[txt]" text line up).
const COL_ICON: f32 = 22.;
/// Default widths of NAME, TYPE, SIZE, MODIFIED, PERMS. The header and every
/// row read the pane's own copy (`cols`), which the user can drag to change.
const DEFAULT_COLS: [f32; 5] = [240., 100., 70., 125., 76.];
const MIN_COL: f32 = 40.;
const MAX_COL: f32 = 700.;

/// Drag marker: the grip on the right edge of column `.0`.
struct ColDrag(usize);

pub enum PaneEvent {
    /// The user interacted with this pane (it should become the active one).
    Activated,
    /// The X in this pane's title bar was clicked.
    CloseRequested,
    /// Path, listing or selection changed (status bar / sidebar should refresh).
    Changed,
    /// Files were dropped on this pane (or on one of its folders): the
    /// Workspace copies/moves them to `dest` and refreshes every affected pane.
    Dropped { paths: Vec<PathBuf>, dest: PathBuf, copy: bool },
}

/// One self-contained directory view with its own history, selection,
/// path bar, inline rename and right-click menu.
pub struct Pane {
    path: PathBuf,
    all: Vec<Entry>,     // everything in the folder
    entries: Vec<Entry>, // what is shown: `all` filtered + sorted
    selected: Option<usize>,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    show_hidden: bool,
    error: Option<SharedString>,
    active: bool,
    closable: bool, // show the X in the title bar (more than one pane open)
    width: Option<f32>, // None = share space equally; Some = dragged to this width
    cols: [f32; 5],     // column widths (name, type, size, modified, perms)
    show: [bool; 5],    // column visibility (name is always shown)
    focus: FocusHandle,
    rename_focus: FocusHandle,
    filter_focus: FocusHandle,
    filter: Option<LineEdit>,
    filter_case: bool,         // "Aa": case-sensitive matching
    filter_kind: KindFilter,   // All / Folders / Files chip
    sort_key: SortKey,
    sort_asc: bool,
    scroll: UniformListScrollHandle,
    loading: bool,
    job: Option<SharedString>, // "Compressing" / "Extracting" while one runs
    loader: Option<Arc<model::DirLoader>>,
    _load_task: Option<Task<()>>,
    restore_sel: Option<PathBuf>, // reselect this file once a reload brings it back
    path_bar: Entity<PathBar>,
    renaming: Option<(usize, LineEdit)>,
    menu: Option<(Point<Pixels>, Entity<ContextMenu>)>,
}

impl EventEmitter<PaneEvent> for Pane {}

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Pane {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let rename_focus = cx.focus_handle();
        let filter_focus = cx.focus_handle();
        let cfg = settings::get(); // starting values for this pane

        let bar_path = path.clone();
        let bar_return = focus.clone();
        let path_bar = cx.new(|cx| PathBar::new(bar_path, bar_return, cx));
        cx.subscribe(&path_bar, |this: &mut Pane, _, event: &PathBarEvent, cx| match event {
            PathBarEvent::Navigate(p) => this.navigate(p.clone(), cx),
            PathBarEvent::Activated => cx.emit(PaneEvent::Activated),
        })
        .detach();

        let mut pane = Self {
            path,
            all: Vec::new(),
            entries: Vec::new(),
            selected: None,
            back: Vec::new(),
            forward: Vec::new(),
            show_hidden: cfg.show_hidden,
            error: None,
            active: false,
            closable: false,
            width: None,
            cols: DEFAULT_COLS,
            show: cfg.pane_columns(),
            focus,
            rename_focus,
            filter_focus,
            filter: None,
            filter_case: cfg.filter_case,
            filter_kind: KindFilter::All,
            sort_key: cfg.sort_key,
            sort_asc: cfg.sort_asc,
            scroll: UniformListScrollHandle::new(),
            loading: false,
            job: None,
            loader: None,
            _load_task: None,
            restore_sel: None,
            path_bar,
            renaming: None,
            menu: None,
        };
        pane.load(cx);
        pane
    }

    // ---- queries -------------------------------------------------------

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn selected_dir(&self) -> Option<PathBuf> {
        self.selected
            .and_then(|i| self.entries.get(i))
            .filter(|e| e.is_dir)
            .map(|e| e.path.clone())
    }

    pub fn summary(&self) -> String {
        let mut s = if self.loading {
            format!("Loading... {} items so far", self.all.len())
        } else if self.entries.len() != self.all.len() {
            format!("{} of {} items", self.entries.len(), self.all.len())
        } else {
            format!("{} items", self.entries.len())
        };
        if let Some(e) = self.selected.and_then(|i| self.entries.get(i)) {
            s.push_str(&format!("  |  {}", e.name));
            if !e.is_dir {
                s.push_str(&format!("  ({})", model::format_size(e.size)));
            }
        }
        s
    }

    // ---- navigation ----------------------------------------------------

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        cx.notify();
    }

    pub fn width(&self) -> Option<f32> {
        self.width
    }

    pub fn set_width(&mut self, width: Option<f32>, cx: &mut Context<Self>) {
        self.width = width;
        cx.notify();
    }

    /// Back to equal pane width, default column widths, all columns shown.
    pub fn reset_layout(&mut self, cx: &mut Context<Self>) {
        self.width = None;
        self.cols = DEFAULT_COLS;
        self.show = settings::get().pane_columns();
        cx.notify();
    }

    /// Settings changed: apply what concerns this pane right now. Pure display
    /// settings (fonts, row height, date format, ...) are read at render time;
    /// only state copied into the pane (hidden files, sort, columns) is synced.
    pub fn settings_changed(&mut self, prev: &Settings, cur: &Settings, cx: &mut Context<Self>) {
        if prev.sort_key != cur.sort_key || prev.sort_asc != cur.sort_asc {
            self.sort_key = cur.sort_key;
            self.sort_asc = cur.sort_asc;
        }
        if prev.columns != cur.columns {
            self.show = cur.pane_columns();
        }
        if prev.filter_case != cur.filter_case {
            self.filter_case = cur.filter_case;
        }
        // also covers folders-first and natural ordering
        self.apply_view();
        if prev.show_hidden != cur.show_hidden && self.show_hidden != cur.show_hidden {
            self.toggle_hidden(cx); // reloads the folder
        }
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn toggle_col(&mut self, i: usize, cx: &mut Context<Self>) {
        self.show[i] = !self.show[i];
        cx.notify();
    }
    pub fn toggle_type(&mut self, cx: &mut Context<Self>) {
        self.toggle_col(1, cx);
    }
    pub fn toggle_size(&mut self, cx: &mut Context<Self>) {
        self.toggle_col(2, cx);
    }
    pub fn toggle_modified(&mut self, cx: &mut Context<Self>) {
        self.toggle_col(3, cx);
    }
    pub fn toggle_perms(&mut self, cx: &mut Context<Self>) {
        self.toggle_col(4, cx);
    }

    /// Copy the selected file or folder next to itself ("name copy").
    pub fn duplicate(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.selected.and_then(|i| self.entries.get(i)).cloned() else { return };
        let dir = e.path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| self.path.clone());
        match model::paste(&e.path, false, &dir) {
            Ok(dst) => {
                if dst.parent() == Some(self.path.as_path()) {
                    self.add_entry(&dst);
                }
                match self.entries.iter().position(|x| x.path == dst) {
                    Some(i) => self.select(i, cx),
                    None => cx.notify(),
                }
            }
            Err(err) => {
                self.error = Some(format!("Duplicate failed: {err}").into());
                cx.notify();
            }
        }
    }

    pub fn set_closable(&mut self, closable: bool, cx: &mut Context<Self>) {
        if self.closable != closable {
            self.closable = closable;
            cx.notify();
        }
    }

    /// Show a message in the pane (used for failed drops).
    pub fn set_error(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(msg.into());
        cx.notify();
    }

    /// Re-read the folder if this pane is showing one of `dirs` (after a drop).
    pub fn refresh_if_showing(&mut self, dirs: &[PathBuf], cx: &mut Context<Self>) {
        if dirs.iter().any(|d| *d == self.path) {
            self.refresh(cx);
        }
    }

    /// Ask the Workspace to copy/move `paths` into `dest`.
    fn drop_into(&mut self, paths: Vec<PathBuf>, dest: PathBuf, copy: bool, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        cx.emit(PaneEvent::Activated);
        cx.emit(PaneEvent::Dropped { paths, dest, copy });
    }

    /// Hand the selected file to a drag-source helper window so it can be
    /// dragged into other applications.
    pub fn drag_out(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected_path() else { return };
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { model::drag_out(&path) }).await;
            if let Err(msg) = result {
                this.update(cx, |pane, cx| {
                    pane.error = Some(format!("Drag out failed: {msg}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- compress / extract ----------------------------------------------

    pub fn compress_selected(&mut self, kind: Archive, cx: &mut Context<Self>) {
        let Some(src) = self.selected_path() else { return };
        self.run_job("Compressing", move || model::compress(&src, kind), cx);
    }

    pub fn extract_selected(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.selected.and_then(|i| self.entries.get(i)) else { return };
        if e.is_dir || !model::is_archive(&e.name) {
            return;
        }
        let src = e.path.clone();
        self.run_job("Extracting", move || model::extract(&src), cx);
    }

    /// Run a blocking archive job off the UI thread; on success the new file or
    /// folder is added to the listing and selected.
    fn run_job(
        &mut self,
        label: &'static str,
        job: impl FnOnce() -> Result<PathBuf, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        self.job = Some(label.into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            this.update(cx, |pane, cx| {
                pane.job = None;
                match result {
                    Ok(path) => {
                        if path.parent() == Some(pane.path.as_path()) {
                            pane.add_entry(&path);
                            if let Some(i) = pane.entries.iter().position(|e| e.path == path) {
                                pane.select(i, cx);
                            }
                        }
                        cx.emit(PaneEvent::Changed);
                    }
                    Err(msg) => pane.error = Some(format!("{label} failed: {msg}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if path == self.path {
            return;
        }
        self.filter = None;
        let old = std::mem::replace(&mut self.path, path);
        self.back.push(old);
        self.forward.clear();
        self.reload(cx);
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(prev) = self.back.pop() {
            self.filter = None;
            let cur = std::mem::replace(&mut self.path, prev);
            self.forward.push(cur);
            self.reload(cx);
        }
    }

    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.forward.pop() {
            self.filter = None;
            let cur = std::mem::replace(&mut self.path, next);
            self.back.push(cur);
            self.reload(cx);
        }
    }

    pub fn up(&mut self, cx: &mut Context<Self>) {
        let parent = self.path.parent().map(|p| p.to_path_buf());
        if let Some(p) = parent {
            self.navigate(p, cx);
        }
    }

    pub fn go_home(&mut self, cx: &mut Context<Self>) {
        if let Some(h) = dirs::home_dir() {
            self.navigate(h, cx);
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected.and_then(|i| self.entries.get(i)).map(|e| e.path.clone())
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.restore_sel = self.selected_path();
        self.reload(cx);
    }

    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        self.restore_sel = self.selected_path();
        self.reload(cx);
    }

    pub fn start_path_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.path_bar.update(cx, |bar, cx| bar.start_edit(window, cx));
    }

    // ---- selection -----------------------------------------------------

    /// Scroll just enough to bring row `index` into view (no jump if visible).
    fn scroll_nearest(&self, index: usize) {
        let top = self.scroll.0.borrow().base_handle.logical_scroll_top().0;
        let rows = self
            .scroll
            .0
            .borrow()
            .last_item_size
            .map(|s| (s.item.height / px(row_height())) as usize)
            .unwrap_or(20)
            .max(1);
        if index < top {
            self.scroll.scroll_to_item(index, ScrollStrategy::Top);
        } else if index + 1 > top + rows {
            self.scroll.scroll_to_item(index, ScrollStrategy::Bottom);
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(index);
        self.scroll_nearest(index);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let next = match self.selected {
            None => {
                if delta > 0 { 0 } else { len - 1 }
            }
            Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
        };
        self.select(next, cx);
    }

    // ---- file operations -------------------------------------------------

    pub fn open_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(i) = self.selected {
            self.open_index(i, cx);
        }
    }

    fn open_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(index).cloned() else { return };
        if entry.is_dir {
            self.navigate(entry.path, cx);
        } else {
            model::open_external(&entry.path);
        }
    }

    pub fn copy_path(&mut self, cx: &mut Context<Self>) {
        let path = self
            .selected
            .and_then(|i| self.entries.get(i))
            .map(|e| e.path.clone())
            .unwrap_or_else(|| self.path.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(path.to_string_lossy().into_owned()));
    }

    pub fn trash_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else { return };
        let Some(entry) = self.entries.get(index).cloned() else { return };
        match model::trash(&entry.path) {
            Ok(()) => {
                self.all.retain(|e| e.path != entry.path);
                self.apply_view();
                self.selected = (!self.entries.is_empty()).then(|| index.min(self.entries.len() - 1));
                cx.emit(PaneEvent::Changed);
            }
            Err(err) => {
                self.error = Some(format!("Couldn't trash \"{}\": {err}", entry.name).into());
            }
        }
        cx.notify();
    }

    pub fn new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match model::create_dir_unique(&self.path) {
            Ok(path) => {
                self.add_entry(&path);
                if let Some(i) = self.entries.iter().position(|e| e.path == path) {
                    self.select(i, cx);
                    self.start_rename(window, cx);
                }
            }
            Err(err) => {
                self.error = Some(format!("Couldn't create folder: {err}").into());
                cx.notify();
            }
        }
    }

    /// Terminal in the folder this pane is showing.
    pub fn open_terminal(&mut self, cx: &mut Context<Self>) {
        let dir = self.path.clone();
        self.open_terminal_at(dir, cx);
    }

    /// Terminal in the selected folder (right-click on a folder), falling
    /// back to the current folder.
    pub fn open_terminal_selected(&mut self, cx: &mut Context<Self>) {
        let dir = self.selected_dir().unwrap_or_else(|| self.path.clone());
        self.open_terminal_at(dir, cx);
    }

    pub fn open_terminal_at(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let custom = settings::get().terminal.clone(); // Settings > General > Terminal program
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { model::open_terminal(&dir, &custom) })
                .await;
            if let Err(msg) = result {
                this.update(cx, |pane, cx| {
                    pane.error = Some(format!("Open terminal failed: {msg}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- new file / clipboard --------------------------------------------

    pub fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match model::create_file_unique(&self.path) {
            Ok(path) => {
                self.add_entry(&path);
                if let Some(i) = self.entries.iter().position(|e| e.path == path) {
                    self.select(i, cx);
                    self.start_rename(window, cx);
                }
            }
            Err(err) => {
                self.error = Some(format!("Couldn't create file: {err}").into());
                cx.notify();
            }
        }
    }

    pub fn copy_file(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.selected.and_then(|i| self.entries.get(i)) {
            model::set_file_clip(e.path.clone(), false);
            self.error = None;
            cx.notify();
        }
    }

    pub fn cut_file(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.selected.and_then(|i| self.entries.get(i)) {
            model::set_file_clip(e.path.clone(), true);
            self.error = None;
            cx.notify();
        }
    }

    /// Paste into the selected folder if one is selected, else the current folder.
    pub fn paste_file(&mut self, cx: &mut Context<Self>) {
        let Some((src, cut)) = model::file_clip() else { return };
        let dir = self.selected_dir().unwrap_or_else(|| self.path.clone());
        match model::paste(&src, cut, &dir) {
            Ok(dst) => {
                if cut {
                    // the source is gone; don't offer to paste it again
                    model::set_file_clip(dst.clone(), false);
                }
                if cut {
                    self.all.retain(|e| e.path != src);
                }
                if dst.parent() == Some(self.path.as_path()) {
                    self.add_entry(&dst);
                } else {
                    self.apply_view();
                }
                if let Some(i) = self.entries.iter().position(|e| e.path == dst) {
                    self.select(i, cx);
                } else {
                    cx.emit(PaneEvent::Changed);
                    cx.notify();
                }
            }
            Err(err) => {
                self.error = Some(format!("Paste failed: {err}").into());
                cx.notify();
            }
        }
    }

    // ---- sorting + filtering ---------------------------------------------

    /// Click on a column header: same column flips the order.
    fn sort_by(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.sort_key == key {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort_key = key;
            // sensible first direction: newest/largest first would surprise;
            // keep ascending for names, descending for size and date
            self.sort_asc = matches!(key, SortKey::Name | SortKey::Type);
        }
        self.apply_view();
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// Rebuild `entries` from `all` (filter + sort), keeping the selection
    /// on the same file when it is still visible.
    fn apply_view(&mut self) {
        let keep = self.selected.and_then(|i| self.entries.get(i)).map(|e| e.path.clone());
        // the filter (text query + folder/file chip) only applies while the bar is open
        let query = self.filter.as_ref().map(|f| Query::parse(&f.text, self.filter_case));
        let kind = if self.filter.is_some() { self.filter_kind } else { KindFilter::All };
        self.entries = self
            .all
            .iter()
            .filter(|e| kind.allows(e.is_dir) && query.as_ref().map_or(true, |q| q.matches(e)))
            .cloned()
            .collect();
        let cfg = settings::get();
        model::sort_entries(
            &mut self.entries,
            self.sort_key,
            self.sort_asc,
            cfg.folders_first,
            cfg.natural_sort,
        );
        self.selected = keep.and_then(|p| self.entries.iter().position(|e| e.path == p));
    }

    pub fn start_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter.is_none() {
            self.filter = Some(LineEdit::new(String::new()));
            self.filter_kind = KindFilter::All; // a fresh filter starts unrestricted
        }
        self.renaming = None;
        self.menu = None;
        window.focus(&self.filter_focus);
        cx.emit(PaneEvent::Activated);
        cx.notify();
    }

    fn toggle_filter_case(&mut self, cx: &mut Context<Self>) {
        self.filter_case = !self.filter_case;
        self.apply_view();
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn cycle_filter_kind(&mut self, cx: &mut Context<Self>) {
        self.filter_kind = self.filter_kind.next();
        self.apply_view();
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = None;
        self.apply_view();
        window.focus(&self.focus);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn filter_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        // arrows walk the results without leaving the text box; Alt+C / Alt+T
        // flip the case and folder/file switches
        let m = ev.keystroke.modifiers;
        match ev.keystroke.key.as_str() {
            "up" => return self.move_selection(-1, cx),
            "down" => return self.move_selection(1, cx),
            "pageup" => return self.move_selection(-10, cx),
            "pagedown" => return self.move_selection(10, cx),
            "c" if m.alt => return self.toggle_filter_case(cx),
            "t" if m.alt => return self.cycle_filter_kind(cx),
            _ => {}
        }
        let Some(edit) = self.filter.as_mut() else { return };
        match edit.handle_key(ev, cx) {
            EditOutcome::Submit => {
                // keep the filter, hand the keyboard back to the list
                window.focus(&self.focus);
                if self.selected.is_none() && !self.entries.is_empty() {
                    self.select(0, cx);
                }
                cx.notify();
            }
            EditOutcome::Cancel => self.clear_filter(window, cx),
            EditOutcome::Edited => {
                self.apply_view();
                cx.emit(PaneEvent::Changed);
                cx.notify();
            }
            EditOutcome::Ignored => {}
        }
    }

    // ---- rename ----------------------------------------------------------

    pub fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else { return };
        let Some(name) = self.entries.get(index).map(|e| e.name.clone()) else { return };
        self.menu = None;
        self.renaming = Some((index, LineEdit::for_rename(name)));
        window.focus(&self.rename_focus);
        cx.notify();
    }

    fn rename_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some((index, edit)) = self.renaming.as_mut() else { return };
        let index = *index;
        match edit.handle_key(ev, cx) {
            EditOutcome::Submit => {
                let name = edit.text.trim().to_string();
                self.renaming = None;
                window.focus(&self.focus);
                self.commit_rename(index, name, cx);
            }
            EditOutcome::Cancel => {
                self.renaming = None;
                window.focus(&self.focus);
                cx.notify();
            }
            EditOutcome::Edited => cx.notify(),
            EditOutcome::Ignored => {}
        }
    }

    fn commit_rename(&mut self, index: usize, new_name: String, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(index).cloned() else { return };
        if new_name.is_empty() || new_name == entry.name {
            cx.notify();
            return;
        }
        if new_name.contains('/') {
            self.error = Some("Names can't contain \"/\"".into());
            cx.notify();
            return;
        }
        let target = entry.path.with_file_name(&new_name);
        if target.exists() {
            self.error = Some(format!("\"{new_name}\" already exists").into());
            cx.notify();
            return;
        }
        match std::fs::rename(&entry.path, &target) {
            Ok(()) => {
                self.all.retain(|e| e.path != entry.path);
                self.add_entry(&target);
                if let Some(i) = self.entries.iter().position(|e| e.path == target) {
                    self.select(i, cx);
                } else {
                    cx.notify();
                }
            }
            Err(err) => {
                self.error = Some(format!("Rename failed: {err}").into());
                cx.notify();
            }
        }
    }

    // ---- context menu ----------------------------------------------------

    fn open_menu(
        &mut self,
        position: Point<Pixels>,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        self.renaming = None;
        self.selected = row;
        let entries = match row.and_then(|i| self.entries.get(i)) {
            Some(e) => menus::row_menu(e.is_dir, !e.is_dir && model::is_archive(&e.name)),
            None => menus::background_menu(self.show_hidden),
        };
        let menu = cx.new(|_| ContextMenu::new(entries));
        cx.subscribe(&menu, |this: &mut Pane, _, _: &ContextMenuEvent, cx| {
            this.menu = None;
            cx.notify();
        })
        .detach();
        self.menu = Some((position, menu));
        cx.emit(PaneEvent::Activated);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn open_columns_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        self.renaming = None;
        let menu = cx.new(|_| ContextMenu::new(menus::columns_menu()));
        cx.subscribe(&menu, |this: &mut Pane, _, _: &ContextMenuEvent, cx| {
            this.menu = None;
            cx.notify();
        })
        .detach();
        self.menu = Some((position, menu));
        cx.emit(PaneEvent::Activated);
        cx.notify();
    }

    // ---- loading -------------------------------------------------------

    /// Add (or replace) one file in the listing without re-reading the folder.
    fn add_entry(&mut self, path: &Path) {
        self.filter = None;
        self.all.retain(|e| e.path != path);
        if let Some(e) = model::entry_for(path) {
            self.all.push(e);
        }
        self.apply_view();
    }

    /// Start listing the current folder on a background thread. The pane
    /// shows up immediately (empty, "Loading..."); files stream in as they
    /// are read, so a huge folder never freezes the window.
    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(old) = self.loader.take() {
            old.cancel();
        }
        self.all.clear();
        self.entries.clear();
        self.selected = None;
        self.renaming = None;
        self.menu = None;
        self.error = None;
        self.loading = true;

        let loader = model::DirLoader::start(self.path.clone(), self.show_hidden);
        self.loader = Some(loader.clone());

        self._load_task = Some(cx.spawn(async move |this, cx| {
            let mut wait = 30u64; // poll quickly at first, then back off
            loop {
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                wait = (wait * 2).min(400);
                let (batch, finished, error) = loader.poll();
                if batch.is_empty() && !finished {
                    continue;
                }
                let keep_going = this
                    .update(cx, |pane, cx| {
                        let current = pane.loader.as_ref().is_some_and(|l| Arc::ptr_eq(l, &loader));
                        if !current {
                            return false;
                        }
                        pane.all.extend(batch);
                        if finished {
                            pane.loading = false;
                            if let Some(e) = error {
                                pane.error = Some(e.into());
                            }
                        }
                        pane.apply_view();
                        if let Some(path) = pane.restore_sel.clone() {
                            if let Some(i) = pane.entries.iter().position(|e| e.path == path) {
                                pane.selected = Some(i);
                                pane.restore_sel = None;
                            } else if finished {
                                pane.restore_sel = None;
                            }
                        }
                        cx.emit(PaneEvent::Changed);
                        cx.notify();
                        !finished
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
        let path = self.path.clone();
        self.path_bar.update(cx, |bar, cx| bar.set_path(path, cx));
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// Build only the rows that are on screen (called by the virtual list).
    fn render_rows(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let cols = self.cols;
        let show = self.show;
        let cfg = settings::get();
        let single_click = cfg.click_to_open == ClickMode::Single;
        let underline = cfg.underline_names;
        let mono = cfg.mono_family.clone();
        let row_h = cfg.row_height as f32;
        range
            .filter_map(|i| {
                let e = self.entries.get(i)?;
                let row = {
                let selected = self.selected == Some(i);
                let size = if e.is_dir { String::new() } else { model::format_size(e.size) };

                let name_el: AnyElement = match &self.renaming {
                    Some((ri, edit)) if *ri == i => div()
                        .id("rename-edit")
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
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(theme::LINK))
                        .when(underline, |d| d.underline())
                        .when(e.is_dir, |d| d.font_weight(FontWeight::BOLD))
                        .child(e.name.clone())
                        .into_any_element(),
                };

                let drag = FileDrag::single(e.path.clone(), &e.name);
                let drop_dir = e.path.clone();
                let is_dir = e.is_dir;

                div()
                    .id(("row", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mx_1()
                    .px_2()
                    .h(px(row_h))
                    .overflow_hidden()
                    .cursor_pointer()
                    // drag this row: to the other split, onto a folder, onto a
                    // sidebar place (moves; hold Ctrl to copy)
                    .on_drag(drag, |drag, _offset, _window, cx| cx.new(|_| DragPreview::new(drag)))
                    // a folder row is itself a drop target (files from the app or from outside)
                    .when(is_dir, |d| {
                        let dest_a = drop_dir.clone();
                        let dest_b = drop_dir.clone();
                        d.drag_over::<FileDrag>(|s, _, _, _| s.bg(dnd::target_tint()))
                            .drag_over::<ExternalPaths>(|s, _, _, _| s.bg(dnd::target_tint()))
                            .on_drop(cx.listener(move |this, drag: &FileDrag, window, cx| {
                                let copy = dnd::internal_is_copy(window);
                                this.drop_into(drag.paths.clone(), dest_a.clone(), copy, cx);
                            }))
                            .on_drop(cx.listener(move |this, ext: &ExternalPaths, window, cx| {
                                let copy = dnd::external_is_copy(window);
                                this.drop_into(ext.paths().to_vec(), dest_b.clone(), copy, cx);
                            }))
                    })
                    .text_color(rgb(theme::TEXT))
                    .when(selected, |d| d.bg(rgb(theme::HILITE)))
                    .when(!selected, |d| d.hover(|s| s.bg(rgb(theme::HOVER_ROW))))
                    // fixed-width slot: "[txt]" placeholders and images of any
                    // size all take the same room, so the columns never drift
                    .child(
                        div()
                            .w(px(COL_ICON))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .child(icon(e)),
                    )
                    .child(div().w(px(cols[0])).flex_shrink_0().flex().px_1().child(name_el))
                    .when(show[1], |d| {
                        d.child(
                            div()
                                .w(px(cols[1]))
                                .flex_shrink_0()
                                .px_1()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(theme::SUBTEXT))
                                .child(e.kind.clone()),
                        )
                    })
                    .when(show[2], |d| {
                        d.child(
                            div()
                                .w(px(cols[2]))
                                .flex_shrink_0()
                                .px_1()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(theme::SUBTEXT))
                                .child(size),
                        )
                    })
                    .when(show[3], |d| {
                        d.child(
                            div()
                                .w(px(cols[3]))
                                .flex_shrink_0()
                                .px_1()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(theme::SUBTEXT))
                                .child(model::format_modified(e.modified)),
                        )
                    })
                    .when(show[4], |d| {
                        d.child(
                            div()
                                .w(px(cols[4]))
                                .flex_shrink_0()
                                .px_1()
                                .truncate()
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(rgb(theme::SUBTEXT))
                                .child(model::format_mode(e.mode)),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            this.renaming = None;
                            this.menu = None;
                            window.focus(&this.focus);
                            cx.emit(PaneEvent::Activated);
                            // double-click mode opens here. Single-click mode opens on
                            // mouse-up instead (see below) so that pressing a row can
                            // still turn into a drag.
                            let open = !single_click && ev.click_count >= 2;
                            if open {
                                this.selected = Some(i);
                                this.open_index(i, cx);
                            } else {
                                this.select(i, cx);
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                            // first click of a single-click open, on the row that was
                            // pressed, and not the end of a drag
                            if single_click
                                && ev.click_count == 1
                                && this.selected == Some(i)
                                && this.renaming.is_none()
                                && !cx.has_active_drag()
                            {
                                this.open_index(i, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            this.open_menu(ev.position, Some(i), window, cx);
                            cx.stop_propagation();
                        }),
                    )
                };
                Some(row.into_any_element())
            })
            .collect()
    }
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filter_focused = self.filter_focus.is_focused(window);
        let title: String = if settings::get().title_full_path {
            self.path.to_string_lossy().into_owned()
        } else {
            self.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".to_string())
        };
        let border = if self.active {
            rgb(theme::ACCENT)
        } else {
            rgb(theme::SURFACE0)
        };


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
            .id("pane")
            .key_context("Pane")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .bg(rgb(theme::BG))
            .border_1()
            .border_color(border)
            .text_color(rgb(theme::TEXT))
            // dropping on the pane itself (not on a folder row) goes into the folder it shows
            .drag_over::<FileDrag>(|s, _, _, _| s.border_color(rgb(theme::ACCENT)).bg(theme::alpha(theme::ACCENT, 0x14)))
            .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(rgb(theme::ACCENT)).bg(theme::alpha(theme::ACCENT, 0x14)))
            .on_drop(cx.listener(|this, drag: &FileDrag, window, cx| {
                let copy = dnd::internal_is_copy(window);
                let dest = this.path.clone();
                this.drop_into(drag.paths.clone(), dest, copy, cx);
            }))
            .on_drop(cx.listener(|this, ext: &ExternalPaths, window, cx| {
                let copy = dnd::external_is_copy(window);
                let dest = this.path.clone();
                this.drop_into(ext.paths().to_vec(), dest, copy, cx);
            }))
            // list navigation (pane-local actions)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPageUp, _, cx| this.move_selection(-10, cx)))
            .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| this.move_selection(10, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                if !this.entries.is_empty() {
                    this.select(0, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                if !this.entries.is_empty() {
                    let last = this.entries.len() - 1;
                    this.select(last, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CloseMenu, _, cx| {
                if this.menu.take().is_some() {
                    cx.notify();
                } else if this.filter.is_some() {
                    this.filter = None;
                    this.apply_view();
                    cx.emit(PaneEvent::Changed);
                    cx.notify();
                }
            }))
            // clicking empty space activates + focuses the pane
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.renaming = None;
                    this.menu = None;
                    window.focus(&this.focus);
                    cx.emit(PaneEvent::Activated);
                    cx.notify();
                }),
            )
            // right-click on empty space
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.open_menu(ev.position, None, window, cx);
                }),
            )
            // mouse side buttons: back / forward
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, _, cx| {
                    cx.emit(PaneEvent::Activated);
                    this.back(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, _, cx| {
                    cx.emit(PaneEvent::Activated);
                    this.forward(cx);
                }),
            )
            // title bar: navy when this is the active pane, gray otherwise
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .bg(rgb(if self.active { theme::TITLE_ACTIVE } else { theme::TITLE_INACTIVE }))
                    .text_color(rgb(0xffffff))
                    .font_weight(FontWeight::BOLD)
                    .child(image_or("folder", px(16.), || {
                        div().font_family(settings::get().mono_family.clone()).child("[dir]").into_any_element()
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    // close this pane (only while there is more than one)
                    .children(self.closable.then(|| {
                        div()
                            .id("close-pane")
                            .group("close-pane")
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .w(px(18.))
                            .h(px(16.))
                            .cursor_pointer()
                            .bg(rgb(theme::MANTLE))
                            .text_color(rgb(theme::TEXT))
                            .shadow(theme::raised())
                            .hover(|s| s.bg(rgb(theme::ERROR)).text_color(rgb(0xffffff)))
                            .child(
                                div()
                                    .group_hover("close-pane", |s| s.text_color(rgb(0xffffff)))
                                    .child(image_or("close-pane", px(10.), || {
                                        div().child("\u{00d7}").into_any_element() // ×
                                    })),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(PaneEvent::CloseRequested);
                                }),
                            )
                    })),
            )
            .child(self.path_bar.clone())
            // filter bar (Ctrl+F). See filter.rs for the query syntax.
            .children(self.filter.as_ref().map(|f| {
                let hint = "name  *.txt  ext:rs  type:folder  size>1mb  newer:7d  -exclude";
                let empty = f.text.is_empty();
                let body: AnyElement = if filter_focused {
                    f.render().into_any_element()
                } else {
                    div().child(f.text.clone()).into_any_element()
                };
                // small bevelled toggle that keeps the keyboard in the text box
                let chip = |id: &'static str, label: &'static str, on: bool| {
                    div()
                        .id(id)
                        .flex_shrink_0()
                        .px_1()
                        .cursor_pointer()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(theme::TEXT))
                        // ON = highlighter yellow + navy frame + sunken (same as toolbar toggles)
                        .border_1()
                        .border_color(if on { rgb(theme::ACCENT) } else { rgba(0x00000000) })
                        .bg(rgb(if on { theme::HILITE } else { theme::MANTLE }))
                        .shadow(if on { theme::sunken() } else { theme::raised() })
                        .hover(move |s| s.bg(rgb(if on { 0xffff66 } else { 0xd4d4d4 })))
                        .child(label)
                };
                div()
                    .id("filter-bar")
                    .key_context("TextInput")
                    .track_focus(&self.filter_focus)
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                        this.filter_key(ev, window, cx)
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.filter_focus);
                        cx.emit(PaneEvent::Activated);
                        cx.notify();
                    }))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .mt_1()
                    .px_2()
                    .py_1()
                    .bg(rgb(theme::BG))
                    .shadow(theme::sunken())
                    .border_1()
                    .border_color(if filter_focused { rgb(theme::ACCENT) } else { rgb(theme::SURFACE0) })
                    .child(image_or("find", px(14.), || div().into_any_element()))
                    .child(div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(theme::TEXT)).child("Filter:"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(body)
                            .children(empty.then(|| {
                                div()
                                    .text_xs()
                                    .whitespace_nowrap()
                                    .text_color(rgb(theme::OVERLAY))
                                    .child(hint)
                            })),
                    )
                    // "3 of 40" while something is filtered out
                    .children((self.entries.len() != self.all.len()).then(|| {
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(rgb(theme::SUBTEXT))
                            .child(format!("{} of {}", self.entries.len(), self.all.len()))
                    }))
                    .child(
                        chip("filter-case", "Aa", self.filter_case).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                window.focus(&this.filter_focus);
                                this.toggle_filter_case(cx);
                            }),
                        ),
                    )
                    .child(
                        chip("filter-kind", self.filter_kind.label(), self.filter_kind != KindFilter::All)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    window.focus(&this.filter_focus);
                                    this.cycle_filter_kind(cx);
                                }),
                            ),
                    )
                    .child(
                        chip("filter-clear", "\u{00d7}", false).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.clear_filter(window, cx);
                            }),
                        ),
                    )
            }))
            // column header (same widths as the rows, so columns line up).
            // Click a title to sort, click again to reverse; drag the grip on a
            // column's right edge to resize it (double-click resets it);
            // right-click for the show/hide menu.
            .child({
                let asc = self.sort_asc;
                let cols = self.cols;
                let show = self.show;
                let title = |key: SortKey, label: &'static str, id: &'static str| {
                    let current = self.sort_key == key;
                    div()
                        .id(id)
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_1()
                        .cursor_pointer()
                        .bg(rgb(theme::MANTLE))
                        .text_color(rgb(theme::TEXT))
                        .shadow(if current { theme::sunken() } else { theme::raised() })
                        .hover(|s| s.bg(rgb(0xd4d4d4)))
                        .child(label)
                        .children(current.then(|| {
                            let (icon_name, glyph) =
                                if asc { ("sort-asc", "\u{25b2}") } else { ("sort-desc", "\u{25bc}") };
                            image_or(icon_name, px(10.), move || {
                                div().child(glyph).into_any_element()
                            })
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(PaneEvent::Activated);
                                this.sort_by(key, cx);
                            }),
                        )
                        .into_any_element()
                };
                // one header cell: its content plus the resize grip on its right edge
                let cell = |i: usize, content: AnyElement| {
                    div()
                        .id(("col-head", i))
                        .relative()
                        .w(px(cols[i]))
                        .flex_shrink_0()
                        .on_drag_move::<ColDrag>(cx.listener(
                            move |this, ev: &DragMoveEvent<ColDrag>, _, cx| {
                                if ev.drag(&*cx).0 == i {
                                    let w = f32::from(ev.event.position.x) - f32::from(ev.bounds.left());
                                    this.cols[i] = w.clamp(MIN_COL, MAX_COL);
                                    cx.notify();
                                }
                            },
                        ))
                        .child(content)
                        .child(
                            div().absolute().top_0().right_0().h_full().child(
                                grip(("col-grip", i), ColDrag(i)).on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                                        if ev.click_count >= 2 {
                                            this.cols[i] = DEFAULT_COLS[i];
                                            cx.notify();
                                        }
                                    }),
                                ),
                            ),
                        )
                };
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .mx_1()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(theme::TEXT))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                            this.open_columns_menu(ev.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(div().w(px(COL_ICON)).flex_shrink_0())
                    .child(cell(0, title(SortKey::Name, "NAME", "sort-name")))
                    .when(show[1], |d| d.child(cell(1, title(SortKey::Type, "TYPE", "sort-type"))))
                    .when(show[2], |d| d.child(cell(2, title(SortKey::Size, "SIZE", "sort-size"))))
                    .when(show[3], |d| {
                        d.child(cell(3, title(SortKey::Modified, "MODIFIED", "sort-mod")))
                    })
                    .when(show[4], |d| {
                        d.child(cell(
                            4,
                            div()
                                .w_full()
                                .px_1()
                                .bg(rgb(theme::MANTLE))
                                .shadow(theme::raised())
                                .child("PERMS")
                                .into_any_element(),
                        ))
                    })
            })
            .children(
                self.error
                    .clone()
                    .map(|e| div().p_3().text_color(rgb(theme::ERROR)).child(e)),
            )
            .children(self.job.clone().map(|j| {
                div().px_3().py_1().text_xs().text_color(rgb(theme::OVERLAY)).child(format!("{j}..."))
            }))
            .children(self.loading.then(|| {
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(theme::OVERLAY))
                    .child(format!("Loading... {} items so far", self.all.len()))
            }))
            .children((self.entries.is_empty() && self.error.is_none() && !self.loading).then(|| {
                div()
                    .p_4()
                    .text_color(rgb(theme::OVERLAY))
                    .child("This folder is empty")
            }))
            // virtual list: only the rows in view are built, so huge folders stay fast
            .child(
                uniform_list(
                    "entries",
                    self.entries.len(),
                    cx.processor(|this, range, window, cx| this.render_rows(range, window, cx)),
                )
                .track_scroll(self.scroll.clone())
                .flex_1()
                .min_h_0(),
            )
            .children(menu)
    }
}