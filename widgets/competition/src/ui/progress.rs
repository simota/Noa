//! The progress strip above the center pane: where the competition is in its
//! rounds, which step the current round is on, and how far each agent got.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, ProgressBar, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use competition::model::types::{Brief, JUDGE_ID, Millis, Phase, Receipt, Round, Session};

use super::{caption, gap, spacer, tag};

const BAR_WIDTH: f32 = 140.0;

/// The steps of one round, in order, as the strip names them.
const STEPS: [(Phase, &str); 4] = [
    (Phase::Briefing, "配信"),
    (Phase::Competing, "制作"),
    (Phase::Judging, "採点"),
    (Phase::Closed, "完了"),
];

/// How far one brief got, as its badge shows it.
pub(super) fn delivery(receipt: &Receipt) -> (&'static str, Tone) {
    if receipt.failed.is_some() {
        ("失敗", Tone::Critical)
    } else if receipt.replied.is_some() {
        ("提出済", Tone::Success)
    } else if receipt.started.is_some() {
        ("制作中", Tone::Accent)
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
            all_briefs(round).any(|b| {
                b.receipt.sent.is_some()
                    && b.receipt.replied.is_none()
                    && b.receipt.failed.is_none()
            })
        })
}

/// Every brief of `round`: the contestants', the judges' scorecard briefs
/// (lead first), then a panel's verdict brief.
fn all_briefs(round: &Round) -> impl Iterator<Item = &Brief> {
    round
        .briefs
        .iter()
        .chain(&round.judge_brief)
        .chain(&round.co_judge_briefs)
        .chain(&round.verdict_brief)
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
    let done = session
        .rounds
        .iter()
        .filter(|r| r.outcome.is_some())
        .count() as u32;
    (done, session.max_rounds)
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    now: Millis,
    expanded: bool,
) -> WidgetId {
    let strip = tree.insert(
        Some(host),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            gap: gap(8.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(10.0),
                bottom: length(10.0),
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
        Container::wrapping_row(8.0, 6.0).align_items(AlignItems::CENTER),
    );
    let (done, max) = rounds_done(session);
    let current = session.current_round();
    let round_label = match (current, max) {
        (Some(r), _) if session.closed && r.outcome.is_none() => {
            format!("終了 · R{} で中断", r.no)
        }
        _ if session.closed => format!("終了 · 全{}ラウンド", session.rounds.len()),
        (Some(r), Some(max)) => format!("ラウンド {} / {max}", r.no),
        (Some(r), None) => format!("ラウンド {}（上限なし）", r.no),
        (None, _) => "開始準備中".to_owned(),
    };
    tree.insert(
        Some(top),
        Label::new(round_label).font_size(14.0).weight(600),
    );
    if let Some(round) = current.filter(|_| !session.closed) {
        let phase = match round.phase {
            Phase::Briefing => "配信中".to_owned(),
            Phase::Competing => "制作中".to_owned(),
            Phase::Judging => judging_label(session, round),
            Phase::Closed => "ラウンド完了".to_owned(),
        };
        tag(tree, top, phase, Tone::Accent);
        let submitted = round
            .briefs
            .iter()
            .filter(|b| b.receipt.replied.is_some())
            .count();
        caption(
            tree,
            top,
            format!("提出 {submitted}/{}", round.briefs.len()),
        );
    }
    spacer(tree, top);
    let toggle = tree.insert(
        Some(top),
        Button::new(if expanded {
            "進捗を閉じる"
        } else {
            "進捗の詳細"
        })
        .control_size(ControlSize::Small)
        .selected(expanded),
    );
    tree.set_accessible_label(
        toggle,
        if expanded {
            "進捗の詳細を閉じる"
        } else {
            "進捗の詳細を表示"
        },
    );

    if session.waits_to_continue() {
        tag(
            tree,
            strip,
            "一時停止中 · 次のラウンドを待っています",
            Tone::Warning,
        );
    }
    if let Some(round) = current {
        if super::judge::awaits_verdict(session, round) {
            tag(tree, strip, "採点完了 · 最終講評待ち", Tone::Warning);
        }
        // Problems remain visible even while the delivery trace is collapsed.
        for brief in all_briefs(round) {
            let notice = if let Some(why) = &brief.receipt.failed {
                Some(format!("✗ {}: {why}", brief.to))
            } else {
                brief
                    .receipt
                    .rejected
                    .as_ref()
                    .filter(|_| brief.receipt.replied.is_none())
                    .map(|why| format!("↻ {}: 修正依頼中 — {why}", brief.to))
            };
            if let Some(notice) = notice {
                tree.insert(Some(strip), Label::new(notice).font_size(13.0).wrap(true));
            }
        }
    }
    if !expanded {
        return toggle;
    }

    let track = tree.insert(
        Some(strip),
        Container::wrapping_row(8.0, 6.0).align_items(AlignItems::CENTER),
    );
    if let Some(max) = max.filter(|m| *m > 0) {
        let value = done.min(max);
        tree.insert(
            Some(track),
            ProgressBar::new(BAR_WIDTH)
                .value(f64::from(value), f64::from(max))
                .control_size(ControlSize::Small),
        );
        caption(tree, track, format!("{value}/{max} ラウンド完了"));
    }
    if let Some(round) = current {
        let phases = tree.insert(Some(strip), Container::wrapping_row(6.0, 4.0));
        steps(tree, phases, session, round);
    }
    let Some(round) = current.filter(|_| !session.closed) else {
        return toggle;
    };
    let parts = tree.insert(Some(strip), Container::wrapping_row(6.0, 6.0));
    for brief in &round.briefs {
        part_chip(tree, parts, brief.to.as_str(), Some(brief), now);
    }
    if session.is_panel() {
        for judge in session.judges() {
            let brief = if judge.id == session.judge.id {
                round.judge_brief.as_ref()
            } else {
                round.co_judge_briefs.iter().find(|b| b.to == judge.id)
            };
            part_chip(tree, parts, &format!("審判 ({})", judge.id), brief, now);
        }
        if let Some(brief) = &round.verdict_brief {
            part_chip(tree, parts, &format!("講評 ({JUDGE_ID})"), Some(brief), now);
        }
    } else {
        part_chip(
            tree,
            parts,
            &format!("審判 ({JUDGE_ID})"),
            round.judge_brief.as_ref(),
            now,
        );
    }
    toggle
}

/// `採点 2/3` while a panel's round is being scored: the judges whose
/// scorecard is in, of all of them. `採点` otherwise.
fn judging_label(session: &Session, round: &Round) -> String {
    let name = STEPS[2].1;
    if !session.is_panel() || round.phase != Phase::Judging {
        return name.to_owned();
    }
    let judges = session.judges().count();
    let scored = session
        .judges()
        .filter(|j| round.scorecards.contains_key(&j.id))
        .count();
    format!("{name} {scored}/{judges}")
}

fn steps(tree: &mut UiTree, parent: WidgetId, session: &Session, round: &Round) {
    let closed = session.closed;
    let at = STEPS
        .iter()
        .position(|(phase, _)| *phase == round.phase)
        .unwrap_or(0);
    for (i, (phase, name)) in STEPS.iter().enumerate() {
        if i > 0 {
            caption(tree, parent, "›");
        }
        let judging;
        let name = if *phase == Phase::Judging {
            judging = judging_label(session, round);
            judging.as_str()
        } else {
            name
        };
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
        tag(tree, chip, "提出待ち", Tone::Neutral);
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
