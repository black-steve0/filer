//! Tiny single-line text editing model (no IME). Shared by the path bar and
//! inline rename. Not a view: owners feed it key events and render it.
//!
//! Supports a selection (anchor + cursor) and the usual shortcuts:
//! Ctrl+A/C/X/V, arrows, Home/End, Ctrl+arrows (word jump),
//! Backspace/Delete, Ctrl+Backspace/Delete (delete word), and Shift with any
//! movement key to extend the selection.

use crate::theme;
use gpui::*;

pub struct LineEdit {
    pub text: String,
    pub cursor: usize, // byte offset on a char boundary (the moving end)
    anchor: usize,     // the fixed end of the selection; == cursor when empty
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    Edited,
    Submit,
    Cancel,
    Ignored,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl LineEdit {
    /// Whole text selected, so typing replaces it (like a browser address bar).
    pub fn new(text: String) -> Self {
        let cursor = text.len();
        Self { text, cursor, anchor: 0 }
    }

    /// For renaming: the name without its extension is selected.
    pub fn for_rename(text: String) -> Self {
        let cursor = match text.rfind('.') {
            Some(i) if i > 0 => i,
            _ => text.len(),
        };
        Self { text, cursor, anchor: 0 }
    }

    // ---- selection helpers -------------------------------------------------

    fn selection(&self) -> Option<(usize, usize)> {
        if self.anchor == self.cursor {
            None
        } else {
            Some((self.anchor.min(self.cursor), self.anchor.max(self.cursor)))
        }
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.text.replace_range(a..b, "");
        self.cursor = a;
        self.anchor = a;
        true
    }

    /// Move the cursor; keeps the anchor when extending, collapses otherwise.
    fn move_to(&mut self, pos: usize, extend: bool) {
        self.cursor = pos;
        if !extend {
            self.anchor = pos;
        }
    }

    // ---- motion ------------------------------------------------------------

    fn prev(&self) -> usize {
        self.text[..self.cursor].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.cursor)
    }

    fn word_left(&self) -> usize {
        let mut pos = self.cursor;
        let mut chars = self.text[..pos].chars().rev().peekable();
        while let Some(&c) = chars.peek() {
            if is_word(c) {
                break;
            }
            pos -= c.len_utf8();
            chars.next();
        }
        while let Some(&c) = chars.peek() {
            if !is_word(c) {
                break;
            }
            pos -= c.len_utf8();
            chars.next();
        }
        pos
    }

    fn word_right(&self) -> usize {
        let mut pos = self.cursor;
        let mut chars = self.text[pos..].chars().peekable();
        while let Some(&c) = chars.peek() {
            if is_word(c) {
                break;
            }
            pos += c.len_utf8();
            chars.next();
        }
        while let Some(&c) = chars.peek() {
            if !is_word(c) {
                break;
            }
            pos += c.len_utf8();
            chars.next();
        }
        pos
    }

    // ---- editing -----------------------------------------------------------

    fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.anchor = self.cursor;
    }

    pub fn handle_key(&mut self, ev: &KeyDownEvent, cx: &App) -> EditOutcome {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        let shift = m.shift;
        // Ctrl on Linux/Windows, Cmd on macOS.
        let cmd = m.control || m.platform;

        match ks.key.as_str() {
            "enter" => return EditOutcome::Submit,
            "escape" => return EditOutcome::Cancel,

            // ---- clipboard / select all ----
            "a" if cmd && !m.alt => {
                self.anchor = 0;
                self.cursor = self.text.len();
            }
            "c" if cmd && !m.alt => {
                if let Some(t) = self.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(t.to_string()));
                }
                return EditOutcome::Ignored; // nothing changed
            }
            "x" if cmd && !m.alt => {
                if let Some(t) = self.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(t.to_string()));
                    self.delete_selection();
                } else {
                    return EditOutcome::Ignored;
                }
            }
            "v" if cmd && !m.alt => {
                if let Some(t) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    let t: String = t.chars().filter(|c| !c.is_control()).collect();
                    self.insert(&t);
                } else {
                    return EditOutcome::Ignored;
                }
            }

            // ---- deleting ----
            "backspace" => {
                if !self.delete_selection() {
                    let target = if cmd { self.word_left() } else { self.prev() };
                    if target < self.cursor {
                        self.text.replace_range(target..self.cursor, "");
                        self.cursor = target;
                        self.anchor = target;
                    }
                }
            }
            "delete" => {
                if !self.delete_selection() {
                    let target = if cmd { self.word_right() } else { self.next() };
                    if target > self.cursor {
                        self.text.replace_range(self.cursor..target, "");
                    }
                }
            }

            // ---- movement (Shift extends the selection) ----
            "left" => match self.selection() {
                // plain Left with a selection collapses to its start
                Some((a, _)) if !shift && !cmd => self.move_to(a, false),
                _ => {
                    let p = if cmd { self.word_left() } else { self.prev() };
                    self.move_to(p, shift);
                }
            },
            "right" => match self.selection() {
                Some((_, b)) if !shift && !cmd => self.move_to(b, false),
                _ => {
                    let p = if cmd { self.word_right() } else { self.next() };
                    self.move_to(p, shift);
                }
            },
            "home" => self.move_to(0, shift),
            "end" => self.move_to(self.text.len(), shift),

            // ---- typing ----
            _ => {
                if cmd || m.alt {
                    return EditOutcome::Ignored;
                }
                match &ks.key_char {
                    Some(ch) if !ch.is_empty() && ch.chars().all(|c| !c.is_control()) => {
                        self.insert(ch)
                    }
                    _ => return EditOutcome::Ignored,
                }
            }
        }
        EditOutcome::Edited
    }

    pub fn render(&self) -> impl IntoElement {
        let caret = || div().w(px(1.5)).h(px(15.)).flex_shrink_0().bg(rgb(theme::ACCENT));
        let row = div().flex().flex_row().items_center().whitespace_nowrap();

        match self.selection() {
            None => {
                let (before, after) = self.text.split_at(self.cursor);
                row.child(before.to_string()).child(caret()).child(after.to_string())
            }
            Some((a, b)) => {
                let selected = div()
                    .bg(theme::alpha(theme::ACCENT, 0x55))
                    .child(self.text[a..b].to_string());
                let row = row.child(self.text[..a].to_string());
                // caret sits on whichever end the cursor is at
                let row = if self.cursor == a {
                    row.child(caret()).child(selected)
                } else {
                    row.child(selected).child(caret())
                };
                row.child(self.text[b..].to_string())
            }
        }
    }
}
