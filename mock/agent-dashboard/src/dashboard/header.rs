use std::cell::{Cell, RefCell};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Container, ControlSize, Kbd, Label, SegmentMode, SegmentedControl, Tag, TagStyle, TextField,
    Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Size;
use noa_gpui_text::{TextStore, TextStyle};

use crate::fleet::{Grouping, Need, Phase, Worker, minutes_text};

use super::filter::Filter;
use super::health::{self, HealthParts};
use super::panel::Panel;
use super::style::{HEADER_HEIGHT, caption, fixed_width, gap, joined, need_color};
use super::{Feed, Pulse, Registry, View};

/// The status label is cut beyond this width, so a long notice cannot
/// push the rest of the header around.
pub(super) const STATUS_MAX_WIDTH: f32 = 200.0;

const STATUS_FONT_SIZE: f32 = 12.0;

/// The status slot never gets narrower than this (a text that needs more
/// is cut with an ellipsis): the link state stays readable at any width.
pub(super) const STATUS_MIN_WIDTH: f32 = 140.0;

/// Space between the header's items and the bar's side padding: what `fit`
/// needs to know of the bar's box (its 1 px border is only painted).
const BAR_GAP: f32 = 10.0;
const BAR_PADDING: f32 = 16.0;
const CHIP_GAP: f32 = 3.0;

/// A hidden item comes back only when it fits with this much to spare, so
/// a text that changed while it was hidden cannot make it flicker.
const SHOW_SLACK: f32 = 8.0;

/// The urgency counts, in this order (AC7): the label, the need and its
/// (the need's colour). 通知 is not counted: it waits on nobody's decision.
const URGENCY: [(&str, Need); 4] = [
    ("承認", Need::Approval),
    ("質問", Need::Question),
    ("失敗", Need::Failure),
    ("静止", Need::Quiet),
];

/// The header's widgets, for `fit` and `health::tick`. `Dashboard` reads
/// only `health`, `filter_field`, `match_count` and `grouping_control`.
pub(super) struct HeaderParts {
    pub(super) bar: WidgetId,
    pub(super) title: WidgetId,
    pub(super) health: HealthParts,
    /// The row of urgency counts.
    pub(super) counts: WidgetId,
    pub(super) longest: Option<WidgetId>,
    /// One chip per hidden panel, in `Panel::ALL` order: its keycap and
    /// title.
    pub(super) chips: Vec<WidgetId>,
    /// Each chip's keycap and title, in the same order.
    pub(super) chip_parts: Vec<ChipParts>,
    pub(super) filter_field: WidgetId,
    pub(super) match_count: WidgetId,
    pub(super) help_key: WidgetId,
    pub(super) group_caption: WidgetId,
    pub(super) grouping_control: WidgetId,
    /// Each droppable item's width when it was last shown (`fit` cannot
    /// read a hidden item's width from the layout).
    widths: Widths,
}

/// The parts of a hidden panel's chip: the keycap, which stays, and the
/// title, which `fit` drops early.
pub(super) struct ChipParts {
    pub(super) key: WidgetId,
    pub(super) title: WidgetId,
}

/// The widths `fit` remembers for what it may hide, and the texts it
/// measures.
#[derive(Default)]
struct Widths {
    title: Cell<f32>,
    /// What the 最長 tag adds to its text's width (its padding), from when
    /// it was last shown: its text changes in place (`Pulse::Longest`), also
    /// while it is hidden, so its width is this plus its text's.
    longest_pad: Cell<f32>,
    longest_text: TextWidth,
    group_caption: Cell<f32>,
    /// Per chip: its width with the title.
    chips: Vec<Cell<f32>>,
    /// The status label's whole text.
    status: TextWidth,
}

/// The width of a text as last measured, shaped again only when the text
/// changes (`fit` runs on every frame).
#[derive(Default)]
struct TextWidth(RefCell<Option<(String, f32)>>);

impl TextWidth {
    fn of(&self, text: &mut TextStore, s: &str, style: TextStyle) -> f32 {
        if let Some((known, width)) = &*self.0.borrow()
            && known == s
        {
            return *width;
        }
        let id = text.create(s, style, None);
        let width = text.size(id).width;
        text.remove(id);
        self.0.replace(Some((s.to_owned(), width)));
        width
    }
}

