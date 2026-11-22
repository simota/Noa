//! Connection health in the header (AC6): a dot and a status text that say
//! whether the link is up, how long ago the server last answered, and when
//! what is shown is old. What the last action said (a notice) has its own
//! line at the foot of the window, so it never hides the link state.

use std::time::{Duration, Instant};

use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{Role, UiTree, WidgetId};
use noa_gpui_scene::Color;

use crate::fleet::{Fleet, minutes_text, one_line};
use crate::live::{Link, Snapshot};

use super::header::HeaderParts;
use super::style::{GRAY, GREEN, ORANGE, RED, YELLOW, set_label};
use super::{Feed, NOTICE_FOR};

/// A live link whose last answer is this old marks the data stale.
pub(super) const STALE_AFTER: Duration = Duration::from_secs(10);

/// The dot before the status label, and the label.
#[derive(Debug, Clone, Copy)]
pub(super) struct HealthParts {
    pub(super) dot: WidgetId,
    pub(super) status: WidgetId,
}

/// How far the header can trust what it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Health {
    /// No link: the canned fleet.
    Demo,
    Connecting,
    Live,
    /// Live, but the server has not answered for `STALE_AFTER`.
    Stale,
    /// The link is lost. `had_data`: the screen still shows the last fleet.
    Down {
        had_data: bool,
    },
}

/// The health of a link whose server last answered `age` ago; `had_data`
/// is whether workers were ever shown (only a `Down` link asks).
pub(super) fn of(link: Option<&Link>, age: Duration, had_data: bool) -> Health {
    match link {
        None => Health::Demo,
        Some(Link::Connecting) => Health::Connecting,
        Some(Link::Live) if age >= STALE_AFTER => Health::Stale,
        Some(Link::Live) => Health::Live,
        Some(Link::Down(_)) => Health::Down { had_data },
    }
}

impl Health {
    pub(super) fn dot(self) -> Color {
        match self {
            Health::Demo => GRAY,
            Health::Connecting => YELLOW,
            Health::Live => GREEN,
            Health::Stale => ORANGE,
            Health::Down { .. } => RED,
        }
    }

    /// The status label's colour; `None` is the muted text colour.
    pub(super) fn text(self) -> Option<Color> {
        match self {
            Health::Stale => Some(ORANGE),
            Health::Down { .. } => Some(RED),
            Health::Demo | Health::Connecting | Health::Live => None,
        }
    }
}

/// What the header's health reads: the feed's link state, or the one a
/// snapshot held back while an IME composes would give it (`Seen::held`).
#[derive(Clone, Copy)]
struct Seen<'a> {
    link: Option<&'a Link>,
    synced_at: Option<Instant>,
    synced_secs: u64,
    had_data: bool,
    workers: usize,
    notice: Option<&'a (String, Instant)>,
}

impl<'a> Seen<'a> {
    fn of(feed: &'a Feed) -> Self {
        Seen {
            link: feed.link.as_ref(),
            synced_at: feed.synced_at,
            synced_secs: feed.synced_secs,
            had_data: feed.had_data,
            workers: feed.fleet.workers.len(),
            notice: feed.notice.as_ref(),
        }
    }

    /// The feed as it will be once it takes in `held`, which came at
    /// `received` (`Feed::accept`, with the fleet `keep_on_down` keeps).
    /// The body waits for the composition to end, as a rebuild would end
    /// it; the header does not: a lost link shows at once (AC6).
    fn held(feed: &'a Feed, held: &'a Snapshot, received: Instant) -> Self {
        let kept = if matches!(held.link, Link::Down(_)) && held.fleet.workers.is_empty() {
            feed.fleet.workers.len()
        } else {
            held.fleet.workers.len()
        };
        Seen {
            link: Some(&held.link),
            synced_at: feed.synced_at_after(held, received),
            synced_secs: held.synced_secs,
            had_data: feed.had_data || kept > 0,
            workers: kept,
            notice: feed.notice.as_ref(),
        }
    }
}

/// The header's status text at `now`: the link state.
#[cfg(test)]
pub(super) fn status(feed: &Feed, now: Instant) -> String {
    link_text(Seen::of(feed), Some(now))
}

fn notice_text(seen: Seen, now: Option<Instant>) -> String {
    now.and_then(|now| notice_at(seen, now))
        .map(one_line)
        .unwrap_or_default()
}

/// The notice the header shows at `now`, if one is still fresh.
fn notice_at<'a>(seen: Seen<'a>, now: Instant) -> Option<&'a str> {
    let (notice, at) = seen.notice?;
    (now.saturating_duration_since(*at) < NOTICE_FOR).then_some(notice.as_str())
}

/// How long the notice the header shows at `now` stays, if one is shown.
fn notice_left(seen: Seen, now: Instant) -> Option<Duration> {
    notice_at(seen, now)?;
    let (_, at) = seen.notice?;
    Some(NOTICE_FOR.saturating_sub(now.saturating_duration_since(*at)))
}

/// The status text for a screen that is just being built: what `status`
/// says at `built_at`.
pub(super) fn initial_status(feed: &Feed) -> String {
    let seen = Seen::of(feed);
    link_text(seen, built_at(seen))
}

