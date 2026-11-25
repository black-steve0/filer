//! Facts shown in the Properties dialog. Pure std, no UI.
//!
//! * `stat`      one item: type, times, owner, permissions, symlink target
//! * `measure`   how much a set of files and folders adds up to (cancellable, so the
//!               dialog can stop it when closed)
//! * permission helpers for the read/write/execute check boxes

use std::{
    collections::{HashMap, HashSet},
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    time::SystemTime,
};

// ---- one item -------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Stat {
    pub path: PathBuf,
    pub name: String,
    /// `None` for `/`.
    pub parent: Option<PathBuf>,
    /// Looking through a symlink: a link to a folder is a folder.
    pub is_dir: bool,
    pub is_symlink: bool,
    pub link_target: Option<PathBuf>,
    /// A symlink whose target is gone.
    pub link_broken: bool,
    /// Bytes for a file. (Folders: see `measure`.)
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    /// Birth time; not every filesystem keeps one.
    pub created: Option<SystemTime>,
    /// Permission bits, 0o7777 (including setuid / setgid / sticky).
    pub mode: u32,
    pub owner: String,
    pub group: String,
}

pub fn stat(path: &Path) -> io::Result<Stat> {
    let link_meta = std::fs::symlink_metadata(path)?;
    let is_symlink = link_meta.file_type().is_symlink();
    // what the link points at; a broken link falls back to describing the link itself
    let target_meta = if is_symlink { std::fs::metadata(path).ok() } else { None };
    let link_broken = is_symlink && target_meta.is_none();
    let meta = target_meta.as_ref().unwrap_or(&link_meta);
    Ok(Stat {
        path: path.to_path_buf(),
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        parent: path.parent().map(Path::to_path_buf).filter(|p| !p.as_os_str().is_empty()),
        is_dir: meta.is_dir(),
        is_symlink,
        link_target: if is_symlink { std::fs::read_link(path).ok() } else { None },
        link_broken,
        size: meta.len(),
        modified: meta.modified().ok(),
        accessed: meta.accessed().ok(),
        created: meta.created().ok(),
        mode: meta.permissions().mode() & 0o7777,
        owner: user_name(meta.uid()),
        group: group_name(meta.gid()),
    })
}

/// `1000:alice:...` style files (`/etc/passwd`, `/etc/group`): name:x:id:...
pub fn parse_ids(text: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut f = line.split(':');
        if let (Some(name), Some(_), Some(id)) = (f.next(), f.next(), f.next()) {
            if let Ok(id) = id.parse::<u32>() {
                map.entry(id).or_insert_with(|| name.to_string()); // first entry wins
            }
        }
    }
    map
}

fn lookup(cell: &'static OnceLock<HashMap<u32, String>>, file: &str, id: u32) -> String {
    let map = cell.get_or_init(|| std::fs::read_to_string(file).map(|t| parse_ids(&t)).unwrap_or_default());
    // accounts that live elsewhere (LDAP, ...) show their number
    map.get(&id).cloned().unwrap_or_else(|| id.to_string())
}

fn user_name(uid: u32) -> String {
    static USERS: OnceLock<HashMap<u32, String>> = OnceLock::new();
    lookup(&USERS, "/etc/passwd", uid)
}

fn group_name(gid: u32) -> String {
    static GROUPS: OnceLock<HashMap<u32, String>> = OnceLock::new();
    lookup(&GROUPS, "/etc/group", gid)
}

// ---- how big ---------------------------------------------------------------------------

/// Totals for a set of files and folders. Folders are walked; symlinks are counted
/// as files and never followed (so a link to `/` can't make a scan run for ever).
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Usage {
    /// Everything that isn't a folder, including symlinks.
    pub files: u64,
    /// Folders, including the ones named in `paths` themselves.
    pub folders: u64,
    /// Sum of the files' sizes.
    pub bytes: u64,
    /// Space they occupy on disk (whole blocks).
    pub on_disk: u64,
    /// Folders that couldn't be read.
    pub unreadable: u64,
}

