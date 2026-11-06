//! [`SearchPrompt`] — the editable buffer behind the Cmd+F search prompt
//! overlay. Pure state, no winit/window types: `App`'s `KeyboardInput`
//! handler drives it from keypress-derived text and applies the returned
//! [`SearchPromptEffect`] to the terminal (`Terminal::set_search_query` /
//! `Terminal::clear_search`), then feeds `buffer()` into
//! `FrameSnapshot::search_prompt` for the renderer overlay.

use unicode_segmentation::UnicodeSegmentation;

/// What the caller should do to the terminal after a [`SearchPrompt`] edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchPromptEffect {
    /// Re-run the search with the buffer's new (non-empty) contents.
    UpdateQuery(String),
    /// The buffer is now empty: clear the search entirely rather than
    /// querying for `""` (which would report zero matches instead of no
    /// search at all).
    ClearQuery,
}

/// A caret movement within the prompt's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretMotion {
    Left,
    Right,
    Start,
    End,
}

/// A deletion relative to the prompt's caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deletion {
    Backward,
    Forward,
    ToStart,
    ToEnd,
    WordBackward,
}

/// The open search prompt's text buffer: a single-line field edited at its
/// caret. The only selection is all of it (`replace_pending`), as a field
/// shows text it was handed from elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPrompt {
    buffer: String,
    /// Byte offset of the caret in `buffer`, always on a char boundary.
    caret: usize,
    /// The buffer came from the find pasteboard and is wholly selected: the
    /// next typed text replaces it, and Backspace clears it.
    replace_pending: bool,
}

impl SearchPrompt {
    /// Open a prompt pre-filled with `initial_query` (the terminal's
    /// current search query, so re-opening the prompt resumes where the
    /// last search left off).
    pub fn open(initial_query: String) -> Self {
        SearchPrompt {
            caret: initial_query.len(),
            buffer: initial_query,
            replace_pending: false,
        }
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// The buffer's text before and after the caret.
    pub fn split_at_caret(&self) -> (&str, &str) {
        self.buffer.split_at(self.caret)
    }

    /// Replace the buffer with `text` from the find pasteboard, wholly
    /// selected (Ghostty's `setNeedle(_, selectAll: true)`). `None` when it
    /// already holds `text`.
    pub fn replace_selected(&mut self, text: &str) -> Option<SearchPromptEffect> {
        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
        if filtered == self.buffer {
            return None;
        }
        self.buffer = filtered;
        self.caret = self.buffer.len();
        self.replace_pending = !self.buffer.is_empty();
        Some(self.query_effect())
    }

    fn query_effect(&self) -> SearchPromptEffect {
        if self.buffer.is_empty() {
            SearchPromptEffect::ClearQuery
        } else {
            SearchPromptEffect::UpdateQuery(self.buffer.clone())
        }
    }

    /// Insert `text` at the caret (a keypress's resolved text, or a committed
    /// IME composition). Control characters (e.g. a stray `\r`/`\t` riding
    /// along in `text`) are dropped rather than appended. Returns `None`
    /// when nothing was appended (`text` was empty or all-control), so the
    /// caller can skip re-querying.
    pub fn push_text(&mut self, text: &str) -> Option<SearchPromptEffect> {
        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
        if filtered.is_empty() {
            return None;
        }
        self.clear_if_selected();
        self.buffer.insert_str(self.caret, &filtered);
        self.caret += filtered.len();
        Some(SearchPromptEffect::UpdateQuery(self.buffer.clone()))
    }

    /// Delete the character before the caret (Backspace). `None` when there
    /// is none: re-running the same search would pull a scrolled viewport back
    /// to the active match and drop navigation still queued for the worker.
    pub fn backspace(&mut self) -> Option<SearchPromptEffect> {
        if !self.clear_if_selected() {
            let (start, _) = self.buffer[..self.caret].char_indices().next_back()?;
            self.remove(start..self.caret);
        }
        Some(self.query_effect())
    }

    /// Apply `deletion`. `None` when it removed nothing, so the caller can
    /// skip re-querying. A wholly selected buffer is cleared by any of them.
    pub fn delete(&mut self, deletion: Deletion) -> Option<SearchPromptEffect> {
        if deletion == Deletion::Backward {
            return self.backspace();
        }
        if self.clear_if_selected() {
            return Some(self.query_effect());
        }
        let range = match deletion {
            Deletion::Backward => unreachable!("handled above"),
            Deletion::Forward => self.caret..self.next_boundary(),
            Deletion::ToStart => 0..self.caret,
            Deletion::ToEnd => self.caret..self.buffer.len(),
            Deletion::WordBackward => self.previous_word_start()..self.caret,
        };
        if range.is_empty() {
            return None;
        }
        self.remove(range);
        Some(self.query_effect())
    }

    /// Move the caret, by whole grapheme clusters so it never lands inside
    /// one. A selection collapses to the edge it moved toward. Returns whether
    /// anything visible changed.
    pub fn move_caret(&mut self, motion: CaretMotion) -> bool {
        let previous = self.caret;
        let was_selected = std::mem::take(&mut self.replace_pending);
        self.caret = match motion {
            CaretMotion::Left if !was_selected => self.previous_boundary(),
            CaretMotion::Right if !was_selected => self.next_boundary(),
            CaretMotion::Left | CaretMotion::Start => 0,
            CaretMotion::Right | CaretMotion::End => self.buffer.len(),
        };
        was_selected || self.caret != previous
    }

    fn clear_if_selected(&mut self) -> bool {
        let selected = std::mem::take(&mut self.replace_pending);
        if selected {
            self.buffer.clear();
            self.caret = 0;
        }
        selected
    }

    fn remove(&mut self, range: std::ops::Range<usize>) {
        self.caret = range.start;
        self.buffer.replace_range(range, "");
    }

    fn previous_boundary(&self) -> usize {
        self.buffer[..self.caret]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(start, _)| start)
    }

