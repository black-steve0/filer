//! File-type filters of the file chooser ("Images", "Text files", ...).
//!
//! The portal hands us a list of named filters; each is a set of rules that
//! are either a glob (`*.png`) or a MIME type (`image/png`, `image/*`).
//! A file passes when any rule matches. Folders are never filtered here.
//!
//! MIME types are guessed from the file name only, using the system's
//! shared-mime-info table (`/usr/share/mime/globs2`) with a small built-in
//! list as a fallback. File contents are never read.

use std::{collections::HashMap, sync::OnceLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// `*.png`, `README*`, ... Case-insensitive.
    Glob(String),
    /// `image/png` or a wildcard such as `image/*`.
    Mime(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub name: String,
    pub rules: Vec<Rule>,
}

impl Filter {
    /// Does a file with this name pass? A filter without rules lets everything through.
    pub fn matches(&self, file_name: &str) -> bool {
        if self.rules.is_empty() {
            return true;
        }
        self.rules.iter().any(|rule| match rule {
            Rule::Glob(pattern) => glob_match(pattern, file_name),
            Rule::Mime(want) => mime_of(mime_table(), file_name).is_some_and(|got| mime_matches(want, got)),
        })
    }
}

// ---- globs ------------------------------------------------------------------------

/// `*` (any run of characters, also none) and `?` (one character), ignoring case.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    // classic iterative matcher with one backtrack point for the last `*`
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None; // (pattern index after `*`, name index to retry from)
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) && p[pi] != '*' {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi + 1, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ---- MIME types -------------------------------------------------------------------

/// `image/*` matches `image/png`; otherwise the types must be equal (ignoring case).
fn mime_matches(want: &str, got: &str) -> bool {
    match want.strip_suffix("/*") {
        Some(major) => got.split('/').next().is_some_and(|g| g.eq_ignore_ascii_case(major)),
        None => want.eq_ignore_ascii_case(got),
    }
}

/// extension (lowercase, without the dot; may hold dots: `tar.gz`) -> MIME type
type MimeTable = HashMap<String, String>;

/// Parse shared-mime-info's `globs2`: lines of `weight:mime/type:glob[:flags]`.
/// Only plain `*.ext` globs are kept; for an extension listed twice, the
/// higher weight wins.
fn parse_globs2(text: &str) -> MimeTable {
    let mut best: HashMap<String, (u32, String)> = HashMap::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(':');
        let (Some(weight), Some(mime), Some(glob)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Some(ext) = glob.strip_prefix("*.") else { continue };
        if ext.is_empty() || ext.contains(['*', '?', '[']) {
            continue;
        }
        let weight: u32 = weight.parse().unwrap_or(50);
        let ext = ext.to_lowercase();
        match best.get(&ext) {
            Some((w, _)) if *w >= weight => {}
            _ => {
                best.insert(ext, (weight, mime.to_string()));
            }
        }
    }
    best.into_iter().map(|(ext, (_, mime))| (ext, mime)).collect()
}

/// The MIME type for `file_name`, trying the longest extension first
/// (`a.tar.gz`: `tar.gz`, then `gz`).
fn mime_of<'a>(table: &'a MimeTable, file_name: &str) -> Option<&'a str> {
    let lower = file_name.to_lowercase();
    let mut rest = lower.as_str();
    while let Some(dot) = rest.find('.') {
        rest = &rest[dot + 1..];
        if let Some(mime) = table.get(rest) {
            return Some(mime);
        }
    }
    None
}

const BUILTIN: &[(&str, &str)] = &[
    ("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"),
    ("webp", "image/webp"), ("bmp", "image/bmp"), ("svg", "image/svg+xml"), ("tif", "image/tiff"),
    ("tiff", "image/tiff"), ("ico", "image/vnd.microsoft.icon"), ("avif", "image/avif"),
    ("mp3", "audio/mpeg"), ("ogg", "audio/ogg"), ("flac", "audio/flac"), ("wav", "audio/x-wav"),
    ("m4a", "audio/mp4"), ("opus", "audio/ogg"),
    ("mp4", "video/mp4"), ("mkv", "video/x-matroska"), ("webm", "video/webm"),
    ("avi", "video/x-msvideo"), ("mov", "video/quicktime"),
    ("txt", "text/plain"), ("md", "text/markdown"), ("html", "text/html"), ("htm", "text/html"),
    ("css", "text/css"), ("csv", "text/csv"), ("json", "application/json"),
    ("pdf", "application/pdf"), ("zip", "application/zip"), ("gz", "application/gzip"),
];

