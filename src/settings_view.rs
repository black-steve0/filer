//! The Settings window (File > Settings..., Ctrl+, or the toolbar button).
//!
//! A second window with a page list on the left and the settings of that page
//! on the right. Every control writes straight through `settings::update`, so
//! changes are saved and applied to the main window immediately; there is no
//! OK / Apply step. Text fields commit on Enter or when they lose focus.

use crate::{
    components::{
        context_menu::MenuEntry,
        controls::{checkbox, chip, group, info_row, row, stepper, wide_row},
        line_edit::{EditOutcome, LineEdit},
    },
    menus, model,
    settings::{self, ClickMode, DateFormat, Settings, SettingsChanged, SizeUnits, SORT_KEYS},
    theme,
};
use gpui::{prelude::*, *};

/// Click handler that changes one setting:
/// `set!(cx, |s| s.show_hidden = !s.show_hidden)`.
macro_rules! set {
    ($cx:expr, |$s:ident| $body:expr) => {
        $cx.listener(move |_, _: &MouseDownEvent, _, cx| {
            settings::update(cx, |$s| {
                $body;
            });
        })
    };
}

// ---- opening the window (only ever one) ------------------------------------------

struct SettingsWindow(WindowHandle<SettingsView>);
impl Global for SettingsWindow {}

/// Open the Settings window, or bring the existing one to the front.
pub fn open(cx: &mut App) {
    if let Some(existing) = cx.try_global::<SettingsWindow>().map(|g| g.0.clone()) {
        if existing.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(780.), px(580.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Settings".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |_, cx| cx.new(|cx| SettingsView::new(cx)),
    );
    if let Ok(handle) = opened {
        cx.set_global(SettingsWindow(handle));
    }
}

// ---- pages and text fields ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Appearance,
    Files,
    Layout,
    Shortcuts,
    About,
}

const PAGES: [(Page, &str); 6] = [
    (Page::General, "General"),
    (Page::Appearance, "Appearance"),
    (Page::Files, "Files & Columns"),
    (Page::Layout, "Layout"),
    (Page::Shortcuts, "Shortcuts"),
    (Page::About, "About & Reset"),
];

/// The settings that are typed in rather than clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Startup,
    Terminal,
    FontFamily,
    MonoFamily,
}

impl Field {
    fn read(self, s: &Settings) -> String {
        match self {
            Field::Startup => s.startup_folder.clone(),
            Field::Terminal => s.terminal.clone(),
            Field::FontFamily => s.font_family.clone(),
            Field::MonoFamily => s.mono_family.clone(),
        }
    }

    /// Shown (gray) while the setting is empty.
    fn placeholder(self) -> &'static str {
        match self {
            Field::Startup => "Home folder",
            Field::Terminal => "Detect automatically",
            Field::FontFamily => "Verdana",
            Field::MonoFamily => "Courier New",
        }
    }
}

/// Keys that work in the file list but are not in any menu.
const LIST_KEYS: [(&str, &str); 8] = [
    ("Up / Down", "Move the selection"),
    ("Home / End", "First / last item"),
    ("PageUp / PageDown", "Move 10 items"),
    ("Esc", "Close a menu or the filter"),
    ("Ctrl+R", "Refresh (same as F5)"),
    ("F6", "Edit the location (same as Ctrl+L)"),
    ("Alt+C  (in the filter)", "Match case on / off"),
    ("Alt+T  (in the filter)", "Cycle All / Folders / Files"),
];

