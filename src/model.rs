//! Filesystem model: pure logic, no UI. Easy to unit test and extend
//! (sorting modes, search, file operations, trash, etc.).

use crate::{
    settings::{self, SizeUnits},
    transfer::{place, unique_dest},
};
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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
        mode: meta
            .as_ref()
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0),
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
    scan(path, show_hidden, &AtomicBool::new(false), |c| {
        out.extend(c)
    })?;
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
        let batch = self
            .buf
            .lock()
            .map(|mut b| std::mem::take(&mut *b))
            .unwrap_or_default();
        let error = if finished {
            self.error.lock().ok().and_then(|mut e| e.take())
        } else {
            None
        };
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
        let name = if n == 0 {
            "New Folder".to_string()
        } else {
            format!("New Folder {n}")
        };
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
    Err(io::Error::other(
        "no trash tool found (install glib's `gio` or `trash-cli`)",
    ))
}

/// Real program name of a terminal: follows symlinks (so Debian's
/// `x-terminal-emulator` -> `gnome-terminal.wrapper` is recognised as
/// gnome-terminal) and strips `.wrapper` / `.real` suffixes.
fn terminal_name(cmd: &str) -> String {
    let plain = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut name = plain(Path::new(cmd));
    // look the command up on $PATH and follow its symlinks
    let found = if cmd.contains('/') {
        Some(PathBuf::from(cmd))
    } else {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(cmd))
                .find(|p| p.is_file())
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
            "x-terminal-emulator",
            "xdg-terminal-exec",
            "gnome-terminal",
            "kgx",
            "ptyxis",
            "konsole",
            "kitty",
            "alacritty",
            "foot",
            "xfce4-terminal",
            "wezterm",
            "tilix",
            "terminator",
            "lxterminal",
            "mate-terminal",
            "xterm",
            "st",
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
            "ghostty" => {
                println!("ghostyy");
                cmd.arg("+new-window")
                    .arg("--working-directory")
                    .arg(&d);
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
        let group = if folders_first {
            b.is_dir.cmp(&a.is_dir)
        } else {
            std::cmp::Ordering::Equal
        };
        group.then_with(|| {
            let by_name = || {
                if natural {
                    natural_cmp(&a.lname, &b.lname)
                } else {
                    a.lname.cmp(&b.lname)
                }
            };
            let o = match key {
                SortKey::Name => by_name(),
                SortKey::Type => a.kind.cmp(&b.kind).then_with(by_name),
                SortKey::Size => {
                    let (x, y) = (
                        if a.is_dir { 0 } else { a.size },
                        if b.is_dir { 0 } else { b.size },
                    );
                    x.cmp(&y).then_with(by_name)
                }
                SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
            };
            if asc {
                o
            } else {
                o.reverse()
            }
        })
    });
}

// ---- new file / copy / cut / paste ---------------------------------------

/// Create an empty "New File", "New File 1", ... inside `parent`.
pub fn create_file_unique(parent: &Path) -> io::Result<PathBuf> {
    let mut n = 0;
    loop {
        let name = if n == 0 {
            "New File".to_string()
        } else {
            format!("New File {n}")
        };
        let path = parent.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Internal file clipboard shared by every pane: (paths, is_cut).
static FILE_CLIP: Mutex<Option<(Vec<PathBuf>, bool)>> = Mutex::new(None);

pub fn set_file_clip(paths: Vec<PathBuf>, cut: bool) {
    if let Ok(mut c) = FILE_CLIP.lock() {
        *c = Some((paths, cut));
    }
}

pub fn file_clip() -> Option<(Vec<PathBuf>, bool)> {
    FILE_CLIP.lock().ok().and_then(|c| c.clone())
}

/// Copy or move `src` into `dir`. Returns the new path.
pub fn paste(src: &Path, cut: bool, dir: &Path) -> io::Result<PathBuf> {
    let name = src
        .file_name()
        .ok_or_else(|| io::Error::other("bad source path"))?;
    if dir.starts_with(src) {
        return Err(io::Error::other("can't paste a folder into itself"));
    }
    if cut && src.parent() == Some(dir) {
        return Ok(src.to_path_buf()); // moving onto itself: nothing to do
    }
    let dst = unique_dest(dir, name);
    place(src, &dst, cut)?;
    Ok(dst)
}

// ---- drag and drop ----------------------------------------------------------
//
// Dropping files into a folder is handled by `crate::transfer`, which asks what to
// do when a name is already taken.

/// gpui can receive drops from other apps but cannot start an OS-level drag
/// out of the window, so dragging *out* goes through a tiny helper window
/// that shows the file and acts as the drag source (`ripdrag`, `dragon-drop`).
pub fn drag_out(paths: &[PathBuf]) -> Result<(), String> {
    for tool in ["ripdrag", "dragon-drop", "dragon", "xdragon"] {
        match Command::new(tool)
            .arg("-x") // close the helper after one successful drop
            .args(paths)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait(); // reap it, no zombie
                });
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{tool}: {e}")),
        }
    }
    Err("no drag-out helper found (install ripdrag or dragon-drop)".into())
}