/// Add up `paths`. Stops early, returning what it has so far, once `cancel` is set.
/// `progress` is called now and then with the running totals.
pub fn measure(paths: &[PathBuf], cancel: &AtomicBool, progress: &mut dyn FnMut(&Usage)) -> Usage {
    let mut usage = Usage::default();
    let mut stack: Vec<PathBuf> = paths.iter().rev().cloned().collect();
    let mut hard_links: HashSet<(u64, u64)> = HashSet::new();
    let mut since_report = 0u32;
    while let Some(path) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            usage.unreadable += 1;
            continue;
        };
        // a file with several names is only counted once
        let again = meta.nlink() > 1 && !meta.is_dir() && !hard_links.insert((meta.dev(), meta.ino()));
        if !again {
            usage.on_disk += meta.blocks() * 512;
        }
        if meta.is_dir() {
            usage.folders += 1;
            match std::fs::read_dir(&path) {
                Ok(rd) => stack.extend(rd.filter_map(|e| e.ok()).map(|e| e.path())),
                Err(_) => usage.unreadable += 1,
            }
        } else if !again {
            usage.files += 1;
            usage.bytes += meta.len();
        }
        since_report += 1;
        if since_report >= 200 {
            since_report = 0;
            progress(&usage);
        }
    }
    progress(&usage);
    usage
}

// ---- permissions -------------------------------------------------------------------------

/// Row of the permission grid: who.
pub const WHO: [&str; 3] = ["Owner", "Group", "Others"];
/// Column of the permission grid: what.
pub const WHAT: [&str; 3] = ["Read", "Write", "Execute"];

fn mask(who: usize, what: usize) -> u32 {
    1 << (8 - (who * 3 + what)) // owner read = 0o400 ... others execute = 0o001
}

pub fn has_bit(mode: u32, who: usize, what: usize) -> bool {
    mode & mask(who, what) != 0
}

/// `mode` with one cell of the grid switched on or off (special bits are kept).
pub fn with_bit(mode: u32, who: usize, what: usize, on: bool) -> u32 {
    if on {
        mode | mask(who, what)
    } else {
        mode & !mask(who, what)
    }
}

/// `rwxr-xr-x`
pub fn rwx(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for who in 0..3 {
        for (what, ch) in ['r', 'w', 'x'].into_iter().enumerate() {
            s.push(if has_bit(mode, who, what) { ch } else { '-' });
        }
    }
    s
}

/// `0755`
pub fn octal(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
}

// ---- text ----------------------------------------------------------------------------------

