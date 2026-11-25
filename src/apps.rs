//! "Open With": which installed applications can open a file, and how to start one.
//! Pure std, no UI.
//!
//! Follows the freedesktop specs so the list matches what other file managers show:
//!
//! * applications are `.desktop` files in `$XDG_DATA_HOME/applications` and
//!   `$XDG_DATA_DIRS/*/applications` (the first one with a given desktop-file ID wins,
//!   so a user's own copy overrides the system one);
//! * an application handles the types named in its `MimeType=` line, plus whatever the
//!   user added in `mimeapps.list`, minus whatever they removed there;
//! * a type also falls back to its parent types (`text/x-rust` -> `text/plain`), using
//!   shared-mime-info's `subclasses` table;
//! * the default is the first installed entry for the type under `[Default Applications]`.
//!
//! Terminal programs (`Terminal=true`, e.g. vim) and hidden entries (`NoDisplay=true`)
//! are not offered.

use std::{
    collections::{HashMap, HashSet},
    io,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Our own desktop file (`dist/filer.desktop`): never offered as a way to open a file.
pub const SELF_ID: &str = "filer.desktop";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopApp {
    /// Desktop-file ID, e.g. `org.gnome.gedit.desktop`.
    pub id: String,
    pub name: String,
    pub exec: String,
    /// `Icon=` as written (a theme icon name or an absolute path).
    pub icon: String,
    pub mimes: Vec<String>,
    pub desktop_file: PathBuf,
}

/// What the Open With dialog needs for one file.
#[derive(Clone, Debug, Default)]
pub struct Lookup {
    /// The file's MIME type, when it could be worked out.
    pub mime: Option<String>,
    /// Applications that handle the type, default first, then the rest of the exact
    /// matches by name, then those that only handle a parent type.
    pub matching: Vec<DesktopApp>,
    /// Every installed application, by name.
    pub all: Vec<DesktopApp>,
    /// ID of the user's default application for the type, if any (it is `matching[0]`).
    pub default_id: Option<String>,
}

// ---- parsing ----------------------------------------------------------------------

/// `["en_US", "en"]` from `$LC_ALL` / `$LC_MESSAGES` / `$LANG`, for `Name[en]` keys.
fn locale_candidates() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty() && v != "C" && v != "POSIX"))
        .unwrap_or_default();
    // "en_US.UTF-8@euro" -> "en_US"
    let base = raw.split(['.', '@']).next().unwrap_or("").to_string();
    let mut out = Vec::new();
    if !base.is_empty() {
        out.push(base.clone());
        if let Some((lang, _)) = base.split_once('_') {
            out.push(lang.to_string());
        }
    }
    out
}

fn in_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).exists();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(cmd).exists()))
        .unwrap_or(false)
}

/// The `[Desktop Entry]` group of one `.desktop` file, as an `DesktopApp`. `None` for
/// anything that should not be offered (not an application, hidden, a terminal
/// program, or its `TryExec` program is not installed).
pub fn parse_desktop(id: &str, path: &Path, text: &str, locales: &[String]) -> Option<DesktopApp> {
    let mut keys: HashMap<&str, &str> = HashMap::new();
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if in_entry {
            if let Some((k, v)) = line.split_once('=') {
                keys.entry(k.trim()).or_insert_with(|| v.trim());
            }
        }
    }
    let flag = |k: &str| keys.get(k).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if keys.get("Type") != Some(&"Application") || flag("Hidden") || flag("NoDisplay") || flag("Terminal") {
        return None;
    }
    if let Some(try_exec) = keys.get("TryExec") {
        if !try_exec.is_empty() && !in_path(try_exec) {
            return None;
        }
    }
    let exec = keys.get("Exec").filter(|e| !e.is_empty())?.to_string();
    let name = locales
        .iter()
        .find_map(|l| keys.get(format!("Name[{l}]").as_str()))
        .or_else(|| keys.get("Name"))
        .filter(|n| !n.is_empty())?
        .to_string();
    let mimes = keys
        .get("MimeType")
        .map(|v| v.split(';').filter(|m| !m.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    Some(DesktopApp {
        id: id.to_string(),
        name,
        exec,
        icon: keys.get("Icon").unwrap_or(&"").to_string(),
        mimes,
        desktop_file: path.to_path_buf(),
    })
}

/// The user's choices from `mimeapps.list`, merged over the system's.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct MimeApps {
    pub default: HashMap<String, Vec<String>>,
    pub added: HashMap<String, Vec<String>>,
    pub removed: HashMap<String, Vec<String>>,
}