    fn next_boundary(&self) -> usize {
        self.buffer[self.caret..]
            .graphemes(true)
            .next()
            .map_or(self.caret, |grapheme| self.caret + grapheme.len())
    }

    /// Start of the word before the caret: trailing whitespace, then the run
    /// of non-whitespace before it, as readline's `Ctrl+W` removes.
    fn previous_word_start(&self) -> usize {
        let before = self.buffer[..self.caret].trim_end();
        before
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map_or(0, |(start, c)| start + c.len_utf8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_buffer_is_selected_until_the_next_edit() {
        let mut prompt = SearchPrompt::open(String::new());
        assert_eq!(
            prompt.replace_selected("needle"),
            Some(SearchPromptEffect::UpdateQuery("needle".to_string()))
        );
        assert_eq!(prompt.replace_selected("needle"), None);
        // Typing replaces the selected text, then appends as usual.
        assert_eq!(
            prompt.push_text("a"),
            Some(SearchPromptEffect::UpdateQuery("a".to_string()))
        );
        assert_eq!(
            prompt.push_text("b"),
            Some(SearchPromptEffect::UpdateQuery("ab".to_string()))
        );

        // Backspace clears the selected text outright.
        prompt.replace_selected("haystack");
        assert_eq!(prompt.backspace(), Some(SearchPromptEffect::ClearQuery));
        assert_eq!(prompt.buffer(), "");
    }

    #[test]
    fn open_prefills_the_buffer_with_the_current_query() {
        let prompt = SearchPrompt::open("needle".to_string());
        assert_eq!(prompt.buffer(), "needle");
    }

    #[test]
    fn push_text_appends_and_reports_the_updated_query() {
        let mut prompt = SearchPrompt::open(String::new());

        assert_eq!(
            prompt.push_text("f"),
            Some(SearchPromptEffect::UpdateQuery("f".to_string()))
        );
        assert_eq!(
            prompt.push_text("oo"),
            Some(SearchPromptEffect::UpdateQuery("foo".to_string()))
        );
        assert_eq!(prompt.buffer(), "foo");
    }

    #[test]
    fn push_text_drops_control_characters_and_reports_none_when_nothing_lands() {
        let mut prompt = SearchPrompt::open(String::new());

        assert_eq!(prompt.push_text("\r"), None);
        assert_eq!(prompt.buffer(), "");

        assert_eq!(
            prompt.push_text("a\rb"),
            Some(SearchPromptEffect::UpdateQuery("ab".to_string())),
            "control characters riding along with real text are dropped, not appended"
        );
    }

    #[test]
    fn backspace_pops_one_char_and_clears_on_the_last_one() {
        let mut prompt = SearchPrompt::open("ab".to_string());

        assert_eq!(
            prompt.backspace(),
            Some(SearchPromptEffect::UpdateQuery("a".to_string()))
        );
        assert_eq!(prompt.backspace(), Some(SearchPromptEffect::ClearQuery));
        assert_eq!(prompt.buffer(), "");
    }

    #[test]
    fn typing_and_backspace_act_at_the_caret() {
        let mut prompt = prompt_with_caret("ab", "cd");

        assert_eq!(
            prompt.push_text("X"),
            Some(SearchPromptEffect::UpdateQuery("abXcd".to_string()))
        );
        assert_eq!(prompt.split_at_caret(), ("abX", "cd"));
        assert_eq!(
            prompt.backspace(),
            Some(SearchPromptEffect::UpdateQuery("abcd".to_string()))
        );
    }

    #[test]
    fn backspace_with_nothing_before_the_caret_skips_the_requery() {
        let mut prompt = prompt_with_caret("", "ab");
        assert_eq!(prompt.backspace(), None);
        assert_eq!(prompt.delete(Deletion::Backward), None);
        assert_eq!(prompt.buffer(), "ab");

        let mut empty = SearchPrompt::open(String::new());
        assert_eq!(empty.backspace(), None);
    }

    fn prompt_with_caret(before: &str, after: &str) -> SearchPrompt {
        let mut prompt = SearchPrompt::open(format!("{before}{after}"));
        prompt.caret = before.len();
        prompt
    }

    #[test]
    fn the_caret_moves_by_grapheme_and_stops_at_the_edges() {
        let mut prompt = SearchPrompt::open("e\u{301}日".to_string());

        assert!(prompt.move_caret(CaretMotion::Left));
        assert_eq!(prompt.split_at_caret(), ("e\u{301}", "日"));
        assert!(prompt.move_caret(CaretMotion::Left));
        assert_eq!(prompt.split_at_caret(), ("", "e\u{301}日"));
        assert!(!prompt.move_caret(CaretMotion::Left));
        assert!(prompt.move_caret(CaretMotion::Right));
        assert_eq!(prompt.split_at_caret(), ("e\u{301}", "日"));
        assert!(prompt.move_caret(CaretMotion::End));
        assert!(!prompt.move_caret(CaretMotion::Right));
        assert!(prompt.move_caret(CaretMotion::Start));
        assert!(!prompt.move_caret(CaretMotion::Start));
    }

    #[test]
    fn moving_collapses_a_selection_toward_the_motion() {
        for (motion, expected) in [
            (CaretMotion::Left, ("", "needle")),
            (CaretMotion::Start, ("", "needle")),
            (CaretMotion::Right, ("needle", "")),
            (CaretMotion::End, ("needle", "")),
        ] {
            let mut prompt = SearchPrompt::open(String::new());
            prompt.replace_selected("needle");
            assert!(prompt.move_caret(motion), "{motion:?}");
            assert_eq!(prompt.split_at_caret(), expected, "{motion:?}");
            // No longer selected: typing inserts instead of replacing.
            assert_eq!(
                prompt.push_text("x"),
                Some(SearchPromptEffect::UpdateQuery(format!(
                    "{}x{}",
                    expected.0, expected.1
                ))),
                "{motion:?}"
            );
        }
    }

    #[test]
    fn each_deletion_removes_its_range_around_the_caret() {
        for (deletion, before, after, want_before, want_after) in [
            (Deletion::Backward, "ab", "cd", "a", "cd"),
            (Deletion::Forward, "ab", "cd", "ab", "d"),
            (Deletion::Forward, "ab", "e\u{301}d", "ab", "d"),
            (Deletion::ToStart, "ab", "cd", "", "cd"),
            (Deletion::ToEnd, "ab", "cd", "ab", ""),
            (Deletion::WordBackward, "foo bar", " baz", "foo ", " baz"),
            (Deletion::WordBackward, "foo bar  ", "baz", "foo ", "baz"),
            (Deletion::WordBackward, "  ", "baz", "", "baz"),
            (Deletion::WordBackward, "日本語", "x", "", "x"),
        ] {
            let mut prompt = prompt_with_caret(before, after);
            let remaining = format!("{want_before}{want_after}");
            assert_eq!(
                prompt.delete(deletion),
                Some(SearchPromptEffect::UpdateQuery(remaining)),
                "{deletion:?} on {before:?}|{after:?}"
            );
            assert_eq!(
                prompt.split_at_caret(),
                (want_before, want_after),
                "{deletion:?} on {before:?}|{after:?}"
            );
        }
    }

    #[test]
    fn a_deletion_with_nothing_to_remove_skips_the_requery() {
        for (deletion, before, after) in [
            (Deletion::Forward, "ab", ""),
            (Deletion::ToStart, "", "ab"),
            (Deletion::ToEnd, "ab", ""),
            (Deletion::WordBackward, "", "ab"),
        ] {
            let mut prompt = prompt_with_caret(before, after);
            assert_eq!(prompt.delete(deletion), None, "{deletion:?}");
            assert_eq!(prompt.buffer(), "ab", "{deletion:?}");
        }
    }

    #[test]
    fn deleting_the_last_text_clears_the_query() {
        let mut prompt = prompt_with_caret("", "ab");
        assert_eq!(
            prompt.delete(Deletion::ToEnd),
            Some(SearchPromptEffect::ClearQuery)
        );
    }

    #[test]
    fn any_deletion_clears_a_selected_buffer() {
        for deletion in [
            Deletion::Backward,
            Deletion::Forward,
            Deletion::ToStart,
            Deletion::ToEnd,
            Deletion::WordBackward,
        ] {
            let mut prompt = SearchPrompt::open(String::new());
            prompt.replace_selected("needle");
            assert_eq!(
                prompt.delete(deletion),
                Some(SearchPromptEffect::ClearQuery),
                "{deletion:?}"
            );
            assert_eq!(prompt.split_at_caret(), ("", ""), "{deletion:?}");
        }
    }
}
