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

const USAGE: &str = "usage: agent-dashboard [--demo] [--group=parent|agent|model|none] \
                     [--snapshot=FILE.png [--size=WxH] [--keys=SEQ]]\n  \
                     --size: snapshot size in logical pixels (default 1440x900; at most \
                     4096 a side and about 8.3 million pixels, e.g. 3840x2160); \
                     --keys: keys sent before the snapshot, e.g. 'jj/codex<Enter>' \
                     (<Enter> <Esc> <Space> <Down> <Up> are named keys; any other < is typed)\n  \
                     live data from the running Noa's control server by default; \
                     --demo shows canned data";
const DEFAULT_SIZE: (u32, u32) = (1440, 900);
const SNAPSHOT_SCALE: f32 = 2.0;
/// How long `--snapshot` waits for the live feed's first snapshot.
const FIRST_SNAPSHOT_WAIT: Duration = Duration::from_secs(3);

/// The window title in the person's language: Japanese where the locale is
/// Japanese, English anywhere else. The locale variables in POSIX order
/// (`LC_ALL`, `LC_MESSAGES`, `LANG`: the first one set wins), else the
/// system's preferred language, as Noa's own board decides
/// (`noa-app` `ctl/board.rs` `Lang::from_locale`).
fn title(
    var: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> Option<String>,
) -> &'static str {
    let tag = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| var(key).filter(|value| !value.is_empty()))
        .or_else(system);
    match tag {
        Some(tag) if tag.starts_with("ja") => "Noa · エージェント運用ダッシュボード",
        _ => "Noa · Agent Operations Dashboard",
    }
}

/// The first of the person's preferred languages, as a BCP 47 tag (`ja-JP`).
#[cfg(target_os = "macos")]
fn system_language() -> Option<String> {
    objc2_foundation::NSLocale::preferredLanguages()
        .firstObject()
        .map(|tag| tag.to_string())
}

#[cfg(not(target_os = "macos"))]
fn system_language() -> Option<String> {
    None
}

fn options() -> AppOptions {
    AppOptions {
        title: title(|key| std::env::var(key).ok(), system_language).into(),
        size: (1440.0, 900.0),
        ..AppOptions::new("Noa Agent Dashboard")
    }
}

/// Largest side of the render target in physical pixels: WebGPU's
/// guaranteed `max_texture_dimension_2d`. noa-gpui-render-wgpu 0.2.1
/// (`gpu::required_limits`) asks the device for the adapter's own texture
/// limit (16384 on Apple GPUs), so this holds on any adapter that meets
/// WebGPU's minimum.
const MAX_SIDE_PX: u64 = 8192;

/// The largest buffer the renderer's device allows: noa-gpui-render-wgpu
/// 0.2.1 starts from wgpu's `Limits::downlevel_webgl2_defaults()`, whose
/// `max_buffer_size` is 256 MiB, and raises only the texture sizes
/// (`using_resolution`). A GPU snapshot reads the whole frame back into one
/// such buffer (`OffscreenRenderer::render`): a larger one panics in wgpu.
const DEVICE_BUFFER_MAX: u64 = 256 << 20;

/// What a snapshot's readback may take: half the device's limit, for
/// margin. The CPU renderer (when there is no GPU) holds the frame as four
/// `f32` per pixel, four times the readback (at most 512 MiB here), and its
/// RGBA result as much again as the readback. 3840x2160 needs 127 MiB.
const READBACK_BUDGET: u64 = DEVICE_BUFFER_MAX / 2;

/// A snapshot of `size` logical pixels as the renderer allocates it: its
/// physical size (`SNAPSHOT_SCALE` times, rounded as the platform rounds a
/// viewport) and the bytes of its GPU readback buffer, whose rows are padded
/// to wgpu's 256-byte copy alignment. Each side is checked before the
/// product is taken, so the product cannot overflow.
fn render_size(size: (u32, u32)) -> Result<((u64, u64), u64), String> {
    let side = |n: u32| (n as f32 * SNAPSHOT_SCALE).round() as u64;
    let (width, height) = (side(size.0), side(size.1));
    if width > MAX_SIDE_PX || height > MAX_SIDE_PX {
        return Err(format!(
            "a {width}x{height} px render target is more than {MAX_SIDE_PX} px a side"
        ));
    }
    const ALIGN: u64 = 256;
    let row = (width * 4).div_ceil(ALIGN) * ALIGN;
    let bytes = row * height;
    if bytes > READBACK_BUDGET {
        return Err(format!(
            "a {width}x{height} px render target needs {} MiB, more than {} MiB",
            bytes.div_ceil(1 << 20),
            READBACK_BUDGET >> 20
        ));
    }
    Ok(((width, height), bytes))
}

/// `"1100x700"` as (1100, 700): each side at least 1, and a snapshot the
/// renderer can allocate (`render_size`).
fn parse_size(arg: &str) -> Result<(u32, u32), String> {
    let bad = || format!("bad size {arg:?}, expected WxH\n{USAGE}");
    let (w, h) = arg.split_once('x').ok_or_else(bad)?;
    let size = match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if w >= 1 && h >= 1 => (w, h),
        _ => return Err(bad()),
    };
    render_size(size).map_err(|why| format!("size {arg:?} is too large: {why}\n{USAGE}"))?;
    Ok(size)
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

/// The modifiers the platform layer puts on a key press with only Shift
/// (or nothing) held: it marks whether Cmd is the command key (on Apple
/// platforms) on every key, as `noa_gpui_platform::command_is_meta` says.
fn modifiers(shift: bool) -> Modifiers {
    let platform = Modifiers {
        command_is_meta: noa_gpui_platform::command_is_meta(),
        ..Modifiers::default()
    };
    Modifiers { shift, ..platform }
}

