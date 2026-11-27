//! The progress strip above the center pane: where the verification is in
//! its rounds, which step it is on (基準策定 → 配信 → 検証 → 証跡収集 → 判定),
//! how far each agent got, and how many re-runs have an outcome.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, ProgressBar, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use verify::model::types::{
    Brief, BriefKind, Evidence, EvidenceBody, Isolation, Millis, Phase, Receipt, Rerun, Round,
    Session,
};

use super::{Action, caption, gap, spacer, tag};

const BAR_WIDTH: f32 = 140.0;

/// The steps of a verification as the strip names them: the criteria
/// phase, then one round's phases.
const STEPS: [(Phase, &str); 5] = [
    (Phase::Criteria, "基準策定"),
    (Phase::Briefing, "配信"),
    (Phase::Verifying, "検証"),
    (Phase::Ingesting, "証跡収集"),
    (Phase::Ruling, "判定"),
];

/// How far one brief got, as its badge shows it.
pub(super) fn delivery(receipt: &Receipt) -> (&'static str, Tone) {
    if receipt.failed.is_some() {
        ("失敗", Tone::Critical)
    } else if receipt.replied.is_some() {
        ("提出済", Tone::Success)
    } else if receipt.started.is_some() {
        ("作業中", Tone::Accent)
    } else if receipt.sent.is_some() {
        ("送信済", Tone::Warning)
    } else {
        ("待機中", Tone::Neutral)
    }
}

/// The time a brief has taken: until its reply once replied, otherwise
/// until `now` while the agent still has it. `None` before it was sent or
/// after it gave up.
pub(super) fn elapsed(receipt: &Receipt, now: Millis) -> Option<String> {
    let since = receipt.sent?;
    let until = match receipt.replied {
        Some(replied) => replied,
        None if receipt.failed.is_some() => return None,
        None => now,
    };
    Some(duration(until.saturating_sub(since)))
}

fn running(brief: &Brief) -> bool {
    brief.receipt.sent.is_some()
        && brief.receipt.replied.is_none()
        && brief.receipt.failed.is_none()
}