pub struct SettingsView {
    page: Page,
    /// The text field being edited, if any.
    editing: Option<(Field, LineEdit)>,
    field_focus: FocusHandle,
    root_focus: FocusHandle,
    /// A red message at the top of the page (e.g. "not a folder").
    note: Option<SharedString>,
    /// "Reset all settings" needs a second click.
    confirm_reset: bool,
    _blur: Option<Subscription>,
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // redraw whenever a setting changes (a click, a reset, ...)
        cx.observe_global::<SettingsChanged>(|_, cx| cx.notify()).detach();
        Self {
            page: Page::General,
            editing: None,
            field_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            note: None,
            confirm_reset: false,
            _blur: None,
        }
    }

    fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        self.commit(cx);
        self.page = page;
        self.note = None;
        self.confirm_reset = false;
        cx.notify();
    }

    // ---- text fields -------------------------------------------------------

    fn start_edit(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(cx);
        let current = field.read(&settings::get());
        self.editing = Some((field, LineEdit::new(current)));
        self.note = None;
        window.focus(&self.field_focus);
        // clicking anywhere else ends the edit and keeps what was typed
        self._blur = Some(cx.on_blur(&self.field_focus, window, |this, _, cx| this.commit(cx)));
        cx.notify();
    }

    /// Save the field being edited (no-op when none is).
    fn commit(&mut self, cx: &mut Context<Self>) {
        let Some((field, edit)) = self.editing.take() else { return };
        let text = edit.text.trim().to_string();
        if field == Field::Startup && !text.is_empty() && !settings::expand_tilde(&text).is_dir() {
            self.note = Some(format!("\"{text}\" is not a folder, so it was not saved.").into());
            cx.notify();
            return;
        }
        settings::update(cx, |s| match field {
            Field::Startup => s.startup_folder = text,
            Field::Terminal => s.terminal = text,
            Field::FontFamily => s.font_family = text,
            Field::MonoFamily => s.mono_family = text,
        });
        cx.notify();
    }

    fn field_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some((_, edit)) = self.editing.as_mut() else { return };
        match edit.handle_key(ev, cx) {
            EditOutcome::Submit => {
                self.commit(cx);
                window.focus(&self.root_focus);
            }
            EditOutcome::Cancel => {
                self.editing = None;
                window.focus(&self.root_focus);
                cx.notify();
            }
            EditOutcome::Edited => cx.notify(),
            EditOutcome::Ignored => {}
        }
    }

    fn text_field(&self, field: Field, cx: &mut Context<Self>) -> AnyElement {
        let frame = div()
            .w(px(260.))
            .min_h(px(22.))
            .px_1()
            .py_0p5()
            .overflow_hidden()
            .bg(rgb(theme::BG))
            .shadow(theme::sunken())
            .border_1();
        match &self.editing {
            Some((f, edit)) if *f == field => frame
                .id(("field", field as usize))
                .key_context("TextInput")
                .track_focus(&self.field_focus)
                .border_color(rgb(theme::ACCENT))
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    this.field_key(ev, window, cx)
                }))
                .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.stop_propagation()))
                .child(edit.render())
                .into_any_element(),
            _ => {
                let current = field.read(&settings::get());
                let (text, color) = if current.is_empty() {
                    (field.placeholder().to_string(), theme::OVERLAY)
                } else {
                    (current, theme::TEXT)
                };
                frame
                    .id(("field", field as usize))
                    .cursor_pointer()
                    .border_color(rgb(theme::SURFACE0))
                    .text_color(rgb(color))
                    .truncate()
                    .child(text)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.start_edit(field, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
            }
        }
    }

    // ---- pages ---------------------------------------------------------------

    fn page_general(&self, st: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let click_chips: Vec<AnyElement> = ClickMode::ALL
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let m = *m;
                chip(("click", i), m.label(), st.click_to_open == m, set!(cx, |s| s.click_to_open = m))
                    .into_any_element()
            })
            .collect();

        vec![
            group(
                "STARTUP",
                vec![
                    row(
                        "Start in folder",
                        "The folder the first pane opens on launch. Empty means your home folder.",
                        self.text_field(Field::Startup, cx),
                    ),
                    row(
                        "Window width",
                        "Size of the main window at launch (takes effect on the next start).",
                        stepper(
                            "win-w",
                            st.window_w,
                            " px",
                            set!(cx, |s| s.window_w = s.window_w.saturating_sub(20)),
                            set!(cx, |s| s.window_w += 20),
                        ),
                    ),
                    row(
                        "Window height",
                        "",
                        stepper(
                            "win-h",
                            st.window_h,
                            " px",
                            set!(cx, |s| s.window_h = s.window_h.saturating_sub(20)),
                            set!(cx, |s| s.window_h += 20),
                        ),
                    ),
                ],
            ),
            group(
                "BEHAVIOR",
                vec![
                    row("Open files and folders with", "", div().flex().gap_1().children(click_chips)),
                    row(
                        "Show hidden files by default",
                        "Applies to every open pane right away (Ctrl+H still toggles one pane).",
                        checkbox(
                            "cb-hidden",
                            st.show_hidden,
                            set!(cx, |s| s.show_hidden = !s.show_hidden),
                        ),
                    ),
                    row(
                        "Terminal program",
                        "Program name only, e.g. kitty. Empty tries $TERMINAL, then the common terminals.",
                        self.text_field(Field::Terminal, cx),
                    ),
                ],
            ),
        ]
    }

    fn page_appearance(&self, st: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            group(
                "TEXT",
                vec![
                    row(
                        "Font",
                        "Name of an installed font. An unknown name falls back to the system font.",
                        self.text_field(Field::FontFamily, cx),
                    ),
                    row(
                        "Monospace font",
                        "Used for the permissions column and file-type tags.",
                        self.text_field(Field::MonoFamily, cx),
                    ),
                    row(
                        "Font size",
                        "Scales text and spacing across the whole app.",
                        stepper(
                            "font-size",
                            st.font_size,
                            " px",
                            set!(cx, |s| s.font_size = s.font_size.saturating_sub(1)),
                            set!(cx, |s| s.font_size += 1),
                        ),
                    ),
                    row(
                        "Row height",
                        "Height of each row in the file list.",
                        stepper(
                            "row-height",
                            st.row_height,
                            " px",
                            set!(cx, |s| s.row_height = s.row_height.saturating_sub(2)),
                            set!(cx, |s| s.row_height += 2),
                        ),
                    ),
                ],
            ),
            group(
                "FILE NAMES",
                vec![
                    row(
                        "Underline names like links",
                        "",
                        checkbox(
                            "cb-underline",
                            st.underline_names,
                            set!(cx, |s| s.underline_names = !s.underline_names),
                        ),
                    ),
                    row(
                        "Full path in the pane title",
                        "Show the whole path instead of just the folder name.",
                        checkbox(
                            "cb-title-path",
                            st.title_full_path,
                            set!(cx, |s| s.title_full_path = !s.title_full_path),
                        ),
                    ),
                ],
            ),
        ]
    }

    fn page_files(&self, st: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let sort_chips: Vec<AnyElement> = SORT_KEYS
            .iter()
            .enumerate()
            .map(|(i, (key, _, label))| {
                let key = *key;
                chip(("sort", i), *label, st.sort_key == key, set!(cx, |s| s.sort_key = key))
                    .into_any_element()
            })
            .collect();

        let column_chips: Vec<AnyElement> = ["Type", "Size", "Modified", "Permissions"]
            .into_iter()
            .enumerate()
            .map(|(i, label)| {
                chip(("col", i), label, st.columns[i], set!(cx, |s| s.columns[i] = !s.columns[i]))
                    .into_any_element()
            })
            .collect();

        let date_chips: Vec<AnyElement> = DateFormat::ALL
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let f = *f;
                chip(("date", i), f.label(), st.date_format == f, set!(cx, |s| s.date_format = f))
                    .into_any_element()
            })
            .collect();

        let unit_chips: Vec<AnyElement> = SizeUnits::ALL
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let u = *u;
                chip(("units", i), u.label(), st.size_units == u, set!(cx, |s| s.size_units = u))
                    .into_any_element()
            })
            .collect();

        vec![
            group(
                "SORTING",
                vec![
                    row(
                        "Sort by",
                        "How new panes sort. Clicking a column header still changes one pane.",
                        div().flex().gap_1().children(sort_chips),
                    ),
                    row(
                        "Order",
                        "",
                        div().flex().gap_1().children([
                            chip("asc", "Ascending", st.sort_asc, set!(cx, |s| s.sort_asc = true))
                                .into_any_element(),
                            chip("desc", "Descending", !st.sort_asc, set!(cx, |s| s.sort_asc = false))
                                .into_any_element(),
                        ]),
                    ),
                    row(
                        "Folders first",
                        "Keep folders above files.",
                        checkbox(
                            "cb-folders-first",
                            st.folders_first,
                            set!(cx, |s| s.folders_first = !s.folders_first),
                        ),
                    ),
                    row(
                        "Natural number order",
                        "Sort file2 before file10.",
                        checkbox(
                            "cb-natural",
                            st.natural_sort,
                            set!(cx, |s| s.natural_sort = !s.natural_sort),
                        ),
                    ),
                ],
            ),
            group(
                "COLUMNS",
                vec![wide_row(
                    "Shown columns",
                    "The Name column is always shown. Right-clicking a column header still toggles one pane.",
                    column_chips,
                )],
            ),
            group(
                "FORMATS",
                vec![
                    wide_row("Date format", "How the Modified column looks.", date_chips),
                    wide_row("Size units", "", unit_chips),
                ],
            ),
            group(
                "FILTER",
                vec![row(
                    "Match case by default",
                    "Starting state of the \"Aa\" switch in the filter bar (Ctrl+F).",
                    checkbox(
                        "cb-filter-case",
                        st.filter_case,
                        set!(cx, |s| s.filter_case = !s.filter_case),
                    ),
                )],
            ),
        ]
    }

    fn page_layout(&self, st: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            group(
                "WINDOW PARTS",
                vec![
                    row(
                        "Menu bar",
                        "Hidden or not, Ctrl+, always opens Settings.",
                        checkbox("cb-menubar", st.show_menubar, set!(cx, |s| s.show_menubar = !s.show_menubar)),
                    ),
                    row(
                        "Toolbar",
                        "",
                        checkbox("cb-toolbar", st.show_toolbar, set!(cx, |s| s.show_toolbar = !s.show_toolbar)),
                    ),
                    row(
                        "Places sidebar",
                        "",
                        checkbox("cb-sidebar", st.show_sidebar, set!(cx, |s| s.show_sidebar = !s.show_sidebar)),
                    ),
                    row(
                        "Status bar",
                        "",
                        checkbox(
                            "cb-statusbar",
                            st.show_statusbar,
                            set!(cx, |s| s.show_statusbar = !s.show_statusbar),
                        ),
                    ),
                ],
            ),
            group(
                "SIDEBAR",
                vec![row(
                    "Sidebar width",
                    "Also what View > Reset Layout goes back to.",
                    stepper(
                        "sidebar-w",
                        st.sidebar_width,
                        " px",
                        set!(cx, |s| s.sidebar_width = s.sidebar_width.saturating_sub(10)),
                        set!(cx, |s| s.sidebar_width += 10),
                    ),
                )],
            ),
        ]
    }

    /// Read-only: built from the menu definitions, so it never goes stale.
    fn page_shortcuts(&self) -> Vec<AnyElement> {
        let mut groups = Vec::new();
        let sections = [
            ("FILE", menus::file_menu()),
            ("EDIT", menus::edit_menu()),
            ("VIEW", menus::view_menu()),
            ("GO", menus::go_menu()),
        ];
        for (title, entries) in sections {
            let rows: Vec<AnyElement> = entries
                .iter()
                .filter_map(|entry| match entry {
                    MenuEntry::Item { label, shortcut, .. } if !shortcut.is_empty() => {
                        Some(info_row(*label, *shortcut))
                    }
                    _ => None,
                })
                .collect();
            if !rows.is_empty() {
                groups.push(group(title, rows));
            }
        }
        groups.push(group(
            "FILE LIST",
            LIST_KEYS.iter().map(|(keys, what)| info_row(*what, *keys)).collect(),
        ));
        groups
    }

    fn page_about(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let file = Settings::file_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "(no config folder found)".to_string());
        let reset_label = if self.confirm_reset { "Click again to reset everything" } else { "Reset all settings..." };

        vec![
            group(
                "SETTINGS FILE",
                vec![
                    info_row("Saved automatically to", file),
                    row(
                        "Open the settings folder",
                        "Holds settings.txt and your saved places.",
                        chip(
                            "open-folder",
                            "Open",
                            false,
                            cx.listener(|_, _: &MouseDownEvent, _, _| {
                                let dir = Settings::file_path().and_then(|p| p.parent().map(|d| d.to_path_buf()));
                                if let Some(dir) = dir {
                                    let _ = std::fs::create_dir_all(&dir);
                                    model::open_external(&dir);
                                }
                            }),
                        ),
                    ),
                ],
            ),
            group(
                "RESET",
                vec![row(
                    "Back to the defaults",
                    "Every setting on every page. Your saved places are not touched.",
                    chip(
                        "reset-all",
                        reset_label,
                        self.confirm_reset,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            if this.confirm_reset {
                                this.confirm_reset = false;
                                settings::update(cx, |s| *s = Settings::default());
                            } else {
                                this.confirm_reset = true;
                            }
                            cx.notify();
                        }),
                    ),
                )],
            ),
        ]
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let st = settings::get();
        // same scaling rule as the main window, so Font size applies here too
        window.set_rem_size(px(16. * st.font_size as f32 / 12.));

        let body = match self.page {
            Page::General => self.page_general(&st, cx),
            Page::Appearance => self.page_appearance(&st, cx),
            Page::Files => self.page_files(&st, cx),
            Page::Layout => self.page_layout(&st, cx),
            Page::Shortcuts => self.page_shortcuts(),
            Page::About => self.page_about(cx),
        };

        let nav: Vec<_> = PAGES
            .iter()
            .enumerate()
            .map(|(i, (page, label))| {
                let page = *page;
                let selected = self.page == page;
                div()
                    .id(("nav", i))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .border_1()
                    .border_color(if selected { rgb(theme::ACCENT) } else { rgba(0x00000000) })
                    .bg(if selected { rgb(theme::HILITE) } else { rgba(0x00000000) })
                    .text_color(rgb(if selected { theme::TEXT } else { theme::LINK }))
                    .when(selected, |d| d.font_weight(FontWeight::BOLD))
                    .when(!selected, |d| {
                        d.underline().hover(|s| s.text_color(rgb(theme::LINK_HOVER)))
                    })
                    .child(*label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.go(page, cx)),
                    )
            })
            .collect();

        div()
            .id("settings-root")
            .key_context("Settings")
            .track_focus(&self.root_focus)
            // a click on anything that isn't a field takes the focus, which ends an edit
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| window.focus(&this.root_focus)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::MANTLE))
            .text_color(rgb(theme::TEXT))
            .font_family(st.font_family.clone())
            .text_size(px(st.font_size as f32))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .p_2()
                    .gap_2()
                    // page list
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_shrink_0()
                            .w(px(170.))
                            .h_full()
                            .p_2()
                            .gap_0p5()
                            .bg(rgb(theme::BG))
                            .border_1()
                            .border_color(rgb(theme::SURFACE0))
                            .shadow(theme::sunken())
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .mb_1()
                                    .bg(rgb(theme::TITLE_ACTIVE))
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(0xffffff))
                                    .child("SETTINGS"),
                            )
                            .children(nav),
                    )
                    // the page itself
                    .child(
                        div()
                            .id("settings-content")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .children(self.note.clone().map(|n| {
                                div()
                                    .mb_2()
                                    .px_2()
                                    .py_1()
                                    .bg(rgb(0xffe0e0))
                                    .border_1()
                                    .border_color(rgb(theme::ERROR))
                                    .text_color(rgb(theme::ERROR))
                                    .child(n)
                            }))
                            .children(body),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .bg(rgb(theme::MANTLE))
                    .border_t_1()
                    .border_color(rgb(theme::SURFACE0))
                    .shadow(theme::raised())
                    .text_xs()
                    .text_color(rgb(theme::SUBTEXT))
                    .child("Changes are saved and applied as you make them.")
                    .child(chip(
                        "close",
                        "Close",
                        false,
                        cx.listener(|_, _: &MouseDownEvent, window, _| window.remove_window()),
                    )),
            )
    }
}
