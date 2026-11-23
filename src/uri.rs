//! `file://` URIs <-> paths, as D-Bus callers send them.

use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

/// `file:///home/me/My%20File.txt` -> `/home/me/My File.txt`.
///
/// Accepts `file:///x` and `file://localhost/x`, plus a bare absolute path
/// (some callers skip the URI part). Anything else (`smb://`, `trash:///`,
/// a file on another host, a relative path) gives `None`. A `?query` or
/// `#fragment` is cut off.
pub fn to_path(uri: &str) -> Option<PathBuf> {
    let uri = uri.trim();
    let encoded = if let Some(rest) = strip_scheme(uri, "file://") {
        let slash = rest.find('/')?;
        let host = &rest[..slash];
        if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
            return None;
        }
        &rest[slash..]
    } else if uri.starts_with('/') {
        uri
    } else {
        return None;
    };
    let encoded = encoded.split(['?', '#']).next().unwrap_or(encoded);
    let bytes = percent_decode(encoded)?;
    if bytes.is_empty() || bytes.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

/// `/home/me/My File.txt` -> `file:///home/me/My%20File.txt`.
pub fn from_path(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Case-insensitive scheme prefix (`FILE://` is legal).
fn strip_scheme<'a>(s: &'a str, scheme: &str) -> Option<&'a str> {
    let head = s.get(..scheme.len())?;
    head.eq_ignore_ascii_case(scheme).then(|| &s[scheme.len()..])
}

/// Decode `%XX` escapes to raw bytes (file names need not be UTF-8).
/// A malformed escape fails the whole string.
fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hi = hex(*b.get(i + 1)?)?;
            let lo = hex(*b.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(s: &str) -> Option<String> {
        to_path(s).map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn plain_and_escaped() {
        assert_eq!(p("file:///home/me/a.txt").as_deref(), Some("/home/me/a.txt"));
        assert_eq!(p("file:///home/me/My%20File%231.txt").as_deref(), Some("/home/me/My File#1.txt"));
        assert_eq!(p("file:///a/%E2%9C%93").as_deref(), Some("/a/\u{2713}"));
    }

    #[test]
    fn host_forms() {
        assert_eq!(p("file://localhost/tmp/x").as_deref(), Some("/tmp/x"));
        assert_eq!(p("FILE:///tmp/x").as_deref(), Some("/tmp/x"));
        assert_eq!(p("file://otherbox/tmp/x"), None);
        assert_eq!(p("file://"), None);
    }

    #[test]
    fn bare_paths_and_junk() {
        assert_eq!(p("/tmp/x").as_deref(), Some("/tmp/x"));
        assert_eq!(p("  file:///tmp/x \n").as_deref(), Some("/tmp/x"));
        assert_eq!(p("relative/path"), None);
        assert_eq!(p("smb://host/share"), None);
        assert_eq!(p("trash:///"), None);
        assert_eq!(p(""), None);
    }

    #[test]
    fn bad_escapes_and_nul() {
        assert_eq!(p("file:///a%zz"), None);
        assert_eq!(p("file:///a%2"), None);
        assert_eq!(p("file:///a%00b"), None);
    }

    #[test]
    fn query_and_fragment_are_dropped() {
        assert_eq!(p("file:///a/b?x=1").as_deref(), Some("/a/b"));
        assert_eq!(p("file:///a/b#frag").as_deref(), Some("/a/b"));
        // an escaped # or ? is part of the name
        assert_eq!(p("file:///a/b%3Fc").as_deref(), Some("/a/b?c"));
    }

    #[test]
    fn round_trip() {
        for s in ["/a/b.txt", "/a/My File #1 (copy).txt", "/a/\u{2713}/%", "/a/100%"] {
            let path = PathBuf::from(s);
            assert_eq!(to_path(&from_path(&path)), Some(path));
        }
        assert_eq!(from_path(Path::new("/a b")), "file:///a%20b");
    }

    #[test]
    fn non_utf8_name_survives() {
        let path = to_path("file:///a/%FF%FE").unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b"/a/\xFF\xFE");
    }
}