/// What the platform layer sends for one key press: `KeyDown` (a character key
/// is lowercase and unshifted, Shift in the modifiers), then `Text` unless the
/// text is a control character.
fn typed(key: Key, text: Option<&str>, shift: bool, events: &mut Vec<Event>) {
    events.push(Event::KeyDown {
        key,
        modifiers: modifiers(shift),
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
/// space is `<Space>`; any other `<` is the character `<`, so `Vec<T>` types
/// those six characters. Control characters are an error.
fn parse_keys(seq: &str) -> Result<Vec<Event>, String> {
    let mut events = Vec::new();
    let mut rest = seq;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && let Some((key, text)) = named_key(&rest[1..end])
        {
            typed(key, text, false, &mut events);
            rest = &rest[end + 1..];
            continue;
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

    #[test]
    fn the_title_follows_the_locale() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };
        let ja = "Noa · エージェント運用ダッシュボード";
        let en = "Noa · Agent Operations Dashboard";
        assert_eq!(title(env(&[("LANG", "ja_JP.UTF-8")]), || None), ja);
        assert_eq!(title(env(&[("LANG", "en_US.UTF-8")]), || None), en);
        // LC_ALL wins over LANG; an empty one is skipped.
        assert_eq!(
            title(
                env(&[("LC_ALL", "en_US.UTF-8"), ("LANG", "ja_JP.UTF-8")]),
                || None
            ),
            en
        );
        assert_eq!(
            title(env(&[("LC_ALL", ""), ("LANG", "ja_JP.UTF-8")]), || None),
            ja
        );
        // No locale variables: the system's preferred language.
        assert_eq!(title(env(&[]), || Some("ja-JP".into())), ja);
        assert_eq!(title(env(&[]), || Some("en-US".into())), en);
        assert_eq!(title(env(&[]), || None), en);
    }

    /// A key press as noa-gpui-platform 0.2.1 sends it (`input.rs`): on
    /// Apple platforms every key carries `command_is_meta`.
    fn key_down(key: Key, shift: bool) -> Event {
        Event::KeyDown {
            key,
            modifiers: Modifiers {
                shift,
                command_is_meta: cfg!(target_vendor = "apple"),
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
    }

    /// Every character of `s` as `parse_keys` types a plain character.
    fn typed_chars(s: &str) -> Vec<Event> {
        let mut e = Vec::new();
        for c in s.chars() {
            match shifted_base(c) {
                Some(base) => e.push(key_down(Key::Char(base), true)),
                None => e.push(key_down(Key::Char(c), false)),
            }
            e.push(text(&c.to_string()));
        }
        e
    }

    #[test]
    fn a_name_that_is_no_key_is_typed() {
        // Only the five names are keys: a search for `Vec<T>` types it.
        let mut e = typed_chars("/Vec<T>");
        e.push(key_down(Key::Enter, false));
        assert_eq!(keys("/Vec<T><Enter>"), events(e));
        for seq in ["<enter>", "<F5>", "<>", "<T>"] {
            assert_eq!(keys(seq), events(typed_chars(seq)), "{seq}");
        }
    }

    #[test]
    fn keys_carry_the_platforms_command_key() {
        // The platform marks whether Cmd is the command key on every key,
        // Shift+/ for `?` included (true on macOS).
        for event in parse_keys("?j<Enter><Space>").unwrap() {
            if let Event::KeyDown { modifiers, .. } = event {
                assert_eq!(
                    modifiers.command_is_meta,
                    cfg!(target_vendor = "apple"),
                    "{modifiers:?}"
                );
            }
        }
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
        assert_eq!(parse_size("1920x1200"), Ok((1920, 1200)));
        assert_eq!(parse_size("3840x2160"), Ok((3840, 2160)));
        assert_eq!(parse_size("4096x2048"), Ok((4096, 2048)));
        assert_eq!(parse_size("4096x1"), Ok((4096, 1)));
        let err = parse_size("1100").unwrap_err();
        assert!(err.contains(USAGE), "{err}");
    }

    #[test]
    fn sizes_beyond_the_render_target_are_rejected() {
        for arg in [
            "8193x700",
            "700x8193",
            "4097x1",
            "1x4097",
            "8192x8192",
            "4096x4096",
            "32768x32768",
            "4294967295x4294967295",
            "0x700",
            "1100x0",
            "axb",
        ] {
            let err = parse_size(arg).unwrap_err();
            assert!(err.contains(USAGE), "{arg}: {err}");
        }
    }

    #[test]
    fn the_readback_counts_its_padded_rows() {
        // 4065 px is 8130 at 2x, 32520 bytes a row, padded to 32768: 4120
        // rows fit the budget unpadded (133982400 bytes) but not padded
        // (135004160).
        let err = parse_size("4065x2060").unwrap_err();
        assert!(err.contains("too large"), "{err}");
        assert_eq!(
            render_size((4065, 2048)).unwrap(),
            ((8130, 4096), READBACK_BUDGET)
        );
    }

    #[test]
    fn what_a_snapshot_allocates_stays_inside_the_renderer() {
        // The largest sizes accepted, each way: the GPU readback stays
        // under the device's buffer limit with half of it to spare, the
        // render target inside WebGPU's guaranteed texture size, and the
        // CPU renderer's frame (16 bytes a pixel) at most 512 MiB.
        for size in [(4096, 2048), (2048, 4096), (3840, 2160), (4096, 1)] {
            let ((width, height), bytes) = render_size(size).unwrap();
            assert!(width <= MAX_SIDE_PX && height <= MAX_SIDE_PX, "{size:?}");
            assert!(bytes <= DEVICE_BUFFER_MAX / 2, "{size:?}: {bytes}");
            assert!(width * height * 16 <= 512 << 20, "{size:?}");
        }
    }
}
