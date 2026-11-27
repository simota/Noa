//! The 制限 tab: the window that runs out first, one card per agent (fixed
//! order Claude Code, Codex, Antigravity) with a gauge per window, and the
//! limit history chart.

use noa_gpui_core::style::{Rect as LayoutRect, length};
use noa_gpui_core::widgets::{Chart, ChartSeries, Container, Label, Tag, Tone};
use noa_gpui_core::{Paint, Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use agent_usage::history::Series;
use agent_usage::model::{
    Agent, Origin, Severity, SourceState, Unix, UsageSource, UsageWindow, binding_window, format,
};
use agent_usage::stats::types::{AgentFilter, Period};
use agent_usage::time::{Tz, local};

use super::charts::{Dot, Gauge, bin_series, history_bins};
use super::style::{
    self, AGY, CLAUDE, CODEX, Faded, GREEN, ORANGE, RED, STALE_OPACITY, caption, cell, gap,
    heading, severity_paint,
};

const LABEL_W: f32 = 96.0;
const PERCENT_W: f32 = 42.0;
const RESET_W: f32 = 84.0;
const ROW_FONT: f32 = 12.0;
/// A card's width before the row shares out what is left; under three of
/// these side by side, the cards wrap.
const CARD_BASIS: f32 = 340.0;

/// The header of a card whose source has not answered yet.
pub const FETCHING: &str = "取得中…";

pub struct Input<'a> {
    /// By [`Agent::index`]; `None` until the source's first answer.
    pub sources: &'a [Option<UsageSource>],
    pub series: &'a [Series],
    pub range: (Unix, Unix),
    pub period: Period,
    pub filter: AgentFilter,
    pub now: Unix,
    pub tz: &'a dyn Tz,
}

/// "Codex 7d 56% · maxes out 11/28(Fri) at this pace", else "No limit is on
/// course to run out"; nothing current: "現在値なし".
pub fn summary_text(sources: &[UsageSource], now: Unix, tz: &dyn Tz) -> String {
    if !sources.iter().any(|s| s.is_current(now)) {
        return "現在値なし".to_owned();
    }
    match binding_window(sources, now) {
        Some((agent, w)) => {
            let mut text = format!(
                "{} {} {}",
                agent.display(),
                w.label,
                format::percent(w.used_percent)
            );
            if let Some(ex) = format::exhaustion(w, now, tz) {
                text.push_str(" · ");
                text.push_str(&ex);
            }
            text
        }
        None => "No limit is on course to run out".to_owned(),
    }
}

/// "+27% over pace · maxes out 11/27(Thu) at this pace" when over pace.
pub fn pace_line(w: &UsageWindow, now: Unix, tz: &dyn Tz) -> Option<String> {
    let mut text = format::over_pace(w, now)?;
    if let Some(ex) = format::exhaustion(w, now, tz) {
        text.push_str(" · ");
        text.push_str(&ex);
    }
    Some(text)
}

fn setup_hint(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "Claude Code にログインすると表示されます（claude を起動してログイン）",
        Agent::Codex => "Codex CLI をインストールしてログインすると表示されます（codex login）",
        Agent::Agy => "Antigravity CLI (agy 1.1.11 以降) をインストールすると表示されます",
    }
}

/// Children of `parent`, in order: the summary row (its label second), the
/// row of cards (one per agent, in `Agent::ALL` order) and the history card.
pub fn build(tree: &mut UiTree, parent: WidgetId, input: &Input) {
    let now = input.now;
    let answered: Vec<UsageSource> = input.sources.iter().flatten().cloned().collect();
    let summary_row = tree.insert(Some(parent), Container::new(style::row(10.0)));
    tree.insert(
        Some(summary_row),
        Label::new("最初に上限に達する枠")
            .font_size(crate::appearance::font(12.0))
            .color(Role::Muted),
    );
    tree.insert(
        Some(summary_row),
        Label::new(summary_text(&answered, now, input.tz))
            .font_size(crate::appearance::font(14.0))
            .weight(600),
    );

    // Side by side where three fit, else wrapped two and one.
    let row = tree.insert(
        Some(parent),
        Container::new(Style {
            align_items: noa_gpui_core::style::AlignItems::START,
            ..style::wrap_row(style::GUTTER)
        }),
    );
    for agent in Agent::ALL {
        match answered.iter().find(|s| s.agent == agent) {
            Some(source) => card(tree, row, source, now, input.tz),
            None => fetching_card(tree, row, agent),
        };
    }
    history_card(tree, parent, input);
}