/// `12345678` -> `12,345,678`
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The folder that holds every path, if they all share one.
pub fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?.parent()?;
    paths.iter().all(|p| p.parent() == Some(first)).then(|| first.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("filer-props-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(paths: &[PathBuf]) -> Usage {
        measure(paths, &AtomicBool::new(false), &mut |_| {})
    }

    #[test]
    fn stats_a_file() {
        let root = tmp("stat");
        let f = root.join("a.txt");
        fs::write(&f, "hello").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o640)).unwrap();
        let s = stat(&f).unwrap();
        assert_eq!((s.name.as_str(), s.size, s.is_dir, s.is_symlink), ("a.txt", 5, false, false));
        assert_eq!(s.parent.as_deref(), Some(root.as_path()));
        assert_eq!(s.mode, 0o640);
        assert!(s.modified.is_some());
        assert!(!s.owner.is_empty() && !s.group.is_empty());
        assert!(stat(&root.join("missing")).is_err());
        assert!(stat(&root).unwrap().is_dir);
    }

    #[test]
    fn stats_symlinks() {
        let root = tmp("link");
        fs::create_dir(root.join("d")).unwrap();
        std::os::unix::fs::symlink("d", root.join("to-dir")).unwrap();
        std::os::unix::fs::symlink("nowhere", root.join("broken")).unwrap();
        let s = stat(&root.join("to-dir")).unwrap();
        assert!(s.is_symlink && s.is_dir && !s.link_broken);
        assert_eq!(s.link_target, Some(PathBuf::from("d")));
        let b = stat(&root.join("broken")).unwrap();
        assert!(b.is_symlink && b.link_broken && !b.is_dir);
        assert_eq!(b.link_target, Some(PathBuf::from("nowhere")));
    }

    #[test]
    fn root_has_no_parent() {
        assert!(stat(Path::new("/")).unwrap().parent.is_none());
    }

    #[test]
    fn id_tables() {
        let m = parse_ids("root:x:0:0:root:/root:/bin/bash\nalice:x:1000:1000::/home/alice:/bin/sh\nbad line\ndup:x:1000:\n");
        assert_eq!(m[&0], "root");
        assert_eq!(m[&1000], "alice");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn measures_a_tree() {
        let root = tmp("tree");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/one"), vec![0u8; 100]).unwrap();
        fs::write(root.join("a/b/two"), vec![0u8; 2000]).unwrap();
        fs::write(root.join("three"), "xyz").unwrap();
        let u = run(&[root.join("a")]);
        assert_eq!((u.files, u.folders, u.bytes), (2, 2, 2100)); // a and a/b
        assert!(u.on_disk >= 2100);
        // several roots add up, and a root counts as an item
        let u = run(&[root.join("a"), root.join("three")]);
        assert_eq!((u.files, u.folders, u.bytes), (3, 2, 2103));
        assert_eq!(run(&[]), Usage::default());
    }

    #[test]
    fn measure_does_not_follow_symlinks() {
        let root = tmp("nofollow");
        fs::create_dir_all(root.join("big")).unwrap();
        fs::write(root.join("big/f"), vec![0u8; 5000]).unwrap();
        fs::create_dir(root.join("d")).unwrap();
        std::os::unix::fs::symlink(root.join("big"), root.join("d/link")).unwrap();
        std::os::unix::fs::symlink(root.join("d"), root.join("d/loop")).unwrap(); // would recurse for ever
        let u = run(&[root.join("d")]);
        assert_eq!((u.files, u.folders), (2, 1)); // the two links, as files
        assert!(u.bytes < 5000);
    }

    #[test]
    fn hard_links_count_once() {
        let root = tmp("hard");
        fs::write(root.join("a"), vec![0u8; 700]).unwrap();
        fs::hard_link(root.join("a"), root.join("b")).unwrap();
        let u = run(&[root.clone()]);
        assert_eq!((u.files, u.bytes), (1, 700));
    }

    #[test]
    fn unreadable_and_missing_are_counted_not_fatal() {
        let root = tmp("missing");
        let u = run(&[root.join("nope"), root.clone()]);
        assert_eq!(u.unreadable, 1);
        assert_eq!(u.folders, 1);
    }

    #[test]
    fn cancel_stops_early_and_progress_reports() {
        let root = tmp("cancel");
        for i in 0..450 {
            fs::write(root.join(format!("f{i}")), "x").unwrap();
        }
        let cancel = AtomicBool::new(true);
        assert_eq!(measure(&[root.clone()], &cancel, &mut |_| {}).files, 0);
        let mut reports = 0;
        let u = measure(&[root.clone()], &AtomicBool::new(false), &mut |_| reports += 1);
        assert_eq!(u.files, 450);
        assert!(reports >= 3, "reported every ~200 items plus the end: {reports}");
    }

    #[test]
    fn permission_grid() {
        assert_eq!(rwx(0o754), "rwxr-xr--");
        assert_eq!(rwx(0), "---------");
        assert_eq!(octal(0o755), "0755");
        assert_eq!(octal(0o4755), "4755");
        assert!(has_bit(0o640, 0, 0) && has_bit(0o640, 0, 1) && !has_bit(0o640, 0, 2));
        assert!(has_bit(0o640, 1, 0) && !has_bit(0o640, 1, 1));
        assert!(!has_bit(0o640, 2, 0));
        assert_eq!(with_bit(0o644, 0, 2, true), 0o744);
        assert_eq!(with_bit(0o644, 2, 0, false), 0o640);
        assert_eq!(with_bit(0o4644, 1, 1, true), 0o4664, "setuid is kept");
        assert_eq!(with_bit(0o644, 0, 0, true), 0o644, "already on");
    }

    #[test]
    fn chmod_works_and_fails_cleanly() {
        let root = tmp("chmod");
        let f = root.join("f");
        fs::write(&f, "x").unwrap();
        set_mode(&f, 0o600).unwrap();
        assert_eq!(stat(&f).unwrap().mode, 0o600);
        assert!(set_mode(&root.join("missing"), 0o600).is_err());
    }

    #[test]
    fn text_helpers() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(12345678), "12,345,678");
        let paths = [PathBuf::from("/a/b/x"), PathBuf::from("/a/b/y")];
        assert_eq!(common_parent(&paths), Some(PathBuf::from("/a/b")));
        assert_eq!(common_parent(&[PathBuf::from("/a/x"), PathBuf::from("/b/y")]), None);
        assert_eq!(common_parent(&[]), None);
    }
}
