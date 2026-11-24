//! Quick folder jump: the data behind the "Go to folder" box (Ctrl+P).
//!
//! * An index of folders (your home folder a few levels deep, mounted drives, the
//!   top of `/`) is built once on a background thread and refreshed now and then.
//! * Folders you have been to are remembered with a visit count (saved in
//!   `~/.local/share/filer/visits.txt`), so the ones you use most come first.
//! * `search` ranks folders for what you typed; `complete_path` completes a path
//!   you are typing (`~/Doc` -> `~/Documents`).
//!
//! Everything here is plain std; nothing touches the UI thread except `search`
//! and `complete_path`, which are fast enough to run on every keystroke.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, RwLock,
    },
    time::{Duration, Instant},
};

const MAX_DIRS: usize = 60_000;
const HOME_DEPTH: usize = 5;
const MAX_VISITS_KEPT: usize = 400;
const REFRESH_AFTER: Duration = Duration::from_secs(300);
/// Folders that are never worth jumping to (and slow to scan).
const SKIP: [&str; 8] =
    ["node_modules", "target", "__pycache__", "site-packages", "venv", "proc", "sys", "dev"];

struct Dir {
    path: PathBuf,
    name: String,  // lowercase last component
    lower: String, // lowercase full path
    depth: usize,  // components below the root it was found under
}

static INDEX: RwLock<Vec<Dir>> = RwLock::new(Vec::new());
static BUILDING: AtomicBool = AtomicBool::new(false);
static BUILT_AT: Mutex<Option<Instant>> = Mutex::new(None);
static VISITS: Mutex<Vec<(PathBuf, u32)>> = Mutex::new(Vec::new());
static SAVE_LOCK: Mutex<()> = Mutex::new(());

// ---- public API ------------------------------------------------------------------

/// Load the visit history and build the index in the background. Call once at startup.
pub fn start() {
    std::thread::spawn(|| {
        load_visits();
        build_index();
    });
}

/// Rebuild the index in the background when it is old (cheap to call often).
pub fn refresh_if_stale() {
    let stale = BUILT_AT.lock().ok().and_then(|t| *t).map_or(true, |t| t.elapsed() > REFRESH_AFTER);
    if stale && !BUILDING.load(Ordering::Relaxed) {
        std::thread::spawn(build_index);
    }
}

/// Remember that the user went to `path`.
pub fn visit(path: &Path) {
    if !path.is_dir() {
        return;
    }
    if let Ok(mut visits) = VISITS.lock() {
        match visits.iter_mut().find(|(p, _)| p == path) {
            Some((_, n)) => *n += 1,
            None => visits.push((path.to_path_buf(), 1)),
        }
        if visits.len() > MAX_VISITS_KEPT {
            visits.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            visits.truncate(MAX_VISITS_KEPT);
        }
        let snapshot = visits.clone();
        drop(visits);
        std::thread::spawn(move || save_visits(&snapshot));
    }
    // a folder created after the last scan is findable right away
    if let Ok(mut index) = INDEX.write() {
        if !index.iter().any(|d| d.path == path) {
            index.push(make_dir(path.to_path_buf(), depth_of(path)));
        }
    }
}

/// `~/Documents` instead of `/home/me/Documents`.
pub fn display(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if path == home {
            return "~".into();
        }
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.to_string_lossy());
        }
    }
    path.to_string_lossy().into_owned()
}

/// `~` and `~/x` to the home folder.
pub fn expand(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Does what the user typed look like a path (rather than words to search for)?
pub fn looks_like_path(text: &str) -> bool {
    text.starts_with('/') || text.starts_with('~') || text.contains('/')
}

/// The best `limit` folders for `query` (words separated by spaces; every word must
/// match). An empty query lists the most visited folders.
pub fn search(query: &str, current: &Path, limit: usize) -> Vec<PathBuf> {
    refresh_if_stale();
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    let visits: HashMap<PathBuf, u32> =
        VISITS.lock().map(|v| v.iter().cloned().collect()).unwrap_or_default();

    if words.is_empty() {
        let mut top: Vec<(&PathBuf, &u32)> = visits.iter().filter(|(p, _)| p.as_path() != current).collect();
        top.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        return top.into_iter().filter(|(p, _)| p.is_dir()).take(limit).map(|(p, _)| p.clone()).collect();
    }

    let Ok(index) = INDEX.read() else { return Vec::new() };
    let mut scored: Vec<(i32, usize)> = Vec::new();
    for (i, dir) in index.iter().enumerate() {
        if dir.path == current {
            continue;
        }
        let Some(mut score) = score_dir(dir, &words) else { continue };
        score += (*visits.get(&dir.path).unwrap_or(&0)).min(20) as i32 * 2;
        score -= dir.depth as i32 * 2;
        if dir.path.parent() == Some(current) {
            score += 15; // a folder right here is the likeliest target
        } else if dir.path.starts_with(current) {
            score += 6;
        }
        scored.push((score, i));
    }
    scored.sort_unstable_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| index[a.1].lower.len().cmp(&index[b.1].lower.len()))
    });
    scored.into_iter().take(limit).map(|(_, i)| index[i].path.clone()).collect()
}

