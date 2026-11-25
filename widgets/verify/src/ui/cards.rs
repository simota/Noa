//! The left pane: the target card (returned, so the screen can add
//! スナップショットを開く to it), one card per verifier (strategy, model,
//! this round's receipt and results, its pane and worktree), and the
//! judge's card.

use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone, Truncation};
use noa_gpui_core::{UiTree, WidgetId};

use verify::model::types::{
    BriefKind, Isolation, Millis, Outcome, Participant, Session, WorktreeState, short_id,
};

use super::backlinks::Row;
use super::progress::{delivery, elapsed};
use super::{Action, caption, card, heading, keep_scroll, note, scroll_column, spacer, tag};

/// `✓ 3 ✗ 1 ⊘ 1` for `part`'s results of the current round (`–` for those
/// it did not test).
pub(super) fn round_tally(session: &Session, part: &Participant) -> String {
    let Some(round) = session.current_round() else {
        return String::new();
    };
    let count = |outcome: Outcome| {
        session
            .results
            .iter()
            .filter(|r| r.round == round.no && r.part == part.id && r.outcome == outcome)
            .count()
    };
    let mut out = format!(
        "{} {} {} {} {} {}",
        Outcome::Pass.mark(),
        count(Outcome::Pass),
        Outcome::Fail.mark(),
        count(Outcome::Fail),
        Outcome::Blocked.mark(),
        count(Outcome::Blocked)
    );
    let untested = count(Outcome::NotTested);
    if untested > 0 {
        out.push_str(&format!(" {} {untested}", Outcome::NotTested.mark()));
    }
    out
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    live: bool,
    now: Millis,
    actions: &mut Vec<(WidgetId, Action)>,
) -> WidgetId {
    heading(tree, host, "対象とパネル");
    let list = scroll_column(tree, host, 8.0);
    keep_scroll(tree, list, session, "cards");

    let m = &session.manifest;
    let target = card(tree, list, false);
    let head = tree.insert(
        Some(target),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    tag(tree, head, session.target.kind.label(), Tone::Neutral);
    tag(
        tree,
        head,
        session.isolation.label(),
        match session.isolation {
            Isolation::Worktree => Tone::Accent,
            Isolation::Shared => Tone::Warning,
        },
    );
    if !m.omitted.is_empty() {
        tag(
            tree,
            head,
            format!("対象外 {}", m.omitted.len()),
            Tone::Warning,
        );
    }
    tree.insert(
        Some(target),
        Label::new(m.label.clone())
            .font_size(12.5)
            .weight(600)
            .wrap(true),
    );
    if let Some(head_sha) = &m.head_sha {
        let head = format!(
            "{} {}",
            m.head_ref.as_deref().unwrap_or("作業ツリー"),
            short_id(head_sha, 8)
        );
        caption(
            tree,
            target,
            match &m.base_sha {
                Some(base) => format!(
                    "{} {} … {head}",
                    m.base_ref.as_deref().unwrap_or(""),
                    short_id(base, 8)
                ),
                None => head,
            },
        );
    }
    let mut stat = format!("{} ファイル", m.files.len());
    if m.additions() > 0 || m.deletions() > 0 {
        stat.push_str(&format!("  +{} −{}", m.additions(), m.deletions()));
    }
    caption(tree, target, stat);
    if let Some(spec) = &session.spec {
        caption(tree, target, format!("仕様: {}", spec.path));
    }

    let round = session.current_round();
    for part in &session.verifiers {
        let c = card(tree, list, false);
        let top = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(top),
            Label::new(part.id.to_string()).font_size(13.0).weight(600),
        );
        if let Some(strategy) = &part.strategy {
            tag(tree, top, strategy.label(), Tone::Accent);
        }
        let line = tree.insert(
            Some(c),
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        caption(tree, line, setup_line(part));
        spacer(tree, line);
        receipt_tag(tree, line, session, part, now);
        if let Some(round) = round {
            let mut text = format!("R{} の結果 {}", round.no, round_tally(session, part));
            let checks = session
                .cross_checks
                .iter()
                .filter(|c| c.round == round.no && c.by == part.id)
                .count();
            if checks > 0 {
                text.push_str(&format!(" · 追試 {checks}"));
            }
            caption(tree, c, text);
        }
        if let Some(summary) = round
            .and_then(|r| r.replies.get(&part.id))
            .map(|n| n.summary.clone())
        {
            tree.insert(
                Some(c),
                Label::new(summary)
                    .font_size(11.5)
                    .wrap(true)
                    .max_lines(3)
                    .truncation(Truncation::Tail),
            );
        }
        let worktree = session.worktrees.get(&part.id);
        match worktree.map(|w| &w.state) {
            Some(WorktreeState::Removed) => caption(tree, c, "worktree: 削除済"),
            Some(WorktreeState::Failed { reason }) => {
                tag(tree, c, format!("worktree 失敗: {reason}"), Tone::Critical)
            }
            _ => {}
        }
        let buttons = tree.insert(Some(c), Container::wrapping_row(6.0, 4.0));
        let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
        if live {
            let open = tree.insert(Some(buttons), small("ペインを開く"));
            actions.push((open, Action::FocusPane(part.id.clone())));
        }
        if worktree.is_some_and(|w| w.state == WorktreeState::Made) {
            let open = tree.insert(
                Some(buttons),
                small("ワークツリーを開く").tooltip(
                    worktree
                        .map(|w| w.path.display().to_string())
                        .unwrap_or_default(),
                ),
            );
            tree.set_accessible_label(open, format!("{} のワークツリーを Finder で開く", part.id));
            actions.push((open, Action::OpenWorktree(part.id.clone())));
        }
    }

    let judge = card(tree, list, false);
    let top = tree.insert(
        Some(judge),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    tree.insert(Some(top), Label::new("判定役").font_size(13.0).weight(600));
    let line = tree.insert(
        Some(judge),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    caption(tree, line, setup_line(&session.judge));
    spacer(tree, line);
    receipt_tag(tree, line, session, &session.judge, now);
    note(
        tree,
        judge,
        "受け入れ基準を作り、毎ラウンド各基準を判定し、最後に総合判定を出します（自分では何も実行しません）",
    );
    if live {
        let open = tree.insert(
            Some(judge),
            Button::new("ペインを開く").control_size(ControlSize::Small),
        );
        actions.push((open, Action::FocusPane(session.judge.id.clone())));
    }
    target
}

/// The target card [`insert`] put under `host`: the first card of the
/// list that follows the heading.
pub(super) fn target_card(tree: &UiTree, host: WidgetId) -> Option<WidgetId> {
    let scroll = *tree.children(host).get(1)?;
    let list = *tree.children(scroll).first()?;
    tree.children(list).first().copied()
}

/// The target card's link rows (L1-5): 「タスク: <badge> <title>」 and
/// 「対象の版: <short id>」, each with 開く when its record can be found.
pub(super) fn insert_links(
    tree: &mut UiTree,
    target: WidgetId,
    rows: &[Row],
    actions: &mut Vec<(WidgetId, Action)>,
) {
    for row in rows {
        let line = tree.insert(
            Some(target),
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        caption(tree, line, format!("{}:", row.kind));
        if let Some(badge) = row.badge {
            tag(tree, line, badge, Tone::Neutral);
        }
        tree.insert(
            Some(line),
            Label::new(row.text.clone())
                .font_size(11.5)
                .max_lines(1)
                .truncation(Truncation::Tail),
        );
        spacer(tree, line);
        if let Some((name, nav)) = &row.open {
            let open = tree.insert(
                Some(line),
                Button::new("開く").control_size(ControlSize::Small),
            );
            tree.set_accessible_label(open, *name);
            actions.push((open, Action::OpenLink(nav.clone())));
        }
    }
}

fn setup_line(part: &Participant) -> String {
    match part.setup.label() {
        Some(setup) => format!("{} · {setup}", part.kind.command()),
        None => part.kind.command().to_owned(),
    }
}

fn receipt_tag(
    tree: &mut UiTree,
    parent: WidgetId,
    session: &Session,
    part: &Participant,
    now: Millis,
) {
    let brief = match session.current_round() {
        Some(round) => round.brief(&part.id, BriefKind::Work),
        // Before round 1 only the judge has a brief: the criteria.
        None if part.id == session.judge.id => session.criteria_brief.as_ref(),
        None => None,
    };
    let Some(brief) = brief else {
        if !session.closed {
            tag(tree, parent, "待機中", Tone::Neutral);
        }
        return;
    };
    let (label, tone) = delivery(&brief.receipt);
    tag(tree, parent, label, tone);
    if let Some(time) = elapsed(&brief.receipt, now).filter(|_| !session.closed) {
        caption(tree, parent, time);
    }
}