/// Loaded once, the first time a MIME rule is checked. The system table is
/// read first and the user's own (`~/.local/share/mime/globs2`, usually tiny)
/// on top of it, so a few custom types do not hide all the others.
fn mime_table() -> &'static MimeTable {
    static TABLE: OnceLock<MimeTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut dirs = vec![std::path::PathBuf::from("/usr/share")];
        if let Some(home) = dirs::data_dir() {
            dirs.push(home);
        }
        let mut table = MimeTable::new();
        for dir in dirs {
            if let Ok(text) = std::fs::read_to_string(dir.join("mime/globs2")) {
                table.extend(parse_globs2(&text));
            }
        }
        if table.is_empty() {
            table = BUILTIN.iter().map(|(e, m)| (e.to_string(), m.to_string())).collect();
        }
        table
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("*.png", "a.png"));
        assert!(glob_match("*.png", "A.PNG")); // case-insensitive
        assert!(glob_match("*.png", ".png")); // `*` may match nothing
        assert!(!glob_match("*.png", "a.png.txt"));
        assert!(!glob_match("*.png", "png"));
        assert!(glob_match("*.tar.gz", "x.y.tar.gz"));
        assert!(glob_match("README*", "README"));
        assert!(glob_match("README*", "readme.md"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*a*b*", "xxaYYbZZ"));
        assert!(!glob_match("*a*b*", "xxbYYaZZ"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn mime_wildcards() {
        assert!(mime_matches("image/*", "image/png"));
        assert!(mime_matches("IMAGE/*", "image/png"));
        assert!(!mime_matches("image/*", "audio/mpeg"));
        assert!(mime_matches("image/png", "image/PNG"));
        assert!(!mime_matches("image/png", "image/jpeg"));
        assert!(!mime_matches("image/*", "imagery"));
    }

    const GLOBS2: &str = "# comment\n\
        50:image/png:*.png\n\
        50:application/gzip:*.gz\n\
        50:application/x-compressed-tar:*.tar.gz\n\
        10:text/x-weak:*.dat\n\
        60:text/x-strong:*.dat\n\
        50:text/x-makefile:Makefile\n\
        50:image/x-foo:*.f[o]o\n\
        50:text/x-cs:*.C:cs\n\
        garbage line\n";

    #[test]
    fn globs2_parsing() {
        let t = parse_globs2(GLOBS2);
        assert_eq!(t.get("png").map(String::as_str), Some("image/png"));
        assert_eq!(t.get("dat").map(String::as_str), Some("text/x-strong")); // higher weight wins
        assert!(!t.contains_key("f[o]o")); // not a plain extension
        assert!(!t.contains_key("makefile")); // not `*.ext`
        assert_eq!(t.get("c").map(String::as_str), Some("text/x-cs")); // lowercased
    }

    #[test]
    fn mime_from_name() {
        let t = parse_globs2(GLOBS2);
        assert_eq!(mime_of(&t, "photo.PNG"), Some("image/png"));
        assert_eq!(mime_of(&t, "x.tar.gz"), Some("application/x-compressed-tar")); // longest first
        assert_eq!(mime_of(&t, "x.gz"), Some("application/gzip"));
        assert_eq!(mime_of(&t, "noext"), None);
        assert_eq!(mime_of(&t, "x.unknown"), None);
    }

    #[test]
    fn filter_semantics() {
        let images = Filter {
            name: "Images".into(),
            rules: vec![Rule::Glob("*.png".into()), Rule::Glob("*.jpg".into())],
        };
        assert!(images.matches("a.PNG"));
        assert!(images.matches("b.jpg"));
        assert!(!images.matches("c.txt"));
        let all = Filter { name: "All".into(), rules: vec![] };
        assert!(all.matches("anything"));
    }

    #[test]
    fn builtin_table_is_sane() {
        let t: MimeTable = BUILTIN.iter().map(|(e, m)| (e.to_string(), m.to_string())).collect();
        assert_eq!(mime_of(&t, "a.jpeg"), Some("image/jpeg"));
        assert!(mime_matches("video/*", mime_of(&t, "a.mkv").unwrap()));
    }
}
