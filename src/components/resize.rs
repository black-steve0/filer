//! Drag handles. A grip starts a gpui drag carrying a small marker value
//! (`ColDrag`, `PaneDrag`, ...); whoever owns the thing being resized listens
//! with `on_drag_move::<Marker>` on the element whose edge the grip sits on
//! and sets its size to "mouse x minus the element's left edge". That is
//! absolute, so there is no start position to remember.

use crate::theme;
use gpui::{prelude::*, *};

/// The (invisible) drag preview: nothing is drawn while dragging.
pub struct Ghost;

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A thin vertical grip. Highlights on hover and shows the resize cursor.
pub fn grip<T: 'static>(id: impl Into<ElementId>, marker: T) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(6.))
        .h_full()
        .flex_shrink_0()
        .cursor_col_resize()
        .hover(|s| s.bg(theme::alpha(theme::ACCENT, 0x80)))
        .on_drag(marker, |_, _, _, cx| cx.new(|_| Ghost))
}