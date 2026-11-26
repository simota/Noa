//! The 証跡 tab: every evidence item, filtered by who gave it (a verifier
//! or あなた), round, kind, criterion and re-run outcome, shown as 一覧 or
//! as ギャラリー (media only, grouped by criterion). Media are shown as
//! placeholder cards only (L1-5a): the window draws no picture and writes
//! no thumbnail; 開く hands the stored copy to the system viewer or player.
//! Also the two parts that stay built across rebuilds: the read-only log
//! view (開く on a command's log) and the 証跡を追加 form.

use std::path::PathBuf;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, Container, ControlSize, Label, Pressable, TextArea, TextField, TextView, Tone,
    Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use verify::model::types::{
    Evidence, EvidenceBody, EvidenceKey, MAX_CAPTION, MAX_MARKS, Mark, Media, Rerun, RoundNo,
    Session, Subject, USER_EVIDENCE_OWNER,
};

use super::defects::filter_row;
use super::{Action, caption, fill, keep_scroll, note, scroll_column, spacer, tag};

/// What kind of evidence an item is, as the filter names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Command,
    Image,
    Video,
    File,
    Observation,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Command,
        Kind::Image,
        Kind::Video,
        Kind::File,
        Kind::Observation,
    ];

    pub fn of(item: &Evidence) -> Kind {
        match &item.body {
            EvidenceBody::Command { .. } => Kind::Command,
            EvidenceBody::File { media: Some(m), .. } if m.is_video() => Kind::Video,
            EvidenceBody::File { media: Some(_), .. } => Kind::Image,
            EvidenceBody::File { media: None, .. } => Kind::File,
            EvidenceBody::Observation { .. } => Kind::Observation,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Command => "コマンド",
            Kind::Image => "画像",
            Kind::Video => "動画",
            Kind::File => "ファイル",
            Kind::Observation => "観察",
        }
    }
}

/// How an item's re-run ended, as the filter names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RerunKind {
    Matched,
    Differed,
    TimedOut,
    NotRun,
    /// No re-run outcome (not marked for one, not a command, or waiting).
    None,
}

impl RerunKind {
    pub const ALL: [RerunKind; 5] = [
        RerunKind::Matched,
        RerunKind::Differed,
        RerunKind::TimedOut,
        RerunKind::NotRun,
        RerunKind::None,
    ];

    pub fn of(item: &Evidence) -> RerunKind {
        match &item.rerun {
            Some(Rerun::Matched { .. }) => RerunKind::Matched,
            Some(Rerun::Differed { .. }) => RerunKind::Differed,
            Some(Rerun::TimedOut { .. }) => RerunKind::TimedOut,
            Some(Rerun::NotRun { .. }) => RerunKind::NotRun,
            None => RerunKind::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RerunKind::Matched => "↻ 一致",
            RerunKind::Differed => "≠ 相違",
            RerunKind::TimedOut => "⏱ 時間切れ",
            RerunKind::NotRun => "– 未実行",
            RerunKind::None => "再実行なし",
        }
    }
}

/// One filter value (each group takes at most one; `None` clears it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// A verifier's id, or [`USER_EVIDENCE_OWNER`] for あなた.
    Owner(Option<String>),
    Round(Option<RoundNo>),
    Kind(Option<Kind>),
    Criterion(Option<String>),
    Rerun(Option<RerunKind>),
}

/// What the evidence tab shows; `None` lets every value through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    pub owner: Option<String>,
    pub round: Option<RoundNo>,
    pub kind: Option<Kind>,
    pub criterion: Option<String>,
    pub rerun: Option<RerunKind>,
}

/// A result or report reference opened temporarily. The ordinary evidence
/// filters remain intact and are restored when this view is left.
pub(super) struct Focus {
    pub title: String,
    pub keys: Vec<EvidenceKey>,
}

impl Filters {
    pub(super) fn set(&mut self, filter: Filter) {
        match filter {
            Filter::Owner(v) => self.owner = v,
            Filter::Round(v) => self.round = v,
            Filter::Kind(v) => self.kind = v,
            Filter::Criterion(v) => self.criterion = v,
            Filter::Rerun(v) => self.rerun = v,
        }
    }

    fn is_on(&self, filter: &Filter) -> bool {
        match filter {
            Filter::Owner(v) => &self.owner == v,
            Filter::Round(v) => &self.round == v,
            Filter::Kind(v) => &self.kind == v,
            Filter::Criterion(v) => &self.criterion == v,
            Filter::Rerun(v) => &self.rerun == v,
        }
    }
}