impl MimeApps {
    pub fn parse(text: &str) -> Self {
        let mut out = MimeApps::default();
        let mut section: Option<&mut HashMap<String, Vec<String>>> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                section = match line {
                    "[Default Applications]" => Some(&mut out.default),
                    "[Added Associations]" => Some(&mut out.added),
                    "[Removed Associations]" => Some(&mut out.removed),
                    _ => None,
                };
                continue;
            }
            if let (Some(map), Some((mime, ids))) = (section.as_mut(), line.split_once('=')) {
                let ids = ids.split(';').filter(|i| !i.is_empty()).map(str::to_string);
                map.entry(mime.trim().to_string()).or_default().extend(ids);
            }
        }
        out
    }

    /// Add `later`'s entries after ours: files read first have priority.
    fn merge(&mut self, later: MimeApps) {
        for (mine, theirs) in [
            (&mut self.default, later.default),
            (&mut self.added, later.added),
            (&mut self.removed, later.removed),
        ] {
            for (mime, ids) in theirs {
                mine.entry(mime).or_default().extend(ids);
            }
        }
    }
}

/// shared-mime-info `subclasses`: lines of `child parent`.
pub fn parse_subclasses(text: &str) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        if let (Some(child), Some(parent)) = (parts.next(), parts.next()) {
            map.entry(child.to_string()).or_default().push(parent.to_string());
        }
    }
    map
}

/// `mime` followed by its parent types, nearest first.
pub fn mime_chain(mime: &str, subclasses: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut chain = vec![mime.to_string()];
    let mut i = 0;
    while i < chain.len() && chain.len() < 16 {
        if let Some(parents) = subclasses.get(&chain[i]) {
            for p in parents {
                if !chain.contains(p) {
                    chain.push(p.clone());
                }
            }
        }
        i += 1;
    }
    // every text type is a kind of text/plain, whether or not the table says so
    if mime.starts_with("text/") && !chain.iter().any(|m| m == "text/plain") {
        chain.push("text/plain".into());
    }
    chain
}

/// The applications for `mime` out of `apps`, and the default's ID. Pure: the caller
/// supplies the installed applications and the user's associations.
pub fn select(
    mime: &str,
    apps: &[DesktopApp],
    assoc: &MimeApps,
    subclasses: &HashMap<String, Vec<String>>,
) -> (Vec<DesktopApp>, Option<String>) {
    let chain = mime_chain(mime, subclasses);
    let by_id: HashMap<&str, &DesktopApp> = apps.iter().map(|a| (a.id.as_str(), a)).collect();
    let mut sorted: Vec<&DesktopApp> = apps.iter().collect();
    sorted.sort_by_key(|a| a.name.to_lowercase());

    let mut out: Vec<DesktopApp> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for m in &chain {
        let removed = |id: &str| assoc.removed.get(m).is_some_and(|r| r.iter().any(|x| x == id));
        let added = assoc.added.get(m).into_iter().flatten().filter_map(|id| by_id.get(id.as_str()).copied());
        let declared = sorted.iter().copied().filter(|a| a.mimes.iter().any(|x| x == m));
        for app in added.chain(declared) {
            if !removed(&app.id) && seen.insert(app.id.as_str()) {
                out.push(app.clone());
            }
        }
    }

    // the default: the first installed application listed for the type (or its parents)
    let default_id = chain
        .iter()
        .flat_map(|m| assoc.default.get(m).into_iter().flatten())
        .find(|id| by_id.contains_key(id.as_str()))
        .cloned();
    if let Some(id) = &default_id {
        match out.iter().position(|a| &a.id == id) {
            Some(i) => {
                let app = out.remove(i);
                out.insert(0, app);
            }
            // the user chose an app that doesn't list the type: still offer it, first
            None => out.insert(0, (*by_id[id.as_str()]).clone()),
        }
    }
    (out, default_id)
}

// ---- finding what is installed -------------------------------------------------------

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

