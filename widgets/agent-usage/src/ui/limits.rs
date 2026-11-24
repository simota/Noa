//! The 制限 tab: the window that runs out first, one card per agent (fixed
//! order Claude Code, Codex, Antigravity) with a gauge per window, and the
//! limit history chart.

use noa_gpui_core::style::{FlexDirection, Rect as LayoutRect, Size as LayoutSize, length};
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
    self, AGY, CLAUDE, CODEX, GREEN, ORANGE, RED, caption, cell, gap, heading, severity_paint,
};

const LABEL_W: f32 = 96.0;
const PERCENT_W: f32 = 42.0;
const RESET_W: f32 = 84.0;
const ROW_FONT: f32 = 12.0;

pub struct Input<'a> {
    pub sources: &'a [UsageSource],
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
    let summary_row = tree.insert(Some(parent), Container::new(style::row(10.0)));
    tree.insert(
        Some(summary_row),
        Label::new("最初に上限に達する枠")
            .font_size(12.0)
            .color(Role::Muted),
    );
    tree.insert(
        Some(summary_row),
        Label::new(summary_text(input.sources, now, input.tz))
            .font_size(14.0)
            .weight(600),
    );

    let row = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: noa_gpui_core::style::AlignItems::START,
            gap: gap(style::GUTTER),
            flex_shrink: 0.0,
            ..Default::default()
        }),
    );
    for agent in Agent::ALL {
        if let Some(source) = input.sources.iter().find(|s| s.agent == agent) {
            card(tree, row, source, now, input.tz);
        }
    }
    history_card(tree, parent, input);
}

fn card(tree: &mut UiTree, parent: WidgetId, s: &UsageSource, now: Unix, tz: &dyn Tz) -> WidgetId {
    let card = style::card(
        tree,
        parent,
        Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: length(0.0),
            min_size: LayoutSize {
                width: length(0.0),
                height: length(0.0),
            },
            gap: gap(8.0),
            ..Default::default()
        },
    );
    let state = s.aged_state(now);
    let header = tree.insert(Some(card), Container::new(style::row(8.0)));
    let (mark, filled) = match &state {
        SourceState::Ok => (Paint::Fixed(GREEN), true),
        SourceState::Stale { .. } => (Paint::Fixed(ORANGE), false),
        SourceState::Error(_) => (Paint::Fixed(RED), true),
        SourceState::Unconfigured => (Paint::Role(Role::Muted), false),
    };
    tree.insert(
        Some(header),
        Dot {
            color: mark,
            filled,
        },
    );
    tree.insert(
        Some(header),
        Label::new(s.agent.display()).font_size(15.0).weight(700),
    );
    if let Some(plan) = &s.plan {
        tree.insert(Some(header), Tag::new(capitalised(plan)).tone(Tone::Accent));
    }
    style::spacer(tree, header);

    let log = if s.origin == Origin::Log {
        "ログから · "
    } else {
        ""
    };
    let (age, reason): (Option<String>, Option<String>) = match &state {
        SourceState::Ok => (
            s.last_updated
                .map(|t| format!("{log}{}に取得", style::ago(t, now))),
            None,
        ),
        SourceState::Stale { since } => (
            Some(if log.is_empty() {
                format!("古い値 · {}", style::ago(*since, now))
            } else {
                format!("{log}{}", style::ago(*since, now))
            }),
            (!s.notes.is_empty()).then(|| s.notes.join(" · ")),
        ),
        SourceState::Error(why) => (
            s.last_updated
                .filter(|_| !s.windows.is_empty())
                .map(|t| format!("古い値 · {}", style::ago(t, now))),
            Some(why.clone()),
        ),
        SourceState::Unconfigured => (Some("未設定".to_owned()), None),
    };
    if let Some(age) = age {
        tree.insert(
            Some(header),
            Label::new(age).font_size(11.0).color(Role::Muted),
        );
    }
    let dimmed = state != SourceState::Ok;
    if let Some(reason) = &reason {
        let color = if matches!(state, SourceState::Error(_)) && s.windows.is_empty() {
            Paint::Fixed(RED)
        } else {
            Paint::Role(Role::Muted)
        };
        tree.insert(
            Some(card),
            Label::new(reason.clone())
                .font_size(11.5)
                .color(color)
                .wrap(true),
        );
    }
    if state == SourceState::Unconfigured {
        tree.insert(
            Some(card),
            Label::new(setup_hint(s.agent))
                .font_size(11.5)
                .color(Role::Muted)
                .wrap(true),
        );
        return card;
    }
    if s.windows.is_empty() && s.spend.is_none() {
        if reason.is_none() {
            caption(tree, card, "取得した枠はありません");
        }
        return card;
    }

    // Scoped buckets go under the 7d row (or last, without one).
    let weekly = s
        .windows
        .iter()
        .position(|w| w.label == "7d" || w.id.ends_with("7d"))
        .unwrap_or(s.windows.len().saturating_sub(1));
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
            dimmed,
        );
        if let Some(line) = pace_line(w, now, tz) {
            sub_line(tree, card, line, if dimmed { None } else { Some(severity) });
        }
        if i == weekly {
            for b in &s.buckets {
                gauge_row(
                    tree,
                    card,
                    &b.label,
                    12.0,
                    Gauge {
                        used: b.used_percent,
                        tick: None,
                        color: severity_paint(Severity::from_used(b.used_percent)),
                        dimmed,
                    },
                    "",
                    dimmed,
                );
            }
        }
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
            dimmed,
        );
        sub_line(tree, card, spend.detail.clone(), None);
    }
    if reason.is_none() {
        for note in &s.notes {
            tree.insert(
                Some(card),
                Label::new(note.clone())
                    .font_size(11.0)
                    .color(Role::Muted)
                    .wrap(true),
            );
        }
    }
    card
}

