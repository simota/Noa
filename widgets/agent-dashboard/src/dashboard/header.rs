use std::cell::{Cell, RefCell};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Container, Kbd, Label, TextField, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Size;
use noa_gpui_text::{TextStore, TextStyle};

use crate::fleet::Worker;

use super::Feed;
use super::filter::Filter;
use super::health::{self, HealthParts};
use super::style::{HEADER_HEIGHT, fixed_width, gap};

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

/// The title comes back only when it fits with this much to spare, so a
/// status that changed while it was hidden cannot make it flicker.
const SHOW_SLACK: f32 = 8.0;

/// The header's widgets, for `fit` and `health::tick`. `Dashboard` reads
/// only `health`, `filter_field` and `match_count`. The grouping control
/// is in the analysis tabs' own row (it orders only their views).
pub(super) struct HeaderParts {
    pub(super) bar: WidgetId,
    pub(super) title: WidgetId,
    pub(super) health: HealthParts,
    pub(super) filter_field: WidgetId,
    pub(super) match_count: WidgetId,
    pub(super) help_key: WidgetId,
    /// The droppable item's width when it was last shown (`fit` cannot
    /// read a hidden item's width from the layout).
    widths: Widths,
}

/// The width `fit` remembers for what it may hide, and the text it
/// measures.
#[derive(Default)]
struct Widths {
    title: Cell<f32>,
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

pub(super) fn header(
    tree: &mut UiTree,
    root: WidgetId,
    feed: &Feed,
    filter: &Filter,
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
        Label::new("NOA / AGENTS")
            .font_size(crate::appearance::font(16.0))
            .weight(700),
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
            .font_size(crate::appearance::font(STATUS_FONT_SIZE))
            .truncation(Truncation::Tail),
    );
    let health = HealthParts { dot, status };
    health::show_built(tree, health, feed);

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
            .font_size(crate::appearance::font(12.0))
            .color(Role::Muted),
    );
    tree.set_hidden(match_count, !filter.active());
    let help_key = tree.insert(Some(bar), Kbd::new(["?"]));
    HeaderParts {
        bar,
        title,
        health,
        filter_field,
        match_count,
        help_key,
        widths: Widths::default(),
    }
}

/// "{matching} / {total} 件".
pub(super) fn match_text(workers: &[Worker], filter: &Filter) -> String {
    let matching = workers.iter().filter(|w| filter.matches(w)).count();
    format!("{matching} / {} 件", workers.len())
}

/// Drops the title when the header does not fit `viewport`, and brings it
/// back when it has room again; true when it changed the tree. The health
/// dot and status, the filter field and the `?` keycap are never dropped.
/// The status counts at its whole text's width, up to `STATUS_MAX_WIDTH`:
/// it is cut (never under `STATUS_MIN_WIDTH`) only when the title is gone.
///
/// The layout `prepare` just made has the width of everything shown; what is
/// hidden has none. Nothing in the bar shrinks but the status slot, so the
/// width the title had when it was last shown is its natural one however
/// tight the bar is, and `parts` keeps it.
pub(super) fn fit(
    tree: &mut UiTree,
    text: &mut TextStore,
    _viewport: Size,
    parts: &HeaderParts,
) -> bool {
    let width_of = |tree: &UiTree, id: WidgetId| tree.layout(id).map_or(0.0, |l| l.rect.width);
    let widths = &parts.widths;
    if !tree.is_hidden(parts.title) {
        widths.title.set(width_of(tree, parts.title));
    }
    let status_text = tree
        .get::<Label>(parts.health.status)
        .map_or("", Label::text);
    let status = widths
        .status
        .of(
            text,
            status_text,
            TextStyle::new(crate::appearance::font(STATUS_FONT_SIZE)),
        )
        .clamp(STATUS_MIN_WIDTH, STATUS_MAX_WIDTH);

    // What never goes: the status slot at its whole text's width (capped),
    // and the rest as laid out.
    let fixed = [
        8.0,
        status,
        0.0,
        width_of(tree, parts.filter_field),
        if tree.is_hidden(parts.match_count) {
            f32::NAN
        } else {
            width_of(tree, parts.match_count)
        },
        width_of(tree, parts.help_key),
    ];
    let mut items: Vec<f32> = fixed.into_iter().filter(|w| !w.is_nan()).collect();
    items.push(widths.title.get());
    let with = items.iter().sum::<f32>() + BAR_GAP * (items.len() - 1) as f32;
    let inner = width_of(tree, parts.bar) - 2.0 * BAR_PADDING;

    // The title comes back only with room to spare.
    let hidden = tree.is_hidden(parts.title);
    let hide = if hidden {
        with + SHOW_SLACK > inner
    } else {
        with > inner
    };
    if hide != hidden {
        tree.set_hidden(parts.title, hide);
    }
    hide != hidden
}
