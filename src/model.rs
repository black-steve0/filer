//! Filesystem model: pure logic, no UI. Easy to unit test and extend
//! (sorting modes, search, file operations, trash, etc.).

use crate::settings::{self, SizeUnits};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub lname: String, // lowercase name, for fast sorting/filtering
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub kind: String, // "Folder", "PNG image", "Rust source", ...
    pub mode: u32,    // unix permission bits (0 when unknown)
}

fn make_entry(path: PathBuf, name: String) -> Entry {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&path).ok(); // follows symlinks
    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    Entry {
        lname: name.to_lowercase(),
        kind: kind_label(&path, is_dir),
        mode: meta.as_ref().map(|m| m.permissions().mode() & 0o777).unwrap_or(0),
        is_dir,
        size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
        modified: meta.and_then(|m| m.modified().ok()),
        name,
        path,
    }
}

/// Human-readable type shown in the "Type" column.
fn kind_label(path: &Path, is_dir: bool) -> String {
    if is_dir {
        return "Folder".into();
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let known = match ext.as_str() {
        "" => return "File".into(),
        "rs" => "Rust source",
        "py" => "Python script",
        "js" | "mjs" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "jsx" => "JavaScript (JSX)",
        "c" | "h" => "C source",
        "cpp" | "hpp" | "cc" => "C++ source",
        "go" => "Go source",
        "java" => "Java source",
        "lua" => "Lua script",
        "sh" | "bash" | "zsh" | "fish" => "Shell script",
        "md" => "Markdown",
        "txt" | "log" => "Text file",
        "rst" => "reStructuredText",
        "toml" => "TOML config",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "ini" | "conf" => "Config file",
        "xml" => "XML",
        "html" | "htm" => "HTML page",
        "css" => "Stylesheet",
        "pdf" => "PDF document",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => "Image",
        "svg" => "SVG image",
        "mp3" | "flac" | "ogg" | "wav" | "m4a" => "Audio",
        "mp4" | "mkv" | "webm" | "avi" | "mov" => "Video",
        "zip" | "tar" | "gz" | "xz" | "bz2" | "7z" | "zst" => "Archive",
        _ => return format!("{} file", ext.to_uppercase()),
    };
    known.into()
}

/// `rwxr-xr-x` style string for the "Permissions" column.
pub fn format_mode(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// Entry for one existing path (used to add a single file to a listing
/// without re-reading the whole folder).
pub fn entry_for(path: &Path) -> Option<Entry> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(make_entry(path.to_path_buf(), name))
}

/// Walk a directory, handing entries to `sink` in chunks. Stops early when
/// `cancel` is set. This is the slow part (one stat per file), so it is meant
/// to run off the UI thread.
fn scan(
    path: &Path,
    show_hidden: bool,
    cancel: &AtomicBool,
    mut sink: impl FnMut(Vec<Entry>),
) -> io::Result<()> {
    let mut chunk = Vec::with_capacity(256);
    for ent in std::fs::read_dir(path)? {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let Ok(ent) = ent else { continue };
        let name = ent.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        chunk.push(make_entry(ent.path(), name));
        if chunk.len() >= 256 {
            sink(std::mem::take(&mut chunk));
        }
    }
    if !chunk.is_empty() {
        sink(chunk);
    }
    Ok(())
}

#[allow(dead_code)]
/// Blocking read of a whole directory, folders first (used by tests / tools).
pub fn read_dir(path: &Path, show_hidden: bool) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    scan(path, show_hidden, &AtomicBool::new(false), |c| out.extend(c))?;
    sort_entries(&mut out, SortKey::Name, true, true, false);
    Ok(out)
}

/// A directory listing that fills in on a background thread. The UI polls it
/// every few tens of milliseconds and appends whatever has arrived.
pub struct DirLoader {
    buf: Mutex<Vec<Entry>>,
    done: AtomicBool,
    cancel: AtomicBool,
    error: Mutex<Option<String>>,
}