/// Who gave `item`: the verifier's id, or [`USER_EVIDENCE_OWNER`].
pub(super) fn owner(item: &Evidence) -> String {
    item.key.split().map_or_else(
        || USER_EVIDENCE_OWNER.to_owned(),
        |(part, _, _)| part.to_string(),
    )
}

/// `claude`, or あなた for what the person attached.
pub(super) fn owner_label(item: &Evidence) -> String {
    match item.key.split() {
        Some((part, _, _)) => part.to_string(),
        None => "あなた".to_owned(),
    }
}

/// The round `item` belongs to: its reply's, or the one current when the
/// person attached it.
pub(super) fn round_of(item: &Evidence) -> RoundNo {
    item.attached
        .map(|a| a.round)
        .or_else(|| item.key.split().map(|(_, round, _)| round))
        .unwrap_or(0)
}

/// Whether `item` may be shown as evidence for `criterion` where it is
/// cited: media whose `shows` names criteria is evidence only for those
/// (L1-5a), whoever cites it.
pub(super) fn shows_allow(item: &Evidence, criterion: &str) -> bool {
    match &item.body {
        EvidenceBody::File { shows, .. } if !shows.is_empty() => {
            shows.iter().any(|s| s == criterion)
        }
        _ => true,
    }
}

/// Whether `item` is evidence about `criterion`: media whose `shows` names
/// it, or (without `shows`) cited by a result on it, a cross-check on a
/// result on it, or a ruling on it.
pub(super) fn relates(session: &Session, item: &Evidence, criterion: &str) -> bool {
    if let EvidenceBody::File { shows, .. } = &item.body
        && !shows.is_empty()
    {
        return shows.iter().any(|s| s == criterion);
    }
    let key = &item.key;
    session
        .results
        .iter()
        .any(|r| r.criterion == criterion && r.evidence.contains(key))
        || session.cross_checks.iter().any(|c| {
            matches!(&c.subject, Subject::Result { criterion: on, .. } if on == criterion)
                && c.evidence.contains(key)
        })
        || session
            .rulings
            .get(criterion)
            .is_some_and(|all| all.iter().any(|r| r.evidence.contains(key)))
}

/// Whether `filters` let `item` through.
pub(super) fn passes(session: &Session, filters: &Filters, item: &Evidence) -> bool {
    filters.owner.as_ref().is_none_or(|o| *o == owner(item))
        && filters.round.is_none_or(|r| r == round_of(item))
        && filters.kind.is_none_or(|k| k == Kind::of(item))
        && filters
            .criterion
            .as_ref()
            .is_none_or(|c| relates(session, item, c))
        && filters.rerun.is_none_or(|r| r == RerunKind::of(item))
}

/// The items `filters` let through, in the order they were recorded.
pub(super) fn shown<'a>(session: &'a Session, filters: &Filters) -> Vec<&'a Evidence> {
    session
        .evidence
        .iter()
        .filter(|e| passes(session, filters, e))
        .collect()
}

/// `1,515 バイト`.
pub(super) fn bytes(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    format!("{out} バイト")
}

/// A mark's time as L1-5a writes it: `00:04`, `00:06.5`, `1:02:03`.
pub(super) fn mark_time(t_ms: u64) -> String {
    let (secs, ms) = (t_ms / 1000, t_ms % 1000);
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    let mut out = if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    };
    if ms > 0 {
        let frac = format!("{ms:03}");
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
    out
}

/// What a re-run gave, in words: `↻ 再実行も 101`.
pub(super) fn rerun_text(item: &Evidence) -> Option<String> {
    Some(match item.rerun.as_ref()? {
        Rerun::Matched { exit, .. } => format!("↻ 再実行も {exit}（一致）"),
        Rerun::Differed { exit, .. } => match exit {
            Some(exit) => format!("≠ 再実行は {exit}（相違）"),
            None => "≠ 再実行は終了コードなし（相違）".to_owned(),
        },
        Rerun::TimedOut { .. } => "⏱ 再実行が時間切れ".to_owned(),
        Rerun::NotRun { why, .. } => format!("– 再実行されず: {why}"),
    })
}

fn rerun_tone(item: &Evidence) -> Tone {
    match &item.rerun {
        Some(Rerun::Matched { .. }) => Tone::Success,
        Some(Rerun::Differed { .. }) => Tone::Critical,
        Some(Rerun::TimedOut { .. }) => Tone::Warning,
        Some(Rerun::NotRun { .. }) | None => Tone::Neutral,
    }
}

