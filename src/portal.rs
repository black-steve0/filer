//! `org.freedesktop.impl.portal.FileChooser`: the backend behind the
//! "Open file" / "Save as" dialogs of Firefox, Discord and any other app that
//! goes through xdg-desktop-portal.
//!
//! The app calls the portal, the portal calls us (on the bus name
//! `org.freedesktop.impl.portal.desktop.filer`, chosen in `portals.conf`) and
//! we show a chooser window. The call stays open until the user answers:
//!
//!   OpenFile(o handle, s app_id, s parent_window, s title, a{sv} options) -> (u response, a{sv} results)
//!   SaveFile(...same...)                                                  -> (u response, a{sv} results)
//!
//! response: 0 = picked, 1 = cancelled, 2 = something went wrong.
//! results["uris"] = the chosen `file://` URIs.
//!
//! Like `dbus.rs`, this runs on its own thread and talks to the UI through
//! channels: a `Chooser` goes out, an `Outcome` comes back.
//!
//! Not supported yet: the activation token for focus.

use std::{collections::HashMap, ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
use crate::filetype::{Filter, Rule};
use zbus::zvariant::{Array, ObjectPath, OwnedValue, Signature, StructureBuilder, Value};

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.filer";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

#[derive(Debug, Clone)]
pub enum Kind {
    /// Pick an existing file (or a folder when `directory`).
    Open {
        /// The app takes several files: Ctrl/Shift+click and Ctrl+A mark them.
        multiple: bool,
        directory: bool,
    },
    /// Pick a folder and a file name; `name` is the app's suggestion.
    Save { name: String },
}

/// What the user decided.
#[derive(Debug)]
pub enum Outcome {
    /// The chosen paths and the filter that was active (if the app sent any).
    Picked(Vec<PathBuf>, Option<Filter>, Vec<(String, String)>),
    Cancelled,
}

/// An extra option of the dialog: a row of exclusive options (`options` not
/// empty) or a checkbox (`options` empty, `selected` is "true" / "false").
#[derive(Debug, Clone)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub options: Vec<(String, String)>, // (id, label)
    pub selected: String,
}

/// One chooser the UI has to show.
pub struct Chooser {
    pub kind: Kind,
    pub title: String,
    pub app_id: String,
    /// The calling app's window as the portal names it (`wayland:HANDLE`, `x11:XID`, or empty).
    pub parent_window: String,
    /// Folder to start in (`current_folder`, or the folder of `current_file`).
    pub folder: Option<PathBuf>,
    /// The app's file-type filters ("Images", ...). Empty = no filtering.
    pub filters: Vec<Filter>,
    /// Index into `filters` of the one to start with.
    pub current_filter: usize,
    /// Extra options of the app (encoding, "open read-only", ...).
    pub choices: Vec<Choice>,
    /// Answer here exactly once. Dropping it counts as "cancelled".
    pub reply: async_channel::Sender<Outcome>,
    /// Fires (or closes) when the app withdraws the request, or once it is over.
    pub cancel: async_channel::Receiver<()>,
}

struct FileChooser {
    tx: async_channel::Sender<Chooser>,
}

type Results = HashMap<String, Value<'static>>;

