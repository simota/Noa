mod dashboard;
mod fleet;
mod live;

use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use noa_gpui_core::{Event, Key, Modifiers};
use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use crate::dashboard::Dashboard;
use crate::fleet::Grouping;
use crate::live::{Link, Snapshot};

const USAGE: &str = "usage: agent-dashboard-mock [--demo] [--group=parent|agent|model|none] \
                     [--snapshot=FILE.png [--size=WxH] [--keys=SEQ]]\n  \
                     --size: snapshot size in logical pixels (default 1440x900); \
                     --keys: keys sent before the snapshot, e.g. 'jj/codex<Enter>' \
                     (<Enter> <Esc> <Space> <Down> <Up> are named keys)\n  \
                     live data from the running Noa's control server by default; \
                     --demo shows canned data";
const DEFAULT_SIZE: (u32, u32) = (1440, 900);
const SNAPSHOT_SCALE: f32 = 2.0;
/// How long `--snapshot` waits for the live feed's first snapshot.
const FIRST_SNAPSHOT_WAIT: Duration = Duration::from_secs(3);

fn options() -> AppOptions {
    AppOptions {
        title: "Noa · エージェント運用ダッシュボード（モック）".into(),
        size: (1440.0, 900.0),
        ..AppOptions::new("Noa Agent Dashboard")
    }
}

/// Largest snapshot side in logical pixels. The render target is
/// `SNAPSHOT_SCALE` times that (16384 px), the most a GPU texture holds on the
/// Apple GPUs this runs on; the CPU fallback's buffer stays near 1 GiB.
const MAX_SIZE: u32 = 8192;

/// `"1100x700"` as (1100, 700); each side 1..=`MAX_SIZE`.
fn parse_size(arg: &str) -> Result<(u32, u32), String> {
    let bad = || format!("bad size {arg:?}, expected WxH with each side 1 to {MAX_SIZE}\n{USAGE}");
    let (w, h) = arg.split_once('x').ok_or_else(bad)?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if (1..=MAX_SIZE).contains(&w) && (1..=MAX_SIZE).contains(&h) => Ok((w, h)),
        _ => Err(bad()),
    }
}

/// The US-layout key that types `c` with Shift held, as the key itself
/// (unshifted, lowercase), which is what the platform layer reports.
fn shifted_base(c: char) -> Option<char> {
    const PAIRS: &str = "~`!1@2#3$4%5^6&7*8(9)0_-+={[}]|\\:;\"'<,>.?/";
    if c.is_ascii_uppercase() {
        return Some(c.to_ascii_lowercase());
    }
    let mut it = PAIRS.chars();
    while let (Some(top), Some(base)) = (it.next(), it.next()) {
        if top == c {
            return Some(base);
        }
    }
    None
}

/// What the platform layer sends for one key press: `KeyDown` (a character key
/// is lowercase and unshifted, Shift in the modifiers), then `Text` unless the
/// text is a control character.
fn typed(key: Key, text: Option<&str>, shift: bool, events: &mut Vec<Event>) {
    events.push(Event::KeyDown {
        key,
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
    });
    if let Some(text) = text {
        events.push(Event::Text(text.to_string()));
    }
}

/// The key names `--keys` knows, as the platform layer sees those keys. Enter,
/// Escape and the arrows carry no text (control characters or none).
fn named_key(name: &str) -> Option<(Key, Option<&'static str>)> {
    Some(match name {
        "Enter" => (Key::Enter, None),
        "Esc" => (Key::Escape, None),
        "Space" => (Key::Space, Some(" ")),
        "Down" => (Key::ArrowDown, None),
        "Up" => (Key::ArrowUp, None),
        _ => return None,
    })
}

/// SEQ as the events the real app produces for those key presses on a US/JIS
/// layout. `<Enter>`, `<Esc>`, `<Space>`, `<Down>` and `<Up>` are named keys; a
/// space is `<Space>`; a `<` that starts no such name is the character `<`,
/// while an unknown `<name>` is an error. Control characters are an error.
fn parse_keys(seq: &str) -> Result<Vec<Event>, String> {
    let mut events = Vec::new();
    let mut rest = seq;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
        {
            let name = &rest[1..end];
            if let Some((key, text)) = named_key(name) {
                typed(key, text, false, &mut events);
                rest = &rest[end + 1..];
                continue;
            }
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(format!("unknown key <{name}>\n{USAGE}"));
            }
        }
        if c.is_control() {
            return Err(format!(
                "control character U+{:04X} in --keys; use <Enter>, <Esc>, <Space>, <Down> or <Up>\n{USAGE}",
                c as u32
            ));
        }
        if c == ' ' {
            let (key, text) = named_key("Space").expect("Space is a named key");
            typed(key, text, false, &mut events);
        } else if let Some(base) = shifted_base(c) {
            typed(Key::Char(base), Some(&c.to_string()), true, &mut events);
        } else {
            typed(Key::Char(c), Some(&c.to_string()), false, &mut events);
        }
        rest = &rest[c.len_utf8()..];
    }
    Ok(events)
}

fn snapshot(
    path: &PathBuf,
    screen: &mut dyn Screen,
    size: (u32, u32),
    keys: Vec<Event>,
) -> Result<(), String> {
    let mut headless = Headless::new_or_software();
    let mut text = TextStore::new(Default::default());
    let viewport = Size::new(size.0 as f32, size.1 as f32);
    screen.prepare(viewport, &mut text);
    for key in keys {
        screen.dispatch(key, &mut text);
        screen.prepare(viewport, &mut text);
    }
    headless
        .render_screen(screen, &mut text, size, SNAPSHOT_SCALE)
        .map_err(|e| e.to_string())?
        .save_png(path)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// The live feed's first snapshot with a connection, or why none came.
fn first_live_snapshot() -> Result<Snapshot, String> {
    let channel = live::start(|| {});
    let until = Instant::now() + FIRST_SNAPSHOT_WAIT;
    let mut reason = "Noa から 3 秒以内に応答がありません".to_owned();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match channel.snapshots.recv_timeout(left) {
            Ok(snapshot) => match &snapshot.link {
                Link::Live => return Ok(snapshot),
                Link::Down(why) => reason.clone_from(why),
                Link::Connecting => {}
            },
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                return Err(reason);
            }
        }
    }
}

