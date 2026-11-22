//! App settings. One plain struct, saved as `key=value` lines in
//! `<config dir>/gpui-explorer/settings.txt` (next to `places.txt`).
//!
//! * Read from anywhere (UI or not):   `settings::get().font_size`
//! * Change from the UI:               `settings::update(cx, |s| s.show_hidden = true)`
//!
//! `update` clamps the values, saves the file and fires `SettingsChanged`, which
//! the workspace observes to refresh the open panes, so every change is live.
//! Unknown keys and unparsable values in the file are ignored (defaults win).

use crate::{model::SortKey, theme};
use gpui::{App, Global};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock, RwLock},
};

// ---- small "pick one" enums -------------------------------------------------

/// Declares a fieldless enum with a stable file key and a UI label per variant.
macro_rules! choice {
    ($name:ident { $($var:ident => ($key:literal, $label:literal)),+ $(,)? }) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum $name { $($var),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),+];

            pub fn key(self) -> &'static str {
                match self { $($name::$var => $key),+ }
            }

            pub fn label(self) -> &'static str {
                match self { $($name::$var => $label),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($key => Some($name::$var),)+ _ => None }
            }
        }
    };
}

choice!(ClickMode {
    Double => ("double", "Double-click"),
    Single => ("single", "Single click"),
});

choice!(DateFormat {
    Iso => ("iso", "2026-11-22 18:36"),
    IsoDate => ("iso-date", "2026-11-22"),
    Eu => ("eu", "22/11/2026 18:36"),
    Us => ("us", "11/22/2026 6:36 PM"),
    Long => ("long", "Nov 22, 2026 18:36"),
});

choice!(SizeUnits {
    Binary => ("binary", "Binary (1 KB = 1024 bytes)"),
    Decimal => ("decimal", "Decimal (1 kB = 1000 bytes)"),
});

impl DateFormat {
    /// chrono `strftime` pattern for the Modified column.
    pub fn pattern(self) -> &'static str {
        match self {
            DateFormat::Iso => "%Y-%m-%d %H:%M",
            DateFormat::IsoDate => "%Y-%m-%d",
            DateFormat::Eu => "%d/%m/%Y %H:%M",
            DateFormat::Us => "%m/%d/%Y %I:%M %p",
            DateFormat::Long => "%b %-d, %Y %H:%M",
        }
    }
}

/// (value, file key, UI label) for every sort column.
pub const SORT_KEYS: [(SortKey, &str, &str); 4] = [
    (SortKey::Name, "name", "Name"),
    (SortKey::Type, "type", "Type"),
    (SortKey::Size, "size", "Size"),
    (SortKey::Modified, "modified", "Modified"),
];

fn sort_key_from(s: &str) -> Option<SortKey> {
    SORT_KEYS.iter().find(|(_, k, _)| *k == s).map(|(v, _, _)| *v)
}

fn sort_key_name(key: SortKey) -> &'static str {
    SORT_KEYS.iter().find(|(v, _, _)| *v == key).map(|(_, n, _)| *n).unwrap_or("name")
}

// ---- the settings -----------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    // General
    /// Folder the first pane opens on launch ("" = home folder).
    pub startup_folder: String,
    pub window_w: u32,
    pub window_h: u32,
    pub click_to_open: ClickMode,
    pub show_hidden: bool,
    /// Terminal program name ("" = $TERMINAL, then common terminals).
    pub terminal: String,

    // Appearance
    pub font_family: String,
    pub mono_family: String,
    pub font_size: u32,
    pub row_height: u32,
    pub underline_names: bool,
    pub title_full_path: bool,

    // Files
    pub sort_key: SortKey,
    pub sort_asc: bool,
    pub folders_first: bool,
    pub natural_sort: bool,
    /// Type, Size, Modified, Permissions (the Name column is always shown).
    pub columns: [bool; 4],
    pub date_format: DateFormat,
    pub size_units: SizeUnits,
    pub filter_case: bool,

    // Layout
    pub show_menubar: bool,
    pub show_toolbar: bool,
    pub show_sidebar: bool,
    pub show_statusbar: bool,
    pub sidebar_width: u32,
}

