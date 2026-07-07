//! Session persistence: the window/tab/split topology and per-pane cwd that
//! `window-save-state` saves on exit and restores on launch (Ghostty parity —
//! topology + cwd only, never terminal contents).
//!
//! The schema is a small, versioned JSON document. `noa-config` has no serde
//! (its config parser is hand-written too), so this module carries a minimal
//! self-contained JSON value model, serializer, and parser rather than pulling
//! in a dependency. A missing, malformed, or version-mismatched file parses to
//! `None` so a bad file can never block startup — the app just starts fresh.

use std::fs;
use std::path::Path;

/// Schema version. Bump on any incompatible shape change; an older/newer
/// `version` field makes [`parse`] return `None` (fall back to a fresh start).
pub const SESSION_VERSION: u32 = 1;

/// The whole persisted session: every logical window, plus which one had OS
/// focus at save time.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionState {
    pub windows: Vec<WindowSession>,
    /// Index into `windows` of the focused window, if any.
    pub focused_window: Option<usize>,
}

/// One logical window (an AppKit tab group): its on-screen frame and the tabs
/// it contains.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSession {
    /// Logical (scale-independent) window frame. `None` when the position was
    /// unavailable at save time; the size is still applied, the position left
    /// to the window manager.
    pub frame: Option<WindowFrame>,
    /// Index into `tabs` of the active tab.
    pub focused_tab: usize,
    pub tabs: Vec<TabSession>,
}

/// A logical-pixel window frame (position optional, size always present).
#[derive(Debug, Clone, PartialEq)]
pub struct WindowFrame {
    pub position: Option<(f64, f64)>,
    pub width: f64,
    pub height: f64,
}

/// One native tab: its split topology and which leaf pane was focused.
#[derive(Debug, Clone, PartialEq)]
pub struct TabSession {
    /// Index (in leaf pre-order, matching [`PaneNode`] traversal) of the
    /// focused pane.
    pub focused_leaf: usize,
    /// The user-set tab title override (tab-title REQ-TTL-10), if any.
    /// Optional in the schema: files predating the field parse to `None`.
    pub title: Option<String>,
    pub split: PaneNode,
}

/// The split topology of one tab: a recursive binary tree whose leaves each
/// carry an optional cwd.
#[derive(Debug, Clone, PartialEq)]
pub enum PaneNode {
    Leaf {
        cwd: Option<String>,
    },
    Split {
        orientation: Orientation,
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

/// Split axis, mirroring `split_tree::SplitOrientation` without depending on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl PaneNode {
    /// The cwd of the left-most (first) leaf — the pane that keeps hosting the
    /// tab's initial surface when the topology is rebuilt on restore.
    pub fn first_leaf_cwd(&self) -> Option<String> {
        match self {
            PaneNode::Leaf { cwd } => cwd.clone(),
            PaneNode::Split { first, .. } => first.first_leaf_cwd(),
        }
    }

    /// Number of leaf panes in this subtree.
    pub fn leaf_count(&self) -> usize {
        match self {
            PaneNode::Leaf { .. } => 1,
            PaneNode::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }
}

/// Serialize a session to the on-disk JSON string.
pub fn serialize(state: &SessionState) -> String {
    let mut out = String::new();
    out.push('{');
    push_key(&mut out, "version");
    out.push_str(&SESSION_VERSION.to_string());
    out.push(',');
    push_key(&mut out, "focused_window");
    match state.focused_window {
        Some(index) => out.push_str(&index.to_string()),
        None => out.push_str("null"),
    }
    out.push(',');
    push_key(&mut out, "windows");
    push_array(&mut out, &state.windows, serialize_window);
    out.push('}');
    out
}

fn serialize_window(out: &mut String, window: &WindowSession) {
    out.push('{');
    push_key(out, "frame");
    match &window.frame {
        Some(frame) => serialize_frame(out, frame),
        None => out.push_str("null"),
    }
    out.push(',');
    push_key(out, "focused_tab");
    out.push_str(&window.focused_tab.to_string());
    out.push(',');
    push_key(out, "tabs");
    push_array(out, &window.tabs, serialize_tab);
    out.push('}');
}

fn serialize_frame(out: &mut String, frame: &WindowFrame) {
    out.push('{');
    push_key(out, "x");
    push_opt_num(out, frame.position.map(|(x, _)| x));
    out.push(',');
    push_key(out, "y");
    push_opt_num(out, frame.position.map(|(_, y)| y));
    out.push(',');
    push_key(out, "width");
    out.push_str(&frame.width.to_string());
    out.push(',');
    push_key(out, "height");
    out.push_str(&frame.height.to_string());
    out.push('}');
}

fn serialize_tab(out: &mut String, tab: &TabSession) {
    out.push('{');
    push_key(out, "focused_leaf");
    out.push_str(&tab.focused_leaf.to_string());
    out.push(',');
    push_key(out, "title");
    match &tab.title {
        Some(title) => push_string(out, title),
        None => out.push_str("null"),
    }
    out.push(',');
    push_key(out, "split");
    serialize_node(out, &tab.split);
    out.push('}');
}

fn serialize_node(out: &mut String, node: &PaneNode) {
    out.push('{');
    match node {
        PaneNode::Leaf { cwd } => {
            push_key(out, "type");
            push_string(out, "leaf");
            out.push(',');
            push_key(out, "cwd");
            match cwd {
                Some(path) => push_string(out, path),
                None => out.push_str("null"),
            }
        }
        PaneNode::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            push_key(out, "type");
            push_string(out, "split");
            out.push(',');
            push_key(out, "orientation");
            push_string(
                out,
                match orientation {
                    Orientation::Horizontal => "horizontal",
                    Orientation::Vertical => "vertical",
                },
            );
            out.push(',');
            push_key(out, "ratio");
            out.push_str(&ratio.to_string());
            out.push(',');
            push_key(out, "first");
            serialize_node(out, first);
            out.push(',');
            push_key(out, "second");
            serialize_node(out, second);
        }
    }
    out.push('}');
}

fn push_array<T>(out: &mut String, items: &[T], mut each: impl FnMut(&mut String, &T)) {
    out.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        each(out, item);
    }
    out.push(']');
}