impl FileChooser {
    #[allow(clippy::too_many_arguments)]
    async fn run(
        &self,
        conn: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        kind: Kind,
        folder: Option<PathBuf>,
        filters: (Vec<Filter>, usize),
        files: Vec<String>,
        choices: Vec<Choice>,
    ) -> (u32, Results) {
        let handle = handle.into_owned();
        let (reply, answer) = async_channel::bounded::<Outcome>(1);
        let (close, cancel) = async_channel::bounded::<()>(1);

        // the portal may call Close() on this object to withdraw the request
        let exported = conn.object_server().at(handle.clone(), Request { cancel: close }).await;
        if let Err(e) = exported {
            eprintln!("portal: could not export request object: {e}");
        }

        let (filters, current_filter) = filters;
        let chooser = Chooser { kind, title, app_id, parent_window, folder, filters, current_filter, choices, reply, cancel };
        let outcome = match self.tx.send(chooser).await {
            Ok(()) => Some(answer.recv().await.unwrap_or(Outcome::Cancelled)),
            Err(_) => None, // the UI side is gone
        };

        let _ = conn.object_server().remove::<Request, _>(handle).await;

        match outcome {
            None => (2, Results::new()),
            Some(Outcome::Picked(paths, filter, chosen)) => {
                // SaveFiles: the user picked a folder; the answer is one path per requested file
                let paths = match paths.first() {
                    Some(dir) if !files.is_empty() => files.iter().map(|n| free_name(dir, n)).collect(),
                    _ => paths,
                };
                let uris: Vec<String> = paths.iter().map(|p| crate::uri::from_path(p)).collect();
                let mut results = Results::new();
                results.insert("uris".into(), Value::from(uris));
                if !chosen.is_empty() {
                    let list = chosen
                        .into_iter()
                        .map(|(id, value)| structure(vec![Value::from(id), Value::from(value)]))
                        .collect();
                    results.insert("choices".into(), array("(ss)", list));
                }
                // apps (Firefox when saving) use this to add the right extension
                if let Some(filter) = filter {
                    results.insert("current_filter".into(), filter_value(&filter));
                }
                (0, results)
            }
            Some(Outcome::Cancelled) => (1, Results::new()),
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        3
    }

    async fn open_file(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        mut options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let kind = Kind::Open {
            multiple: take_bool(&mut options, "multiple"),
            directory: take_bool(&mut options, "directory"),
        };
        let folder = take_path(&mut options, "current_folder");
        let filters = take_filters(&mut options);
        let choices = take_choices(&mut options);
        self.run(conn, handle, app_id, parent_window, title, kind, folder, filters, Vec::new(), choices).await
    }

    async fn save_file(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        mut options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let mut name = take_string(&mut options, "current_name").unwrap_or_default();
        let mut folder = take_path(&mut options, "current_folder");
        // `current_file`: the file being "saved as", which also tells name + folder
        if let Some(file) = take_path(&mut options, "current_file") {
            if name.is_empty() {
                name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            }
            if folder.is_none() {
                folder = file.parent().map(|p| p.to_path_buf());
            }
        }
        let filters = take_filters(&mut options);
        let choices = take_choices(&mut options);
        self.run(conn, handle, app_id, parent_window, title, Kind::Save { name }, folder, filters, Vec::new(), choices).await
    }

    /// Several files to save at once: the user picks the folder they all go into.
    async fn save_files(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        mut options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        // `files` is `aay`: byte-string names with a trailing NUL
        let files: Vec<String> = options
            .remove("files")
            .and_then(|v| Vec::<Vec<u8>>::try_from(v).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|b| String::from_utf8_lossy(&b).trim_end_matches('\0').to_string())
            .filter(|n| !n.is_empty() && !n.contains('/') && n != "." && n != "..")
            .collect();
        if files.is_empty() {
            return (2, Results::new());
        }
        let folder = take_path(&mut options, "current_folder");
        let title = if title.is_empty() { "Save Files".to_string() } else { title };
        let kind = Kind::Open { multiple: false, directory: true };
        self.run(conn, handle, app_id, parent_window, title, kind, folder, (Vec::new(), 0), files, Vec::new()).await
    }
}

/// `dir/name`, or `dir/name (1)`, `name (2)`, ... (before the extension) if that exists.
fn free_name(dir: &std::path::Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..1000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// `org.freedesktop.impl.portal.Request`, exported at the call's `handle`
/// while the chooser is open.
struct Request {
    cancel: async_channel::Sender<()>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    async fn close(&self) {
        let _ = self.cancel.try_send(());
    }
}

// ---- option helpers -------------------------------------------------------------

fn take_bool(options: &mut HashMap<String, OwnedValue>, key: &str) -> bool {
    options.remove(key).and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
}

fn take_string(options: &mut HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    options.remove(key).and_then(|v| String::try_from(v).ok())
}

/// `filters` is `a(sa(us))`: name plus rules, each rule a kind (0 = glob,
/// 1 = MIME type) and a pattern. `current_filter` is one such struct; it is
/// matched to the list by name. Returns the filters and the index to start on.
fn take_filters(options: &mut HashMap<String, OwnedValue>) -> (Vec<Filter>, usize) {
    let filters: Vec<Filter> = match options.remove("filters").as_deref() {
        Some(Value::Array(list)) => list.iter().filter_map(parse_filter).collect(),
        _ => Vec::new(),
    };
    let current = options.remove("current_filter");
    let current = current.as_deref().and_then(parse_filter);
    let index = current
        .and_then(|c| filters.iter().position(|f| f.name == c.name))
        .unwrap_or(0);
    (filters, index)
}

/// `choices` is `a(ssa(ss)s)`: id, label, options (id, label) and the initial
/// selection. No options = a checkbox whose selection is "true" or "false".
fn take_choices(options: &mut HashMap<String, OwnedValue>) -> Vec<Choice> {
    let raw = options.remove("choices");
    let Some(Value::Array(list)) = raw.as_deref() else { return Vec::new() };
    list.iter()
        .filter_map(|item| {
            let Value::Structure(c) = item else { return None };
            let [Value::Str(id), Value::Str(label), Value::Array(opts), Value::Str(selected)] = c.fields() else {
                return None;
            };
            let options = opts
                .iter()
                .filter_map(|o| {
                    let Value::Structure(o) = o else { return None };
                    let [Value::Str(oid), Value::Str(olabel)] = o.fields() else { return None };
                    Some((oid.as_str().to_string(), olabel.as_str().to_string()))
                })
                .collect();
            Some(Choice {
                id: id.as_str().to_string(),
                label: label.as_str().to_string(),
                options,
                selected: selected.as_str().to_string(),
            })
        })
        .collect()
}

fn parse_filter(value: &Value<'_>) -> Option<Filter> {
    let Value::Structure(filter) = value else { return None };
    let [Value::Str(name), Value::Array(rules)] = filter.fields() else { return None };
    let rules = rules
        .iter()
        .filter_map(|rule| {
            let Value::Structure(rule) = rule else { return None };
            let [Value::U32(kind), Value::Str(pattern)] = rule.fields() else { return None };
            match *kind {
                0 => Some(Rule::Glob(pattern.as_str().to_string())),
                1 => Some(Rule::Mime(pattern.as_str().to_string())),
                _ => None,
            }
        })
        .collect();
    Some(Filter { name: name.as_str().to_string(), rules })
}

/// A struct value from its fields.
fn structure(fields: Vec<Value<'static>>) -> Value<'static> {
    let mut builder = StructureBuilder::new();
    for field in fields {
        builder = builder.append_field(field);
    }
    Value::Structure(builder.build().expect("a structure with fields"))
}

/// An array whose element type is `element` (a D-Bus signature such as `(us)`),
/// also right when `items` is empty.
fn array(element: &str, items: Vec<Value<'static>>) -> Value<'static> {
    let signature: Signature = element.parse().expect("a valid signature");
    let mut array = Array::new(&signature);
    for item in items {
        let _ = array.append(item);
    }
    Value::Array(array)
}

/// A filter back as `(sa(us))`, the shape the app sent it in.
fn filter_value(filter: &Filter) -> Value<'static> {
    let rules = filter
        .rules
        .iter()
        .map(|rule| {
            let (kind, pattern) = match rule {
                Rule::Glob(p) => (0u32, p.clone()),
                Rule::Mime(m) => (1u32, m.clone()),
            };
            structure(vec![Value::U32(kind), Value::from(pattern)])
        })
        .collect();
    structure(vec![Value::from(filter.name.clone()), array("(us)", rules)])
}

/// Paths come as byte strings ("ay") with a trailing NUL.
fn take_path(options: &mut HashMap<String, OwnedValue>, key: &str) -> Option<PathBuf> {
    let mut bytes = options.remove(key).and_then(|v| Vec::<u8>::try_from(v).ok())?;
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    (!bytes.is_empty()).then(|| PathBuf::from(OsString::from_vec(bytes)))
}

// ---- serving ----------------------------------------------------------------------

/// Claim the portal bus name on a background thread; returns the queue of
/// choosers to show. If the name is taken or there is no session bus, the
/// thread logs it and ends, and the queue stays empty.
pub fn start() -> Option<async_channel::Receiver<Chooser>> {
    let (tx, rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new().name("portal".into()).spawn(move || {
        if let Err(e) = zbus::block_on(serve(tx)) {
            eprintln!("portal: {BUS_NAME} not available: {e}");
        }
    });
    match spawned {
        Ok(_) => Some(rx),
        Err(e) => {
            eprintln!("portal: could not start thread: {e}");
            None
        }
    }
}

async fn serve(tx: async_channel::Sender<Chooser>) -> zbus::Result<()> {
    let _connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, FileChooser { tx })?
        .build()
        .await?;
    eprintln!("portal: serving {BUS_NAME}");
    std::future::pending::<()>().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn images() -> Filter {
        Filter {
            name: "Images".into(),
            rules: vec![Rule::Glob("*.png".into()), Rule::Mime("image/jpeg".into())],
        }
    }

    #[test]
    fn filter_survives_a_round_trip() {
        let value = filter_value(&images());
        assert_eq!(value.value_signature().to_string(), "(sa(us))");
        assert_eq!(parse_filter(&value), Some(images()));
    }

    #[test]
    fn filter_without_rules_keeps_its_type() {
        let empty = Filter { name: "All".into(), rules: vec![] };
        let value = filter_value(&empty);
        assert_eq!(value.value_signature().to_string(), "(sa(us))");
        assert_eq!(parse_filter(&value), Some(empty));
    }

    #[test]
    fn filters_option_is_read_and_current_is_matched_by_name() {
        let all = Filter { name: "All".into(), rules: vec![] };
        let mut options = HashMap::new();
        options.insert(
            "filters".to_string(),
            OwnedValue::try_from(array("(sa(us))", vec![filter_value(&all), filter_value(&images())])).unwrap(),
        );
        options.insert("current_filter".to_string(), OwnedValue::try_from(filter_value(&images())).unwrap());
        let (filters, current) = take_filters(&mut options);
        assert_eq!(filters.len(), 2);
        assert_eq!(current, 1);
    }

    #[test]
    fn choices_are_read() {
        let encoding = structure(vec![
            Value::from("encoding".to_string()),
            Value::from("Encoding".to_string()),
            array(
                "(ss)",
                vec![
                    structure(vec![Value::from("utf8".to_string()), Value::from("UTF-8".to_string())]),
                    structure(vec![Value::from("latin1".to_string()), Value::from("Latin-1".to_string())]),
                ],
            ),
            Value::from("utf8".to_string()),
        ]);
        let readonly = structure(vec![
            Value::from("ro".to_string()),
            Value::from("Read only".to_string()),
            array("(ss)", vec![]),
            Value::from("false".to_string()),
        ]);
        let mut options = HashMap::new();
        options.insert(
            "choices".to_string(),
            OwnedValue::try_from(array("(ssa(ss)s)", vec![encoding, readonly])).unwrap(),
        );
        let choices = take_choices(&mut options);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].options.len(), 2);
        assert_eq!(choices[0].selected, "utf8");
        assert!(choices[1].options.is_empty()); // a checkbox
        assert_eq!(choices[1].selected, "false");
    }

    #[test]
    fn free_name_avoids_existing_files() {
        let dir = std::env::temp_dir().join(format!("filer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_name(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), "").unwrap();
        assert_eq!(free_name(&dir, "a.txt"), dir.join("a (1).txt"));
        std::fs::write(dir.join("a (1).txt"), "").unwrap();
        assert_eq!(free_name(&dir, "a.txt"), dir.join("a (2).txt"));
        std::fs::write(dir.join("README"), "").unwrap();
        assert_eq!(free_name(&dir, "README"), dir.join("README (1)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