impl DirLoader {
    pub fn start(path: PathBuf, show_hidden: bool) -> Arc<Self> {
        let loader = Arc::new(Self {
            buf: Mutex::new(Vec::new()),
            done: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let worker = loader.clone();
        std::thread::spawn(move || {
            let result = scan(&path, show_hidden, &worker.cancel, |chunk| {
                if let Ok(mut b) = worker.buf.lock() {
                    b.extend(chunk);
                }
            });
            if let Err(e) = result {
                if let Ok(mut slot) = worker.error.lock() {
                    *slot = Some(e.to_string());
                }
            }
            worker.done.store(true, Ordering::Release);
        });
        loader
    }

    /// Stop the background scan (results are no longer wanted).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Everything that arrived since the last poll, whether the scan has
    /// finished, and the error if it failed.
    pub fn poll(&self) -> (Vec<Entry>, bool, Option<String>) {
        // read `done` first: if it is set, the buffer already holds the rest
        let finished = self.done.load(Ordering::Acquire);
        let batch = self.buf.lock().map(|mut b| std::mem::take(&mut *b)).unwrap_or_default();
        let error = if finished { self.error.lock().ok().and_then(|mut e| e.take()) } else { None };
        (batch, finished, error)
    }
}

pub fn format_size(bytes: u64) -> String {
    // Settings > Files > Size units
    let (base, units): (f64, [&str; 5]) = match settings::get().size_units {
        SizeUnits::Binary => (1024.0, ["B", "KB", "MB", "GB", "TB"]),
        SizeUnits::Decimal => (1000.0, ["B", "kB", "MB", "GB", "TB"]),
    };
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= base && i < units.len() - 1 {
        v /= base;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", units[i])
    }
}

pub fn format_modified(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => chrono::DateTime::<chrono::Local>::from(t)
            .format(settings::get().date_format.pattern())
            .to_string(),
        None => String::new(),
    }
}

/// Open a file with the desktop's default handler.
pub fn open_external(path: &Path) {
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// Create "New Folder", "New Folder 1", ... inside `parent`.
pub fn create_dir_unique(parent: &Path) -> io::Result<PathBuf> {
    let mut n = 0;
    loop {
        let name = if n == 0 { "New Folder".to_string() } else { format!("New Folder {n}") };
        let path = parent.join(&name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Move to the freedesktop trash using `gio trash` (or `trash-put` as a fallback).
pub fn trash(path: &Path) -> io::Result<()> {
    for (cmd, pre) in [("gio", Some("trash")), ("trash-put", None)] {
        let mut c = Command::new(cmd);
        if let Some(a) = pre {
            c.arg(a);
        }
        match c.arg(path).status() {
            Ok(s) if s.success() => return Ok(()),
            Ok(_) => return Err(io::Error::other(format!("`{cmd}` failed"))),
            Err(_) => continue,
        }
    }
    Err(io::Error::other("no trash tool found (install glib's `gio` or `trash-cli`)"))
}

/// Real program name of a terminal: follows symlinks (so Debian's
/// `x-terminal-emulator` -> `gnome-terminal.wrapper` is recognised as
/// gnome-terminal) and strips `.wrapper` / `.real` suffixes.
fn terminal_name(cmd: &str) -> String {
    let plain = |p: &Path| {
        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    };
    let mut name = plain(Path::new(cmd));
    // look the command up on $PATH and follow its symlinks
    let found = if cmd.contains('/') {
        Some(PathBuf::from(cmd))
    } else {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).map(|d| d.join(cmd)).find(|p| p.is_file())
        })
    };
    if let Some(real) = found.and_then(|p| std::fs::canonicalize(p).ok()) {
        let real = plain(&real);
        if !real.is_empty() {
            name = real;
        }
    }
    for suffix in [".wrapper", ".real"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.to_string();
        }
    }
    name
}

/// Open a terminal emulator in `dir` (honours $TERMINAL, then tries common
/// ones). Each terminal gets its own "start in this folder" flag, and one that
/// starts but dies straight away is skipped in favour of the next. Blocks for
/// a fraction of a second, so call it off the UI thread.
pub fn open_terminal(dir: &Path, custom: &str) -> Result<(), String> {
    use std::process::Stdio;

    let mut candidates: Vec<String> = Vec::new();
    // Settings > General > Terminal program wins over everything else
    if !custom.trim().is_empty() {
        candidates.push(custom.trim().to_string());
    }
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.trim().is_empty() {
            candidates.push(t);
        }
    }
    candidates.extend(
        [
            "x-terminal-emulator", "xdg-terminal-exec", "gnome-terminal", "kgx", "ptyxis",
            "konsole", "kitty", "alacritty", "foot", "xfce4-terminal", "wezterm",
            "tilix", "terminator", "lxterminal", "mate-terminal", "xterm", "st",
        ]
        .map(String::from),
    );

    let d = dir.to_string_lossy().into_owned();
    for c in candidates {
        let name = terminal_name(&c);
        let mut cmd = Command::new(&c);
        cmd.current_dir(dir)
            // shells trust $PWD when it matches, so don't leave the old one in place
            .env("PWD", dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match name.as_str() {
            // server-style terminals ignore the launcher's cwd: they need the flag
            "gnome-terminal" | "xfce4-terminal" | "tilix" | "terminator" | "foot"
            | "lxterminal" | "mate-terminal" | "ptyxis" => {
                cmd.arg(format!("--working-directory={d}"));
            }
            "kgx" => {
                cmd.arg("--working-directory").arg(&d);
            }
            "konsole" => {
                cmd.arg("--workdir").arg(&d);
            }
            "kitty" => {
                cmd.arg("--directory").arg(&d);
            }
            "alacritty" => {
                cmd.arg("--working-directory").arg(&d);
            }
            "wezterm" | "wezterm-gui" => {
                cmd.arg("start").arg("--cwd").arg(&d);
            }
            _ => {} // xterm, st, ...: inherit the cwd
        }
        let Ok(mut child) = cmd.spawn() else { continue };
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(40));
            match child.try_wait() {
                Ok(Some(status)) if !status.success() => break, // died: try the next one
                Ok(Some(_)) | Err(_) => return Ok(()),          // launcher finished fine
                Ok(None) => {}                                  // still running
            }
        }
        if matches!(child.try_wait(), Ok(None)) {
            return Ok(()); // it's up and running
        }
    }
    Err("no working terminal found (set the TERMINAL environment variable)".into())
}