fn chip(
    tree: &mut UiTree,
    parent: WidgetId,
    label: String,
    filters: &Filters,
    filter: Filter,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let on = filters.is_on(&filter);
    let id = tree.insert(
        Some(parent),
        Button::new(label)
            .control_size(ControlSize::Small)
            .selected(on),
    );
    // Pressed again, a chip that is on lets its group go.
    let action = if on {
        Action::FilterEvidence(match filter {
            Filter::Owner(_) => Filter::Owner(None),
            Filter::Round(_) => Filter::Round(None),
            Filter::Kind(_) => Filter::Kind(None),
            Filter::Criterion(_) => Filter::Criterion(None),
            Filter::Rerun(_) => Filter::Rerun(None),
        })
    } else {
        Action::FilterEvidence(filter)
    };
    actions.push((id, action));
}

/// The filter rows, each chip counting what choosing it alone in its group
/// shows (the other groups as they are).
fn filter_bar(
    tree: &mut UiTree,
    list: WidgetId,
    session: &Session,
    filters: &Filters,
    gallery: bool,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let bar = tree.insert(
        Some(list),
        Container::column()
            .gap(4.0)
            .padding(8.0)
            .background(Role::SurfaceAlt)
            .radius(8.0),
    );
    let only = |filter: Filter| {
        let mut alone = filters.clone();
        alone.set(filter);
        session
            .evidence
            .iter()
            .filter(|e| passes(session, &alone, e) && (!gallery || e.media().is_some()))
            .count()
    };
    let row = filter_row(tree, bar, "提出者");
    let mut owners: Vec<(String, String)> = session
        .verifiers
        .iter()
        .map(|p| (p.id.to_string(), p.id.to_string()))
        .collect();
    owners.push((USER_EVIDENCE_OWNER.to_owned(), "あなた".to_owned()));
    for (id, label) in owners {
        let n = only(Filter::Owner(Some(id.clone())));
        chip(
            tree,
            row,
            format!("{label} {n}"),
            filters,
            Filter::Owner(Some(id)),
            actions,
        );
    }
    let row = filter_row(tree, bar, "ラウンド");
    for round in &session.rounds {
        let n = only(Filter::Round(Some(round.no)));
        chip(
            tree,
            row,
            format!("R{} {n}", round.no),
            filters,
            Filter::Round(Some(round.no)),
            actions,
        );
    }
    let row = filter_row(tree, bar, "種類");
    for kind in Kind::ALL {
        if gallery && !matches!(kind, Kind::Image | Kind::Video) {
            continue;
        }
        let n = only(Filter::Kind(Some(kind)));
        chip(
            tree,
            row,
            format!("{} {n}", kind.label()),
            filters,
            Filter::Kind(Some(kind)),
            actions,
        );
    }
    let row = filter_row(tree, bar, "基準");
    for c in &session.criteria {
        let n = only(Filter::Criterion(Some(c.id.clone())));
        chip(
            tree,
            row,
            format!("{} {n}", c.id),
            filters,
            Filter::Criterion(Some(c.id.clone())),
            actions,
        );
    }
    if !gallery {
        let row = filter_row(tree, bar, "再実行");
        for kind in RerunKind::ALL {
            let n = only(Filter::Rerun(Some(kind)));
            chip(
                tree,
                row,
                format!("{} {n}", kind.label()),
                filters,
                Filter::Rerun(Some(kind)),
                actions,
            );
        }
    }
}

