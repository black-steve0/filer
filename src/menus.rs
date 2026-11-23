//! Menu definitions (menu bar + right-click). Pure data: add an entry here
//! and it shows up; behaviour lives in the action handlers.
//!
//! Each entry names an icon (`assets/icons/<icon>.svg|png`). A missing image
//! just leaves a blank slot, so menus stay aligned either way.

use crate::{actions::*, components::context_menu::MenuEntry};
use gpui::Action;

fn item(
    icon: &'static str,
    label: &'static str,
    shortcut: &'static str,
    action: impl Action,
) -> MenuEntry {
    MenuEntry::Item { icon, label, shortcut, action: Box::new(action) }
}

pub fn file_menu() -> Vec<MenuEntry> {
    vec![
        item("new-file", "New File", "Ctrl+N", NewFile),
        item("new-folder", "New Folder", "Ctrl+Shift+N", NewFolder),
        item("terminal", "Open Terminal Here", "Ctrl+Alt+T", OpenTerminal),
        MenuEntry::Separator,
        item("settings", "Settings...", "Ctrl+,", OpenSettings),
        MenuEntry::Separator,
        item("quit", "Quit", "Ctrl+Q", QuitApp),
    ]
}

pub fn edit_menu() -> Vec<MenuEntry> {
    vec![
        item("open", "Open", "Enter", OpenSelected),
        item("open-other-pane", "Open in Other Pane", "", OpenInOtherPane),
        MenuEntry::Separator,
        item("cut", "Cut", "Ctrl+X", CutFile),
        item("copy", "Copy", "Ctrl+C", CopyFile),
        item("paste", "Paste", "Ctrl+V", PasteFile),
        MenuEntry::Separator,
        item("rename", "Rename", "F2", StartRename),
        item("duplicate", "Duplicate", "Ctrl+D", Duplicate),
        item("copy-path", "Copy Path", "Ctrl+Shift+C", CopyPath),
        item("add-place", "Add to Places", "", AddPlace),
        item("compress", "Compress to .zip", "", CompressZip),
        item("compress", "Compress to .tar.gz", "", CompressTarGz),
        item("extract", "Extract Archive", "", ExtractArchive),
        item("trash", "Move to Trash", "Del", MoveToTrash),
        MenuEntry::Separator,
        item("find", "Filter...", "Ctrl+F", FindInPane),
    ]
}

pub fn view_menu() -> Vec<MenuEntry> {
    vec![
        item("hidden", "Toggle Hidden Files", "Ctrl+H", ToggleHidden),
        item("split", "Split Pane", "F3", SplitPane),
        item("close-split", "Close Pane", "Ctrl+W", ClosePane),
        item("switch-pane", "Switch Pane", "Tab", SwitchPane),
        MenuEntry::Separator,
        item("column", "Toggle Type Column", "", ToggleColType),
        item("column", "Toggle Size Column", "", ToggleColSize),
        item("column", "Toggle Modified Column", "", ToggleColModified),
        item("column", "Toggle Perms Column", "", ToggleColPerms),
        item("reset", "Reset Layout", "", ResetLayout),
        MenuEntry::Separator,
        item("refresh", "Refresh", "F5", Refresh),
    ]
}

/// Right-click on the column header.
pub fn columns_menu() -> Vec<MenuEntry> {
    vec![
        item("column", "Toggle Type Column", "", ToggleColType),
        item("column", "Toggle Size Column", "", ToggleColSize),
        item("column", "Toggle Modified Column", "", ToggleColModified),
        item("column", "Toggle Perms Column", "", ToggleColPerms),
        MenuEntry::Separator,
        item("reset", "Reset Layout", "", ResetLayout),
    ]
}

pub fn go_menu() -> Vec<MenuEntry> {
    vec![
        item("back", "Back", "Alt+Left", GoBack),
        item("forward", "Forward", "Alt+Right", GoForward),
        item("up", "Up", "Alt+Up", GoUp),
        item("home", "Home", "Alt+Home", GoHome),
        MenuEntry::Separator,
        item("location", "Location...", "Ctrl+L", FocusPath),
    ]
}