// ---- archives (compress / extract) ------------------------------------------
//
// Done with the usual command-line tools, so there are no extra crates: the
// first tool of each list that is installed is used. Both functions block, so
// call them from a background thread.

#[derive(Clone, Copy)]
pub enum Archive {
    Zip,
    TarGz,
}

const TAR_EXTS: [&str; 7] = [
    ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tbz2", ".txz",
];
const SINGLE_EXTS: [&str; 4] = [".gz", ".bz2", ".xz", ".zst"];
const OTHER_EXTS: [&str; 4] = [".zip", ".7z", ".rar", ".tar"];

fn has_ext(name: &str, ext: &str) -> bool {
    name.len() >= ext.len()
        && name
            .get(name.len() - ext.len()..)
            .is_some_and(|t| t.eq_ignore_ascii_case(ext))
}

/// Does the file name look like something `extract` can open?
pub fn is_archive(name: &str) -> bool {
    TAR_EXTS
        .iter()
        .chain(&SINGLE_EXTS)
        .chain(&OTHER_EXTS)
        .any(|e| has_ext(name, e))
}

/// "photos.tar.gz" -> "photos"
fn strip_archive_ext(name: &str) -> String {
    for e in TAR_EXTS.iter().chain(&OTHER_EXTS).chain(&SINGLE_EXTS) {
        if has_ext(name, e) && name.len() > e.len() {
            return name[..name.len() - e.len()].to_string();
        }
    }
    name.to_string()
}

/// `base`, or `stem 2.ext`, `stem 3.ext`, ... if taken. `ext` includes the dot (or is empty).
fn free_name(dir: &Path, stem: &str, ext: &str) -> String {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem}{ext}")
        } else {
            format!("{stem} {n}{ext}")
        };
        if !dir.join(&name).exists() {
            return name;
        }
        n += 1;
    }
}

