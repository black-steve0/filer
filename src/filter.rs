//! The pane filter's query language. Pure logic, no UI.
//!
//! The text is split on spaces; every term must match (AND).
//!
//!   report          name contains "report"
//!   *.txt  img_??   wildcards (`*` any run, `?` one char) match the whole name
//!   -draft          NOT: name does not contain "draft" (works on any term)
//!   ext:rs          extension is rs
//!   type:folder     folders  (also `type:file`, or any text from the Type
//!                   column, e.g. `type:image`, `type:rust`)
//!   size>10mb       size comparison: > >= < <= =   (units b kb mb gb tb)
//!   newer:7d        modified within 7 days  (units h d w y)
//!   older:1y        modified more than a year ago
//!
//! Matching ignores case unless the "Aa" toggle is on.

use crate::model::Entry;
use std::{
    path::Path,
    time::{Duration, SystemTime},
};

/// Quick folder/file switch shown as a chip in the filter bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KindFilter {
    All,
    Folders,
    Files,
}

impl KindFilter {
    pub fn next(self) -> Self {
        match self {
            KindFilter::All => KindFilter::Folders,
            KindFilter::Folders => KindFilter::Files,
            KindFilter::Files => KindFilter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            KindFilter::All => "All",
            KindFilter::Folders => "Folders",
            KindFilter::Files => "Files",
        }
    }

    pub fn allows(self, is_dir: bool) -> bool {
        match self {
            KindFilter::All => true,
            KindFilter::Folders => is_dir,
            KindFilter::Files => !is_dir,
        }
    }
}

enum Cmp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
}

enum Kind {
    Text(String),
    Glob(Vec<char>),
    Ext(String),
    Dir,
    File,
    TypeText(String),
    Size(Cmp, u64),
    Newer(Duration),
    Older(Duration),
}

struct Term {
    neg: bool,
    kind: Kind,
}

pub struct Query {
    terms: Vec<Term>,
    case: bool,
    now: SystemTime,
}

fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num.parse().ok()?;
    let mult: f64 = match unit {
        "" | "b" => 1.,
        "k" | "kb" => 1024.,
        "m" | "mb" => 1024. * 1024.,
        "g" | "gb" => 1024. * 1024. * 1024.,
        "t" | "tb" => 1024f64.powi(4),
        _ => return None,
    };
    Some((value * mult) as u64)
}

fn parse_age(s: &str) -> Option<Duration> {
    let s = s.trim().to_lowercase();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().ok()?;
    let secs = match unit {
        "h" => 3600,
        "d" | "" => 86_400,
        "w" => 7 * 86_400,
        "y" => 365 * 86_400,
        _ => return None,
    };
    Some(Duration::from_secs(n * secs))
}

fn parse_term(body: &str, case: bool) -> Kind {
    if let Some((key, val)) = body.split_once(':') {
        if !val.is_empty() {
            let lower = val.to_lowercase();
            match key.to_lowercase().as_str() {
                "ext" => return Kind::Ext(lower.trim_start_matches('.').to_string()),
                "type" | "is" => {
                    return match lower.as_str() {
                        "dir" | "folder" | "directory" => Kind::Dir,
                        "file" => Kind::File,
                        _ => Kind::TypeText(lower),
                    }
                }
                "newer" | "within" => {
                    if let Some(d) = parse_age(val) {
                        return Kind::Newer(d);
                    }
                }
                "older" => {
                    if let Some(d) = parse_age(val) {
                        return Kind::Older(d);
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(rest) = body.to_lowercase().strip_prefix("size") {
        for (op, cmp) in [
            (">=", Cmp::Ge),
            ("<=", Cmp::Le),
            (">", Cmp::Gt),
            ("<", Cmp::Lt),
            ("=", Cmp::Eq),
        ] {
            if let Some(num) = rest.strip_prefix(op) {
                if let Some(n) = parse_size(num) {
                    return Kind::Size(cmp, n);
                }
            }
        }
    }
    let text = if case { body.to_string() } else { body.to_lowercase() };
    if text.contains('*') || text.contains('?') {
        Kind::Glob(text.chars().collect())
    } else {
        Kind::Text(text)
    }
}

/// Shell-style wildcard match of the whole of `t` against `p`.
fn glob(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<usize> = None;
    let mut mark = 0;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl Query {
    pub fn parse(text: &str, case: bool) -> Self {
        let terms = text
            .split_whitespace()
            .map(|tok| {
                let neg = tok.len() > 1 && tok.starts_with('-');
                let body = if neg { &tok[1..] } else { tok };
                Term { neg, kind: parse_term(body, case) }
            })
            .collect();
        Self { terms, case, now: SystemTime::now() }
    }

    fn term_matches(&self, kind: &Kind, e: &Entry) -> bool {
        match kind {
            Kind::Text(t) => {
                if self.case { e.name.contains(t.as_str()) } else { e.lname.contains(t.as_str()) }
            }
            Kind::Glob(p) => {
                let name: Vec<char> =
                    if self.case { e.name.chars().collect() } else { e.lname.chars().collect() };
                glob(p, &name)
            }
            Kind::Ext(x) => {
                !e.is_dir
                    && Path::new(&e.name)
                        .extension()
                        .is_some_and(|ext| ext.to_string_lossy().to_lowercase() == *x)
            }
            Kind::Dir => e.is_dir,
            Kind::File => !e.is_dir,
            Kind::TypeText(t) => e.kind.to_lowercase().contains(t.as_str()),
            Kind::Size(cmp, n) => {
                !e.is_dir
                    && match cmp {
                        Cmp::Gt => e.size > *n,
                        Cmp::Ge => e.size >= *n,
                        Cmp::Lt => e.size < *n,
                        Cmp::Le => e.size <= *n,
                        Cmp::Eq => e.size == *n,
                    }
            }
            Kind::Newer(d) => e
                .modified
                .is_some_and(|m| self.now.duration_since(m).map(|age| age <= *d).unwrap_or(true)),
            Kind::Older(d) => e
                .modified
                .is_some_and(|m| self.now.duration_since(m).map(|age| age > *d).unwrap_or(false)),
        }
    }

    pub fn matches(&self, e: &Entry) -> bool {
        self.terms.iter().all(|t| self.term_matches(&t.kind, e) != t.neg)
    }
}