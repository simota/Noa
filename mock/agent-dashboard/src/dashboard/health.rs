//! Connection health in the header (AC6): a dot and a status text that say
//! whether the link is up, how long ago the server last answered, and when
//! what is shown is old.

use std::time::{Duration, Instant};

use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{Role, UiTree, WidgetId};
use noa_gpui_scene::Color;

use crate::fleet::{Fleet, minutes_text};
use crate::live::Link;

use super::header::HeaderParts;
use super::style::{GRAY, GREEN, ORANGE, RED, YELLOW};
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

/// The header's status text at `now`: a fresh notice, else the link state.
pub(super) fn status(feed: &Feed, now: Instant) -> String {
    if let Some((notice, at)) = &feed.notice
        && now.saturating_duration_since(*at) < NOTICE_FOR
    {
        return notice.clone();
    }
    link_text(feed, Some(now))
}

/// The status text for a screen that is just being built: what `status`
/// says at `built_at`.
pub(super) fn initial_status(feed: &Feed) -> String {
    text_at(feed, built_at(feed))
}

/// The moment a screen that is just being built shows the health of.
/// Building has no time of its own to read: a frame runs `tick` with the
/// frame's one `now` before anything is drawn. So this is the latest moment
/// the feed itself knows (the snapshot's arrival, or the notice), and reads
/// no clock. `None` when nothing has come (the canned fleet, or a link still
/// connecting): there is no age to count and no notice to expire.
fn built_at(feed: &Feed) -> Option<Instant> {
    let received = feed
        .synced_at
        .map(|at| at + Duration::from_secs(feed.synced_secs));
    let noticed = feed.notice.as_ref().map(|(_, at)| *at);
    received.max(noticed)
}

/// `status` at `now`, or the link state with nothing to count from.
fn text_at(feed: &Feed, now: Option<Instant>) -> String {
    match now {
        Some(now) => status(feed, now),
        None => link_text(feed, None),
    }
}

/// The link state in words. `now` is `None` only when `feed` has never
/// been synced, which leaves nothing to count from.
fn link_text(feed: &Feed, now: Option<Instant>) -> String {
    let n = feed.fleet.workers.len();
    let age = age(feed, now);
    let ago = minutes_text(age.as_secs_f64() / 60.0);
    let reason = match &feed.link {
        Some(Link::Down(reason)) => reason.as_str(),
        _ => "",
    };
    match health(feed, now) {
        Health::Demo => format!("ワーカー {n} · ダミーデータ"),
        Health::Connecting => "接続中…".to_owned(),
        Health::Live => format!("ワーカー {n} · 同期 {ago}前"),
        Health::Stale => format!("ワーカー {n} · 同期 {ago}前 · 古いデータ"),
        Health::Down { had_data: false } => format!("切断: {reason}"),
        // The reason can be any length and the label cuts its tail, so
        // what must stay readable comes first: the stale mark and the
        // data's age (164 px for "59 分", in a slot of up to 200 px, which
        // leaves room for the reason's first characters).
        Health::Down { had_data: true } => {
            format!("切断: 古いデータ（{ago}前）· {reason}")
        }
    }
}

/// The feed's health at `now`.
fn health(feed: &Feed, now: Option<Instant>) -> Health {
    of(
        feed.link.as_ref(),
        age(feed, now),
        !feed.fleet.workers.is_empty(),
    )
}

/// How long ago the server last answered, as of `now`; the last
/// snapshot's own count when there is no `now` or no arrival time.
fn age(feed: &Feed, now: Option<Instant>) -> Duration {
    match feed.synced_at.zip(now) {
        Some((at, now)) => now.saturating_duration_since(at),
        None => Duration::from_secs(feed.synced_secs),
    }
}

/// Brings the dot and the status label up to `now`. The setters change
/// nothing when the value is the same, so this runs on every frame.
pub(super) fn tick(tree: &mut UiTree, parts: &HeaderParts, feed: &Feed, now: Instant) {
    show(tree, parts.health, feed, Some(now));
}

/// Gives a header that is just being built the text, colour and dot of
/// `built_at`, all three from that one moment.
pub(super) fn show_built(tree: &mut UiTree, parts: HealthParts, feed: &Feed) {
    show(tree, parts, feed, built_at(feed));
}

/// Sets the dot's colour, the status text and its colour as of `now`.
fn show(tree: &mut UiTree, parts: HealthParts, feed: &Feed, now: Option<Instant>) {
    let HealthParts { dot, status: label } = parts;
    let health = health(feed, now);
    tree.update::<Container, _>(dot, |c, cx| c.set_background(cx, Some(health.dot().into())));
    let text = text_at(feed, now);
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