/// The moment a screen that is just being built shows the health of.
/// Building has no time of its own to read: a frame runs `tick` with the
/// frame's one `now` before anything is drawn. So this is the latest moment
/// the feed itself knows (the snapshot's arrival, or the notice), and reads
/// no clock. `None` when nothing has come (the canned fleet, or a link still
/// connecting): there is no age to count and no notice to expire.
fn built_at(seen: Seen) -> Option<Instant> {
    let received = seen
        .synced_at
        .map(|at| at + Duration::from_secs(seen.synced_secs));
    let noticed = seen.notice.map(|(_, at)| *at);
    received.max(noticed)
}

/// The link state in words. `now` is `None` only when the feed has never
/// been synced, which leaves nothing to count from.
fn link_text(seen: Seen, now: Option<Instant>) -> String {
    let n = seen.workers;
    let age = age(seen, now);
    let ago = minutes_text(age.as_secs_f64() / 60.0);
    let reason = match seen.link {
        Some(Link::Down(reason)) => one_line(reason),
        _ => String::new(),
    };
    match health(seen, now) {
        Health::Demo => format!("ワーカー {n} · ダミーデータ"),
        Health::Connecting => "接続中…".to_owned(),
        Health::Live => format!("ワーカー {n} · 同期 {ago}前"),
        // The label cuts its tail (the slot is at most 200 px wide), so
        // what must stay readable comes first: the stale mark and the
        // data's age; the worker count may be cut.
        Health::Stale => format!("古いデータ（同期 {ago}前）· ワーカー {n}"),
        Health::Down { had_data: false } => format!("切断: {reason}"),
        // The reason can be any length: here too the stale mark and the
        // data's age come first (164 px for "59 分", in a slot of up to
        // 200 px, which leaves room for the reason's first characters).
        Health::Down { had_data: true } => {
            format!("切断: 古いデータ（{ago}前）· {reason}")
        }
    }
}

/// The health at `now`.
fn health(seen: Seen, now: Option<Instant>) -> Health {
    of(seen.link, age(seen, now), seen.had_data)
}

/// How long ago the server last answered, as of `now`; the last
/// snapshot's own count when there is no `now` or no arrival time.
fn age(seen: Seen, now: Option<Instant>) -> Duration {
    match seen.synced_at.zip(now) {
        Some((at, now)) => now.saturating_duration_since(at),
        None => Duration::from_secs(seen.synced_secs),
    }
}

/// Brings the dot and the status label up to `now`. The setters change
/// nothing when the value is the same, so this runs on every frame.
/// A rebuild ticks again at the last tick's `now`, so a header rebuilt
/// between frames shows what the last frame drew.
///
/// `held` is a snapshot the body waits with while an IME composes, and
/// when it came: the header already shows the link state it brings.
///
/// While a notice is shown, the dot asks for a timer at the moment the
/// notice ends: the app draws a frame when a widget's timer is due, and
/// that frame's `tick` puts the link state back. Without it nothing would
/// redraw a screen no feed wakes (the demo, or a feed that has stopped),
/// and the notice would stay. A frame that comes a little early asks again
/// for what is left.
pub(super) fn tick(
    tree: &mut UiTree,
    parts: &HeaderParts,
    notice: WidgetId,
    feed: &Feed,
    held: Option<(&Snapshot, Instant)>,
    now: Instant,
) {
    let seen = match held {
        Some((snapshot, received)) => Seen::held(feed, snapshot, received),
        None => Seen::of(feed),
    };
    show(tree, parts.health, seen, Some(now));
    set_label(tree, notice, notice_text(seen, Some(now)));
    if let Some(left) = notice_left(seen, now) {
        tree.update::<Container, _>(parts.health.dot, |_, cx| {
            cx.request_timer(left.as_secs_f64());
        });
    }
}

/// Gives a header that is just being built the text, colour and dot of
/// `built_at`, all three from that one moment.
pub(super) fn show_built(tree: &mut UiTree, parts: HealthParts, feed: &Feed) {
    let seen = Seen::of(feed);
    show(tree, parts, seen, built_at(seen));
}

/// The notice line of a screen that is just being built, as of `built_at`.
pub(super) fn initial_notice(feed: &Feed) -> String {
    let seen = Seen::of(feed);
    notice_text(seen, built_at(seen))
}

/// Sets the dot's colour, the status text and its colour as of `now`.
fn show(tree: &mut UiTree, parts: HealthParts, seen: Seen, now: Option<Instant>) {
    let HealthParts { dot, status: label } = parts;
    let health = health(seen, now);
    tree.update::<Container, _>(dot, |c, cx| c.set_background(cx, Some(health.dot().into())));
    let text = link_text(seen, now);
    tree.update::<Label, _>(label, |l, cx| {
        l.set_text(cx, text);
        match health.text() {
            Some(color) => l.set_color(cx, color),
            None => l.set_color(cx, Role::Muted),
        }
    });
}

/// The fleet to show after a snapshot: a `Down` link brings an empty fleet,
/// and when workers were on screen those stay (marked stale by the status),
/// so a lost link does not blank the screen. With nothing shown before, the
/// empty state says why.
pub(super) fn keep_on_down(old: &Fleet, new: Fleet, link: &Link) -> Fleet {
    if matches!(link, Link::Down(_)) && new.workers.is_empty() && !old.workers.is_empty() {
        old.clone()
    } else {
        new
    }
}
