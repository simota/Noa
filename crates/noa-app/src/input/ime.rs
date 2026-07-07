use winit::event::Ime;

use super::text::encode_text;

/// Tracks IME composition state and encodes committed IME text for the pty.
/// The composing (pre-edit) string is retained so the renderer can draw it
/// inline at the cursor, updating live as the user types; `preedit_cursor` is
/// the byte range within `preedit` winit reports for the composition caret.
#[derive(Debug, Default)]
pub struct ImeState {
    preedit: String,
    preedit_cursor: Option<(usize, usize)>,
    pending_commit_echo: Option<String>,
}

impl ImeState {
    pub fn handle_event(&mut self, event: &Ime) -> Option<Vec<u8>> {
        match event {
            Ime::Enabled | Ime::Disabled => {
                self.clear_preedit();
                self.pending_commit_echo = None;
                None
            }
            Ime::Preedit(text, cursor_range) => {
                text.clone_into(&mut self.preedit);
                self.preedit_cursor = *cursor_range;
                // macOS sends an empty `Preedit` right after `Commit` to clear
                // the marked text; the commit's `KeyboardInput.text` echo
                // arrives after it, so only a new composition may drop the
                // pending echo guard.
                if !text.is_empty() {
                    self.pending_commit_echo = None;
                }
                None
            }
            Ime::Commit(text) => {
                self.clear_preedit();
                self.pending_commit_echo = (!text.is_empty()).then(|| text.clone());
                encode_text(text)
            }
        }
    }

    pub fn preedit_active(&self) -> bool {
        !self.preedit.is_empty()
    }

    /// The current composing string (empty when not composing).
    pub fn preedit_text(&self) -> &str {
        &self.preedit
    }

    /// The composition caret's byte range within [`Self::preedit_text`], if
    /// winit reported one.
    pub fn preedit_cursor(&self) -> Option<(usize, usize)> {
        self.preedit_cursor
    }

    pub fn commit_preedit(&mut self) {
        self.clear_preedit();
    }

    /// Consume a `KeyboardInput.text` echo that some platform IME paths emit
    /// immediately after `Ime::Commit`. `Ime::Commit` is already the source of
    /// truth for committed composition text; sending the matching key text as
    /// well would double-insert it into the pty.
    pub fn consume_commit_echo(&mut self, text: Option<&str>) -> bool {
        let Some(text) = text else {
            return false;
        };
        let Some(expected) = self.pending_commit_echo.take() else {
            return false;
        };
        text == expected
    }

    fn clear_preedit(&mut self) {
        self.preedit.clear();
        self.preedit_cursor = None;
    }
}
