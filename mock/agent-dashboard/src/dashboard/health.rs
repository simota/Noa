//! Connection health in the header (AC6). U1 moves today's status text here
//! and draws the dot in gray; U2 fills in the health table, the dot and
//! status colours and the Down-keeps-data rule.

use std::time::Instant;

use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{UiTree, WidgetId};

use crate::fleet::{Fleet, minutes_text};
use crate::live::Link;

use super::header::HeaderParts;
use super::style::GRAY;
use super::{Feed, NOTICE_FOR};

/// The dot before the status label, and the label.
#[derive(Debug, Clone, Copy)]
pub(super) struct HealthParts {
    pub(super) dot: WidgetId,
    pub(super) status: WidgetId,
}

/// The header's status text at `now`: a fresh notice, else the link state.
pub(super) fn status(feed: &Feed, now: Instant) -> String {
    if let Some((notice, at)) = &feed.notice
        && now.saturating_duration_since(*at) < NOTICE_FOR
    {
        return notice.clone();
    }
    let n = feed.fleet.workers.len();
    match &feed.link {
        None => format!("ワーカー {n} · ダミーデータ"),
        Some(Link::Connecting) => "接続中…".to_owned(),
        Some(Link::Live) => format!(
            "ワーカー {n} · 同期 {}前",
            minutes_text(age(feed, now) / 60.0)
        ),
        Some(Link::Down(reason)) => format!("切断: {reason}"),
    }
}

/// Seconds since the server last answered, as of `now`.
fn age(feed: &Feed, now: Instant) -> f64 {
    feed.synced_at.map_or(feed.synced_secs as f64, |at| {
        now.saturating_duration_since(at).as_secs_f64()
    })
}

/// Brings the dot and the status label up to `now`. The setters change
/// nothing when the value is the same, so this runs on every frame.
pub(super) fn tick(tree: &mut UiTree, parts: &HeaderParts, feed: &Feed, now: Instant) {
    let HealthParts { dot, status: label } = parts.health;
    tree.update::<Container, _>(dot, |c, cx| c.set_background(cx, Some(GRAY.into())));
    let text = status(feed, now);
    tree.update::<Label, _>(label, |l, cx| l.set_text(cx, text));
}

/// The fleet to show after a snapshot: U2 keeps `old` when a `Down` link
/// brings an empty fleet.
pub(super) fn keep_on_down(_old: &Fleet, new: Fleet, _link: &Link) -> Fleet {
    new
}