/// Why a stale card's values are old, from the source's own data: a window
/// that reset after they were read, or their age past `stale_after`. `None`
/// when neither holds (the source marked them stale; its notes say why).
pub fn stale_reason(s: &UsageSource, now: Unix) -> Option<String> {
    let last = s.last_updated?;
    let reset: Vec<&str> = s
        .windows
        .iter()
        .filter(|w| w.resets_at.is_some_and(|r| r <= now && last < r))
        .map(|w| w.label.as_str())
        .collect();
    if !reset.is_empty() {
        Some(format!("古い値: {} がリセット済み", reset.join("・")))
    } else if now - last > s.stale_after {
        Some(format!(
            "古い値: 取得から{}経過",
            style::duration((now - last) as u64)
        ))
    } else {
        None
    }
}

/// A card and its header row, which starts with the state mark and the
/// agent's name.
fn card_head(
    tree: &mut UiTree,
    parent: WidgetId,
    agent: Agent,
    mark: Paint,
    filled: bool,
) -> (WidgetId, WidgetId) {
    let card = style::card(
        tree,
        parent,
        Style {
            gap: gap(8.0),
            ..style::wrap_item(CARD_BASIS, 1.0)
        },
    );
    let header = tree.insert(Some(card), Container::new(style::row(8.0)));
    tree.insert(
        Some(header),
        Dot {
            color: mark,
            filled,
        },
    );
    tree.insert(
        Some(header),
        Label::new(agent.display())
            .font_size(crate::appearance::font(15.0))
            .weight(700),
    );
    (card, header)
}

/// The card of a source that has not answered yet: "取得中…". Only the
/// source can tell that it is unconfigured (L1-2.4), and a first answer can
/// take a while (a CLI starting, a request timing out).
fn fetching_card(tree: &mut UiTree, parent: WidgetId, agent: Agent) -> WidgetId {
    let (card, header) = card_head(tree, parent, agent, Paint::Role(Role::Muted), false);
    style::spacer(tree, header);
    tree.insert(
        Some(header),
        Label::new(FETCHING)
            .font_size(crate::appearance::font(11.0))
            .color(Role::Muted),
    );
    card
}