fn gauge_row(
    tree: &mut UiTree,
    parent: WidgetId,
    label: &str,
    indent: f32,
    gauge: Gauge,
    reset: &str,
    dimmed: bool,
) {
    let row = tree.insert(Some(parent), Container::new(style::row(8.0)));
    let text = if dimmed { Role::Muted } else { Role::Text };
    let name = Label::new(label.to_owned())
        .font_size(ROW_FONT)
        .color(if indent > 0.0 { Role::Muted } else { text });
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
            .font_size(ROW_FONT)
            .weight(600)
            .color(text),
    );
    cell(
        tree,
        row,
        RESET_W,
        true,
        Label::new(reset.to_owned())
            .font_size(11.5)
            .color(Role::Muted),
    );
}

/// A line under a row, lined up with the gauge.
fn sub_line(tree: &mut UiTree, parent: WidgetId, text: String, severity: Option<Severity>) {
    let host = tree.insert(
        Some(parent),
        Container::new(Style {
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
        }),
    );
    let color = match severity {
        Some(s) => severity_paint(s),
        None => Paint::Role(Role::Muted),
    };
    tree.insert(
        Some(host),
        Label::new(text).font_size(11.0).color(color).wrap(true),
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

/// The chart's category labels for the history bins.
pub fn bin_labels(bins: &[Unix], tz: &dyn Tz) -> Vec<String> {
    let step = if bins.len() > 1 {
        bins[1] - bins[0]
    } else {
        3600
    };
    bins.iter()
        .map(|t| {
            let l = local(*t, tz);
            // Several days of sub-day bins: the date at midnight, the hour
            // otherwise, so the labels stay short enough to show often.
            if step >= 86_400 || (bins.len() > 25 && l.hour == 0) {
                format!("{}/{}", l.date.month, l.date.day)
            } else {
                format!("{}時", l.hour)
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
            ChartSeries::new(s.label.clone(), bin_series(s, &bins, input.range.1))
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