/// Right-click on a file or folder.
pub fn row_menu(is_dir: bool, is_archive: bool) -> Vec<MenuEntry> {
    let mut v = vec![item("open", "Open", "Enter", OpenSelected)];
    if is_dir {
        v.push(item("open-other-pane", "Open in Other Pane", "", OpenInOtherPane));
        v.push(item("terminal", "Open Terminal in This Folder", "", OpenTerminalInSelected));
        v.push(item("add-place", "Add to Places", "", AddPlace));
    }
    v.push(MenuEntry::Separator);
    v.push(item("cut", "Cut", "Ctrl+X", CutFile));
    v.push(item("copy", "Copy", "Ctrl+C", CopyFile));
    v.push(item("paste", "Paste", "Ctrl+V", PasteFile));
    v.push(MenuEntry::Separator);
    v.push(item("rename", "Rename", "F2", StartRename));
    v.push(item("duplicate", "Duplicate", "Ctrl+D", Duplicate));
    v.push(item("copy-path", "Copy Path", "Ctrl+Shift+C", CopyPath));
    v.push(item("drag-out", "Drag Out of App...", "Ctrl+Shift+D", DragOut));
    v.push(MenuEntry::Separator);
    if is_archive {
        v.push(item("extract", "Extract to Folder", "", ExtractArchive));
    }
    v.push(item("compress", "Compress to .zip", "", CompressZip));
    v.push(item("compress", "Compress to .tar.gz", "", CompressTarGz));
    v.push(MenuEntry::Separator);
    v.push(item("trash", "Move to Trash", "Del", MoveToTrash));
    v.push(MenuEntry::Separator);
    v.push(item("new-file", "New File", "Ctrl+N", NewFile));
    v.push(item("new-folder", "New Folder", "Ctrl+Shift+N", NewFolder));
    v.push(item("terminal", "Open Terminal Here", "Ctrl+Alt+T", OpenTerminal));
    v
}

/// Right-click on empty space.
pub fn background_menu(show_hidden: bool) -> Vec<MenuEntry> {
    vec![
        item("new-file", "New File", "Ctrl+N", NewFile),
        item("new-folder", "New Folder", "Ctrl+Shift+N", NewFolder),
        item("paste", "Paste", "Ctrl+V", PasteFile),
        item("terminal", "Open Terminal Here", "Ctrl+Alt+T", OpenTerminal),
        MenuEntry::Separator,
        item("refresh", "Refresh", "F5", Refresh),
        item(
            "hidden",
            if show_hidden { "Hide Hidden Files" } else { "Show Hidden Files" },
            "Ctrl+H",
            ToggleHidden,
        ),
        item("copy-path", "Copy Path", "Ctrl+Shift+C", CopyPath),
        item("add-place", "Add This Folder to Places", "", AddPlace),
    ]
}

/// Right-click on an entry in the sidebar. Every item acts on the place that
/// was clicked, not on the pane's selection.
pub fn place_menu() -> Vec<MenuEntry> {
    vec![
        item("open", "Open", "", PlaceOpen),
        item("open-other-pane", "Open in Other Pane", "", PlaceOpenInOtherPane),
        item("terminal", "Open Terminal Here", "", PlaceOpenTerminal),
        MenuEntry::Separator,
        item("rename", "Rename", "", PlaceRename),
        item("color", "Change Color", "", PlaceChangeColor),
        item("move-up", "Move Up", "", PlaceMoveUp),
        item("move-down", "Move Down", "", PlaceMoveDown),
        MenuEntry::Separator,
        item("copy-path", "Copy Path", "", PlaceCopyPath),
        MenuEntry::Separator,
        item("remove-place", "Remove from Places", "", RemovePlace),
    ]
}