fn card(tree: &mut UiTree, parent: WidgetId, s: &UsageSource, now: Unix, tz: &dyn Tz) -> WidgetId {
    let state = s.aged_state(now);
    let (mark, filled) = match &state {
        SourceState::Ok => (Paint::Fixed(GREEN), true),
        SourceState::Stale { .. } => (Paint::Fixed(ORANGE), false),
        SourceState::Error(_) => (Paint::Fixed(RED), true),
        SourceState::Unconfigured => (Paint::Role(Role::Muted), false),
    };
    let (card, header) = card_head(tree, parent, s.agent, mark, filled);
    if let Some(plan) = &s.plan {
        tree.insert(Some(header), Tag::new(capitalised(plan)).tone(Tone::Accent));
    }
    style::spacer(tree, header);

    let log = if s.origin == Origin::Log {
        "ログから · "
    } else {
        ""
    };
    // The lines under the header saying why the values are old or missing;
    // the source's notes are one of them for a stale card, else under the rows.
    let mut reasons: Vec<String> = Vec::new();
    let mut notes_shown = false;
    let age = match &state {
        SourceState::Ok => s
            .last_updated
            .map(|t| format!("{log}{}に取得", style::ago(t, now))),
        SourceState::Stale { since } => {
            reasons.extend(stale_reason(s, now));
            if !s.notes.is_empty() {
                reasons.push(s.notes.join(" · "));
                notes_shown = true;
            }
            if reasons.is_empty() {
                reasons.push("古い値: 最後に取得できた値です".to_owned());
            }
            Some(if log.is_empty() {
                format!("古い値 · {}", style::ago(*since, now))
            } else {
                format!("{log}{}", style::ago(*since, now))
            })
        }
        SourceState::Error(why) => {
            reasons.push(why.clone());
            s.last_updated
                .filter(|_| !s.windows.is_empty())
                .map(|t| format!("古い値 · {}", style::ago(t, now)))
        }
        SourceState::Unconfigured => Some("未設定".to_owned()),
    };
    if let Some(age) = age {
        tree.insert(
            Some(header),
            Label::new(age)
                .font_size(crate::appearance::font(11.0))
                .color(Role::Muted),
        );
    }
    let has_rows = !s.windows.is_empty() || !s.buckets.is_empty() || s.spend.is_some();
    let reason_color = if matches!(state, SourceState::Error(_)) && !has_rows {
        Paint::Fixed(RED)
    } else {
        Paint::Role(Role::Muted)
    };
    for reason in &reasons {
        tree.insert(
            Some(card),
            Label::new(reason.clone())
                .font_size(crate::appearance::font(11.5))
                .color(reason_color)
                .wrap(true),
        );
    }
    if state == SourceState::Unconfigured {
        tree.insert(
            Some(card),
            Label::new(setup_hint(s.agent))
                .font_size(crate::appearance::font(11.5))
                .color(Role::Muted)
                .wrap(true),
        );
        return card;
    }
    if !has_rows && reasons.is_empty() {
        caption(tree, card, "取得した枠はありません");
    }

    // Values that are not current: every row at half opacity (L1-2.4).
    let dimmed = state != SourceState::Ok;
    let opacity = if dimmed { STALE_OPACITY } else { 1.0 };
    let bucket_rows = |tree: &mut UiTree, indent: f32| {
        for b in &s.buckets {
            gauge_row(
                tree,
                card,
                &b.label,
                indent,
                Gauge {
                    used: b.used_percent,
                    tick: None,
                    color: severity_paint(Severity::from_used(b.used_percent)),
                    dimmed,
                },
                "",
                opacity,
            );
        }
    };
    // Scoped buckets go under the 7d row, else the last window; with no
    // window at all they are rows of their own.
    let weekly = s
        .windows
        .iter()
        .position(|w| w.label == "7d" || w.id.ends_with("7d"))
        .or(s.windows.len().checked_sub(1));
    for (i, w) in s.windows.iter().enumerate() {
        let severity = w.severity(now);
        gauge_row(
            tree,
            card,
            &w.label,
            0.0,
            Gauge {
                used: w.used_percent,
                tick: w.elapsed_fraction(now),
                color: severity_paint(severity),
                dimmed,
            },
            &format::resets_in(w.resets_at, now, tz),
            opacity,
        );
        if let Some(line) = pace_line(w, now, tz) {
            sub_line(
                tree,
                card,
                line,
                if dimmed { None } else { Some(severity) },
                opacity,
            );
        }
        if Some(i) == weekly {
            bucket_rows(tree, 12.0);
        }
    }
    if weekly.is_none() {
        bucket_rows(tree, 0.0);
    }
    if let Some(spend) = &s.spend {
        gauge_row(
            tree,
            card,
            &spend.label,
            0.0,
            Gauge {
                used: spend.used_percent,
                tick: None,
                color: severity_paint(Severity::from_used(spend.used_percent)),
                dimmed,
            },
            "",
            opacity,
        );
        sub_line(tree, card, spend.detail.clone(), None, opacity);
    }
    if !notes_shown {
        // A first failure's source may also put its reason among the notes:
        // the reason is on the card once.
        for note in s.notes.iter().filter(|n| !reasons.contains(n)) {
            tree.insert(
                Some(card),
                Label::new(note.clone())
                    .font_size(crate::appearance::font(11.0))
                    .color(Role::Muted)
                    .wrap(true),
            );
        }
    }
    card
}

/// One window, bucket or spend: label, gauge, percent and reset, drawn at
/// `opacity`.
fn gauge_row(
    tree: &mut UiTree,
    parent: WidgetId,
    label: &str,
    indent: f32,
    gauge: Gauge,
    reset: &str,
    opacity: f32,
) {
    let row = tree.insert(
        Some(parent),
        Faded {
            style: style::row(8.0),
            opacity,
        },
    );
    let name = Label::new(label.to_owned())
        .font_size(crate::appearance::font(ROW_FONT))
        .color(if indent > 0.0 {
            Role::Muted
        } else {
            Role::Text
        });
    if indent > 0.0 {
        let host = tree.insert(
            Some(row),
            Container::new(Style {
                padding: LayoutRect {
                    left: length(indent),
                    right: length(0.0),
                    top: length(0.0),
                    bottom: length(0.0),
                },
                ..Default::default()
            }),
        );
        cell(tree, host, LABEL_W - indent, false, name);
    } else {
        cell(tree, row, LABEL_W, false, name);
    }
    let used = gauge.used;
    tree.insert(Some(row), gauge);
    cell(
        tree,
        row,
        PERCENT_W,
        true,
        Label::new(format::percent(used))
            .font_size(crate::appearance::font(ROW_FONT))
            .weight(600),
    );
    cell(
        tree,
        row,
        RESET_W,
        true,
        Label::new(reset.to_owned())
            .font_size(crate::appearance::font(11.5))
            .color(Role::Muted),
    );
}