fn split_dirs(var: &str, default: &str) -> Vec<PathBuf> {
    let v = std::env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string());
    v.split(':').filter(|d| !d.is_empty()).map(PathBuf::from).collect()
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS` (plus Flatpak's export dirs, which some
/// sessions forget to add).
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![data_home()];
    dirs.extend(split_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
    for extra in [PathBuf::from("/var/lib/flatpak/exports/share"), home().join(".local/share/flatpak/exports/share")] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    dirs
}

/// Every `.desktop` file under `dir`, with its desktop-file ID (`sub/dir/x.desktop` is
/// `sub-dir-x.desktop`). `seen` holds the IDs already provided by earlier directories;
/// a shadowed or hidden entry still counts, so a user can hide a system one.
fn scan_dir(dir: &Path, prefix: &str, locales: &[String], seen: &mut HashSet<String>, out: &mut Vec<DesktopApp>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for ent in entries {
        let path = ent.path();
        let file = ent.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            scan_dir(&path, &format!("{prefix}{file}-"), locales, seen, out);
        } else if file.ends_with(".desktop") {
            let id = format!("{prefix}{file}");
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(app) = std::fs::read_to_string(&path).ok().and_then(|t| parse_desktop(&id, &path, &t, locales)) {
                out.push(app);
            }
        }
    }
}

/// All installed applications worth offering.
pub fn installed() -> Vec<DesktopApp> {
    let locales = locale_candidates();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in data_dirs() {
        scan_dir(&dir.join("applications"), "", &locales, &mut seen, &mut out);
    }
    out.retain(|a| a.id != SELF_ID);
    out
}

fn read_mimeapps() -> MimeApps {
    let mut files = vec![config_home().join("mimeapps.list")];
    files.extend(split_dirs("XDG_CONFIG_DIRS", "/etc/xdg").into_iter().map(|d| d.join("mimeapps.list")));
    files.extend(data_dirs().into_iter().map(|d| d.join("applications/mimeapps.list")));
    let mut all = MimeApps::default();
    for f in files {
        if let Ok(text) = std::fs::read_to_string(f) {
            all.merge(MimeApps::parse(&text));
        }
    }
    all
}

fn read_subclasses() -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for dir in data_dirs() {
        if let Ok(text) = std::fs::read_to_string(dir.join("mime/subclasses")) {
            for (child, parents) in parse_subclasses(&text) {
                let slot = map.entry(child).or_default();
                for p in parents {
                    if !slot.contains(&p) {
                        slot.push(p);
                    }
                }
            }
        }
    }
    map
}

/// Everything the dialog shows for a file of type `mime`. Reads a few hundred small
/// files, so call it off the UI thread.
pub fn lookup(mime: Option<&str>) -> Lookup {
    let mut all = installed();
    all.sort_by_key(|a| a.name.to_lowercase());
    let (matching, default_id) = match mime {
        Some(m) => select(m, &all, &read_mimeapps(), &read_subclasses()),
        None => (Vec::new(), None),
    };
    Lookup { mime: mime.map(str::to_string), matching, all, default_id }
}

// ---- starting an application ------------------------------------------------------------

/// Split an `Exec=` value into arguments: spaces separate them, double quotes group,
/// and inside quotes a backslash escapes `"`, `` ` ``, `$` and `\`.
pub fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted && matches!(chars.peek(), Some('"' | '`' | '$' | '\\')) => {
                cur.push(chars.next().unwrap());
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(cur);
    }
    args
}

/// The command lines to run to open `paths` with `app`, one per process.
///
/// `%f`/`%u` take a single file, so one process is started per path; `%F`/`%U` take
/// them all in one. An application whose `Exec` has no file code at all still gets the
/// files appended: choosing it from "Open With" means "give it this file".
pub fn expand_exec(app: &DesktopApp, paths: &[PathBuf]) -> Result<Vec<Vec<String>>, String> {
    let tokens = split_exec(&app.exec);
    if tokens.is_empty() {
        return Err(format!("{} has no command to run", app.name));
    }
    let has = |codes: &[&str]| tokens.iter().any(|t| codes.iter().any(|c| t.contains(c)));
    let single = has(&["%f", "%u"]);
    let multi = has(&["%F", "%U"]);

    // arguments for one process opening `files`
    let build = |files: &[PathBuf]| -> Vec<String> {
        let path_of = |i: usize| files.get(i).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let uri_of = |i: usize| files.get(i).map(|p| crate::uri::from_path(p)).unwrap_or_default();
        let mut argv: Vec<String> = Vec::new();
        for tok in &tokens {
            match tok.as_str() {
                "%F" => argv.extend(files.iter().map(|p| p.to_string_lossy().into_owned())),
                "%U" => argv.extend(files.iter().map(|p| crate::uri::from_path(p))),
                "%i" => {
                    if !app.icon.is_empty() {
                        argv.extend(["--icon".to_string(), app.icon.clone()]);
                    }
                }
                _ => {
                    let mut s = String::new();
                    let mut it = tok.chars();
                    while let Some(c) = it.next() {
                        if c != '%' {
                            s.push(c);
                            continue;
                        }
                        match it.next() {
                            Some('f') => s.push_str(&path_of(0)),
                            Some('u') => s.push_str(&uri_of(0)),
                            Some('c') => s.push_str(&app.name),
                            Some('k') => s.push_str(&app.desktop_file.to_string_lossy()),
                            Some('%') => s.push('%'),
                            _ => {} // %F %U inside a longer word, %i, deprecated codes: dropped
                        }
                    }
                    // a token that was only a dropped code disappears
                    if !s.is_empty() || !tok.starts_with('%') {
                        argv.push(s);
                    }
                }
            }
        }
        if !single && !multi {
            argv.extend(files.iter().map(|p| p.to_string_lossy().into_owned()));
        }
        argv
    };

    let runs: Vec<Vec<String>> = if single && !multi {
        paths.iter().map(|p| build(std::slice::from_ref(p))).collect()
    } else {
        vec![build(paths)]
    };
    if runs.iter().any(|argv| argv.is_empty()) {
        return Err(format!("{} has no command to run", app.name));
    }
    Ok(runs)
}