/// The 一覧 or ギャラリー view of the evidence tab.
pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filters: &Filters,
    focus: Option<&Focus>,
    gallery: bool,
    can_attach: bool,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let list = scroll_column(tree, host, 8.0);
    keep_scroll(
        tree,
        list,
        session,
        if focus.is_some() {
            "evidence-reference"
        } else if gallery {
            "gallery"
        } else {
            "evidence"
        },
    );
    if let Some(focus) = focus {
        let top = tree.insert(Some(list), Container::wrapping_row(6.0, 4.0));
        tree.insert(
            Some(top),
            Label::new(focus.title.clone())
                .font_size(13.0)
                .weight(600)
                .wrap(true),
        );
        let back = tree.insert(
            Some(top),
            Button::new("元の表示に戻る").control_size(ControlSize::Small),
        );
        actions.push((back, Action::LeaveEvidenceFocus));
        note(
            tree,
            list,
            "この参照が引用した証跡を表示しています。元の絞り込み条件は保持しています",
        );
    }
    let top = tree.insert(
        Some(list),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    for (label, to_gallery) in [("一覧", false), ("ギャラリー", true)] {
        let id = tree.insert(
            Some(top),
            Button::new(label)
                .control_size(ControlSize::Small)
                .selected(gallery == to_gallery),
        );
        actions.push((
            id,
            Action::ShowTab(if to_gallery {
                super::CenterTab::Gallery
            } else {
                super::CenterTab::Evidence
            }),
        ));
    }
    spacer(tree, top);
    let items = match focus {
        Some(focus) => focus
            .keys
            .iter()
            .filter_map(|key| session.evidence_item(key))
            .collect(),
        None => shown(session, filters),
    };
    let shown: Vec<&Evidence> = items
        .into_iter()
        .filter(|e| !gallery || e.media().is_some())
        .collect();
    let all = if gallery {
        session
            .evidence
            .iter()
            .filter(|e| e.media().is_some())
            .count()
    } else {
        session.evidence.len()
    };
    caption(tree, top, format!("表示 {} / 全 {all}", shown.len()));
    if focus.is_none() {
        let clear = tree.insert(
            Some(top),
            Button::new("条件をすべて解除").control_size(ControlSize::Small),
        );
        actions.push((clear, Action::ClearEvidenceFilters));
    }
    if can_attach {
        let attach = tree.insert(
            Some(top),
            Button::new("証跡を追加…")
                .control_size(ControlSize::Small)
                .tooltip("スクリーンショットや画面収録を選ぶか、ウィンドウにドロップします"),
        );
        actions.push((attach, Action::AttachEvidence));
    }
    if focus.is_none() {
        filter_bar(tree, list, session, filters, gallery, actions);
    }
    if session.evidence.is_empty() {
        note(
            tree,
            list,
            "まだ証跡はありません。検証者の返答を取り込むと、ここに並びます。",
        );
        return;
    }
    if shown.is_empty() {
        note(
            tree,
            list,
            if focus.is_some() {
                "この参照には表示できる証跡がありません。ギャラリーでは画像・動画だけを表示します"
            } else {
                "条件に合う証跡はありません。"
            },
        );
        return;
    }
    if gallery {
        gallery_groups(tree, list, session, &shown, actions);
    } else {
        for item in shown {
            row(tree, list, item, actions);
        }
    }
}