fn push_key(out: &mut String, key: &str) {
    push_string(out, key);
    out.push(':');
}

fn push_opt_num(out: &mut String, value: Option<f64>) {
    match value {
        Some(number) => out.push_str(&number.to_string()),
        None => out.push_str("null"),
    }
}

fn push_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parse a session JSON string. Returns `None` on any malformed input or a
/// `version` that is not [`SESSION_VERSION`], so the caller always has a clean
/// "no session" signal rather than a partial/garbled state.
pub fn parse(source: &str) -> Option<SessionState> {
    let value = json::parse(source)?;
    let object = value.as_object()?;
    let version = object.field("version")?.as_u64()?;
    if version != u64::from(SESSION_VERSION) {
        return None;
    }
    let focused_window = match object.field("focused_window") {
        None | Some(json::Value::Null) => None,
        Some(other) => Some(usize::try_from(other.as_u64()?).ok()?),
    };
    let windows = object
        .field("windows")?
        .as_array()?
        .iter()
        .map(parse_window)
        .collect::<Option<Vec<_>>>()?;
    Some(SessionState {
        windows,
        focused_window,
    })
}

fn parse_window(value: &json::Value) -> Option<WindowSession> {
    let object = value.as_object()?;
    let frame = match object.field("frame") {
        None | Some(json::Value::Null) => None,
        Some(frame) => Some(parse_frame(frame)?),
    };
    let focused_tab = usize::try_from(object.field("focused_tab")?.as_u64()?).ok()?;
    let tabs = object
        .field("tabs")?
        .as_array()?
        .iter()
        .map(parse_tab)
        .collect::<Option<Vec<_>>>()?;
    Some(WindowSession {
        frame,
        focused_tab,
        tabs,
    })
}

fn parse_frame(value: &json::Value) -> Option<WindowFrame> {
    let object = value.as_object()?;
    let width = object.field("width")?.as_f64()?;
    let height = object.field("height")?.as_f64()?;
    let x = object.field("x").and_then(json::Value::as_f64);
    let y = object.field("y").and_then(json::Value::as_f64);
    let position = match (x, y) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    };
    Some(WindowFrame {
        position,
        width,
        height,
    })
}

fn parse_tab(value: &json::Value) -> Option<TabSession> {
    let object = value.as_object()?;
    let focused_leaf = usize::try_from(object.field("focused_leaf")?.as_u64()?).ok()?;
    // Absent (pre-field session files) and null both mean "no override".
    let title = match object.field("title") {
        None | Some(json::Value::Null) => None,
        Some(title) => Some(title.as_str()?.to_string()),
    };
    let split = parse_node(object.field("split")?)?;
    Some(TabSession {
        focused_leaf,
        title,
        split,
    })
}