/// Open `paths` with `app`, detached from us. The files' folder is the working directory.
pub fn launch(app: &DesktopApp, paths: &[PathBuf]) -> Result<(), String> {
    let cwd = paths.first().and_then(|p| p.parent()).filter(|d| d.is_dir());
    for argv in expand_exec(app, paths)? {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", app.name))?;
        // reap it when it exits, so it doesn't linger as a zombie
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    Ok(())
}

/// Make `app_id` the default for `mime` (what "Always use this application" does).
/// `xdg-mime` writes it to the user's `mimeapps.list`.
pub fn set_default(app_id: &str, mime: &str) -> io::Result<()> {
    let out = Command::new("xdg-mime")
        .args(["default", app_id, mime])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| io::Error::other(format!("xdg-mime: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("xdg-mime failed: {}", String::from_utf8_lossy(&out.stderr).trim())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn app(id: &str, name: &str, exec: &str, mimes: &[&str]) -> DesktopApp {
        DesktopApp {
            id: id.into(),
            name: name.into(),
            exec: exec.into(),
            icon: "icon-x".into(),
            mimes: mimes.iter().map(|m| m.to_string()).collect(),
            desktop_file: PathBuf::from(format!("/usr/share/applications/{id}")),
        }
    }

    const GEDIT: &str = "[Desktop Entry]\nType=Application\nName=Text Editor\nName[ar]=محرر\nName[de_DE]=Texteditor\nName[de]=Editor\n\
        Exec=gedit %U\nIcon=gedit\nMimeType=text/plain;text/x-rust;\n\n[Desktop Action new]\nName=New Window\nExec=gedit --new-window\n";

    #[test]
    fn parses_a_desktop_file() {
        let a = parse_desktop("gedit.desktop", Path::new("/x/gedit.desktop"), GEDIT, &[]).unwrap();
        assert_eq!(a.name, "Text Editor");
        assert_eq!(a.exec, "gedit %U"); // not the action's Exec
        assert_eq!(a.mimes, ["text/plain", "text/x-rust"]);
        assert_eq!(a.icon, "gedit");
    }

    #[test]
    fn localized_names_prefer_the_most_specific() {
        let l = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let n = |loc: &[&str]| parse_desktop("g.desktop", Path::new("/g"), GEDIT, &l(loc)).unwrap().name;
        assert_eq!(n(&["de_DE", "de"]), "Texteditor");
        assert_eq!(n(&["de_AT", "de"]), "Editor");
        assert_eq!(n(&["fr_FR", "fr"]), "Text Editor");
    }

    #[test]
    fn hides_what_should_not_be_offered() {
        let p = |body: &str| {
            parse_desktop("x.desktop", Path::new("/x"), &format!("[Desktop Entry]\nType=Application\nName=X\nExec=x\n{body}"), &[])
        };
        assert!(p("").is_some());
        assert!(p("NoDisplay=true").is_none());
        assert!(p("Hidden=true").is_none());
        assert!(p("Terminal=true").is_none());
        assert!(p("TryExec=/definitely/not/installed").is_none());
        assert!(p("TryExec=sh").is_some());
        assert!(parse_desktop("x.desktop", Path::new("/x"), "[Desktop Entry]\nType=Link\nName=X\nExec=x\n", &[]).is_none());
        assert!(parse_desktop("x.desktop", Path::new("/x"), "[Desktop Entry]\nType=Application\nName=X\n", &[]).is_none());
    }

    #[test]
    fn splits_exec_like_the_spec() {
        assert_eq!(split_exec("gedit %U"), ["gedit", "%U"]);
        assert_eq!(split_exec(r#"sh -c "echo \"hi\" \$HOME" %f"#), ["sh", "-c", r#"echo "hi" $HOME"#, "%f"]);
        assert_eq!(split_exec(r#""/opt/My DesktopApp/run" --x"#), ["/opt/My DesktopApp/run", "--x"]);
        assert_eq!(split_exec("a  b   c"), ["a", "b", "c"]);
        assert_eq!(split_exec(r#"a "" b"#), ["a", "", "b"]);
    }

    #[test]
    fn exec_codes() {
        let files = [PathBuf::from("/h/a b.txt"), PathBuf::from("/h/c.txt")];
        // %F: one process, every file
        let a = app("m.desktop", "M", "m %F", &[]);
        assert_eq!(expand_exec(&a, &files).unwrap(), [["m", "/h/a b.txt", "/h/c.txt"]]);
        // %f: one process per file
        let a = app("m.desktop", "M", "m --open=%f", &[]);
        assert_eq!(expand_exec(&a, &files).unwrap(), [vec!["m", "--open=/h/a b.txt"], vec!["m", "--open=/h/c.txt"]]);
        // %U: URIs
        let a = app("m.desktop", "M", "m %U", &[]);
        assert_eq!(expand_exec(&a, &files[..1]).unwrap(), [["m", "file:///h/a%20b.txt"]]);
        // no file code: the file is appended anyway
        let a = app("m.desktop", "M", "m --flag", &[]);
        assert_eq!(expand_exec(&a, &files[..1]).unwrap(), [["m", "--flag", "/h/a b.txt"]]);
        // %i -> --icon, %c -> name, %% -> %, %k -> desktop file, deprecated codes vanish
        let a = app("m.desktop", "My DesktopApp", r#"m %i --name=%c 100%% %k %d %f"#, &[]);
        let argv = &expand_exec(&a, &files[..1]).unwrap()[0];
        assert_eq!(
            argv,
            &["m", "--icon", "icon-x", "--name=My DesktopApp", "100%", "/usr/share/applications/m.desktop", "/h/a b.txt"]
        );
        // flatpak's file-forwarding markers pass through untouched
        let a = app("f.desktop", "F", "flatpak run --file-forwarding org.x.DesktopApp @@ %F @@", &[]);
        assert_eq!(
            expand_exec(&a, &files[..1]).unwrap(),
            [["flatpak", "run", "--file-forwarding", "org.x.DesktopApp", "@@", "/h/a b.txt", "@@"]]
        );
        // nothing to run
        assert!(expand_exec(&app("e.desktop", "E", "  ", &[]), &files).is_err());
    }

    #[test]
    fn parses_mimeapps() {
        let m = MimeApps::parse(
            "[Default Applications]\ntext/plain=a.desktop;b.desktop;\n\n[Added Associations]\nimage/png=c.desktop;\n\
             [Removed Associations]\ntext/plain=d.desktop;\n[Other]\nx/y=z.desktop\n",
        );
        assert_eq!(m.default["text/plain"], ["a.desktop", "b.desktop"]);
        assert_eq!(m.added["image/png"], ["c.desktop"]);
        assert_eq!(m.removed["text/plain"], ["d.desktop"]);
        assert!(!m.default.contains_key("x/y"));
    }

    #[test]
    fn mime_parents() {
        let sub = parse_subclasses("# c\ntext/x-rust text/plain\ntext/plain application/octet-stream\nimage/svg+xml application/xml\n");
        assert_eq!(mime_chain("text/x-rust", &sub), ["text/x-rust", "text/plain", "application/octet-stream"]);
        assert_eq!(mime_chain("text/x-unknown", &sub), ["text/x-unknown", "text/plain"]); // any text/* falls back
        assert_eq!(mime_chain("image/png", &sub), ["image/png"]);
        // cycles don't hang
        let cyc = parse_subclasses("a/x a/y\na/y a/x\n");
        assert_eq!(mime_chain("a/x", &cyc), ["a/x", "a/y"]);
    }

    #[test]
    fn selection_order_defaults_and_removals() {
        let apps = vec![
            app("zed.desktop", "Zed", "zed %F", &["text/x-rust"]),
            app("gedit.desktop", "gedit", "gedit %U", &["text/plain"]),
            app("kate.desktop", "Kate", "kate %U", &["text/plain", "text/x-rust"]),
            app("viewer.desktop", "Viewer", "viewer %f", &["image/png"]),
            app("nano.desktop", "Nano", "nano %f", &["text/plain"]),
        ];
        let sub = parse_subclasses("text/x-rust text/plain\n");
        let names = |v: &[DesktopApp]| v.iter().map(|a| a.name.clone()).collect::<Vec<_>>();

        // exact matches by name first, then parent-type apps by name
        let (got, def) = select("text/x-rust", &apps, &MimeApps::default(), &sub);
        assert_eq!(names(&got), ["Kate", "Zed", "gedit", "Nano"]);
        assert_eq!(def, None);

        // the user's default jumps to the front (even from the parent type)
        let assoc = MimeApps::parse("[Default Applications]\ntext/plain=missing.desktop;gedit.desktop;\n");
        let (got, def) = select("text/x-rust", &apps, &assoc, &sub);
        assert_eq!(names(&got), ["gedit", "Kate", "Zed", "Nano"]);
        assert_eq!(def.as_deref(), Some("gedit.desktop")); // skipped the uninstalled one

        // a default that doesn't declare the type is still offered, first
        let assoc = MimeApps::parse("[Default Applications]\nimage/png=nano.desktop\n");
        let (got, _) = select("image/png", &apps, &assoc, &sub);
        assert_eq!(names(&got), ["Nano", "Viewer"]);

        // added and removed associations
        let assoc = MimeApps::parse("[Added Associations]\nimage/png=zed.desktop;\n[Removed Associations]\nimage/png=viewer.desktop;\n");
        let (got, _) = select("image/png", &apps, &assoc, &sub);
        assert_eq!(names(&got), ["Zed"]);

        // nothing handles it
        assert!(select("video/mp4", &apps, &MimeApps::default(), &sub).0.is_empty());
    }

    #[test]
    fn scans_directories_with_overrides_and_subfolders() {
        let root = std::env::temp_dir().join(format!("filer-apps-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (user, system) = (root.join("user"), root.join("system"));
        fs::create_dir_all(system.join("kde")).unwrap();
        fs::create_dir_all(&user).unwrap();
        let entry = |name: &str, extra: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec=x\n{extra}");
        fs::write(system.join("a.desktop"), entry("A system", "")).unwrap();
        fs::write(system.join("b.desktop"), entry("B system", "")).unwrap();
        fs::write(system.join("kde/k.desktop"), entry("K", "")).unwrap();
        fs::write(system.join("readme.txt"), "not a desktop file").unwrap();
        fs::write(user.join("a.desktop"), entry("A user", "")).unwrap(); // overrides
        fs::write(user.join("b.desktop"), entry("B user", "NoDisplay=true")).unwrap(); // hides

        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for dir in [&user, &system] {
            scan_dir(dir, "", &[], &mut seen, &mut out);
        }
        let mut got: Vec<(String, String)> = out.iter().map(|a| (a.id.clone(), a.name.clone())).collect();
        got.sort();
        assert_eq!(got, [("a.desktop".into(), "A user".into()), ("kde-k.desktop".into(), "K".into())]);
    }

    #[test]
    fn launches_detached_in_the_files_folder() {
        let root = std::env::temp_dir().join(format!("filer-apps-launch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("doc one.txt");
        fs::write(&file, "x").unwrap();
        // `touch "<file>.ran"`: proves %f was filled in; `pwd` proves the working directory
        let a = app("t.desktop", "T", r#"sh -c "pwd > \"$0.ran\"" %f"#, &[]);
        launch(&a, std::slice::from_ref(&file)).unwrap();
        let ran = root.join("doc one.txt.ran");
        for _ in 0..100 {
            if ran.exists() && fs::metadata(&ran).map(|m| m.len() > 0).unwrap_or(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let cwd = fs::read_to_string(&ran).expect("the program ran");
        assert_eq!(Path::new(cwd.trim()).canonicalize().unwrap(), root.canonicalize().unwrap());
        // a program that doesn't exist is an error, not a panic
        assert!(launch(&app("n.desktop", "Nope", "/no/such/program %f", &[]), &[file]).is_err());
    }
}
