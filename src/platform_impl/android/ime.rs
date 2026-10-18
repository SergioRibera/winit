//! Turning `GameTextInput` buffer snapshots into IME edits.
//!
//! `GameTextInput` reports the whole text field after every edit, while
//! winit's IME events are deltas relative to the caret: characters to
//! delete (sent as Backspace / Delete key presses) and text to commit. The [`ImeMirror`]
//! keeps the last snapshot to diff the next one against.

/// Last-observed soft-keyboard buffer and caret.
#[derive(Debug, Default)]
pub(crate) struct ImeMirror {
    text: String,
    /// Caret, in bytes into `text`.
    cursor: usize,
    /// Other end of the selection, in bytes into `text`.
    anchor: usize,
}

/// The delta between two snapshots, as winit IME events.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ImeEdit {
    /// Characters to delete before the caret.
    pub before: usize,
    /// Characters to delete after the caret.
    pub after: usize,
    /// Text to commit at the caret afterwards.
    pub commit: String,
}

impl ImeMirror {
    /// Forget the buffer, e.g. when the IME is enabled for a new field.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = 0;
    }

    /// Adopt `text` with the selection from byte `anchor` to byte `cursor`,
    /// as the application describes the field. Returns the caret and the
    /// anchor as UTF-16 offsets, to load the same state into
    /// `GameTextInput`, or `None` when the keyboard already has it: the
    /// application echoing back an edit the keyboard just made. Loading it
    /// again would restart the input connection and lose the composing word.
    pub fn load(&mut self, text: &str, cursor: usize, anchor: usize) -> Option<(usize, usize)> {
        let (cursor, anchor) = (cursor.min(text.len()), anchor.min(text.len()));
        if self.text == text && self.cursor == cursor && self.anchor == anchor {
            return None;
        }
        text.clone_into(&mut self.text);
        self.cursor = cursor;
        self.anchor = anchor;
        Some((utf16_offset(text, cursor), utf16_offset(text, anchor)))
    }

    /// Record the snapshot `text`, with the selection from `anchor_utf16` to
    /// `cursor_utf16` (UTF-16 code units, as `GameTextInput` counts), and
    /// return the edit that produced it.
    pub fn update(
        &mut self,
        text: &str,
        anchor_utf16: usize,
        cursor_utf16: usize,
    ) -> Option<ImeEdit> {
        self.anchor = byte_offset(text, anchor_utf16);
        let cursor = byte_offset(text, cursor_utf16);
        let old_cursor = std::mem::replace(&mut self.cursor, cursor);
        if self.text == text {
            return None;
        }
        let edit = edit(&self.text, text, old_cursor);
        text.clone_into(&mut self.text);
        Some(edit)
    }
}

/// The edit turning `old` into `new` when the caret sat at byte `cursor`
/// of `old`. The changed span lies between the common prefix and the
/// common suffix of both texts. It is widened to reach the caret, since
/// the deltas are applied there: a keyboard correcting the word behind
/// the caret deletes back to the start of the word and commits it again.
fn edit(old: &str, new: &str, cursor: usize) -> ImeEdit {
    // Char-wise: byte-wise would split multi-byte UTF-8.
    let prefix: usize =
        old.chars().zip(new.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum();
    let suffix: usize = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    let cursor = cursor.min(old.len());
    let start = prefix.min(cursor);
    let end = (old.len() - suffix).max(cursor);
    // Past the changed span both texts match, so `old[end..]` is the tail
    // of `new` too.
    let commit = new[start..new.len() - (old.len() - end)].to_owned();
    ImeEdit {
        before: old[start..cursor].chars().count(),
        after: old[cursor..end].chars().count(),
        commit,
    }
}

/// UTF-16 offset of byte `byte` in `text`, clamped to it.
fn utf16_offset(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|(at, _)| *at < byte).map(|(_, ch)| ch.len_utf16()).sum()
}

/// Byte offset of the UTF-16 offset `units` in `text`, clamped to it.
fn byte_offset(text: &str, units: usize) -> usize {
    let mut count = 0;
    for (byte, ch) in text.char_indices() {
        if count >= units {
            return byte;
        }
        count += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(mirror: &mut ImeMirror, text: &str) -> Option<ImeEdit> {
        let caret = text.encode_utf16().count();
        mirror.update(text, caret, caret)
    }

    #[test]
    fn typing_at_the_end_commits_the_new_text() {
        let mut mirror = ImeMirror::default();
        assert_eq!(
            typed(&mut mirror, "ho"),
            Some(ImeEdit { before: 0, after: 0, commit: "ho".into() })
        );
        assert_eq!(
            typed(&mut mirror, "hol"),
            Some(ImeEdit { before: 0, after: 0, commit: "l".into() })
        );
        assert_eq!(
            typed(&mut mirror, "ho"),
            Some(ImeEdit { before: 1, after: 0, commit: String::new() })
        );
    }

    #[test]
    fn edits_in_the_middle_only_touch_the_changed_span() {
        let mut mirror = ImeMirror::default();
        typed(&mut mirror, "hello world");
        // Caret moved after "hello" (e.g. space-bar cursor drag).
        assert_eq!(mirror.update("hello world", 5, 5), None);
        assert_eq!(
            mirror.update("hellox world", 6, 6),
            Some(ImeEdit { before: 0, after: 0, commit: "x".into() })
        );
    }

    #[test]
    fn a_correction_replaces_the_word_behind_the_caret() {
        let mut mirror = ImeMirror::default();
        typed(&mut mirror, "helo ");
        assert_eq!(
            typed(&mut mirror, "hello "),
            Some(ImeEdit { before: 2, after: 0, commit: "lo ".into() })
        );
        typed(&mut mirror, "hello wrld");
        assert_eq!(
            typed(&mut mirror, "hello world"),
            Some(ImeEdit { before: 3, after: 0, commit: "orld".into() })
        );
    }

    #[test]
    fn loaded_text_is_the_new_baseline() {
        let mut mirror = ImeMirror::default();
        assert_eq!(mirror.load("día uno", 5, 5), Some((4, 4)));
        assert_eq!(mirror.load("día uno", 5, 5), None, "already loaded");
        // The keyboard types after "día " (5 bytes, 4 UTF-16 units).
        assert_eq!(
            mirror.update("día xuno", 5, 5),
            Some(ImeEdit { before: 0, after: 0, commit: "x".into() })
        );
        // The application echoes the edit back: nothing to reload.
        assert_eq!(mirror.load("día xuno", 6, 6), None);
    }

    #[test]
    fn multi_byte_text_is_split_on_char_boundaries() {
        let mut mirror = ImeMirror::default();
        typed(&mut mirror, "año");
        assert_eq!(
            typed(&mut mirror, "añ"),
            Some(ImeEdit { before: 1, after: 0, commit: String::new() })
        );
        assert_eq!(
            typed(&mut mirror, "a😀"),
            Some(ImeEdit { before: 1, after: 0, commit: "😀".into() })
        );
    }
}
