//! [`SearchPrompt`] — the editable buffer behind the Cmd+F search prompt
//! overlay. Pure state, no winit/window types: `App`'s `KeyboardInput`
//! handler drives it from keypress-derived text and applies the returned
//! [`SearchPromptEffect`] to the terminal (`Terminal::set_search_query` /
//! `Terminal::clear_search`), then feeds `buffer()` into
//! `FrameSnapshot::search_prompt` for the renderer overlay.

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

/// The open search prompt's text buffer. Holds no cursor/selection state of
/// its own — editing always happens at the end of the buffer, matching a
/// single-line "type to search" field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPrompt {
    buffer: String,
}

impl SearchPrompt {
    /// Open a prompt pre-filled with `initial_query` (the terminal's
    /// current search query, so re-opening the prompt resumes where the
    /// last search left off).
    pub fn open(initial_query: String) -> Self {
        SearchPrompt {
            buffer: initial_query,
        }
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Append `text` (a keypress's resolved text, or a committed IME
    /// composition). Control characters (e.g. a stray `\r`/`\t` riding
    /// along in `text`) are dropped rather than appended. Returns `None`
    /// when nothing was appended (`text` was empty or all-control), so the
    /// caller can skip re-querying.
    pub fn push_text(&mut self, text: &str) -> Option<SearchPromptEffect> {
        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
        if filtered.is_empty() {
            return None;
        }
        self.buffer.push_str(&filtered);
        Some(SearchPromptEffect::UpdateQuery(self.buffer.clone()))
    }

    /// Pop one character (Backspace). A no-op pop (buffer already empty)
    /// still yields `ClearQuery` — harmless since the terminal search is
    /// already clear, and it keeps the caller's dispatch unconditional.
    pub fn backspace(&mut self) -> SearchPromptEffect {
        self.buffer.pop();
        if self.buffer.is_empty() {
            SearchPromptEffect::ClearQuery
        } else {
            SearchPromptEffect::UpdateQuery(self.buffer.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            SearchPromptEffect::UpdateQuery("a".to_string())
        );
        assert_eq!(prompt.backspace(), SearchPromptEffect::ClearQuery);
        assert_eq!(prompt.buffer(), "");
    }

    #[test]
    fn backspace_on_an_empty_buffer_stays_a_clear_query_noop() {
        let mut prompt = SearchPrompt::open(String::new());
        assert_eq!(prompt.backspace(), SearchPromptEffect::ClearQuery);
        assert_eq!(prompt.buffer(), "");
    }
}
