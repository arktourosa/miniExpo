//! A tiny single-line text field (GPUI 0.2.2 ships no text input widget).

use std::ops::Range;

use gpui::{div, prelude::*, px, rgb, Div, KeyDownEvent};

pub enum Outcome {
    None,
    Changed,
    Enter,
    Escape,
    /// The caller should put this text on the clipboard.
    Copy(String),
    /// Not a key we own: don't swallow it, so the platform still generates `WM_CHAR` / IME
    /// composition for it. Plain printable text (including the space bar, which GPUI never
    /// surfaces as a `key_char` on Windows) relies on that to reach the input handler.
    PassThrough,
}

/// The byte offset `utf16_offset` UTF-16 code units into `s`, clamped to its end.
pub fn utf16_to_byte(s: &str, utf16_offset: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= utf16_offset {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

/// The number of UTF-16 code units `s[..byte_offset]` takes up.
pub fn byte_to_utf16(s: &str, byte_offset: usize) -> usize {
    let b = byte_offset.min(s.len());
    s[..b].chars().map(char::len_utf16).sum()
}

#[derive(Default, Clone)]
pub struct TextInput {
    pub text: String,
    /// Caret position as a byte offset on a char boundary.
    cursor: usize,
    /// Selection anchor (the other end of the selection is `cursor`).
    anchor: Option<usize>,
    /// The byte range of an in-progress IME composition (e.g. unconfirmed pinyin), if any.
    /// Platform text input (Chinese/Japanese/Korean IME, and on Windows even the space bar)
    /// is delivered only through GPUI's `EntityInputHandler`/`InputHandler` machinery, which
    /// this field backs; see `ime_*` below.
    marked: Option<Range<usize>>,
}

impl TextInput {
    pub fn new(text: &str) -> Self {
        Self { text: text.to_string(), cursor: text.len(), anchor: None, marked: None }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
        self.anchor = None;
        self.marked = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Selects the file name without its extension (what Explorer does when renaming).
    pub fn select_stem(&mut self) {
        let end = match self.text.rfind('.') {
            Some(i) if i > 0 => i,
            _ => self.text.len(),
        };
        self.anchor = Some(0);
        self.cursor = end;
    }

    /// A mouse press at byte offset `at`: the caret goes there and a drag from it selects.
    pub fn press_at(&mut self, at: usize) {
        self.cursor = at.min(self.text.len());
        self.anchor = Some(self.cursor);
    }

    /// The pointer moved with the button held: stretch the selection to `to`.
    pub fn drag_to(&mut self, to: usize) {
        self.cursor = to.min(self.text.len());
    }

    fn selection(&self) -> Option<(usize, usize)> {
        self.anchor.filter(|a| *a != self.cursor).map(|a| (a.min(self.cursor), a.max(self.cursor)))
    }

    fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection() {
            self.text.replace_range(a..b, "");
            self.cursor = a;
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    // ───────────────────────── platform text input (IME) ─────────────────────────
    // GPUI's Windows backend delivers *all* typed and IME-composed text exclusively through
    // the `EntityInputHandler`/`InputHandler` machinery (never through `KeyDownEvent.key_char`
    // for this), in UTF-16 code-unit offsets. `handle_key` above stays byte-based for the keys
    // it owns outright (arrows, backspace, Enter, Ctrl shortcuts, ...); the methods below are
    // the byte-based core that the UTF-16-facing glue in `main.rs`'s `EntityInputHandler` impl
    // calls into.

    /// The byte range of an in-progress IME composition (unconfirmed pinyin, etc.), if any.
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// The current selection as a byte range, regardless of which end the caret is on.
    pub fn selection_range(&self) -> Range<usize> {
        self.selection().map(|(a, b)| a..b).unwrap_or(self.cursor..self.cursor)
    }

    /// Whether the caret (`cursor`) sits at the start of the selection rather than the end.
    pub fn selection_reversed(&self) -> bool {
        self.anchor.is_some_and(|a| self.cursor < a)
    }

    /// Ends IME composition without changing the text (platform `unmarkText`).
    pub fn ime_unmark(&mut self) {
        self.marked = None;
    }

    /// `range` is `None` when the platform means "replace whatever's currently marked, or
    /// selected, or just insert at the caret" — on Windows this is the only form it ever sends
    /// for ordinary typing and IME composition (a concrete range is only used for e.g.
    /// accessibility tooling), so that priority mirrors what Windows' own text fields do.
    fn ime_target(&self, range: Option<Range<usize>>) -> Range<usize> {
        range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.selection_range())
    }

    /// Replace a range of text and commit it — no longer part of any IME composition
    /// (platform `replace_text_in_range`, e.g. a plain `WM_CHAR` or an IME composition result).
    pub fn ime_replace(&mut self, range: Option<Range<usize>>, text: &str) {
        let r = self.ime_target(range);
        let r = r.start.min(self.text.len())..r.end.min(self.text.len());
        self.text.replace_range(r.clone(), text);
        self.cursor = r.start + text.len();
        self.anchor = None;
        self.marked = None;
    }

    /// Replace a range of text and mark the result as an in-progress IME composition
    /// (platform `replace_and_mark_text_in_range`, e.g. unconfirmed pinyin).
    /// `new_selected` is a *byte* range within `text` itself, not within the whole document —
    /// the platform specifies it relative to the inserted string.
    pub fn ime_replace_and_mark(&mut self, range: Option<Range<usize>>, text: &str, new_selected: Option<Range<usize>>) {
        let r = self.ime_target(range);
        let r = r.start.min(self.text.len())..r.end.min(self.text.len());
        self.text.replace_range(r.clone(), text);
        self.marked = if text.is_empty() { None } else { Some(r.start..r.start + text.len()) };
        let (a, b) = new_selected
            .map(|s| (r.start + s.start.min(text.len()), r.start + s.end.min(text.len())))
            .unwrap_or((r.start + text.len(), r.start + text.len()));
        self.anchor = if a == b { None } else { Some(a) };
        self.cursor = b;
    }

    /// The substring for a byte range, clamped to the text's bounds and to char boundaries.
    pub fn text_in_range(&self, range: Range<usize>) -> (String, Range<usize>) {
        fn floor_boundary(s: &str, mut i: usize) -> usize {
            i = i.min(s.len());
            while i > 0 && !s.is_char_boundary(i) {
                i -= 1;
            }
            i
        }
        let start = floor_boundary(&self.text, range.start);
        let end = floor_boundary(&self.text, range.end).max(start);
        (self.text[start..end].to_string(), start..end)
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map(|(p, _)| p).unwrap_or(0)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }

    fn move_caret(&mut self, to: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = to;
    }

    /// `paste` is the clipboard text (only used for Ctrl+V).
    pub fn handle_key(&mut self, ev: &KeyDownEvent, paste: Option<String>) -> Outcome {
        let k = &ev.keystroke;
        let m = k.modifiers;
        let ctrl = m.control || m.platform;
        match k.key.as_str() {
            "enter" => return Outcome::Enter,
            "escape" => return Outcome::Escape,
            "left" => {
                let to = if !m.shift && self.selection().is_some() {
                    self.selection().map(|s| s.0).unwrap_or(0)
                } else {
                    self.prev(self.cursor)
                };
                self.move_caret(to, m.shift);
                return Outcome::None;
            }
            "right" => {
                let to = if !m.shift && self.selection().is_some() {
                    self.selection().map(|s| s.1).unwrap_or(0)
                } else {
                    self.next(self.cursor)
                };
                self.move_caret(to, m.shift);
                return Outcome::None;
            }
            "home" => {
                self.move_caret(0, m.shift);
                return Outcome::None;
            }
            "end" => {
                self.move_caret(self.text.len(), m.shift);
                return Outcome::None;
            }
            "backspace" => {
                if !self.delete_selection() && self.cursor > 0 {
                    let p = self.prev(self.cursor);
                    self.text.replace_range(p..self.cursor, "");
                    self.cursor = p;
                }
                return Outcome::Changed;
            }
            "delete" => {
                if !self.delete_selection() && self.cursor < self.text.len() {
                    let n = self.next(self.cursor);
                    self.text.replace_range(self.cursor..n, "");
                }
                return Outcome::Changed;
            }
            "a" if ctrl => {
                self.select_all();
                return Outcome::None;
            }
            "c" if ctrl => {
                return match self.selection() {
                    Some((a, b)) => Outcome::Copy(self.text[a..b].to_string()),
                    None => Outcome::None,
                };
            }
            "x" if ctrl => {
                return match self.selection() {
                    Some((a, b)) => {
                        let s = self.text[a..b].to_string();
                        self.delete_selection();
                        Outcome::Copy(s)
                    }
                    None => Outcome::None,
                };
            }
            "v" if ctrl => {
                if let Some(p) = paste {
                    let p: String = p.chars().filter(|c| !c.is_control()).collect();
                    self.insert(&p);
                    return Outcome::Changed;
                }
                return Outcome::None;
            }
            _ => {}
        }
        if ctrl || m.alt {
            // Consumed (there's nothing for an unrecognised Ctrl/Alt combo to do here), but
            // still ours to swallow so it doesn't leak out as e.g. a list shortcut.
            return Outcome::None;
        }
        // Anything else — plain printable keys, space included — is left to the platform: it
        // arrives as `WM_CHAR` or an IME composition update through the registered input
        // handler instead (see `EntityInputHandler` in main.rs), which is the only path that
        // supports IME composition and, on Windows, the space bar (GPUI never gives it a
        // `key_char` there). The caller must not call `cx.stop_propagation()` for this outcome.
        Outcome::PassThrough
    }

    /// Text with selection highlight and caret. The caller supplies the box around it.
    pub fn render(&self, focused: bool, caret_on: bool, color: u32) -> Div {
        let caret = div().flex_none().w(px(1.)).h(px(15.)).bg(rgb(if focused && caret_on { color } else { 0xFFFFFF }));
        let mut row = div().flex().flex_row().items_center().overflow_hidden().whitespace_nowrap().text_color(rgb(color));
        if let Some((a, b)) = self.selection() {
            row = row
                .child(div().child(self.text[..a].to_string()))
                .child(div().bg(rgb(0x0078D4)).text_color(rgb(0xFFFFFF)).child(self.text[a..b].to_string()))
                .child(div().child(self.text[b..].to_string()));
        } else if let Some(m) = self.marked.clone() {
            // An in-progress IME composition (unconfirmed pinyin, etc.): underline it instead
            // of highlighting it, with the caret at its usual spot inside or at the edge.
            let split = self.cursor.clamp(m.start, m.end);
            let underline = |s: String| div().border_b_2().border_color(rgb(color)).child(s);
            let caret = if focused { caret } else { div().flex_none().w(px(0.)) };
            row = row
                .child(div().child(self.text[..m.start].to_string()))
                .child(underline(self.text[m.start..split].to_string()))
                .child(caret)
                .child(underline(self.text[split..m.end].to_string()))
                .child(div().child(self.text[m.end..].to_string()));
        } else {
            let caret = if focused { caret } else { div().flex_none().w(px(0.)) };
            row = row
                .child(div().child(self.text[..self.cursor].to_string()))
                .child(caret)
                .child(div().child(self.text[self.cursor..].to_string()));
        }
        row
    }
}