/// Folders that complete the path being typed: `~/Do` -> `~/Documents`, `~/Downloads`.
pub fn complete_path(input: &str, limit: usize) -> Vec<PathBuf> {
    let Some(slash) = input.rfind('/') else { return Vec::new() };
    let (dir_part, prefix) = (&input[..=slash], input[slash + 1..].to_lowercase());
    let dir = expand(dir_part);
    let Ok(read) = std::fs::read_dir(&dir) else { return Vec::new() };
    let show_hidden = prefix.starts_with('.');

    let mut starts: Vec<(String, PathBuf)> = Vec::new();
    let mut contains: Vec<(String, PathBuf)> = Vec::new();
    for entry in read.flatten().take(5000) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !show_hidden {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let lower = name.to_lowercase();
        if lower.starts_with(&prefix) {
            starts.push((lower, path));
        } else if lower.contains(&prefix) {
            contains.push((lower, path));
        }
    }
    starts.sort();
    contains.sort();
    starts.into_iter().chain(contains).take(limit).map(|(_, p)| p).collect()
}

// ---- scoring ---------------------------------------------------------------------

/// Score of one folder for all `words`, or `None` when a word does not match.
fn score_dir(dir: &Dir, words: &[String]) -> Option<i32> {
    let mut total = 0;
    for word in words {
        total += score_word(dir, word)?;
    }
    Some(total)
}

fn score_word(dir: &Dir, word: &str) -> Option<i32> {
    let name = dir.name.as_str();
    if name == word {
        return Some(120);
    }
    if name.starts_with(word) {
        return Some(90);
    }
    // a word inside the name: "my-projects" matches "proj"
    if name.match_indices(word).any(|(i, _)| {
        name[..i].chars().next_back().is_some_and(|c| !c.is_alphanumeric())
    }) {
        return Some(70);
    }
    if name.contains(word) {
        return Some(50);
    }
    // a parent folder: "rust filer" finds .../rust/filer
    if dir.lower.contains(&format!("/{word}")) {
        return Some(25);
    }
    if word.chars().count() >= 2 && is_subsequence(word, name) {
        return Some(20);
    }
    if dir.lower.contains(word) {
        return Some(12);
    }
    None
}

/// "prj" is a subsequence of "projects".
fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut hay = hay.chars();
    needle.chars().all(|n| hay.any(|h| h == n))
}

// ---- the index -------------------------------------------------------------------

fn make_dir(path: PathBuf, depth: usize) -> Dir {
    let lower = path.to_string_lossy().to_lowercase();
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    Dir { path, name, lower, depth }
}

fn depth_of(path: &Path) -> usize {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h).ok().map(|r| r.components().count())) {
        Some(n) => n,
        None => path.components().count().saturating_sub(1),
    }
}

fn skipped(name: &str) -> bool {
    name.starts_with('.') || SKIP.contains(&name)
}

