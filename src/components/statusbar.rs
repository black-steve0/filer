use crate::theme;
use gpui::*;

#[derive(Default)]
pub struct StatusBar {
    text: SharedString,
}

impl StatusBar {
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.text = text.into();
        cx.notify();
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .bg(rgb(theme::MANTLE))
            .border_t_1()
            .border_color(rgb(theme::SURFACE0))
            .shadow(theme::raised())
            .text_color(rgb(theme::SUBTEXT))
            .text_xs()
            .child(self.text.clone())
    }
}