//! Drag and drop plumbing shared by the panes and the sidebar.
//!
//! * `FileDrag` is the value a row carries while it is dragged inside the app.
//!   Anything that accepts files listens for it with `on_drop::<FileDrag>`.
//! * Files dragged in from other applications arrive as gpui's `ExternalPaths`.
//!
//! Neither component moves files itself: they emit an event with
//! (paths, destination, copy?) and the Workspace does the work, so every
//! pane that shows an affected folder can refresh.

use crate::theme;
use gpui::{prelude::*, *};
use std::path::PathBuf;

/// What a dragged row carries.
#[derive(Clone)]
pub struct FileDrag {
    pub paths: Vec<PathBuf>,
    pub label: SharedString,
}

impl FileDrag {
    pub fn single(path: PathBuf, name: &str) -> Self {
        Self { paths: vec![path], label: name.to_string().into() }
    }
}

/// The little label that follows the mouse while dragging.
pub struct DragPreview {
    label: SharedString,
}

impl DragPreview {
    pub fn new(drag: &FileDrag) -> Self {
        let label = if drag.paths.len() > 1 {
            format!("{} items", drag.paths.len()).into()
        } else {
            drag.label.clone()
        };
        Self { label }
    }
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .bg(rgb(theme::HILITE))
            .border_1()
            .border_color(rgb(theme::ACCENT))
            .text_color(rgb(theme::TEXT))
            .shadow(theme::raised())
            .child(self.label.clone())
    }
}

/// Drop highlight for a folder row / place (the thing that would receive the files).
pub fn target_tint() -> Rgba {
    theme::alpha(theme::ACCENT, 0x40)
}

/// Dragging from another app copies (like most Linux file managers);
/// holding Shift moves instead.
pub fn external_is_copy(window: &Window) -> bool {
    !window.modifiers().shift
}

/// Dragging inside the app moves; holding Ctrl copies.
pub fn internal_is_copy(window: &Window) -> bool {
    window.modifiers().control
}