/// Whether some brief of the verification's current step is still
/// running, so its elapsed time moves with the clock.
pub(super) fn has_running(session: &Session) -> bool {
    if session.closed {
        return false;
    }
    match session.current_round() {
        Some(round) => round
            .briefs
            .iter()
            .chain(&round.rerun_briefs)
            .chain(&round.judge_brief)
            .any(running),
        None => session.criteria_brief.as_ref().is_some_and(running),
    }
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

/// Rounds ruled, of the round limit.
fn rounds_done(session: &Session) -> (u32, u32) {
    // A round is done once the judge's ruling of it was taken in; a
    // verification ended part way through a round does not count it.
    let done = session
        .rounds
        .iter()
        .filter(|r| r.outcome.is_some())
        .count() as u32;
    (done, session.max_rounds)
}

/// The round's re-runs: how many items have an outcome, of how many were
/// asked for (every `rerun: true` command of the round, a verifier's
/// whose script could not be written among them: it ends `not_run`
/// without a request). None in 共有 mode, which runs none.
pub(super) fn reruns(session: &Session, round: &Round) -> (usize, usize) {
    if session.isolation == Isolation::Shared {
        return (0, 0);
    }
    let asked: Vec<&Evidence> = session
        .evidence
        .iter()
        .filter(|e| {
            matches!(e.body, EvidenceBody::Command { rerun: true, .. })
                && e.key.split().is_some_and(|(_, r, _)| r == round.no)
        })
        .collect();
    let done = asked.iter().filter(|e| e.rerun.is_some()).count();
    (done, asked.len())
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    now: Millis,
    expanded: bool,
    actions: &mut Vec<(WidgetId, Action)>,
) {
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

    // Wraps, so the steps move under the round label in a narrow pane
    // instead of running past its edge.
    let top = tree.insert(
        Some(strip),
        Container::wrapping_row(8.0, 6.0).align_items(AlignItems::CENTER),
    );
    let (done, max) = rounds_done(session);
    let current = session.current_round();
    let round_label = match current {
        Some(r) if session.closed && r.outcome.is_none() => {
            format!("終了 · R{} で中断", r.no)
        }
        _ if session.closed && session.rounds.is_empty() => "終了 · 基準の段階で中断".to_owned(),
        _ if session.closed => format!("終了 · 全{}ラウンド", session.rounds.len()),
        Some(r) => format!("ラウンド {} / {max} · {}", r.no, r.kind.label()),
        None if session.phase() == Phase::Confirming => "基準確認待ち".to_owned(),
        None => "基準策定中".to_owned(),
    };
    tree.insert(
        Some(top),
        Label::new(round_label)
            .font_size(crate::appearance::font(13.0))
            .weight(600),
    );
    if max > 0 {
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
    let details = tree.insert(
        Some(top),
        Button::new(if expanded {
            "進捗の内訳を閉じる"
        } else {
            "進捗の内訳"
        })
        .control_size(ControlSize::Small)
        .selected(expanded),
    );
    actions.push((details, Action::ToggleProgress));
    if expanded {
        let phases = tree.insert(Some(strip), Container::wrapping_row(6.0, 4.0));
        steps(tree, phases, session);
    } else if !session.closed {
        caption(tree, strip, session.phase().label());
    }

    if session.closed {
        return;
    }
    let parts = tree.insert(Some(strip), Container::wrapping_row(6.0, 6.0));
    match current {
        Some(round) => {
            let submitted = round
                .briefs
                .iter()
                .filter(|b| b.receipt.replied.is_some())
                .count();
            let total = round.briefs.len();
            let mid = tree.insert(
                Some(strip),
                Container::wrapping_row(8.0, 4.0).align_items(AlignItems::CENTER),
            );
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
            let (reran, asked) = reruns(session, round);
            if asked > 0 {
                tag(
                    tree,
                    mid,
                    format!("再実行 {reran} / {asked}"),
                    if reran == asked {
                        Tone::Neutral
                    } else {
                        Tone::Accent
                    },
                );
            }
            let mut warnings = [0; 3];
            for item in session
                .evidence
                .iter()
                .filter(|e| e.key.split().is_some_and(|(_, r, _)| r == round.no))
            {
                match item.rerun {
                    Some(Rerun::Differed { .. }) => warnings[0] += 1,
                    Some(Rerun::TimedOut { .. }) => warnings[1] += 1,
                    Some(Rerun::NotRun { .. }) => warnings[2] += 1,
                    _ => {}
                }
            }
            for (label, count) in ["相違", "時間切れ", "未実行"].into_iter().zip(warnings)
            {
                if count > 0 {
                    tag(tree, mid, format!("再実行{label} {count}"), Tone::Warning);
                }
            }
            if expanded {
                for brief in &round.briefs {
                    let rerun = round.brief(&brief.to, BriefKind::Rerun);
                    part_chip(tree, parts, brief.to.as_str(), Some(brief), rerun, now);
                }
                part_chip(tree, parts, "判定役", round.judge_brief.as_ref(), None, now);
            }
        }
        None if expanded => part_chip(
            tree,
            parts,
            "判定役",
            session.criteria_brief.as_ref(),
            None,
            now,
        ),
        None => {}
    }
    if session.waits_to_continue() {
        let row = tree.insert(
            Some(strip),
            Container::row().align_items(AlignItems::CENTER),
        );
        tag(tree, row, "停止中（次のラウンドへ で再開）", Tone::Warning);
    } else if session.phase() == Phase::Confirming {
        let row = tree.insert(
            Some(strip),
            Container::row().align_items(AlignItems::CENTER),
        );
        tag(
            tree,
            row,
            "基準確認待ち（基準を確定して検証を開始）",
            Tone::Warning,
        );
    }

    let briefs: Vec<&Brief> = match current {
        Some(round) => round
            .briefs
            .iter()
            .chain(&round.rerun_briefs)
            .chain(&round.judge_brief)
            .collect(),
        None => session.criteria_brief.iter().collect(),
    };
    for brief in briefs {
        let who = if brief.to.as_str() == verify::model::types::JUDGE_ID {
            "判定役".to_owned()
        } else {
            brief.to.to_string()
        };
        if let Some(why) = &brief.receipt.failed {
            caption(tree, strip, format!("✗ {who}: {why}"));
        } else if let Some(why) = brief
            .receipt
            .rejected
            .as_ref()
            .filter(|_| brief.receipt.replied.is_none())
        {
            caption(tree, strip, format!("↻ {who}: 修正依頼中 — {why}"));
        }
    }
}

/// Where in [`STEPS`] the verification is, and whether everything up to
/// and including it is done.
fn step_at(session: &Session) -> (usize, bool) {
    let index = |phase: Phase| STEPS.iter().position(|(p, _)| *p == phase).unwrap_or(0);
    match session.current_round() {
        // The round's own close: a held verification shows it checked.
        Some(round) if round.phase == Phase::Closed => (STEPS.len() - 1, true),
        Some(round) => (index(round.phase), false),
        None if session.frozen_at.is_some() => (index(Phase::Briefing), false),
        None => (0, false),
    }
}

fn steps(tree: &mut UiTree, parent: WidgetId, session: &Session) {
    let (at, all_done) = step_at(session);
    for (i, (_, name)) in STEPS.iter().enumerate() {
        if i > 0 {
            caption(tree, parent, "›");
        }
        let (text, tone) = if i < at || all_done {
            (format!("✓ {name}"), Tone::Success)
        } else if i == at && session.closed {
            (format!("中断 {name}"), Tone::Warning)
        } else if i == at && session.phase() == Phase::Confirming {
            (format!("● {name}（確認待ち）"), Tone::Warning)
        } else if i == at {
            (format!("● {name}"), Tone::Accent)
        } else {
            ((*name).to_owned(), Tone::Neutral)
        };
        tag(tree, parent, text, tone);
    }
}

fn part_chip(
    tree: &mut UiTree,
    parent: WidgetId,
    name: &str,
    brief: Option<&Brief>,
    rerun: Option<&Brief>,
    now: Millis,
) {
    let chip = tree.insert(
        Some(parent),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(6.0))
            .padding(crate::appearance::space(6.0))
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(6.0),
    );
    tree.insert(
        Some(chip),
        Label::new(name.to_owned())
            .font_size(crate::appearance::font(12.0))
            .weight(600),
    );
    let Some(brief) = brief else {
        tag(tree, chip, "判定待ち", Tone::Neutral);
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
    if let Some(rerun) = rerun {
        let (label, tone) = match delivery(&rerun.receipt) {
            ("提出済", _) => ("再実行済", Tone::Success),
            ("失敗", tone) => ("再実行失敗", tone),
            (_, tone) if rerun.receipt.sent.is_some() => ("再実行中", tone),
            (_, tone) => ("再実行待ち", tone),
        };
        tag(tree, chip, label, tone);
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
    fn a_verification_ended_part_way_through_a_round_counts_only_its_ruled_rounds() {
        let mut session = crate::demo::held();
        let mut second = session.rounds[0].clone();
        second.no = 2;
        second.outcome = None;
        second.phase = Phase::Verifying;
        session.rounds.push(second);
        session.closed = true;
        assert_eq!(rounds_done(&session), (1, session.max_rounds));
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
        // Before round 1, the judge's criteria brief is what runs.
        let mut drafting = crate::demo::criteria_held();
        assert!(!has_running(&drafting));
        drafting.criteria_brief.as_mut().unwrap().receipt = receipt(Some(0), None, false);
        assert!(has_running(&drafting));
    }

    #[test]
    fn the_re_runs_of_a_round_count_those_with_an_outcome() {
        let session = crate::demo::session();
        let (done, asked) = reruns(&session, &session.rounds[0]);
        assert_eq!((done, asked), (9, 9));
        let mut session = session;
        let key = session.rounds[0].reruns.values().next().unwrap().items[0]
            .key
            .clone();
        session.evidence_item_mut(&key).unwrap().rerun = None;
        assert_eq!(reruns(&session, &session.rounds[0]), (8, 9));
    }
}