fn main() -> Result<(), String> {
    let mut snapshot_path = None;
    let mut grouping = Grouping::Orchestrator;
    let mut demo = false;
    let mut size = DEFAULT_SIZE;
    let mut keys = Vec::new();
    for arg in std::env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--snapshot=") {
            snapshot_path = Some(PathBuf::from(path));
        } else if let Some(name) = arg.strip_prefix("--group=") {
            grouping =
                Grouping::parse(name).ok_or_else(|| format!("unknown group {name:?}\n{USAGE}"))?;
        } else if let Some(arg) = arg.strip_prefix("--size=") {
            size = parse_size(arg)?;
        } else if let Some(seq) = arg.strip_prefix("--keys=") {
            keys = parse_keys(seq)?;
        } else if arg == "--demo" {
            demo = true;
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }
    if let Some(path) = snapshot_path {
        let mut screen = if demo {
            Dashboard::new(grouping)
        } else {
            Dashboard::of_snapshot(grouping, first_live_snapshot()?)
        };
        snapshot(&path, &mut screen, size, keys)?;
        println!("wrote {}", path.display());
        return Ok(());
    }
    App::new(options(), move |setup| {
        if demo {
            return Box::new(Dashboard::new(grouping));
        }
        let waker = setup.waker.clone();
        let channel = live::start(move || waker.wake());
        Box::new(Dashboard::live(grouping, channel))
    })
    .run()
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_down(key: Key, shift: bool) -> Event {
        Event::KeyDown {
            key,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
        }
    }

    fn text(s: &str) -> Event {
        Event::Text(s.to_string())
    }

    fn keys(seq: &str) -> String {
        format!("{:?}", parse_keys(seq).unwrap())
    }

    fn events(events: Vec<Event>) -> String {
        format!("{events:?}")
    }

    #[test]
    fn keys_parse_into_events() {
        let mut expected = Vec::new();
        for c in ['j', '/', 'c', 'o'] {
            expected.push(key_down(Key::Char(c), false));
            expected.push(text(&c.to_string()));
        }
        expected.push(key_down(Key::Enter, false));
        assert_eq!(keys("j/co<Enter>"), events(expected));
    }

    #[test]
    fn named_keys_send_what_the_platform_sends() {
        // Enter, Escape and the arrows carry no text; Space carries " ".
        for (seq, key) in [
            ("<Enter>", Key::Enter),
            ("<Esc>", Key::Escape),
            ("<Down>", Key::ArrowDown),
            ("<Up>", Key::ArrowUp),
        ] {
            assert_eq!(keys(seq), events(vec![key_down(key, false)]), "{seq}");
        }
        let space = events(vec![key_down(Key::Space, false), text(" ")]);
        assert_eq!(keys("<Space>"), space);
        assert_eq!(keys(" "), space, "a literal space is the Space key");
        assert_eq!(
            keys("/claude<Space>sonnet").matches("Space").count(),
            1,
            "the query's space is one Space press"
        );
    }

    #[test]
    fn shifted_characters_report_their_unshifted_key() {
        assert_eq!(
            keys("?"),
            events(vec![key_down(Key::Char('/'), true), text("?")])
        );
        assert_eq!(
            keys("J"),
            events(vec![key_down(Key::Char('j'), true), text("J")])
        );
    }

    #[test]
    fn a_lone_less_than_is_a_character() {
        let lt = vec![key_down(Key::Char(','), true), text("<")];
        assert_eq!(keys("<"), events(lt.clone()));
        assert_eq!(keys("a<b"), {
            let mut e = vec![key_down(Key::Char('a'), false), text("a")];
            e.extend(lt.clone());
            e.extend([key_down(Key::Char('b'), false), text("b")]);
            events(e)
        });
        let mut e = lt;
        e.push(key_down(Key::Enter, false));
        assert_eq!(keys("<<Enter>"), events(e));
        assert!(
            parse_keys("<enter>")
                .unwrap_err()
                .contains("unknown key <enter>")
        );
        assert!(parse_keys("<F5>").unwrap_err().contains("unknown key <F5>"));
    }

    #[test]
    fn control_characters_are_rejected() {
        for seq in ["/a\tb", "a\nb", "\r", "\u{1b}", "\u{7f}"] {
            let err = parse_keys(seq).unwrap_err();
            assert!(err.contains("control character"), "{seq:?}: {err}");
            assert!(err.contains(USAGE), "{err}");
        }
    }

    #[test]
    fn size_parses() {
        assert_eq!(parse_size("1100x700"), Ok((1100, 700)));
        assert_eq!(parse_size("8192x8192"), Ok((8192, 8192)));
        let err = parse_size("1100").unwrap_err();
        assert!(err.contains(USAGE), "{err}");
    }

    #[test]
    fn sizes_beyond_the_render_target_are_rejected() {
        for arg in [
            "8193x700",
            "700x8193",
            "32768x32768",
            "0x700",
            "1100x0",
            "axb",
        ] {
            let err = parse_size(arg).unwrap_err();
            assert!(err.contains(USAGE), "{arg}: {err}");
        }
    }
}
