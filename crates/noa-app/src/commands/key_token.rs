use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyTrigger {
    mods: TriggerMods,
    key: KeyToken,
}

impl KeyTrigger {
    pub(super) fn parse(input: &str) -> Result<Self, KeybindParseError> {
        let mut mods = TriggerMods::default();
        let mut key = None;
        for token in input
            .split('+')
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
        {
            let normalized = token.to_ascii_lowercase();
            match normalized.as_str() {
                "cmd" | "command" | "super" | "meta" => mods.super_key = true,
                "ctrl" | "control" => mods.control = true,
                "alt" | "option" => mods.alt = true,
                "shift" => mods.shift = true,
                _ => {
                    if key.is_some() {
                        return Err(KeybindParseError::MultipleKeys);
                    }
                    key = Some(KeyToken::parse(&normalized)?);
                }
            }
        }
        let Some(key) = key else {
            return Err(KeybindParseError::MissingKey);
        };
        Ok(Self { mods, key })
    }

    pub(super) fn matches(&self, logical_key: &Key, mods: ModifiersState) -> bool {
        self.mods.matches(mods) && self.key.matches(logical_key)
    }
}

impl std::fmt::Display for KeyTrigger {
    /// Renders the config-style chord text (`cmd+ctrl+alt+shift+key`), in the
    /// same modifier order the parser accepts, so the output round-trips back
    /// through [`KeyTrigger::parse`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.mods.super_key {
            f.write_str("cmd+")?;
        }
        if self.mods.control {
            f.write_str("ctrl+")?;
        }
        if self.mods.alt {
            f.write_str("alt+")?;
        }
        if self.mods.shift {
            f.write_str("shift+")?;
        }
        write!(f, "{}", self.key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TriggerMods {
    shift: bool,
    control: bool,
    alt: bool,
    super_key: bool,
}

impl TriggerMods {
    fn matches(self, mods: ModifiersState) -> bool {
        self.shift == mods.shift_key()
            && self.control == mods.control_key()
            && self.alt == mods.alt_key()
            && self.super_key == mods.super_key()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyToken {
    Character(char),
    Named(NamedKeyToken),
}

impl KeyToken {
    fn parse(token: &str) -> Result<Self, KeybindParseError> {
        if token == "plus" {
            return Ok(Self::Character('+'));
        }
        let mut chars = token.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            return Ok(Self::Character(ch));
        }
        Ok(Self::Named(match token {
            "arrowup" | "up" => NamedKeyToken::ArrowUp,
            "arrowdown" | "down" => NamedKeyToken::ArrowDown,
            "arrowleft" | "left" => NamedKeyToken::ArrowLeft,
            "arrowright" | "right" => NamedKeyToken::ArrowRight,
            "pageup" => NamedKeyToken::PageUp,
            "pagedown" => NamedKeyToken::PageDown,
            "home" => NamedKeyToken::Home,
            "end" => NamedKeyToken::End,
            "enter" | "return" => NamedKeyToken::Enter,
            _ => return Err(KeybindParseError::UnknownKey(token.to_string())),
        }))
    }

    fn matches(self, logical_key: &Key) -> bool {
        match (self, logical_key) {
            (Self::Character(expected), Key::Character(actual)) => {
                actual.chars().next().is_some_and(|actual| {
                    actual.eq_ignore_ascii_case(&expected)
                        || (expected == '[' && actual == '{')
                        || (expected == ']' && actual == '}')
                })
            }
            (Self::Named(expected), Key::Named(actual)) => expected.matches(*actual),
            _ => false,
        }
    }
}

impl std::fmt::Display for KeyToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // `+` parses from the `plus` alias, so render it back that way
            // (a bare `+` would read as a separator on re-parse).
            Self::Character('+') => f.write_str("plus"),
            Self::Character(ch) => write!(f, "{ch}"),
            Self::Named(named) => f.write_str(named.as_str()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedKeyToken {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
}

impl NamedKeyToken {
    /// The canonical chord token for this key, matching a name
    /// [`KeyToken::parse`] accepts (so [`KeyTrigger`]'s `Display` round-trips).
    fn as_str(self) -> &'static str {
        match self {
            Self::ArrowUp => "arrowup",
            Self::ArrowDown => "arrowdown",
            Self::ArrowLeft => "arrowleft",
            Self::ArrowRight => "arrowright",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Home => "home",
            Self::End => "end",
            Self::Enter => "enter",
        }
    }

    fn matches(self, key: NamedKey) -> bool {
        matches!(
            (self, key),
            (Self::ArrowUp, NamedKey::ArrowUp)
                | (Self::ArrowDown, NamedKey::ArrowDown)
                | (Self::ArrowLeft, NamedKey::ArrowLeft)
                | (Self::ArrowRight, NamedKey::ArrowRight)
                | (Self::PageUp, NamedKey::PageUp)
                | (Self::PageDown, NamedKey::PageDown)
                | (Self::Home, NamedKey::Home)
                | (Self::End, NamedKey::End)
                | (Self::Enter, NamedKey::Enter)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeybindParseError {
    MissingKey,
    MultipleKeys,
    UnknownKey(String),
}

impl std::fmt::Display for KeybindParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingKey => f.write_str("keybind is missing a key"),
            Self::MultipleKeys => f.write_str("keybind contains multiple keys"),
            Self::UnknownKey(key) => write!(f, "unknown key in keybind: {key}"),
        }
    }
}

impl std::error::Error for KeybindParseError {}
