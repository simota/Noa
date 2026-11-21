use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Container, ControlSize, Kbd, Label, SegmentMode, SegmentedControl, Tag, TagStyle, TextField,
    Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use crate::fleet::{Grouping, Need, Phase, Worker, minutes_text};

use super::filter::Filter;
use super::health::{HealthParts, status};
use super::panel::Panel;
use super::style::{GRAY, HEADER_HEIGHT, caption, fixed_width, gap, joined, spacer};
use super::{Feed, Pulse, Registry, View};

/// The status label is cut beyond this width, so a long notice cannot
/// push the rest of the header around.
pub(super) const STATUS_MAX_WIDTH: f32 = 200.0;

/// The header's widgets, for `fit` and `health::tick`. `Dashboard` reads
/// only `health`, `filter_field`, `match_count` and `grouping_control`.
pub(super) struct HeaderParts {
    pub(super) bar: WidgetId,
    pub(super) title: WidgetId,
    pub(super) health: HealthParts,
    pub(super) longest: Option<WidgetId>,
    /// One chip per hidden panel, in `Panel::ALL` order: its keycap and
    /// title.
    pub(super) chips: Vec<WidgetId>,
    pub(super) filter_field: WidgetId,
    pub(super) match_count: WidgetId,
    pub(super) help_key: WidgetId,
    pub(super) group_caption: WidgetId,
    pub(super) grouping_control: WidgetId,
}

pub(super) fn header(
    tree: &mut UiTree,
    root: WidgetId,
    feed: &Feed,
    view: &View,
    filter: &Filter,
    reg: &mut Registry,
) -> HeaderParts {
    let workers = &feed.fleet.workers;
    let bar = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(10.0),
            padding: LayoutRect {
                left: length(16.0),
                right: length(16.0),
                top: length(0.0),
                bottom: length(0.0),
            },
            size: LayoutSize {
                width: percent(1.0),
                height: length(HEADER_HEIGHT),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator),
    );
    let title = tree.insert(
        Some(bar),
        Label::new("NOA / AGENTS").font_size(16.0).weight(700),
    );

    let dot = tree.insert(
        Some(bar),
        Container::new(fixed_width(8.0))
            .height(8.0)
            .radius(4.0)
            .background(GRAY),
    );
    let status_slot = tree.insert(
        Some(bar),
        Container::new(Style {
            flex_shrink: 1.0,
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            max_size: LayoutSize {
                width: length(STATUS_MAX_WIDTH),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let status = tree.insert(
        Some(status_slot),
        Label::new(status(feed, std::time::Instant::now()))
            .font_size(12.0)
            .color(Role::Muted)
            .truncation(Truncation::Tail),
    );

    let longest = longest(workers).map(|worker| {
        let id = tree.insert(
            Some(bar),
            Tag::new(longest_text(worker))
                .style(TagStyle::Outlined)
                .tone(Tone::Warning),
        );
        reg.pulses.push((id, Pulse::Longest));
        id
    });

    let chips = Panel::ALL
        .into_iter()
        .filter(|panel| !view.shown[*panel as usize])
        .map(|panel| chip(tree, bar, panel))
        .collect();

    spacer(tree, bar);
    let filter_field = tree.insert(Some(bar), TextField::new("絞り込み（/）", 160.0));
    let caret = filter.caret.min(filter.query.len());
    tree.update::<TextField, _>(filter_field, |field, cx| {
        field.set_text(cx, filter.query.as_str());
        field.select(cx, caret..caret);
    });
    let match_count = tree.insert(
        Some(bar),
        Label::new(match_text(workers, filter))
            .font_size(12.0)
            .color(Role::Muted),
    );
    tree.set_hidden(match_count, filter.query.is_empty());
    let help_key = tree.insert(Some(bar), Kbd::new(["?"]));
    let group_caption = caption(tree, bar, "グループ");
    let selected = Grouping::ALL
        .iter()
        .position(|candidate| *candidate == view.grouping)
        .unwrap_or(0);
    let grouping_control = tree.insert(
        Some(bar),
        SegmentedControl::new(Grouping::ALL.map(Grouping::title), SegmentMode::SelectOne)
            .with_selected(&[selected])
            .control_size(ControlSize::Small),
    );
    HeaderParts {
        bar,
        title,
        health: HealthParts { dot, status },
        longest,
        chips,
        filter_field,
        match_count,
        help_key,
        group_caption,
        grouping_control,
    }
}

/// A hidden panel in the header: its keycap and its title, muted, no fill.
fn chip(tree: &mut UiTree, bar: WidgetId, panel: Panel) -> WidgetId {
    let chip = tree.insert(
        Some(bar),
        Container::row().align_items(AlignItems::CENTER).gap(3.0),
    );
    tree.insert(Some(chip), Kbd::new([panel.digit()]));
    tree.insert(
        Some(chip),
        Label::new(panel.title()).font_size(11.0).color(Role::Muted),
    );
    chip
}

/// "{matching} / {total} 件".
pub(super) fn match_text(workers: &[Worker], filter: &Filter) -> String {
    let matching = workers.iter().filter(|w| filter.matches(w)).count();
    format!("{matching} / {} 件", workers.len())
}

/// Drops header items by priority until the header fits `viewport`; true
/// when it changed the tree. U2 fills this in; in U1 everything fits the
/// sizes the tests use.
pub(super) fn fit(
    tree: &mut UiTree,
    _text: &mut TextStore,
    _viewport: Size,
    parts: &HeaderParts,
) -> bool {
    let shown = [parts.bar, parts.title, parts.help_key, parts.group_caption]
        .into_iter()
        .chain(parts.longest)
        .chain(parts.chips.iter().copied());
    let mut changed = false;
    for id in shown {
        if tree.is_hidden(id) {
            tree.set_hidden(id, false);
            changed = true;
        }
    }
    changed
}

pub(super) fn waited_text(workers: &[Worker]) -> String {
    let waited: f64 = workers
        .iter()
        .map(|w| w.minutes_where(Phase::waits_on_a_person))
        .sum();
    format!("人待ち合計 {}", minutes_text(waited))
}

/// The worker waiting longest on a person. Only these needs have a wait
/// text, known or a lower bound; a 通知 or a quiet worker has none.
pub(super) fn longest(workers: &[Worker]) -> Option<&Worker> {
    workers
        .iter()
        .filter(|w| {
            matches!(
                w.need(),
                Some(Need::Approval | Need::Failure | Need::Question)
            )
        })
        .max_by(|a, b| a.wait_minutes.total_cmp(&b.wait_minutes))
}

/// Longer worker labels are cut to this many characters in the header's
/// "longest" tag, which cannot truncate itself: an uncut pane title there
/// squeezes the link status to nothing.
pub(super) const LONGEST_LABEL_CHARS: usize = 12;

pub(super) fn longest_text(worker: &Worker) -> String {
    let label = if worker.label.chars().count() > LONGEST_LABEL_CHARS {
        let cut: String = worker.label.chars().take(LONGEST_LABEL_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        worker.label.clone()
    };
    joined(&["最長", &label, &worker.since], " ")
}