fn build_index() {
    if BUILDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut roots: Vec<(PathBuf, usize)> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push((home, HOME_DEPTH));
    }
    for p in ["/mnt", "/media", "/run/media"] {
        roots.push((PathBuf::from(p), 3));
    }
    roots.push((PathBuf::from("/"), 1));

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut found: Vec<Dir> = Vec::new();
    'roots: for (root, max_depth) in roots {
        let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
        if seen.insert(root.clone()) {
            found.push(make_dir(root.clone(), 0));
        }
        queue.push_back((root, 0));
        while let Some((dir, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let Ok(read) = std::fs::read_dir(&dir) else { continue };
            for entry in read.flatten() {
                // is_dir() on the entry's file type is false for symlinks: they are not followed
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if skipped(&name) {
                    continue;
                }
                let path = entry.path();
                if !seen.insert(path.clone()) {
                    continue;
                }
                found.push(make_dir(path.clone(), depth + 1));
                if found.len() >= MAX_DIRS {
                    break 'roots;
                }
                queue.push_back((path, depth + 1));
            }
        }
    }
    if let Ok(mut index) = INDEX.write() {
        // keep folders added by `visit` that the scan did not reach (deeper than the limit)
        let extra: Vec<Dir> = index.drain(..).filter(|d| !seen.contains(&d.path) && d.path.is_dir()).collect();
        *index = found;
        index.extend(extra);
    }
    if let Ok(mut t) = BUILT_AT.lock() {
        *t = Some(Instant::now());
    }
    BUILDING.store(false, Ordering::SeqCst);
}

// ---- visit history ---------------------------------------------------------------

fn visits_file() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("filer").join("visits.txt"))
}

fn load_visits() {
    let Some(text) = visits_file().and_then(|f| std::fs::read_to_string(f).ok()) else { return };
    let loaded: Vec<(PathBuf, u32)> = text
        .lines()
        .filter_map(|line| {
            let (count, path) = line.split_once('\t')?;
            Some((PathBuf::from(path), count.trim().parse().ok()?))
        })
        .collect();
    if let Ok(mut visits) = VISITS.lock() {
        // visits made before the file was read are kept
        for (p, n) in loaded {
            match visits.iter_mut().find(|(q, _)| *q == p) {
                Some((_, m)) => *m += n,
                None => visits.push((p, n)),
            }
        }
    }
}

fn save_visits(visits: &[(PathBuf, u32)]) {
    let Some(file) = visits_file() else { return };
    let Ok(_guard) = SAVE_LOCK.lock() else { return };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: String = visits.iter().map(|(p, n)| format!("{n}\t{}\n", p.to_string_lossy())).collect();
    let tmp = file.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str) -> Dir {
        make_dir(PathBuf::from(path), 3)
    }

    fn words(q: &str) -> Vec<String> {
        q.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn exact_name_beats_prefix_beats_contains() {
        let exact = score_dir(&dir("/h/proj"), &words("proj")).unwrap();
        let prefix = score_dir(&dir("/h/projects"), &words("proj")).unwrap();
        let inside = score_dir(&dir("/h/myproj"), &words("proj")).unwrap();
        assert!(exact > prefix && prefix > inside);
    }

    #[test]
    fn every_word_must_match() {
        assert!(score_dir(&dir("/h/rust/filer"), &words("rust filer")).is_some());
        assert!(score_dir(&dir("/h/rust/filer"), &words("rust zzz")).is_none());
    }

    #[test]
    fn letters_in_order_match_loosely() {
        assert!(score_dir(&dir("/h/projects"), &words("prj")).is_some());
        assert!(score_dir(&dir("/h/projects"), &words("jrp")).is_none());
    }

    #[test]
    fn word_start_inside_a_name_ranks_above_plain_contains() {
        let boundary = score_dir(&dir("/h/my-projects"), &words("proj")).unwrap();
        let plain = score_dir(&dir("/h/myprojects"), &words("proj")).unwrap();
        assert!(boundary > plain);
    }

    #[test]
    fn expands_the_home_shortcut() {
        assert_eq!(expand("/etc"), PathBuf::from("/etc"));
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand("~"), home);
            assert_eq!(expand("~/x"), home.join("x"));
            assert_eq!(display(&home.join("x")), "~/x");
        }
    }

    #[test]
    fn paths_are_told_from_search_words() {
        assert!(looks_like_path("~/doc") && looks_like_path("/etc") && looks_like_path("a/b"));
        assert!(!looks_like_path("docs"));
    }

    #[test]
    fn completes_folders_by_prefix() {
        let base = std::env::temp_dir().join(format!("filer-jump-{}", std::process::id()));
        for d in ["alpha", "alps", "beta", ".hidden"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("alpine.txt"), "").unwrap(); // files never complete
        let typed = format!("{}/al", base.display());
        let got: Vec<String> = complete_path(&typed, 5)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, ["alpha", "alps"]);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
