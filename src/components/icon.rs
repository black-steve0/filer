use crate::{model::Entry, theme};
use gpui::*;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

// ---- image lookup ---------------------------------------------------------
//
// Every icon is looked up as `assets/icons/<name>.svg` (or `.png`). If no
// such file exists, callers draw placeholder text instead. Searched in:
// the working directory, next to the executable, then the project root.
// Results are cached, so restart the app after adding new images.

fn icon_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut dirs = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(cwd.join("assets/icons"));
        }
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            dirs.push(dir.join("assets/icons"));
        }
        dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/icons"));
        dirs
    })
}

/// Path of the image for `name`, if one exists on disk.
pub fn find_image(name: &str) -> Option<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    cache
        .entry(name.to_string())
        .or_insert_with(|| {
            icon_dirs()
                .iter()
                .flat_map(|d| ["svg", "png"].map(|ext| d.join(format!("{name}.{ext}"))))
                .find(|p| p.is_file())
        })
        .clone()
}

/// A `size` x `size` image for `name`, or whatever `placeholder` builds when
/// the image is missing (or fails to load / decode).
pub fn image_or(
    name: &str,
    size: Pixels,
    placeholder: impl Fn() -> AnyElement + 'static,
) -> AnyElement {
    match find_image(name) {
        Some(path) => img(path)
            .w(size)
            .h(size)
            .flex_shrink_0()
            .object_fit(ObjectFit::Contain)
            .with_fallback(move || placeholder())
            .into_any_element(),
        None => placeholder(),
    }
}

fn placeholder_text(label: String, color: u32) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .min_w(px(22.))
        .h(px(18.))
        .px_0p5()
        .text_color(rgb(color))
        .font_family(crate::settings::get().mono_family.clone())
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .whitespace_nowrap()
        .child(label)
        .into_any_element()
}

fn kind_color(ext: &str) -> u32 {
    match ext {
        "rs" => 0xb34700,
        "py" | "js" | "ts" | "tsx" | "jsx" | "lua" => 0x8a6d00,
        "c" | "h" | "cpp" | "hpp" | "go" | "java" => 0x0044aa,
        "md" | "txt" | "log" | "rst" => 0x555555,
        "toml" | "json" | "yaml" | "yml" | "ini" | "conf" | "xml" => 0x007a6e,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" => 0x1a7f1a,
        "mp3" | "flac" | "ogg" | "wav" | "m4a" => 0xaa2288,
        "mp4" | "mkv" | "webm" | "avi" | "mov" => 0x6a2cb0,
        "zip" | "tar" | "gz" | "xz" | "bz2" | "7z" | "zst" | "pdf" => 0xcc0000,
        "sh" | "bash" | "zsh" | "fish" => 0x1a7f1a,
        _ => theme::OVERLAY,
    }
}

/// Folder: `assets/icons/folder.{svg,png}`, else the text `[dir]`.
/// File: `file-<ext>`, then generic `file`, else the text `[<ext>]`
/// (or `[file]` when there is no extension).
pub fn icon(entry: &Entry) -> AnyElement {
    const SIZE: f32 = 18.;

    if entry.is_dir {
        return image_or("folder", px(SIZE), || placeholder_text("[dir]".into(), theme::FOLDER));
    }

    let ext = entry
        .path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let color = kind_color(&ext);

    let specific = (!ext.is_empty()).then(|| format!("file-{ext}"));
    let name = match specific {
        Some(n) if find_image(&n).is_some() => n,
        _ => "file".to_string(),
    };
    let label = if ext.is_empty() { "[file]".to_string() } else { format!("[{ext}]") };
    image_or(&name, px(SIZE), move || placeholder_text(label.clone(), color))
}
