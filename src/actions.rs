//! Every command in the app is a GPUI action. Keyboard shortcuts, menus,
//! the toolbar and the context menu all just dispatch these.

use gpui::{actions, App, KeyBinding};

actions!(
    explorer,
    [
        // app-level commands (handled by Workspace)
        GoBack, GoForward, GoUp, GoHome, Refresh, ToggleHidden, SplitPane, ClosePane,
        SwitchPane, FocusPath, GoToFolder, OpenSelected, OpenInOtherPane, CopyPath,
        MoveToTrash, NewFolder, NewFile, StartRename, OpenTerminal, QuitApp,
        CopyFile, CutFile, PasteFile, FindInPane, AddPlace, RemovePlace,
        // "Open With..." dialog / Properties dialog
        OpenWith, ShowProperties,
        // terminal in the selected folder (right-click on a folder)
        OpenTerminalInSelected,
        // hand the selected file to a small drag-source window (drag out of the app)
        DragOut,
        // archives: act on the selected file or folder
        CompressZip, CompressTarGz, ExtractArchive,
        // sidebar "Places" context menu (act on the place that was right-clicked)
        PlaceOpen, PlaceOpenInOtherPane, PlaceOpenTerminal, PlaceCopyPath,
        PlaceRename, PlaceMoveUp, PlaceMoveDown, PlaceChangeColor,
        // extras
        Duplicate, ResetLayout, OpenSettings,
        ToggleColType, ToggleColSize, ToggleColModified, ToggleColPerms,
        // pane-local (handled by the focused Pane)
        SelectPrev, SelectNext, SelectFirst, SelectLast, ExtendPrev, ExtendNext,
        SelectPageUp, SelectPageDown, SelectAll, CloseMenu,
    ]
);

pub fn bind_keys(cx: &mut App) {
    // "TextInput" is the key context of any focused text field; list/app
    // shortcuts are disabled there so typing is never hijacked.
    let list = Some("Pane && !TextInput");
    let app = Some("Workspace && !TextInput");

    cx.bind_keys([
        // list navigation
        KeyBinding::new("up", SelectPrev, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("shift-up", ExtendPrev, list),
        KeyBinding::new("shift-down", ExtendNext, list),
        KeyBinding::new("home", SelectFirst, list),
        KeyBinding::new("end", SelectLast, list),
        KeyBinding::new("pageup", SelectPageUp, list),
        KeyBinding::new("pagedown", SelectPageDown, list),
        KeyBinding::new("escape", CloseMenu, list),
        KeyBinding::new("ctrl-a", SelectAll, list),
        // file actions
        KeyBinding::new("enter", OpenSelected, list),
        KeyBinding::new("backspace", GoUp, list),
        KeyBinding::new("delete", MoveToTrash, list),
        KeyBinding::new("f2", StartRename, list),
        KeyBinding::new("alt-enter", ShowProperties, list),
        KeyBinding::new("ctrl-d", Duplicate, list),
        KeyBinding::new("ctrl-shift-d", DragOut, list),
        KeyBinding::new("tab", SwitchPane, list),
        KeyBinding::new("ctrl-c", CopyFile, list),
        KeyBinding::new("ctrl-x", CutFile, list),
        KeyBinding::new("ctrl-v", PasteFile, list),
        // navigation
        KeyBinding::new("alt-left", GoBack, app),
        KeyBinding::new("alt-right", GoForward, app),
        KeyBinding::new("alt-up", GoUp, app),
        KeyBinding::new("alt-home", GoHome, app),
        // view
        KeyBinding::new("ctrl-h", ToggleHidden, app),
        KeyBinding::new("f5", Refresh, app),
        KeyBinding::new("ctrl-r", Refresh, app),
        KeyBinding::new("f3", SplitPane, app),
        KeyBinding::new("ctrl-w", ClosePane, app),
        // misc
        KeyBinding::new("ctrl-shift-n", NewFolder, app),
        KeyBinding::new("ctrl-n", NewFile, app),
        KeyBinding::new("ctrl-f", FindInPane, app),
        KeyBinding::new("ctrl-shift-c", CopyPath, app),
        KeyBinding::new("ctrl-alt-t", OpenTerminal, app),
        KeyBinding::new("ctrl-l", FocusPath, Some("Workspace")),
        KeyBinding::new("f6", FocusPath, Some("Workspace")),
        // jump to a folder by typing a few letters of its name
        KeyBinding::new("ctrl-p", GoToFolder, Some("Workspace")),
        KeyBinding::new("ctrl-q", QuitApp, Some("Workspace")),
        KeyBinding::new("ctrl-,", OpenSettings, app),
    ]);
}