/// The style `Tag` shapes its text in (noa-gpui's `Tag::text_style`).
fn tag_style() -> TextStyle {
    TextStyle::new(12.0).weight(500)
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
        Container::new(fixed_width(8.0)).height(8.0).radius(4.0),
    );
    let status_slot = tree.insert(
        Some(bar),
        Container::new(Style {
            flex_shrink: 1.0,
            min_size: LayoutSize {
                width: length(STATUS_MIN_WIDTH),
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
        Label::new(health::initial_status(feed))
            .font_size(STATUS_FONT_SIZE)
            .truncation(Truncation::Tail),
    );
    let health = HealthParts { dot, status };
    health::show_built(tree, health, feed);

    let counts = urgency_counts(tree, bar, workers);

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

    let (chips, chip_parts): (Vec<_>, Vec<_>) = Panel::ALL
        .into_iter()
        .filter(|panel| !view.shown[*panel as usize])
        .map(|panel| chip(tree, bar, panel))
        .collect();

    tree.insert(Some(bar), Container::row().grow(1.0));
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
    tree.set_hidden(match_count, !filter.active());
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
    let widths = Widths {
        chips: chips.iter().map(|_| Cell::default()).collect(),
        ..Widths::default()
    };
    HeaderParts {
        bar,
        title,
        health,
        counts,
        longest,
        chips,
        chip_parts,
        filter_field,
        match_count,
        help_key,
        group_caption,
        grouping_control,
        widths,
    }
}

/// The four urgency counts, each "{title} {n}" in its need's colour, or
/// muted and light at zero. Counted over every worker, filter or not.
fn urgency_counts(tree: &mut UiTree, bar: WidgetId, workers: &[Worker]) -> WidgetId {
    let row = tree.insert(
        Some(bar),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    for (title, need) in URGENCY {
        let n = workers.iter().filter(|w| w.need() == Some(need)).count();
        let label = Label::new(format!("{title} {n}")).font_size(12.0);
        let label = if n == 0 {
            label.weight(400).color(Role::Muted)
        } else {
            label.weight(700).color(need_color(need))
        };
        tree.insert(Some(row), label);
    }
    row
}

/// A hidden panel in the header: its keycap and its title, muted, no fill.
fn chip(tree: &mut UiTree, bar: WidgetId, panel: Panel) -> (WidgetId, ChipParts) {
    let chip = tree.insert(
        Some(bar),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(CHIP_GAP),
    );
    let key = tree.insert(Some(chip), Kbd::new([panel.digit()]));
    let title = tree.insert(
        Some(chip),
        Label::new(panel.title()).font_size(11.0).color(Role::Muted),
    );
    (chip, ChipParts { key, title })
}

/// "{matching} / {total} 件".
pub(super) fn match_text(workers: &[Worker], filter: &Filter) -> String {
    let matching = workers.iter().filter(|w| filter.matches(w)).count();
    format!("{matching} / {} 件", workers.len())
}

/// Drops header items by priority until the header fits `viewport`, and
/// brings them back when it has room again; true when it changed the tree.
/// In this order: (1) the グループ caption, (2) the titles of the hidden-panel
/// chips (their keycaps stay), (3) the 最長 tag, (4) the title. The health
/// dot and status, the urgency counts, the filter field, the `?` keycap and
/// the grouping control are never dropped. The status counts at its whole
/// text's width, up to `STATUS_MAX_WIDTH`: it is cut (never under
/// `STATUS_MIN_WIDTH`) only when everything droppable is gone.
///
/// The layout `prepare` just made has the width of everything shown; what is
/// hidden has none. Nothing in the bar shrinks but the status slot, so the
/// width an item had when it was last shown is its natural one however tight
/// the bar is, and `parts` keeps it. The 最長 tag's text can change while it
/// is hidden, so its width is measured from its text now; the title, the
/// caption and the chips never change theirs.
pub(super) fn fit(
    tree: &mut UiTree,
    text: &mut TextStore,
    _viewport: Size,
    parts: &HeaderParts,
) -> bool {
    let width_of = |tree: &UiTree, id: WidgetId| tree.layout(id).map_or(0.0, |l| l.rect.width);
    let remember = |tree: &UiTree, id: WidgetId, cell: &Cell<f32>| {
        if !tree.is_hidden(id) {
            cell.set(width_of(tree, id));
        }
    };
    let widths = &parts.widths;
    remember(tree, parts.title, &widths.title);
    remember(tree, parts.group_caption, &widths.group_caption);
    for ((chip, cell), titled) in parts.chips.iter().zip(&widths.chips).zip(&parts.chip_parts) {
        if !tree.is_hidden(titled.title) {
            cell.set(width_of(tree, *chip));
        }
    }
    let longest = parts.longest.map(|id| {
        let tag_text = tree.get::<Tag>(id).map_or("", Tag::text);
        let text_width = widths.longest_text.of(text, tag_text, tag_style());
        if tree.is_hidden(id) {
            widths.longest_pad.get() + text_width
        } else {
            let width = width_of(tree, id);
            widths.longest_pad.set(width - text_width);
            width
        }
    });
    let status_text = tree
        .get::<Label>(parts.health.status)
        .map_or("", Label::text);
    let status = widths
        .status
        .of(text, status_text, TextStyle::new(STATUS_FONT_SIZE))
        .clamp(STATUS_MIN_WIDTH, STATUS_MAX_WIDTH);

    // What never goes: the status slot at its whole text's width (capped),
    // and the rest as laid out.
    let fixed = [
        8.0,
        status,
        width_of(tree, parts.counts),
        0.0,
        width_of(tree, parts.filter_field),
        if tree.is_hidden(parts.match_count) {
            f32::NAN
        } else {
            width_of(tree, parts.match_count)
        },
        width_of(tree, parts.help_key),
        width_of(tree, parts.grouping_control),
    ];
    let fixed: Vec<f32> = fixed.into_iter().filter(|w| !w.is_nan()).collect();
    let needed = |level: u8| -> f32 {
        let mut items = fixed.clone();
        if level < DROP_TITLE {
            items.push(widths.title.get());
        }
        if let Some(longest) = longest
            && level < DROP_LONGEST
        {
            items.push(longest);
        }
        if level < DROP_GROUP_CAPTION {
            items.push(widths.group_caption.get());
        }
        for (full, chip) in widths.chips.iter().zip(&parts.chip_parts) {
            items.push(if level < DROP_CHIP_TITLES {
                full.get()
            } else {
                width_of(tree, chip.key)
            });
        }
        items.iter().sum::<f32>() + BAR_GAP * (items.len() - 1) as f32
    };
    let inner = width_of(tree, parts.bar) - 2.0 * BAR_PADDING;

    let shown = |id| !tree.is_hidden(id);
    let current = if !shown(parts.title) {
        DROP_TITLE
    } else if parts.longest.is_some_and(|id| !shown(id)) {
        DROP_LONGEST
    } else if parts.chip_parts.iter().any(|chip| !shown(chip.title)) {
        DROP_CHIP_TITLES
    } else if !shown(parts.group_caption) {
        DROP_GROUP_CAPTION
    } else {
        0
    };
    let levels = 0..=DROP_TITLE;
    let target = levels
        .clone()
        .find(|level| needed(*level) <= inner)
        .unwrap_or(DROP_TITLE);
    let level = if target < current {
        // Items come back only with room to spare.
        levels
            .filter(|level| *level >= target && *level < current)
            .find(|level| needed(*level) + SHOW_SLACK <= inner)
            .unwrap_or(current)
    } else {
        target
    };

    let mut changed = false;
    let mut set = |tree: &mut UiTree, id: WidgetId, hidden: bool| {
        if tree.is_hidden(id) != hidden {
            tree.set_hidden(id, hidden);
            changed = true;
        }
    };
    set(tree, parts.group_caption, level >= DROP_GROUP_CAPTION);
    for chip in &parts.chip_parts {
        set(tree, chip.title, level >= DROP_CHIP_TITLES);
    }
    if let Some(id) = parts.longest {
        set(tree, id, level >= DROP_LONGEST);
    }
    set(tree, parts.title, level >= DROP_TITLE);
    changed
}

/// How much `fit` has dropped: each level drops what the ones before it do.
const DROP_GROUP_CAPTION: u8 = 1;
const DROP_CHIP_TITLES: u8 = 2;
const DROP_LONGEST: u8 = 3;
const DROP_TITLE: u8 = 4;

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