fn parse_node(value: &json::Value) -> Option<PaneNode> {
    let object = value.as_object()?;
    match object.field("type")?.as_str()? {
        "leaf" => {
            let cwd = match object.field("cwd") {
                None | Some(json::Value::Null) => None,
                Some(cwd) => Some(cwd.as_str()?.to_string()),
            };
            Some(PaneNode::Leaf { cwd })
        }
        "split" => {
            let orientation = match object.field("orientation")?.as_str()? {
                "horizontal" => Orientation::Horizontal,
                "vertical" => Orientation::Vertical,
                _ => return None,
            };
            let ratio = object.field("ratio")?.as_f64()? as f32;
            let first = Box::new(parse_node(object.field("first")?)?);
            let second = Box::new(parse_node(object.field("second")?)?);
            Some(PaneNode::Split {
                orientation,
                ratio,
                first,
                second,
            })
        }
        _ => None,
    }
}

/// Atomically write the session to `path`, creating the parent directory.
/// Writes to a sibling temp file then renames, so a crash mid-write cannot
/// truncate an existing good session file.
pub fn save(path: &Path, state: &SessionState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serialize(state))?;
    fs::rename(&tmp, path)
}

/// Load and parse the session at `path`, or `None` if it is absent, unreadable,
/// malformed, or a different schema version.
pub fn load(path: &Path) -> Option<SessionState> {
    let source = fs::read_to_string(path).ok()?;
    parse(&source)
}

