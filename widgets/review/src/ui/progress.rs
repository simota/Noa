//! The progress strip above the center pane: where the review is in its
//! rounds, which step the current round is on, and how far each agent got.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Container, ControlSize, Label, ProgressBar, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use review::model::types::{Brief, Millis, Phase, Receipt, Round, Session};

use super::{caption, gap, spacer, tag};

const BAR_WIDTH: f32 = 140.0;

/// The steps of one round, in order, as the strip names them.
const STEPS: [(Phase, &str); 4] = [
    (Phase::Briefing, "配信"),
    (Phase::Reviewing, "レビュー"),
    (Phase::Consolidating, "取りまとめ"),
    (Phase::Closed, "完了"),
];

/// How far one brief got, as its badge shows it.
pub(super) fn delivery(receipt: &Receipt) -> (&'static str, Tone) {
    if receipt.failed.is_some() {
        ("失敗", Tone::Critical)
    } else if receipt.replied.is_some() {
        ("提出済", Tone::Success)
    } else if receipt.started.is_some() {
        ("レビュー中", Tone::Accent)
    } else if receipt.sent.is_some() {
        ("送信済", Tone::Warning)
    } else {
        ("待機中", Tone::Neutral)
    }
}

/// The time a brief has taken: until its reply once replied, otherwise until
/// `now` while the agent still has it. `None` before it was sent or after it
/// gave up.
pub(super) fn elapsed(receipt: &Receipt, now: Millis) -> Option<String> {
    let since = receipt.sent?;
    let until = match receipt.replied {
        Some(replied) => replied,
        None if receipt.failed.is_some() => return None,
        None => now,
    };
    Some(duration(until.saturating_sub(since)))
}

/// Whether some brief of the session's current round is still running, so
/// its elapsed time moves with the clock.
pub(super) fn has_running(session: &Session) -> bool {
    !session.closed
        && session.current_round().is_some_and(|round| {
            round.briefs.iter().chain(&round.lead_brief).any(|b| {
                b.receipt.sent.is_some()
                    && b.receipt.replied.is_none()
                    && b.receipt.failed.is_none()
            })
        })
}

fn duration(ms: Millis) -> String {
    let mins = ms / 60_000;
    if mins == 0 {
        "1分未満".to_owned()
    } else if mins < 60 {
        format!("{mins}分")
    } else {
        format!("{}時間{}分", mins / 60, mins % 60)
    }
}

/// Rounds finished, and the round limit when there is one.
fn rounds_done(session: &Session) -> (u32, Option<u32>) {
    // A round is done once the lead's consolidation of it was taken in; a
    // review ended part way through a round does not count that round.
    let done = session
        .rounds
        .iter()
        .filter(|r| r.outcome.is_some())
        .count() as u32;
    (done, Some(session.max_rounds))
}