/// One row of the 一覧: kind, command or path, exit status (the
/// verifier's and the re-run's), digest and size.
fn row(tree: &mut UiTree, list: WidgetId, item: &Evidence, actions: &mut Vec<(WidgetId, Action)>) {
    let c = tree.insert(
        Some(list),
        Container::column()
            .gap(4.0)
            .padding(10.0)
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(8.0),
    );
    let top = tree.insert(
        Some(c),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    tree.insert(
        Some(top),
        Label::new(item.key.to_string()).font_size(12.0).weight(700),
    );
    let kind = Kind::of(item);
    let format = item.media().map(Media::name);
    tag(
        tree,
        top,
        match format {
            Some(format) => format!("{} · {format}", kind.label()),
            None => kind.label().to_owned(),
        },
        Tone::Neutral,
    );
    if item.key.is_user() {
        tag(tree, top, "あなた", Tone::Accent);
    }
    caption(tree, top, format!("R{}", round_of(item)));
    match &item.body {
        EvidenceBody::Command { cmd, cwd, exit, .. } => {
            tree.insert(
                Some(c),
                Label::new(format!("$ {cmd}"))
                    .font_size(12.0)
                    .wrap(true)
                    .max_lines(3)
                    .truncation(Truncation::Tail)
                    .selectable(true),
            );
            let facts = tree.insert(
                Some(c),
                Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
            );
            caption(tree, facts, format!("cwd {cwd} · 終了コード {exit}"));
            if let Some(text) = rerun_text(item) {
                tag(tree, facts, text, rerun_tone(item));
            }
        }
        EvidenceBody::File {
            path, caption: cap, ..
        } => {
            tree.insert(
                Some(c),
                Label::new(path.clone())
                    .font_size(12.0)
                    .truncation(Truncation::Tail),
            );
            if let Some(cap) = cap {
                caption(tree, c, cap.clone());
            }
        }
        EvidenceBody::Observation { text } => {
            let line = tree.insert(
                Some(c),
                Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
            );
            tag(tree, line, "未裏付け", Tone::Warning);
            tree.insert(
                Some(line),
                Label::new(text.clone()).font_size(12.0).wrap(true),
            );
        }
    }
    stored_line(tree, c, item, actions);
}

/// The stored copy's digest and size, with 開く and Finder で表示; or why
/// there is none.
pub(super) fn stored_line(
    tree: &mut UiTree,
    parent: WidgetId,
    item: &Evidence,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    if matches!(item.body, EvidenceBody::Observation { .. }) {
        return;
    }
    let line = tree.insert(
        Some(parent),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    let Some(stored) = &item.stored else {
        caption(
            tree,
            line,
            "保存されたコピーはありません（取り込み前に消えたか、まだ取り込まれていません）",
        );
        return;
    };
    caption(
        tree,
        line,
        format!("{:016x} · {}", stored.digest, bytes(stored.bytes)),
    );
    open_buttons(tree, line, item, actions);
}

fn open_buttons(
    tree: &mut UiTree,
    parent: WidgetId,
    item: &Evidence,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    let open = tree.insert(Some(parent), small("開く"));
    tree.set_accessible_label(open, format!("{} の保存されたコピーを開く", item.key));
    actions.push((open, Action::OpenEvidence(item.key.clone())));
    let reveal = tree.insert(Some(parent), small("Finder で表示"));
    tree.set_accessible_label(reveal, format!("{} を Finder で表示", item.key));
    actions.push((reveal, Action::RevealEvidence(item.key.clone())));
}

/// The ギャラリー: media cards grouped by the criterion they are evidence
/// about (an item about several shows under each), then those about none.
fn gallery_groups(
    tree: &mut UiTree,
    list: WidgetId,
    session: &Session,
    shown: &[&Evidence],
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let mut placed = vec![false; shown.len()];
    for c in &session.criteria {
        let items: Vec<(usize, &Evidence)> = shown
            .iter()
            .enumerate()
            .filter(|(_, e)| relates(session, e, &c.id))
            .map(|(i, e)| (i, *e))
            .collect();
        if items.is_empty() {
            continue;
        }
        tree.insert(
            Some(list),
            Label::new(format!("{} {}", c.id, c.statement))
                .font_size(12.5)
                .weight(600)
                .truncation(Truncation::Tail),
        );
        let strip = tree.insert(Some(list), Container::wrapping_row(8.0, 8.0));
        for (i, item) in items {
            placed[i] = true;
            media_card(tree, strip, item, actions);
        }
    }
    let rest: Vec<&Evidence> = shown
        .iter()
        .zip(&placed)
        .filter(|(_, placed)| !**placed)
        .map(|(e, _)| *e)
        .collect();
    if !rest.is_empty() {
        tree.insert(
            Some(list),
            Label::new("基準に紐づかない証跡（不具合の根拠など）")
                .font_size(12.5)
                .weight(600),
        );
        let strip = tree.insert(Some(list), Container::wrapping_row(8.0, 8.0));
        for item in rest {
            media_card(tree, strip, item, actions);
        }
    }
}

/// The width of a placeholder card.
pub(super) const CARD: f32 = 188.0;

/// A media item's placeholder card (L1-5a): kind, format, size, caption,
/// who gave it and in which round, and for a video ▶ and its marks. No
/// picture: the box where one would go only names the kind and format,
/// and a click on it opens the stored copy.
pub(super) fn media_card(
    tree: &mut UiTree,
    parent: WidgetId,
    item: &Evidence,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let Some(media) = item.media() else {
        return;
    };
    let card = tree.insert(
        Some(parent),
        Container::column()
            .gap(4.0)
            .padding(8.0)
            .width(CARD)
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(8.0),
    );
    let caption_text = match &item.body {
        EvidenceBody::File { caption, .. } => caption.clone().unwrap_or_default(),
        _ => String::new(),
    };
    let press = tree.insert(
        Some(card),
        Pressable::new(Style {
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .label(format!(
            "{} {} を開く: {caption_text}",
            media.label(),
            media.name()
        ))
        .radius(6.0),
    );
    actions.push((press, Action::OpenEvidence(item.key.clone())));
    let preview = tree.insert(
        Some(press),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::CENTER,
            justify_content: JustifyContent::CENTER,
            flex_grow: 1.0,
            size: LayoutSize {
                width: percent(1.0),
                height: length(64.0),
            },
            gap: super::gap(2.0),
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .radius(6.0),
    );
    tree.insert(
        Some(preview),
        Label::new(if media.is_video() { "▶" } else { "▣" })
            .font_size(18.0)
            .color(Role::Muted),
    );
    tree.insert(
        Some(preview),
        Label::new(format!("{} · {}", media.label(), media.name()))
            .font_size(11.5)
            .weight(600),
    );
    if !caption_text.is_empty() {
        tree.insert(
            Some(card),
            Label::new(caption_text)
                .font_size(11.5)
                .wrap(true)
                .max_lines(3)
                .truncation(Truncation::Tail),
        );
    }
    let who = tree.insert(
        Some(card),
        Container::wrapping_row(4.0, 2.0).align_items(AlignItems::CENTER),
    );
    if item.key.is_user() {
        tag(tree, who, "あなた", Tone::Accent);
    } else {
        caption(tree, who, owner_label(item));
    }
    caption(tree, who, format!("R{}", round_of(item)));
    if let Some(stored) = &item.stored {
        caption(tree, card, bytes(stored.bytes));
    }
    if let EvidenceBody::File { marks, .. } = &item.body {
        for mark in marks {
            tree.insert(
                Some(card),
                Label::new(format!("▶ {} {}", mark_time(mark.t_ms), mark.note))
                    .font_size(11.0)
                    .color(Role::Muted)
                    .wrap(true),
            );
        }
    }
    if item.stored.is_some() {
        let buttons = tree.insert(Some(card), Container::wrapping_row(4.0, 4.0));
        open_buttons(tree, buttons, item, actions);
    }
}

// ---- the log view ----

/// The read-only view a command's stored log opens in, with search. Built
/// once and shown in place of the center pane, so its scroll and search
/// survive the screen's rebuilds.
pub(super) struct LogParts {
    pub(super) root: WidgetId,
    title: WidgetId,
    pub(super) search: WidgetId,
    found: WidgetId,
    pub(super) view: WidgetId,
    pub(super) close: WidgetId,
}

pub(super) fn insert_log(tree: &mut UiTree, parent: WidgetId) -> LogParts {
    let root = tree.insert(
        Some(parent),
        Container::new(Style {
            gap: super::gap(6.0),
            padding: noa_gpui_core::style::Rect::length(12.0),
            ..fill()
        }),
    );
    let head = tree.insert(
        Some(root),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let title = tree.insert(
        Some(head),
        Label::new("")
            .font_size(13.0)
            .weight(600)
            .truncation(Truncation::Tail),
    );
    spacer(tree, head);
    let close = tree.insert(
        Some(head),
        Button::new("閉じる").control_size(ControlSize::Small),
    );
    let bar = tree.insert(
        Some(root),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let search = tree.insert(Some(bar), TextField::new("ログを検索", 300.0));
    tree.set_accessible_label(search, "ログを検索");
    let found = tree.insert(Some(bar), Label::new("").font_size(11.0).color(Role::Muted));
    let host = tree.insert(Some(root), Container::new(fill()));
    let view = tree.insert(
        Some(host),
        TextView::new(200.0).fill_height().read_only(true),
    );
    tree.set_hidden(root, true);
    LogParts {
        root,
        title,
        search,
        found,
        view,
        close,
    }
}

/// Shows `text` (the stored log of `title`) in the view, its search empty.
pub(super) fn show_log(tree: &mut UiTree, parts: &LogParts, title: String, text: String) {
    tree.update::<Label, _>(parts.title, |l, cx| l.set_text(cx, title));
    tree.update::<TextField, _>(parts.search, |f, cx| f.set_text(cx, ""));
    tree.update::<Label, _>(parts.found, |l, cx| l.set_text(cx, ""));
    tree.update::<TextView, _>(parts.view, |v, cx| v.set_text(cx, text));
    tree.set_hidden(parts.root, false);
}

/// Highlights `query` in the log; the count shows beside the field.
pub(super) fn search_log(tree: &mut UiTree, parts: &LogParts, query: &str) {
    let action = if query.is_empty() {
        noa_gpui_core::widgets::FindAction::Close
    } else {
        noa_gpui_core::widgets::FindAction::Search {
            query: query.to_owned(),
            options: Default::default(),
        }
    };
    let status = tree.update::<TextView, _>(parts.view, |v, cx| v.find(cx, &action));
    let found = match status {
        Some(status) if !query.is_empty() => format!("{} 件", status.total),
        _ => String::new(),
    };
    tree.update::<Label, _>(parts.found, |l, cx| l.set_text(cx, found));
}

// ---- 証跡を追加 ----

/// The 証跡を追加 form, at the top of the right pane while a chosen or
/// dropped file waits for its caption. Built once, so what is typed
/// survives the screen's rebuilds.
pub(super) struct AttachParts {
    pub(super) root: WidgetId,
    title: WidgetId,
    kind: WidgetId,
    pub(super) caption: WidgetId,
    pub(super) shows: WidgetId,
    marks_label: WidgetId,
    pub(super) marks: WidgetId,
    error: WidgetId,
    /// Why 追加 is off once the verification has closed (L1-5a).
    pub(super) closed: WidgetId,
    pub(super) submit: WidgetId,
    pub(super) cancel: WidgetId,
}

/// Shown in an open 証跡を追加 form once the verification has closed: what
/// was typed stays, and 追加 is off.
pub(super) const ATTACH_CLOSED: &str =
    "検証が終了したため、この証跡は追加できません（入力した内容はそのまま残っています）";

/// A file waiting to be attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AttachDraft {
    pub(super) source: PathBuf,
    /// What its extension says (the runner sniffs the bytes).
    pub(super) media: Option<Media>,
}

pub(super) fn insert_attach(tree: &mut UiTree, parent: WidgetId) -> AttachParts {
    let root = tree.insert(
        Some(parent),
        Container::column()
            .gap(6.0)
            .padding(10.0)
            .background(Role::Background)
            .border(1.0, Role::Accent)
            .radius(8.0),
    );
    let title = tree.insert(
        Some(root),
        Label::new("証跡を追加")
            .font_size(13.0)
            .weight(600)
            .wrap(true),
    );
    let kind = tree.insert(
        Some(root),
        Label::new("").font_size(11.0).color(Role::Muted).wrap(true),
    );
    caption(tree, root, format!("説明（必須・{MAX_CAPTION}文字まで）"));
    let caption_area = tree.insert(
        Some(root),
        TextArea::new("例: 右のペインで cd /tmp のあと ⌘T、新しいタブは $HOME").min_lines(2),
    );
    caption(tree, root, "示す基準（必須・カンマ区切り）");
    let shows = tree.insert(Some(root), TextField::new("例: AC-1, AC-2", 300.0));
    let marks_label = tree.insert(
        Some(root),
        Label::new(format!(
            "見どころ（動画のみ・任意・1行に「mm:ss 説明」・{MAX_MARKS}個まで）"
        ))
        .font_size(11.0)
        .color(Role::Muted),
    );
    let marks = tree.insert(
        Some(root),
        TextArea::new("00:04 ⌘T を押す\n00:06.5 新しいタブが $HOME を表示").min_lines(2),
    );
    let error = tree.insert(Some(root), Label::new("").font_size(11.5).wrap(true));
    let closed = tree.insert(
        Some(root),
        Label::new(ATTACH_CLOSED)
            .font_size(11.5)
            .color(Role::Muted)
            .wrap(true),
    );
    tree.set_hidden(closed, true);
    let buttons = tree.insert(Some(root), Container::row().gap(6.0));
    let submit = tree.insert(
        Some(buttons),
        Button::new("追加").control_size(ControlSize::Small),
    );
    let cancel = tree.insert(
        Some(buttons),
        Button::new("やめる").control_size(ControlSize::Small),
    );
    tree.set_hidden(root, true);
    AttachParts {
        root,
        title,
        kind,
        caption: caption_area,
        shows,
        marks_label,
        marks,
        error,
        closed,
        submit,
        cancel,
    }
}

/// Whether the form's 追加 can be used: only while 証跡を追加 can (L1-5a).
/// Otherwise it is off and the form says why; やめる closes it.
pub(super) fn attach_open(tree: &mut UiTree, parts: &AttachParts, open: bool) {
    tree.set_enabled(parts.submit, open);
    tree.set_hidden(parts.closed, open);
}

/// Shows the form for `draft`, empty but for `shows` (the selected
/// criterion, when there is one); `waiting` more files follow it.
pub(super) fn show_attach(
    tree: &mut UiTree,
    parts: &AttachParts,
    draft: &AttachDraft,
    shows: String,
    waiting: usize,
) {
    let name = draft
        .source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| draft.source.display().to_string());
    let mut title = format!("証跡を追加: {name}");
    if waiting > 0 {
        title.push_str(&format!("（ほか {waiting} 件が待機中）"));
    }
    let kind = match draft.media {
        Some(m) => format!("{} · {}（追加時に中身で確かめます）", m.label(), m.name()),
        None => "画像・動画として扱えない拡張子です（追加時に中身で確かめます）".to_owned(),
    };
    tree.update::<Label, _>(parts.title, |l, cx| l.set_text(cx, title));
    tree.update::<Label, _>(parts.kind, |l, cx| l.set_text(cx, kind));
    tree.update::<TextArea, _>(parts.caption, |a, cx| a.set_text(cx, ""));
    tree.update::<TextField, _>(parts.shows, |f, cx| f.set_text(cx, shows));
    tree.update::<TextArea, _>(parts.marks, |a, cx| a.set_text(cx, ""));
    tree.update::<Label, _>(parts.error, |l, cx| l.set_text(cx, ""));
    let video = draft.media.is_some_and(Media::is_video);
    tree.set_hidden(parts.marks_label, !video);
    tree.set_hidden(parts.marks, !video);
    tree.set_hidden(parts.root, false);
}

pub(super) fn hide_attach(tree: &mut UiTree, parts: &AttachParts) {
    tree.set_hidden(parts.root, true);
}

pub(super) fn attach_error(tree: &mut UiTree, parts: &AttachParts, why: String) {
    tree.update::<Label, _>(parts.error, |l, cx| l.set_text(cx, why));
}

/// What the form holds, checked as a verifier's media is (L1-5a): a
/// caption of 1–300 characters, marks only on a video (at most 20, each
/// `mm:ss[.f] note`, in time order), and `shows` naming criteria.
pub(super) fn read_attach(
    tree: &UiTree,
    parts: &AttachParts,
    draft: &AttachDraft,
    session: &Session,
) -> Result<(String, Vec<Mark>, Vec<String>), String> {
    let caption = tree
        .get::<TextArea>(parts.caption)
        .map(|a| a.text().trim().to_owned())
        .unwrap_or_default();
    let n = caption.chars().count();
    if n == 0 {
        return Err(
            "説明を入力してください（絵を開けない人にも何が写っているか分かるように）".into(),
        );
    }
    if n > MAX_CAPTION {
        return Err(format!("説明は{MAX_CAPTION}文字までです（{n}文字）"));
    }
    let shows_text = tree
        .get::<TextField>(parts.shows)
        .map(|f| f.text().to_owned())
        .unwrap_or_default();
    let shows: Vec<String> = shows_text
        .split([',', '、', ' ', '　'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if shows.is_empty() {
        return Err("この証跡が示す基準を入力してください（例: AC-1）".into());
    }
    for id in &shows {
        if session.criterion(id).is_none() {
            return Err(format!("{id} という基準はありません"));
        }
    }
    let marks_text = tree
        .get::<TextArea>(parts.marks)
        .map(|a| a.text().to_owned())
        .unwrap_or_default();
    let video = draft.media.is_some_and(Media::is_video);
    let mut marks = Vec::new();
    for line in marks_text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !video {
            return Err("見どころは動画にだけ付けられます".into());
        }
        let (t, note) = line.split_once([' ', '\t', '　']).unwrap_or((line, ""));
        let t_ms = verify::media::parse_mark(t).map_err(|e| format!("見どころ「{line}」: {e}"))?;
        let note = note.trim();
        if note.is_empty() {
            return Err(format!("見どころ「{line}」に説明がありません"));
        }
        if marks.last().is_some_and(|m: &Mark| m.t_ms > t_ms) {
            return Err(format!("見どころ「{line}」が前の時刻より前です"));
        }
        marks.push(Mark {
            t_ms,
            note: note.to_owned(),
        });
    }
    if marks.len() > MAX_MARKS {
        return Err(format!(
            "見どころは{MAX_MARKS}個までです（{}個）",
            marks.len()
        ));
    }
    Ok((caption, marks, shows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use verify::model::types::EvidenceKey;

    #[test]
    fn marks_and_sizes_read_as_the_cards_show_them() {
        assert_eq!(mark_time(4_000), "00:04");
        assert_eq!(mark_time(6_500), "00:06.5");
        assert_eq!(mark_time(65_250), "01:05.25");
        assert_eq!(mark_time(3_723_000), "1:02:03");
        assert_eq!(bytes(0), "0 バイト");
        assert_eq!(bytes(1_515), "1,515 バイト");
        assert_eq!(bytes(4_812_331), "4,812,331 バイト");
    }

    #[test]
    fn evidence_relates_to_a_criterion_by_citation_or_by_shows() {
        let s = crate::demo::session();
        let item = |k: &str| s.evidence_item(&EvidenceKey::new(k)).unwrap();
        assert!(relates(&s, item("claude/r1/v1"), "AC-1"));
        assert!(!relates(&s, item("claude/r1/v1"), "AC-2"));
        // A cross-check on codex's AC-1 cites claude-2's command.
        assert!(relates(&s, item("claude-2/r2/e1"), "AC-1"));
        // The person's screenshot shows AC-1 only.
        assert!(relates(&s, item("user/1"), "AC-1"));
        assert!(!relates(&s, item("user/1"), "AC-2"));
        assert_eq!(owner(item("user/1")), USER_EVIDENCE_OWNER);
        assert_eq!(round_of(item("user/1")), 1);
        assert_eq!(round_of(item("codex/r2/e1")), 2);
    }
}