/// A minimal JSON value model + recursive-descent parser, scoped to what the
/// session schema needs. Not a general JSON library — no number exponent
/// corner cases beyond what `f64::from_str` accepts, and objects preserve
/// insertion order via a `Vec` (the schema never needs fast key lookup).
mod json {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        Number(f64),
        String(String),
        Array(Vec<Value>),
        Object(Vec<(String, Value)>),
    }

    impl Value {
        pub fn as_object(&self) -> Option<&[(String, Value)]> {
            match self {
                Value::Object(entries) => Some(entries),
                _ => None,
            }
        }

        pub fn as_array(&self) -> Option<&[Value]> {
            match self {
                Value::Array(items) => Some(items),
                _ => None,
            }
        }

        pub fn as_str(&self) -> Option<&str> {
            match self {
                Value::String(text) => Some(text),
                _ => None,
            }
        }

        pub fn as_f64(&self) -> Option<f64> {
            match self {
                Value::Number(number) => Some(*number),
                _ => None,
            }
        }

        pub fn as_u64(&self) -> Option<u64> {
            match self {
                Value::Number(number) if *number >= 0.0 && number.fract() == 0.0 => {
                    Some(*number as u64)
                }
                _ => None,
            }
        }
    }

    /// Object-entry lookup by key. Defined as an extension so callers can write
    /// `object.field("key")` against the `&[(String, Value)]` slice.
    pub trait ObjectExt {
        fn field(&self, key: &str) -> Option<&Value>;
    }

    impl ObjectExt for &[(String, Value)] {
        fn field(&self, key: &str) -> Option<&Value> {
            self.iter()
                .find(|(entry_key, _)| entry_key == key)
                .map(|(_, value)| value)
        }
    }

    pub fn parse(source: &str) -> Option<Value> {
        let mut parser = Parser {
            chars: source.chars().collect(),
            pos: 0,
        };
        parser.skip_whitespace();
        let value = parser.parse_value()?;
        parser.skip_whitespace();
        // Trailing non-whitespace means the document is malformed.
        if parser.pos != parser.chars.len() {
            return None;
        }
        Some(value)
    }

    struct Parser {
        chars: Vec<char>,
        pos: usize,
    }

    impl Parser {
        fn peek(&self) -> Option<char> {
            self.chars.get(self.pos).copied()
        }

        fn bump(&mut self) -> Option<char> {
            let ch = self.peek()?;
            self.pos += 1;
            Some(ch)
        }

        fn skip_whitespace(&mut self) {
            while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                self.pos += 1;
            }
        }

        fn parse_value(&mut self) -> Option<Value> {
            self.skip_whitespace();
            match self.peek()? {
                '{' => self.parse_object(),
                '[' => self.parse_array(),
                '"' => self.parse_string().map(Value::String),
                't' | 'f' => self.parse_bool(),
                'n' => self.parse_null(),
                _ => self.parse_number(),
            }
        }

        fn parse_object(&mut self) -> Option<Value> {
            self.expect('{')?;
            let mut entries = Vec::new();
            self.skip_whitespace();
            if self.peek() == Some('}') {
                self.pos += 1;
                return Some(Value::Object(entries));
            }
            loop {
                self.skip_whitespace();
                let key = self.parse_string()?;
                self.skip_whitespace();
                self.expect(':')?;
                let value = self.parse_value()?;
                entries.push((key, value));
                self.skip_whitespace();
                match self.bump()? {
                    ',' => continue,
                    '}' => break,
                    _ => return None,
                }
            }
            Some(Value::Object(entries))
        }

        fn parse_array(&mut self) -> Option<Value> {
            self.expect('[')?;
            let mut items = Vec::new();
            self.skip_whitespace();
            if self.peek() == Some(']') {
                self.pos += 1;
                return Some(Value::Array(items));
            }
            loop {
                let value = self.parse_value()?;
                items.push(value);
                self.skip_whitespace();
                match self.bump()? {
                    ',' => continue,
                    ']' => break,
                    _ => return None,
                }
            }
            Some(Value::Array(items))
        }

        fn parse_string(&mut self) -> Option<String> {
            self.expect('"')?;
            let mut out = String::new();
            loop {
                match self.bump()? {
                    '"' => break,
                    '\\' => match self.bump()? {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'b' => out.push('\u{0008}'),
                        'f' => out.push('\u{000c}'),
                        'u' => {
                            let code = self.parse_hex4()?;
                            out.push(char::from_u32(u32::from(code))?);
                        }
                        _ => return None,
                    },
                    ch => out.push(ch),
                }
            }
            Some(out)
        }

        fn parse_hex4(&mut self) -> Option<u16> {
            let mut value: u16 = 0;
            for _ in 0..4 {
                let digit = self.bump()?.to_digit(16)?;
                value = value * 16 + digit as u16;
            }
            Some(value)
        }

        fn parse_bool(&mut self) -> Option<Value> {
            if self.consume("true") {
                Some(Value::Bool(true))
            } else if self.consume("false") {
                Some(Value::Bool(false))
            } else {
                None
            }
        }

        fn parse_null(&mut self) -> Option<Value> {
            self.consume("null").then_some(Value::Null)
        }

        fn parse_number(&mut self) -> Option<Value> {
            let start = self.pos;
            while matches!(self.peek(), Some('0'..='9' | '-' | '+' | '.' | 'e' | 'E')) {
                self.pos += 1;
            }
            if self.pos == start {
                return None;
            }
            let text: String = self.chars[start..self.pos].iter().collect();
            text.parse::<f64>().ok().map(Value::Number)
        }

        fn expect(&mut self, expected: char) -> Option<()> {
            (self.bump()? == expected).then_some(())
        }

        fn consume(&mut self, literal: &str) -> bool {
            let end = self.pos + literal.chars().count();
            if end <= self.chars.len()
                && self.chars[self.pos..end]
                    .iter()
                    .copied()
                    .eq(literal.chars())
            {
                self.pos = end;
                true
            } else {
                false
            }
        }
    }
}