pub(super) fn insert(tree: &mut UiTree, host: WidgetId, session: &Session, now: Millis) {
    let strip = tree.insert(
        Some(host),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            gap: gap(6.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(8.0),
                bottom: length(8.0),
            },
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(Role::SurfaceAlt),
    );

    let top = tree.insert(
        Some(strip),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let (done, max) = rounds_done(session);
    let current = session.current_round();
    let round_label = match (current, max) {
        (Some(r), _) if session.closed && r.outcome.is_none() => {
            format!("終了 · R{} で中断", r.no)
        }
        _ if session.closed => format!("終了 · 全{}ラウンド", session.rounds.len()),
        (Some(r), Some(max)) => format!("ラウンド {} / {max} · {}", r.no, r.kind.label()),
        (Some(r), None) => format!("ラウンド {}（上限なし）", r.no),
        (None, _) => "開始準備中".to_owned(),
    };
    tree.insert(
        Some(top),
        Label::new(round_label).font_size(13.0).weight(600),
    );
    if let Some(max) = max.filter(|m| *m > 0) {
        let value = done.min(max);
        tree.insert(
            Some(top),
            ProgressBar::new(BAR_WIDTH)
                .value(f64::from(value), f64::from(max))
                .control_size(ControlSize::Small),
        );
        caption(tree, top, format!("{value}/{max} 完了"));
    }
    spacer(tree, top);
    if let Some(round) = current {
        steps(tree, top, round, session.closed);
    }

    let Some(round) = current.filter(|_| !session.closed) else {
        return;
    };
    let mid = tree.insert(
        Some(strip),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let submitted = round
        .briefs
        .iter()
        .filter(|b| b.receipt.replied.is_some())
        .count();
    let total = round.briefs.len();
    caption(tree, mid, format!("R{} 返答", round.no));
    if total > 0 {
        tree.insert(
            Some(mid),
            ProgressBar::new(BAR_WIDTH)
                .value(submitted as f64, total as f64)
                .control_size(ControlSize::Small),
        );
    }
    caption(tree, mid, format!("{submitted}/{total}"));

    let parts = tree.insert(Some(strip), Container::wrapping_row(6.0, 6.0));
    for brief in &round.briefs {
        part_chip(tree, parts, brief.to.as_str(), Some(brief), now);
    }
    match &round.lead_brief {
        Some(brief) => part_chip(tree, parts, "リード", Some(brief), now),
        None => part_chip(tree, parts, "リード", None, now),
    }
    if session.waits_to_continue() {
        let row = tree.insert(
            Some(strip),
            Container::row().align_items(AlignItems::CENTER),
        );
        tag(tree, row, "停止中（次のラウンドへ で再開）", Tone::Warning);
    }

    for brief in round.briefs.iter().chain(&round.lead_brief) {
        if let Some(why) = &brief.receipt.failed {
            caption(tree, strip, format!("✗ {}: {why}", brief.to));
        } else if let Some(why) = brief
            .receipt
            .rejected
            .as_ref()
            .filter(|_| brief.receipt.replied.is_none())
        {
            caption(tree, strip, format!("↻ {}: 修正依頼中 — {why}", brief.to));
        }
    }
}

fn steps(tree: &mut UiTree, parent: WidgetId, round: &Round, closed: bool) {
    let at = STEPS
        .iter()
        .position(|(phase, _)| *phase == round.phase)
        .unwrap_or(0);
    for (i, (_, name)) in STEPS.iter().enumerate() {
        if i > 0 {
            caption(tree, parent, "›");
        }
        let (text, tone) = if i < at || round.phase == Phase::Closed {
            (format!("✓ {name}"), Tone::Success)
        } else if i == at && closed {
            (format!("中断 {name}"), Tone::Warning)
        } else if i == at {
            (format!("● {name}"), Tone::Accent)
        } else {
            ((*name).to_owned(), Tone::Neutral)
        };
        tag(tree, parent, text, tone);
    }
}

fn part_chip(tree: &mut UiTree, parent: WidgetId, name: &str, brief: Option<&Brief>, now: Millis) {
    let chip = tree.insert(
        Some(parent),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(6.0)
            .padding(6.0)
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(6.0),
    );
    tree.insert(
        Some(chip),
        Label::new(name.to_owned()).font_size(12.0).weight(600),
    );
    let Some(brief) = brief else {
        tag(tree, chip, "取りまとめ待ち", Tone::Neutral);
        return;
    };
    let (label, tone) = delivery(&brief.receipt);
    tag(tree, chip, label, tone);
    if let Some(time) = elapsed(&brief.receipt, now) {
        caption(tree, chip, time);
    }
    if brief.receipt.retries > 0 {
        caption(tree, chip, format!("再送{}回", brief.receipt.retries));
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(sent: Option<Millis>, replied: Option<Millis>, failed: bool) -> Receipt {
        Receipt {
            sent,
            started: sent,
            replied,
            failed: failed.then(|| "gone".to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn a_review_ended_part_way_through_a_round_counts_only_its_consolidated_rounds() {
        let mut session = crate::demo::held();
        let mut second = session.rounds[0].clone();
        second.no = 2;
        second.outcome = None;
        second.phase = Phase::Reviewing;
        session.rounds.push(second);
        session.closed = true;
        assert_eq!(rounds_done(&session), (1, Some(session.max_rounds)));
        let report = review::model::export::report_markdown(&session);
        assert!(
            report.contains("- R2（独立レビュー）: （中断）"),
            "{report}"
        );
    }

    #[test]
    fn elapsed_runs_to_now_and_stops_at_the_reply() {
        let running = receipt(Some(0), None, false);
        assert_eq!(elapsed(&running, 30_000).as_deref(), Some("1分未満"));
        assert_eq!(elapsed(&running, 5 * 60_000).as_deref(), Some("5分"));
        assert_eq!(elapsed(&running, 125 * 60_000).as_deref(), Some("2時間5分"));
        let replied = receipt(Some(0), Some(3 * 60_000), false);
        assert_eq!(elapsed(&replied, 99 * 60_000).as_deref(), Some("3分"));
        assert_eq!(elapsed(&receipt(None, None, false), 1), None);
        assert_eq!(elapsed(&receipt(Some(0), None, true), 1), None);
    }

    #[test]
    fn only_a_sent_unanswered_brief_counts_as_running() {
        let mut session = crate::demo::session();
        assert!(!has_running(&session));
        session.closed = false;
        let last = session.rounds.len() - 1;
        session.rounds[last].briefs[0].receipt = receipt(Some(0), None, false);
        assert!(has_running(&session));
        session.rounds[last].briefs[0].receipt = receipt(Some(0), None, true);
        assert!(!has_running(&session));
    }
}