// ---- sorting -----------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Type,
    Size,
    Modified,
}

/// Compare names so that digit runs compare as numbers: `file2` < `file10`.
/// Expects already-lowercased names (`Entry::lname`).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    use std::{iter::Peekable, str::Chars};

    fn take_number(it: &mut Peekable<Chars<'_>>) -> String {
        let mut s = String::new();
        while let Some(&c) = it.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                it.next();
            } else {
                break;
            }
        }
        s
    }

    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = take_number(&mut ai);
                let nb = take_number(&mut bi);
                let (na, nb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                // more digits = bigger number; same length compares digit by digit
                let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// Sort in place. With `folders_first` folders stay on top; `asc` flips the
/// order within each group. `natural` orders digit runs numerically.
pub fn sort_entries(v: &mut [Entry], key: SortKey, asc: bool, folders_first: bool, natural: bool) {
    v.sort_by(|a, b| {
        let group = if folders_first { b.is_dir.cmp(&a.is_dir) } else { std::cmp::Ordering::Equal };
        group.then_with(|| {
            let by_name = || {
                if natural { natural_cmp(&a.lname, &b.lname) } else { a.lname.cmp(&b.lname) }
            };
            let o = match key {
                SortKey::Name => by_name(),
                SortKey::Type => a.kind.cmp(&b.kind).then_with(by_name),
                SortKey::Size => {
                    let (x, y) = (if a.is_dir { 0 } else { a.size }, if b.is_dir { 0 } else { b.size });
                    x.cmp(&y).then_with(by_name)
                }
                SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
            };
            if asc { o } else { o.reverse() }
        })
    });
}

// ---- new file / copy / cut / paste ---------------------------------------

/// Create an empty "New File", "New File 1", ... inside `parent`.
pub fn create_file_unique(parent: &Path) -> io::Result<PathBuf> {
    let mut n = 0;
    loop {
        let name = if n == 0 { "New File".to_string() } else { format!("New File {n}") };
        let path = parent.join(&name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Internal file clipboard shared by every pane: (path, is_cut).
static FILE_CLIP: Mutex<Option<(PathBuf, bool)>> = Mutex::new(None);

pub fn set_file_clip(path: PathBuf, cut: bool) {
    if let Ok(mut c) = FILE_CLIP.lock() {
        *c = Some((path, cut));
    }
}

pub fn file_clip() -> Option<(PathBuf, bool)> {
    FILE_CLIP.lock().ok().and_then(|c| c.clone())
}

/// `dir/name`, or `dir/name copy`, `dir/name copy 2`, ... if taken.
fn unique_dest(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 1;
    loop {
        let label = if n == 1 { " copy".to_string() } else { format!(" copy {n}") };
        let cand = dir.join(format!("{stem}{label}{ext}"));
        if !cand.exists() {
            return cand;
        }
        n += 1;
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if std::fs::metadata(src)?.is_dir() {
        std::fs::create_dir(dst)?;
        for ent in std::fs::read_dir(src)? {
            let ent = ent?;
            copy_recursive(&ent.path(), &dst.join(ent.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// Copy or move `src` into `dir`. Returns the new path.
pub fn paste(src: &Path, cut: bool, dir: &Path) -> io::Result<PathBuf> {
    let name = src.file_name().ok_or_else(|| io::Error::other("bad source path"))?;
    if dir.starts_with(src) {
        return Err(io::Error::other("can't paste a folder into itself"));
    }
    if cut && src.parent() == Some(dir) {
        return Ok(src.to_path_buf()); // moving onto itself: nothing to do
    }
    let dst = unique_dest(dir, name);
    if cut {
        if std::fs::rename(src, &dst).is_err() {
            // different filesystem: copy then delete
            copy_recursive(src, &dst)?;
            if std::fs::metadata(src)?.is_dir() {
                std::fs::remove_dir_all(src)?;
            } else {
                std::fs::remove_file(src)?;
            }
        }
    } else {
        copy_recursive(src, &dst)?;
    }
    Ok(dst)
}