impl Default for Settings {
    /// Exactly how the app behaved before settings existed.
    fn default() -> Self {
        Self {
            startup_folder: String::new(),
            window_w: 1100,
            window_h: 700,
            click_to_open: ClickMode::Double,
            show_hidden: false,
            terminal: String::new(),

            font_family: theme::FONT.to_string(),
            mono_family: theme::FONT_MONO.to_string(),
            font_size: 12,
            row_height: 26,
            underline_names: true,
            title_full_path: false,

            sort_key: SortKey::Name,
            sort_asc: true,
            folders_first: true,
            natural_sort: false,
            columns: [true; 4],
            date_format: DateFormat::Iso,
            size_units: SizeUnits::Binary,
            filter_case: false,

            show_menubar: true,
            show_toolbar: true,
            show_sidebar: true,
            show_statusbar: true,
            sidebar_width: 190,
        }
    }
}

/// Control characters out, ends trimmed (keeps the one-line file format safe).
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
}

/// `~` and `~/x` mean the home folder.
pub fn expand_tilde(s: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    if s == "~" {
        home
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(s)
    }
}

impl Settings {
    pub fn file_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("gpui-explorer").join("settings.txt"))
    }

    /// Where the first pane opens: the configured folder if it exists, else home.
    pub fn startup_dir(&self) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        if self.startup_folder.is_empty() {
            return home;
        }
        let p = expand_tilde(&self.startup_folder);
        if p.is_dir() { p } else { home }
    }

    /// Pane column visibility in the pane's own layout (Name first, always on).
    pub fn pane_columns(&self) -> [bool; 5] {
        [true, self.columns[0], self.columns[1], self.columns[2], self.columns[3]]
    }

    /// Force every value into its valid range / non-empty form.
    fn clamp(&mut self) {
        self.window_w = self.window_w.clamp(640, 3840);
        self.window_h = self.window_h.clamp(400, 2160);
        self.font_size = self.font_size.clamp(9, 20);
        self.row_height = self.row_height.clamp(18, 48);
        self.sidebar_width = self.sidebar_width.clamp(90, 500);

        let startup = clean(&self.startup_folder);
        self.startup_folder = startup;
        let terminal = clean(&self.terminal);
        self.terminal = terminal;
        let font = clean(&self.font_family);
        self.font_family = if font.is_empty() { theme::FONT.to_string() } else { font };
        let mono = clean(&self.mono_family);
        self.mono_family = if mono.is_empty() { theme::FONT_MONO.to_string() } else { mono };
    }

    // ---- file format ----------------------------------------------------

    fn load() -> Self {
        let mut s = Self::default();
        let Some(text) = Self::file_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return s;
        };
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.trim_start().starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            s.set_key(key.trim(), value);
        }
        s.clamp();
        s
    }

    fn set_key(&mut self, key: &str, v: &str) {
        let flag = |default: bool| match v.trim() {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => default,
        };
        let num = |default: u32| v.trim().parse::<u32>().unwrap_or(default);

        match key {
            "startup_folder" => self.startup_folder = v.to_string(),
            "window_w" => self.window_w = num(self.window_w),
            "window_h" => self.window_h = num(self.window_h),
            "click_to_open" => {
                if let Some(m) = ClickMode::parse(v.trim()) {
                    self.click_to_open = m;
                }
            }
            "show_hidden" => self.show_hidden = flag(self.show_hidden),
            "terminal" => self.terminal = v.to_string(),

            "font_family" => self.font_family = v.to_string(),
            "mono_family" => self.mono_family = v.to_string(),
            "font_size" => self.font_size = num(self.font_size),
            "row_height" => self.row_height = num(self.row_height),
            "underline_names" => self.underline_names = flag(self.underline_names),
            "title_full_path" => self.title_full_path = flag(self.title_full_path),

            "sort_key" => {
                if let Some(k) = sort_key_from(v.trim()) {
                    self.sort_key = k;
                }
            }
            "sort_asc" => self.sort_asc = flag(self.sort_asc),
            "folders_first" => self.folders_first = flag(self.folders_first),
            "natural_sort" => self.natural_sort = flag(self.natural_sort),
            "col_type" => self.columns[0] = flag(self.columns[0]),
            "col_size" => self.columns[1] = flag(self.columns[1]),
            "col_modified" => self.columns[2] = flag(self.columns[2]),
            "col_perms" => self.columns[3] = flag(self.columns[3]),
            "date_format" => {
                if let Some(d) = DateFormat::parse(v.trim()) {
                    self.date_format = d;
                }
            }
            "size_units" => {
                if let Some(u) = SizeUnits::parse(v.trim()) {
                    self.size_units = u;
                }
            }
            "filter_case" => self.filter_case = flag(self.filter_case),

            "show_menubar" => self.show_menubar = flag(self.show_menubar),
            "show_toolbar" => self.show_toolbar = flag(self.show_toolbar),
            "show_sidebar" => self.show_sidebar = flag(self.show_sidebar),
            "show_statusbar" => self.show_statusbar = flag(self.show_statusbar),
            "sidebar_width" => self.sidebar_width = num(self.sidebar_width),
            _ => {}
        }
    }

    fn to_text(&self) -> String {
        let rows: Vec<(&str, String)> = vec![
            ("startup_folder", self.startup_folder.clone()),
            ("window_w", self.window_w.to_string()),
            ("window_h", self.window_h.to_string()),
            ("click_to_open", self.click_to_open.key().to_string()),
            ("show_hidden", self.show_hidden.to_string()),
            ("terminal", self.terminal.clone()),
            ("font_family", self.font_family.clone()),
            ("mono_family", self.mono_family.clone()),
            ("font_size", self.font_size.to_string()),
            ("row_height", self.row_height.to_string()),
            ("underline_names", self.underline_names.to_string()),
            ("title_full_path", self.title_full_path.to_string()),
            ("sort_key", sort_key_name(self.sort_key).to_string()),
            ("sort_asc", self.sort_asc.to_string()),
            ("folders_first", self.folders_first.to_string()),
            ("natural_sort", self.natural_sort.to_string()),
            ("col_type", self.columns[0].to_string()),
            ("col_size", self.columns[1].to_string()),
            ("col_modified", self.columns[2].to_string()),
            ("col_perms", self.columns[3].to_string()),
            ("date_format", self.date_format.key().to_string()),
            ("size_units", self.size_units.key().to_string()),
            ("filter_case", self.filter_case.to_string()),
            ("show_menubar", self.show_menubar.to_string()),
            ("show_toolbar", self.show_toolbar.to_string()),
            ("show_sidebar", self.show_sidebar.to_string()),
            ("show_statusbar", self.show_statusbar.to_string()),
            ("sidebar_width", self.sidebar_width.to_string()),
        ];
        let mut out = String::from("# gpui-explorer settings (edited from File > Settings)\n");
        for (k, v) in rows {
            out.push_str(&format!("{k}={v}\n"));
        }
        out
    }

    fn save(&self) {
        let Some(file) = Self::file_path() else { return };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, self.to_text());
    }
}

// ---- global access ------------------------------------------------------------

fn cell() -> &'static RwLock<Arc<Settings>> {
    static CELL: OnceLock<RwLock<Arc<Settings>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(Arc::new(Settings::load())))
}

/// The current settings (cheap: clones an `Arc`).
pub fn get() -> Arc<Settings> {
    cell().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Marker global: observers (`cx.observe_global::<SettingsChanged>`) run
/// whenever `update` actually changed something.
pub struct SettingsChanged;
impl Global for SettingsChanged {}

pub fn init(cx: &mut App) {
    cx.set_global(SettingsChanged);
}

/// Change settings: clamp, save to disk, notify observers. No-op when nothing changed.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let mut next = (*get()).clone();
    change(&mut next);
    next.clamp();
    if next == *get() {
        return;
    }
    next.save();
    *cell().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(next);
    cx.set_global(SettingsChanged);
}