/// A line under a row, lined up with the gauge, drawn at `opacity`.
fn sub_line(
    tree: &mut UiTree,
    parent: WidgetId,
    text: String,
    severity: Option<Severity>,
    opacity: f32,
) {
    let host = tree.insert(
        Some(parent),
        Faded {
            style: Style {
                padding: LayoutRect {
                    left: length(LABEL_W + 8.0),
                    right: length(0.0),
                    top: length(0.0),
                    bottom: length(0.0),
                },
                margin: LayoutRect {
                    left: length(0.0),
                    right: length(0.0),
                    top: length(-4.0),
                    bottom: length(0.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            opacity,
        },
    );
    let color = match severity {
        Some(s) => severity_paint(s),
        None => Paint::Role(Role::Muted),
    };
    tree.insert(
        Some(host),
        Label::new(text)
            .font_size(crate::appearance::font(11.0))
            .color(color)
            .wrap(true),
    );
}

/// "max" → "Max".
fn capitalised(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `c` mixed toward white by `f`.
fn tint(c: Color, f: f32) -> Color {
    Color::rgba(
        c.r + (1.0 - c.r) * f,
        c.g + (1.0 - c.g) * f,
        c.b + (1.0 - c.b) * f,
        c.a,
    )
}

pub fn series_color(label: &str) -> Color {
    let lower = label.to_ascii_lowercase();
    let base = if lower.starts_with("claude") {
        CLAUDE
    } else if lower.starts_with("codex") {
        CODEX
    } else {
        AGY
    };
    if lower.contains("5h") {
        tint(base, 0.45)
    } else {
        base
    }
}

pub fn period_label(period: Period) -> &'static str {
    match period {
        Period::Today => "今日",
        Period::Days7 => "7日",
        Period::Days30 => "30日",
        Period::All => "全期間",
    }
}

pub fn filter_label(filter: AgentFilter) -> &'static str {
    match filter {
        AgentFilter::All => "全体",
        AgentFilter::One(a) => match a {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Agy => "agy",
        },
    }
}

/// The chart's category labels for the history bins: the clock time within
/// one day ("9:05", or "9時" for whole hours), the date and time over several
/// days ("11/18 9:30"), the date alone for whole-day bins.
pub fn bin_labels(bins: &[Unix], tz: &dyn Tz) -> Vec<String> {
    let step = match bins {
        [a, b, ..] => b - a,
        _ => 3600,
    };
    let one_day = match (bins.first(), bins.last()) {
        (Some(a), Some(b)) => local(*a, tz).date == local(*b, tz).date,
        _ => true,
    };
    bins.iter()
        .map(|t| {
            let l = local(*t, tz);
            let date = format!("{}/{}", l.date.month, l.date.day);
            let minute = (t + i64::from(tz.offset_secs(*t))).rem_euclid(3600) / 60;
            let clock = if step % 3600 == 0 {
                format!("{}時", l.hour)
            } else {
                format!("{}:{minute:02}", l.hour)
            };
            if step >= 86_400 {
                date
            } else if one_day {
                clock
            } else {
                format!("{date} {clock}")
            }
        })
        .collect()
}

fn history_card(tree: &mut UiTree, parent: WidgetId, input: &Input) {
    let card = style::card(tree, parent, Style::default());
    let head = tree.insert(Some(card), Container::new(style::row(10.0)));
    heading(tree, head, "制限の推移");
    caption(
        tree,
        head,
        format!(
            "{} · {} · 使用率 (%)",
            period_label(input.period),
            filter_label(input.filter)
        ),
    );
    let bins = history_bins(input.range);
    let series: Vec<ChartSeries> = input
        .series
        .iter()
        .filter(|s| s.points.iter().any(|(_, v)| v.is_finite()))
        .map(|s| {
            ChartSeries::new(
                s.label.clone(),
                bin_series(s, &bins, input.range.1, super::MAX_GAP),
            )
            .color(series_color(&s.label))
        })
        .collect();
    if series.is_empty() {
        caption(
            tree,
            card,
            "記録された値はまだありません（取得できた値を limits.jsonl に記録します）",
        );
        return;
    }
    let mut chart = Chart::line(bin_labels(&bins, input.tz))
        .range(0.0, 100.0)
        .format(|v| format!("{v:.0}%"))
        .height(250.0);
    for s in series {
        chart = chart.series(s);
    }
    tree.insert(Some(card), chart);
}
