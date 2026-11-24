//! Copy / move engine for drag and drop. Pure std, no UI.
//!
//! Dropping files into a folder that already holds items with the same names
//! needs a decision from the user, which arrives later (a dialog). So the work
//! is a `TransferJob` that runs until it hits a clash, hands the clash back,
//! and carries on after `resolve`:
//!
//!   folder onto folder  -> Rename (keep both) / Merge / Skip
//!   file onto file      -> Replace / Skip
//!   file <-> folder     -> Rename (keep both) / Skip   (can't merge or replace those)
//!
//! "Apply to all" remembers the answer for the rest of the job, per kind of
//! clash. Merging a folder walks into it, so clashes between the files inside
//! are asked about in turn (a moved folder is removed afterwards if it ends up
//! empty; anything that was skipped stays where it was).

use std::{
    collections::{HashMap, VecDeque},
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

// ---- small file helpers (also used by `model::paste`) ------------------------

/// Is there anything at `path`? (Unlike `exists`, a dangling symlink counts.)
fn taken(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn is_dir(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false) // follows symlinks
}

/// `dir/name`, or `dir/name copy`, `dir/name copy 2`, ... if taken.
pub(crate) fn unique_dest(dir: &Path, name: &OsStr) -> PathBuf {
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 1;
    loop {
        let label = if n == 1 { " copy".to_string() } else { format!(" copy {n}") };
        let cand = dir.join(format!("{stem}{label}{ext}"));
        if !taken(&cand) {
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

/// Move (`cut`) or copy `src` to exactly `dst`, which must not exist yet.
pub(crate) fn place(src: &Path, dst: &Path, cut: bool) -> io::Result<()> {
    if cut {
        if std::fs::rename(src, dst).is_err() {
            // different filesystem: copy then delete
            copy_recursive(src, dst)?;
            if std::fs::metadata(src)?.is_dir() {
                std::fs::remove_dir_all(src)?;
            } else {
                std::fs::remove_file(src)?;
            }
        }
    } else {
        copy_recursive(src, dst)?;
    }
    Ok(())
}

/// Overwrite the file `dst` with `src` (moving or copying it). The new content is
/// written next to `dst` first and renamed over it, so a failure half-way never
/// destroys the old file.
fn replace_file(src: &Path, dst: &Path, cut: bool) -> io::Result<()> {
    if cut && std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    let name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dst.with_file_name(format!(".{name}.filer-{}.tmp", std::process::id()));
    if let Err(e) = std::fs::copy(src, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if cut {
        std::fs::remove_file(src)?;
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

// ---- conflicts ------------------------------------------------------------------

/// Which kind of name clash a drop ran into.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ConflictKind {
    /// A folder dropped where a folder of that name exists.
    Folder,
    /// A file dropped where a file of that name exists.
    File,
    /// One is a file, the other a folder.
    Mismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    /// Keep both: the dropped item gets a free name ("name copy").
    Rename,
    /// Put the contents of the dropped folder into the existing one.
    Merge,
    /// Overwrite the existing file.
    Replace,
    /// Leave this item alone.
    Skip,
}

impl Choice {
    pub fn label(self) -> &'static str {
        match self {
            Choice::Rename => "Rename",
            Choice::Merge => "Merge",
            Choice::Replace => "Replace",
            Choice::Skip => "Skip",
        }
    }
}

impl ConflictKind {
    /// The buttons to offer, in order.
    pub fn choices(self) -> &'static [Choice] {
        match self {
            ConflictKind::Folder => &[Choice::Rename, Choice::Merge, Choice::Skip],
            ConflictKind::File => &[Choice::Replace, Choice::Skip],
            ConflictKind::Mismatch => &[Choice::Rename, Choice::Skip],
        }
    }

    /// What Enter picks: never the one that overwrites anything.
    pub fn default_choice(self) -> Choice {
        match self {
            ConflictKind::File => Choice::Skip,
            _ => Choice::Rename,
        }
    }
}

/// A dropped item whose name is already taken in the destination.
#[derive(Clone, Debug)]
pub struct Conflict {
    /// The item being dropped.
    pub src: PathBuf,
    /// What is already there.
    pub existing: PathBuf,
    pub kind: ConflictKind,
}

impl Conflict {
    pub fn name(&self) -> String {
        self.existing.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The folder the item is being dropped into.
    pub fn folder(&self) -> &Path {
        self.existing.parent().unwrap_or(&self.existing)
    }
}

// ---- the job --------------------------------------------------------------------

/// What a finished job did: which folders changed (so panes can refresh) and what failed.
pub struct Transfer {
    /// Where the files were dropped (failures are reported in the pane showing it).
    pub dest: PathBuf,
    pub touched: Vec<PathBuf>,
    pub errors: Vec<String>,
}

pub enum Progress {
    /// Waiting for an answer: call `resolve` (or `cancel`).
    Conflict(Conflict),
    Done(Transfer),
}

enum Op {
    /// Put `src` into the folder `dir`.
    Item { src: PathBuf, dir: PathBuf },
    /// After merging a moved folder: remove it if nothing was left behind in it.
    RemoveIfEmpty(PathBuf),
}

pub struct TransferJob {
    dest: PathBuf,
    copy: bool,
    queue: VecDeque<Op>,
    pending: Option<Conflict>,
    remembered: HashMap<ConflictKind, Choice>,
    touched: Vec<PathBuf>,
    errors: Vec<String>,
}

impl TransferJob {
    /// Copy (`copy`) or move every path in `paths` into the folder `dest`.
    /// Nothing happens until `run`.
    pub fn new(paths: Vec<PathBuf>, dest: PathBuf, copy: bool) -> Self {
        let mut job = Self {
            dest: dest.clone(),
            copy,
            queue: VecDeque::new(),
            pending: None,
            remembered: HashMap::new(),
            touched: vec![dest.clone()],
            errors: Vec::new(),
        };
        if !dest.is_dir() {
            job.errors.push(format!("\"{}\" is not a folder", dest.display()));
            return job;
        }
        for src in paths {
            if src == dest {
                continue; // a folder dropped on itself
            }
            job.queue.push_back(Op::Item { src, dir: dest.clone() });
        }
        job
    }

    /// Work until the first clash that has no remembered answer, or until done.
    pub fn run(&mut self) -> Progress {
        if let Some(c) = &self.pending {
            return Progress::Conflict(c.clone()); // still waiting for an answer
        }
        while let Some(op) = self.queue.pop_front() {
            match op {
                Op::Item { src, dir } => {
                    if let Some(conflict) = self.step(src, dir) {
                        self.pending = Some(conflict.clone());
                        return Progress::Conflict(conflict);
                    }
                }
                Op::RemoveIfEmpty(dir) => {
                    // fails (harmlessly) when something was skipped and is still inside
                    if std::fs::remove_dir(&dir).is_ok() {
                        self.touch_parent(&dir);
                    }
                }
            }
        }
        Progress::Done(self.finish())
    }

    /// Answer the clash `run` returned, then carry on. With `apply_to_all`, every
    /// later clash of the same kind gets the same answer without asking.
    pub fn resolve(&mut self, choice: Choice, apply_to_all: bool) -> Progress {
        let Some(conflict) = self.pending.take() else { return self.run() };
        if !conflict.kind.choices().contains(&choice) {
            self.pending = Some(conflict.clone()); // not an option here: ask again
            return Progress::Conflict(conflict);
        }
        if apply_to_all {
            self.remembered.insert(conflict.kind, choice);
        }
        self.apply(conflict, choice);
        self.run()
    }

    /// Stop here. What was already done stays done.
    pub fn cancel(mut self) -> Transfer {
        self.queue.clear();
        self.pending = None;
        self.finish()
    }

    fn finish(&mut self) -> Transfer {
        let mut touched = std::mem::take(&mut self.touched);
        touched.sort();
        touched.dedup();
        Transfer { dest: self.dest.clone(), touched, errors: std::mem::take(&mut self.errors) }
    }

    fn touch_parent(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.touched.push(parent.to_path_buf());
        }
    }

    fn fail(&mut self, name: &str, err: impl std::fmt::Display) {
        self.errors.push(format!("\"{name}\": {err}"));
    }

    /// Put `src` at exactly `dst` (which is free), inside `dir`.
    fn put(&mut self, src: &Path, dst: &Path, dir: &Path) {
        match place(src, dst, !self.copy) {
            Ok(()) => {
                self.touched.push(dir.to_path_buf());
                if !self.copy {
                    self.touch_parent(src);
                }
            }
            Err(e) => self.fail(&file_label(src), e),
        }
    }

    /// Handle one item. `Some` = its name is taken and nobody has said what to do yet.
    fn step(&mut self, src: PathBuf, dir: PathBuf) -> Option<Conflict> {
        let Some(name) = src.file_name().map(|n| n.to_owned()) else {
            self.errors.push(format!("\"{}\": bad source path", src.display()));
            return None;
        };
        if dir.starts_with(&src) {
            self.fail(&name.to_string_lossy(), "can't paste a folder into itself");
            return None;
        }
        if src.parent() == Some(dir.as_path()) {
            // already in this folder: a move changes nothing; a copy makes "name copy"
            if self.copy {
                let dst = unique_dest(&dir, &name);
                self.put(&src, &dst, &dir);
            }
            return None;
        }
        let dst = dir.join(&name);
        if !taken(&dst) {
            self.put(&src, &dst, &dir);
            return None;
        }
        if same_file(&src, &dst) {
            return None; // e.g. reached through a symlink: it already is there
        }
        let kind = match (is_dir(&src), is_dir(&dst)) {
            (true, true) => ConflictKind::Folder,
            (false, false) => ConflictKind::File,
            _ => ConflictKind::Mismatch,
        };
        let conflict = Conflict { src, existing: dst, kind };
        match self.remembered.get(&kind).copied() {
            Some(choice) => {
                self.apply(conflict, choice);
                None
            }
            None => Some(conflict),
        }
    }

    fn apply(&mut self, conflict: Conflict, choice: Choice) {
        let Conflict { src, existing, .. } = conflict;
        let dir = existing.parent().map(Path::to_path_buf).unwrap_or_default();
        match choice {
            Choice::Skip => {}
            Choice::Rename => {
                let name = existing.file_name().unwrap_or_default().to_owned();
                let dst = unique_dest(&dir, &name);
                self.put(&src, &dst, &dir);
            }
            Choice::Replace => match replace_file(&src, &existing, !self.copy) {
                Ok(()) => {
                    self.touched.push(dir);
                    if !self.copy {
                        self.touch_parent(&src);
                    }
                }
                Err(e) => self.fail(&file_label(&src), e),
            },
            Choice::Merge => {
                let mut items: Vec<PathBuf> = match std::fs::read_dir(&src) {
                    Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
                    Err(e) => {
                        self.fail(&file_label(&src), e);
                        return;
                    }
                };
                items.sort();
                // the folder's contents are handled next, before the rest of the queue
                if !self.copy {
                    self.queue.push_front(Op::RemoveIfEmpty(src.clone()));
                }
                for item in items.into_iter().rev() {
                    self.queue.push_front(Op::Item { src: item, dir: existing.clone() });
                }
                self.touched.push(existing);
                if !self.copy {
                    self.touched.push(src);
                }
            }
        }
    }
}

fn file_label(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("filer-transfer-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: impl AsRef<Path>, text: &str) {
        let p = p.as_ref();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn read(p: impl AsRef<Path>) -> String {
        fs::read_to_string(p).unwrap()
    }

    fn expect_conflict(p: Progress) -> Conflict {
        match p {
            Progress::Conflict(c) => c,
            Progress::Done(_) => panic!("expected a conflict, job finished"),
        }
    }

    fn expect_done(p: Progress) -> Transfer {
        match p {
            Progress::Done(t) => t,
            Progress::Conflict(c) => panic!("unexpected conflict on {:?}", c.existing),
        }
    }

    #[test]
    fn no_clash_needs_no_answer() {
        let root = tmp("plain");
        write(root.join("a/f.txt"), "1");
        fs::create_dir_all(root.join("b")).unwrap();
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("b"), false);
        let t = expect_done(job.run());
        assert!(t.errors.is_empty());
        assert!(!root.join("a/f.txt").exists());
        assert_eq!(read(root.join("b/f.txt")), "1");
        assert!(t.touched.contains(&root.join("a")) && t.touched.contains(&root.join("b")));
    }

    #[test]
    fn file_replace() {
        let root = tmp("replace");
        write(root.join("a/f.txt"), "new");
        write(root.join("b/f.txt"), "old");
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("b"), false);
        let c = expect_conflict(job.run());
        assert_eq!(c.kind, ConflictKind::File);
        assert_eq!(c.name(), "f.txt");
        // nothing touched while waiting
        assert_eq!(read(root.join("b/f.txt")), "old");
        expect_done(job.resolve(Choice::Replace, false));
        assert_eq!(read(root.join("b/f.txt")), "new");
        assert!(!root.join("a/f.txt").exists(), "move: source gone");
        // no temp files left behind
        assert_eq!(fs::read_dir(root.join("b")).unwrap().count(), 1);
    }

    #[test]
    fn file_replace_by_copy_keeps_source() {
        let root = tmp("replace-copy");
        write(root.join("a/f.txt"), "new");
        write(root.join("b/f.txt"), "old");
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("b"), true);
        expect_conflict(job.run());
        expect_done(job.resolve(Choice::Replace, false));
        assert_eq!(read(root.join("b/f.txt")), "new");
        assert_eq!(read(root.join("a/f.txt")), "new");
    }

    #[test]
    fn file_skip_leaves_both() {
        let root = tmp("skip");
        write(root.join("a/f.txt"), "new");
        write(root.join("b/f.txt"), "old");
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("b"), false);
        expect_conflict(job.run());
        let t = expect_done(job.resolve(Choice::Skip, false));
        assert!(t.errors.is_empty());
        assert_eq!(read(root.join("a/f.txt")), "new");
        assert_eq!(read(root.join("b/f.txt")), "old");
    }

    #[test]
    fn file_has_no_merge_option() {
        let root = tmp("badchoice");
        write(root.join("a/f.txt"), "new");
        write(root.join("b/f.txt"), "old");
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("b"), false);
        expect_conflict(job.run());
        // Merge isn't offered for files: the same question comes back, nothing changes
        expect_conflict(job.resolve(Choice::Merge, true));
        assert_eq!(read(root.join("b/f.txt")), "old");
        expect_done(job.resolve(Choice::Skip, false));
    }

    #[test]
    fn folder_rename_keeps_both() {
        let root = tmp("frename");
        write(root.join("a/dir/x.txt"), "x");
        write(root.join("b/dir/y.txt"), "y");
        let mut job = TransferJob::new(vec![root.join("a/dir")], root.join("b"), false);
        let c = expect_conflict(job.run());
        assert_eq!(c.kind, ConflictKind::Folder);
        expect_done(job.resolve(Choice::Rename, false));
        assert_eq!(read(root.join("b/dir/y.txt")), "y");
        assert_eq!(read(root.join("b/dir copy/x.txt")), "x");
        assert!(!root.join("a/dir").exists());
    }

    #[test]
    fn folder_merge_move_asks_about_inner_files_and_cleans_up() {
        let root = tmp("fmerge");
        write(root.join("a/dir/only-src.txt"), "s");
        write(root.join("a/dir/both.txt"), "src-both");
        write(root.join("a/dir/sub/deep.txt"), "deep-src");
        write(root.join("b/dir/only-dst.txt"), "d");
        write(root.join("b/dir/both.txt"), "dst-both");
        write(root.join("b/dir/sub/deep.txt"), "deep-dst");
        let mut job = TransferJob::new(vec![root.join("a/dir")], root.join("b"), false);
        assert_eq!(expect_conflict(job.run()).kind, ConflictKind::Folder);
        // merge: walks in, then asks about the clashing files in order (both.txt, sub/ then sub/deep.txt)
        let c = expect_conflict(job.resolve(Choice::Merge, false));
        assert_eq!(c.existing, root.join("b/dir/both.txt"));
        let c = expect_conflict(job.resolve(Choice::Replace, false));
        assert_eq!(c.kind, ConflictKind::Folder);
        assert_eq!(c.existing, root.join("b/dir/sub"));
        let c = expect_conflict(job.resolve(Choice::Merge, false));
        assert_eq!(c.existing, root.join("b/dir/sub/deep.txt"));
        let t = expect_done(job.resolve(Choice::Skip, false));
        assert!(t.errors.is_empty(), "{:?}", t.errors);
        assert_eq!(read(root.join("b/dir/only-src.txt")), "s");
        assert_eq!(read(root.join("b/dir/only-dst.txt")), "d");
        assert_eq!(read(root.join("b/dir/both.txt")), "src-both");
        assert_eq!(read(root.join("b/dir/sub/deep.txt")), "deep-dst");
        // deep.txt was skipped, so it (and the folders around it) must still be at the source
        assert_eq!(read(root.join("a/dir/sub/deep.txt")), "deep-src");
        assert!(!root.join("a/dir/both.txt").exists());
        assert!(!root.join("a/dir/only-src.txt").exists());
        assert!(t.touched.contains(&root.join("b/dir/sub")));
    }

    #[test]
    fn folder_merge_move_removes_emptied_source() {
        let root = tmp("fmerge-clean");
        write(root.join("a/dir/one.txt"), "1");
        write(root.join("b/dir/two.txt"), "2");
        let mut job = TransferJob::new(vec![root.join("a/dir")], root.join("b"), false);
        expect_conflict(job.run());
        expect_done(job.resolve(Choice::Merge, false));
        assert!(!root.join("a/dir").exists(), "emptied source folder is removed");
        assert_eq!(read(root.join("b/dir/one.txt")), "1");
        assert_eq!(read(root.join("b/dir/two.txt")), "2");
    }

    #[test]
    fn folder_merge_copy_keeps_source() {
        let root = tmp("fmerge-copy");
        write(root.join("a/dir/one.txt"), "1");
        write(root.join("b/dir/two.txt"), "2");
        let mut job = TransferJob::new(vec![root.join("a/dir")], root.join("b"), true);
        expect_conflict(job.run());
        expect_done(job.resolve(Choice::Merge, false));
        assert_eq!(read(root.join("a/dir/one.txt")), "1");
        assert_eq!(read(root.join("b/dir/one.txt")), "1");
        assert_eq!(read(root.join("b/dir/two.txt")), "2");
    }

    #[test]
    fn apply_to_all_is_per_kind_and_asks_only_once() {
        let root = tmp("all");
        for n in ["f1", "f2", "f3"] {
            write(root.join(format!("a/{n}.txt")), "new");
            write(root.join(format!("b/{n}.txt")), "old");
        }
        write(root.join("a/d1/x"), "new");
        write(root.join("b/d1/x"), "old");
        write(root.join("a/d2/x"), "new");
        write(root.join("b/d2/x"), "old");
        let srcs = vec![
            root.join("a/f1.txt"),
            root.join("a/d1"),
            root.join("a/f2.txt"),
            root.join("a/d2"),
            root.join("a/f3.txt"),
        ];
        let mut job = TransferJob::new(srcs, root.join("b"), false);
        // f1: Replace for all files
        assert_eq!(expect_conflict(job.run()).existing, root.join("b/f1.txt"));
        // d1 is a folder: a different kind, so it still asks
        let c = expect_conflict(job.resolve(Choice::Replace, true));
        assert_eq!(c.existing, root.join("b/d1"));
        // Rename for all folders; f2, f3, d2 then need no answer
        let t = expect_done(job.resolve(Choice::Rename, true));
        assert!(t.errors.is_empty());
        for n in ["f1", "f2", "f3"] {
            assert_eq!(read(root.join(format!("b/{n}.txt"))), "new");
        }
        assert_eq!(read(root.join("b/d1/x")), "old");
        assert_eq!(read(root.join("b/d1 copy/x")), "new");
        assert_eq!(read(root.join("b/d2 copy/x")), "new");
    }

    #[test]
    fn file_vs_folder_only_offers_rename_or_skip() {
        let root = tmp("mismatch");
        write(root.join("a/thing"), "file");
        write(root.join("b/thing/inner"), "i");
        let mut job = TransferJob::new(vec![root.join("a/thing")], root.join("b"), false);
        let c = expect_conflict(job.run());
        assert_eq!(c.kind, ConflictKind::Mismatch);
        assert_eq!(c.kind.choices(), &[Choice::Rename, Choice::Skip]);
        expect_done(job.resolve(Choice::Rename, false));
        assert_eq!(read(root.join("b/thing copy")), "file");
        assert_eq!(read(root.join("b/thing/inner")), "i");
    }

    #[test]
    fn cancel_keeps_what_was_done() {
        let root = tmp("cancel");
        write(root.join("a/ok.txt"), "ok");
        write(root.join("a/f.txt"), "new");
        write(root.join("a/later.txt"), "later");
        write(root.join("b/f.txt"), "old");
        let srcs = vec![root.join("a/ok.txt"), root.join("a/f.txt"), root.join("a/later.txt")];
        let mut job = TransferJob::new(srcs, root.join("b"), false);
        expect_conflict(job.run());
        let t = job.cancel();
        assert_eq!(read(root.join("b/ok.txt")), "ok");
        assert_eq!(read(root.join("b/f.txt")), "old");
        assert!(root.join("a/f.txt").exists() && root.join("a/later.txt").exists());
        assert!(t.touched.contains(&root.join("b")) && t.errors.is_empty());
    }

    #[test]
    fn copying_into_own_folder_makes_a_copy_without_asking() {
        let root = tmp("self-copy");
        write(root.join("a/f.txt"), "1");
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("a"), true);
        expect_done(job.run());
        assert_eq!(read(root.join("a/f copy.txt")), "1");
    }

    #[test]
    fn moving_into_own_folder_does_nothing() {
        let root = tmp("self-move");
        write(root.join("a/f.txt"), "1");
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("a"), false);
        let t = expect_done(job.run());
        assert!(t.errors.is_empty());
        assert_eq!(fs::read_dir(root.join("a")).unwrap().count(), 1);
    }

    #[test]
    fn folder_into_itself_is_an_error_not_a_clash() {
        let root = tmp("into-self");
        write(root.join("a/sub/f.txt"), "1");
        let mut job = TransferJob::new(vec![root.join("a")], root.join("a/sub"), false);
        let t = expect_done(job.run());
        assert_eq!(t.errors.len(), 1);
        assert!(root.join("a/sub/f.txt").exists());
    }

    #[test]
    fn dropping_a_folder_on_itself_is_ignored() {
        let root = tmp("on-self");
        write(root.join("a/f.txt"), "1");
        let mut job = TransferJob::new(vec![root.join("a")], root.join("a"), false);
        let t = expect_done(job.run());
        assert!(t.errors.is_empty());
        assert_eq!(read(root.join("a/f.txt")), "1");
    }

    #[test]
    fn destination_must_be_a_folder() {
        let root = tmp("notdir");
        write(root.join("a/f.txt"), "1");
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("a/f.txt"), false);
        let t = expect_done(job.run());
        assert_eq!(t.errors.len(), 1);
    }

    #[test]
    fn dangling_symlink_counts_as_taken() {
        let root = tmp("dangling");
        write(root.join("a/f.txt"), "new");
        fs::create_dir_all(root.join("b")).unwrap();
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("b/f.txt")).unwrap();
        let mut job = TransferJob::new(vec![root.join("a/f.txt")], root.join("b"), false);
        expect_conflict(job.run()); // must not silently write through the symlink
        expect_done(job.resolve(Choice::Replace, false));
        assert_eq!(read(root.join("b/f.txt")), "new");
        assert!(!root.join("nowhere").exists());
    }
}