use json::ObjectExt;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SessionState {
        SessionState {
            focused_window: Some(1),
            windows: vec![
                WindowSession {
                    frame: Some(WindowFrame {
                        position: Some((100.0, 50.0)),
                        width: 800.0,
                        height: 600.0,
                    }),
                    focused_tab: 0,
                    tabs: vec![TabSession {
                        focused_leaf: 0,
                        title: Some("api server \u{65e5}\u{672c}\u{8a9e} \u{1f680}".to_string()),
                        split: PaneNode::Leaf {
                            cwd: Some("/home/user".to_string()),
                        },
                    }],
                },
                WindowSession {
                    frame: None,
                    focused_tab: 1,
                    tabs: vec![
                        TabSession {
                            focused_leaf: 2,
                            title: None,
                            split: PaneNode::Split {
                                orientation: Orientation::Horizontal,
                                ratio: 0.4,
                                first: Box::new(PaneNode::Leaf {
                                    cwd: Some("/a".to_string()),
                                }),
                                second: Box::new(PaneNode::Split {
                                    orientation: Orientation::Vertical,
                                    ratio: 0.25,
                                    first: Box::new(PaneNode::Leaf { cwd: None }),
                                    second: Box::new(PaneNode::Leaf {
                                        cwd: Some("/b/c".to_string()),
                                    }),
                                }),
                            },
                        },
                        TabSession {
                            focused_leaf: 0,
                            title: Some("logs \"quoted\"\\slash".to_string()),
                            split: PaneNode::Leaf { cwd: None },
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn roundtrips_a_nested_session() {
        let state = sample();
        let text = serialize(&state);
        let parsed = parse(&text).expect("serialized session must parse");
        assert_eq!(parsed, state);
    }

    #[test]
    fn recursive_split_tree_roundtrips_exactly() {
        // A deeper, asymmetric tree exercises the recursive first/second paths.
        let split = PaneNode::Split {
            orientation: Orientation::Vertical,
            ratio: 0.7,
            first: Box::new(PaneNode::Split {
                orientation: Orientation::Horizontal,
                ratio: 0.33,
                first: Box::new(PaneNode::Leaf {
                    cwd: Some("/one".to_string()),
                }),
                second: Box::new(PaneNode::Leaf {
                    cwd: Some("/two".to_string()),
                }),
            }),
            second: Box::new(PaneNode::Leaf { cwd: None }),
        };
        assert_eq!(split.leaf_count(), 3);
        assert_eq!(split.first_leaf_cwd(), Some("/one".to_string()));

        let state = SessionState {
            focused_window: None,
            windows: vec![WindowSession {
                frame: None,
                focused_tab: 0,
                tabs: vec![TabSession {
                    focused_leaf: 1,
                    title: None,
                    split,
                }],
            }],
        };
        assert_eq!(parse(&serialize(&state)), Some(state));
    }

    #[test]
    fn cwd_with_special_characters_is_escaped_and_restored() {
        let state = SessionState {
            focused_window: Some(0),
            windows: vec![WindowSession {
                frame: None,
                focused_tab: 0,
                tabs: vec![TabSession {
                    focused_leaf: 0,
                    title: None,
                    split: PaneNode::Leaf {
                        cwd: Some("/path/with \"quote\"\tand\\slash".to_string()),
                    },
                }],
            }],
        };
        assert_eq!(parse(&serialize(&state)), Some(state));
    }

    #[test]
    fn tab_without_title_field_parses_to_none_override() {
        // A session file written before the tab-title field existed
        // (tab-title AC-TTL-10 backward compatibility).
        let source = concat!(
            "{\"version\":1,\"focused_window\":0,\"windows\":[",
            "{\"frame\":null,\"focused_tab\":0,\"tabs\":[",
            "{\"focused_leaf\":0,\"split\":{\"type\":\"leaf\",\"cwd\":null}}",
            "]}]}",
        );
        let state = parse(source).expect("pre-title session must parse");
        assert_eq!(state.windows[0].tabs[0].title, None);
    }

    #[test]
    fn malformed_input_parses_to_none() {
        for source in [
            "",
            "{",
            "not json",
            "{\"version\": 1",
            "{\"version\": 1, \"windows\": [}",
            "{\"version\": 1, \"focused_window\": 0}", // missing windows
            "[]",
        ] {
            assert!(parse(source).is_none(), "{source:?} should be None");
        }
    }

    #[test]
    fn version_mismatch_parses_to_none() {
        let mut state = sample();
        state.focused_window = None;
        let text = serialize(&state);
        assert!(parse(&text).is_some());

        let bumped = text.replacen(
            &format!("\"version\":{SESSION_VERSION}"),
            "\"version\":999",
            1,
        );
        assert_ne!(bumped, text);
        assert!(parse(&bumped).is_none());
    }

    #[test]
    fn save_then_load_roundtrips_through_the_filesystem() {
        let dir = std::env::temp_dir().join(format!("noa-session-test-{}", std::process::id()));
        let path = dir.join("noa").join("session.json");
        let state = sample();

        save(&path, &state).expect("save must succeed");
        let loaded = load(&path).expect("load must return the saved session");
        assert_eq!(loaded, state);

        // Loading a path with no file yields None rather than erroring.
        let _ = fs::remove_dir_all(&dir);
        assert!(load(&path).is_none());
    }
}