/// Run the first installed tool; fail with its last stderr line if it fails.
fn try_tools(cwd: &Path, tools: &[(&str, Vec<String>)], hint: &str) -> Result<(), String> {
    for (tool, args) in tools {
        match Command::new(tool)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .output()
        {
            // unzip exits 1 for harmless warnings
            Ok(out)
                if out.status.success() || (*tool == "unzip" && out.status.code() == Some(1)) =>
            {
                return Ok(())
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                let last = err
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("unknown error");
                return Err(format!("{tool}: {}", last.trim()));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{tool}: {e}")),
        }
    }
    Err(format!("no tool found, install {hint}"))
}

/// Compress a file or folder next to itself ("name.zip" / "name.tar.gz").
pub fn compress(src: &Path, kind: Archive) -> Result<PathBuf, String> {
    let parent = src.parent().ok_or("bad path")?;
    let name = src
        .file_name()
        .ok_or("bad path")?
        .to_string_lossy()
        .into_owned();
    compress_names(parent, &name, std::slice::from_ref(&name), kind)
}

/// Compress several items into one archive next to them, named after their folder
/// ("photos.zip"). One item is the same as `compress`.
pub fn compress_many(srcs: &[PathBuf], kind: Archive) -> Result<PathBuf, String> {
    match srcs {
        [] => Err("nothing to compress".into()),
        [one] => compress(one, kind),
        [first, ..] => {
            let parent = first.parent().ok_or("bad path")?;
            let mut names = Vec::new();
            for src in srcs {
                if src.parent() != Some(parent) {
                    return Err("the items are in different folders".into());
                }
                names.push(
                    src.file_name()
                        .ok_or("bad path")?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            let base = parent
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Archive".to_string());
            compress_names(parent, &base, &names, kind)
        }
    }
}

/// Pack `names` (all inside `parent`) into a new archive called `base` + extension.
fn compress_names(
    parent: &Path,
    base: &str,
    names: &[String],
    kind: Archive,
) -> Result<PathBuf, String> {
    let (ext, hint) = match kind {
        Archive::Zip => (".zip", "zip, bsdtar or 7z"),
        Archive::TarGz => (".tar.gz", "tar"),
    };
    let out_name = free_name(parent, base, ext);
    // tool arguments: the fixed part, the archive name, then every item
    let args = |fixed: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = fixed.iter().map(|x| x.to_string()).collect();
        v.push(out_name.clone());
        v.extend(names.iter().cloned());
        v
    };
    let tools: Vec<(&str, Vec<String>)> = match kind {
        Archive::Zip => vec![
            ("zip", args(&["-r", "-q"])),
            ("bsdtar", args(&["-a", "-cf"])),
            ("7z", args(&["a", "-y"])),
            ("7zz", args(&["a", "-y"])),
        ],
        Archive::TarGz => vec![("tar", args(&["-czf"])), ("bsdtar", args(&["-czf"]))],
    };
    let out = parent.join(&out_name);
    let res = try_tools(parent, &tools, hint);
    if res.is_err() {
        let _ = std::fs::remove_file(&out); // no half-written archive
    }
    res.map(|_| out)
}

/// Extract into a new folder next to the archive (named after it), so an
/// archive full of loose files can never litter the current folder.
pub fn extract(archive: &Path) -> Result<PathBuf, String> {
    let parent = archive.parent().ok_or("bad path")?;
    let name = archive
        .file_name()
        .ok_or("bad path")?
        .to_string_lossy()
        .into_owned();
    let dest = parent.join(free_name(parent, &strip_archive_ext(&name), ""));
    std::fs::create_dir(&dest).map_err(|e| format!("can't create folder: {e}"))?;

    let s = |x: &str| x.to_string();
    let a = archive.to_string_lossy().into_owned();
    let d = dest.to_string_lossy().into_owned();
    let res = if has_ext(&name, ".zip") {
        try_tools(
            parent,
            &[
                ("unzip", vec![s("-q"), a.clone(), s("-d"), d.clone()]),
                ("bsdtar", vec![s("-xf"), a.clone(), s("-C"), d.clone()]),
                ("7z", vec![s("x"), s("-y"), format!("-o{d}"), a.clone()]),
            ],
            "unzip, bsdtar or 7z",
        )
    } else if has_ext(&name, ".tar") || TAR_EXTS.iter().any(|e| has_ext(&name, e)) {
        try_tools(
            parent,
            &[
                ("tar", vec![s("-xf"), a.clone(), s("-C"), d.clone()]),
                ("bsdtar", vec![s("-xf"), a.clone(), s("-C"), d.clone()]),
            ],
            "tar",
        )
    } else if has_ext(&name, ".7z") {
        try_tools(
            parent,
            &[
                ("7z", vec![s("x"), s("-y"), format!("-o{d}"), a.clone()]),
                ("7zz", vec![s("x"), s("-y"), format!("-o{d}"), a.clone()]),
                ("bsdtar", vec![s("-xf"), a.clone(), s("-C"), d.clone()]),
            ],
            "7z",
        )
    } else if has_ext(&name, ".rar") {
        try_tools(
            parent,
            &[
                ("unrar", vec![s("x"), s("-y"), a.clone(), format!("{d}/")]),
                ("7z", vec![s("x"), s("-y"), format!("-o{d}"), a.clone()]),
                ("bsdtar", vec![s("-xf"), a.clone(), s("-C"), d.clone()]),
            ],
            "unrar, 7z or bsdtar",
        )
    } else {
        // a single compressed file (.gz, .bz2, .xz, .zst): decompress to dest/<stem>
        let tool = if has_ext(&name, ".gz") {
            "gzip"
        } else if has_ext(&name, ".bz2") {
            "bzip2"
        } else if has_ext(&name, ".xz") {
            "xz"
        } else {
            "zstd"
        };
        decompress_single(tool, archive, &dest.join(strip_archive_ext(&name)))
    };
    if res.is_err() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    res.map(|_| dest)
}

fn decompress_single(tool: &str, archive: &Path, out: &Path) -> Result<(), String> {
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    match Command::new(tool)
        .arg("-dc")
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(file))
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            Err(format!(
                "{tool}: {}",
                err.lines().next().unwrap_or("failed").trim()
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err(format!("no tool found, install {tool}"))
        }
        Err(e) => Err(format!("{tool}: {e}")),
    }
}
