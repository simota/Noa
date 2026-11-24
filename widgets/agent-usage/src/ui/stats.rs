//! The 統計 tab: KPIs for the selection, daily and cumulative tokens, the
//! per-model share, the hour × weekday heatmap, and tables per agent, model,
//! project, tool and Noa worker. A value an agent cannot provide is "—".

use noa_gpui_core::style::{FlexDirection, FlexWrap, Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{Chart, ChartSeries, Container, Label, ScrollView, ScrollerStyle};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use agent_usage::model::Agent;
use agent_usage::stats::types::{AgentFilter, Availability, Counts, Period, StatsReport};

use super::charts::{Heatmap, ShareBar, Swatch, WEEKDAYS_JA, fold_shares, heat_max};
use super::limits::{filter_label, period_label};
use super::style::{
    self, agent_color, caption, card, cell, count, duration, gap, heading, tokens, usd,
};
use super::style::{wrap_item, wrap_row};

pub const DASH: &str = "—";
const PROJECTS_SHOWN: usize = 15;
const HEAT_COLOR: Color = Color::hex(0x0a84ff);
const LEGEND_NAME: f32 = 190.0;
const LEGEND_TOKENS: f32 = 80.0;
const LEGEND_PERCENT: f32 = 56.0;
/// A legend line: swatch, name, tokens and percent with the gaps between.
const SHARE_LEGEND: f32 = 10.0 + LEGEND_NAME + LEGEND_TOKENS + LEGEND_PERCENT + 3.0 * 8.0;
/// A card's padding and border on both sides.
const CARD_CHROME: f32 = 2.0 * 12.0 + 2.0;
/// The room a table's scroll view keeps for its bars (the legacy track).
const SCROLL_TRACK: f32 = 14.0;

type Columns = [(&'static str, f32)];

const AGENT_COLUMNS: [(&str, f32); 14] = [
    ("エージェント", 104.0),
    ("セッション", 68.0),
    ("ターン", 60.0),
    ("メッセージ", 72.0),
    ("ツール", 60.0),
    ("稼働時間", 84.0),
    ("総トークン", 84.0),
    ("入力", 70.0),
    ("出力", 70.0),
    ("キャッシュ読取", 92.0),
    ("キャッシュ書込", 92.0),
    ("推論", 64.0),
    ("推定コスト", 220.0),
    ("ワーカー", 60.0),
];
const MODEL_COLUMNS: [(&str, f32); 10] = [
    ("エージェント", 110.0),
    ("モデル", 190.0),
    ("入力", 84.0),
    ("出力", 84.0),
    ("キャッシュ読取", 104.0),
    ("キャッシュ書込", 104.0),
    ("推論", 80.0),
    ("合計", 90.0),
    ("メッセージ", 84.0),
    ("推定コスト", 220.0),
];
const PROJECT_COLUMNS: [(&str, f32); 5] = [
    ("プロジェクト", 220.0),
    ("セッション", 72.0),
    ("ターン", 66.0),
    ("トークン", 86.0),
    ("推定コスト", 220.0),
];
const TOOL_COLUMNS: [(&str, f32); 2] = [("ツール", 150.0), ("回数", 70.0)];
const WORKER_COLUMNS: [(&str, f32); 2] = [("エージェント", 130.0), ("起動数", 70.0)];

/// The tables' horizontal scroll views, by a name that survives a rebuild.
pub type Scrolls = Vec<(&'static str, WidgetId)>;

/// Whether the selection has anything with token counts (agy alone has none,
/// an agent whose logs are missing has none either).
pub fn tokens_known(report: &StatsReport) -> bool {
    report.per_agent.is_empty()
        || report
            .per_agent
            .iter()
            .any(|(_, _, a)| *a == Availability::Full)
}

/// Whether any selected agent's logs were read: sessions, turns and active
/// time come from them. Noa workers come from the ctl journals instead and
/// are known either way.
pub fn logs_known(report: &StatsReport) -> bool {
    report.per_agent.is_empty()
        || report
            .per_agent
            .iter()
            .any(|(_, _, a)| !matches!(a, Availability::Missing(_)))
}

fn has_tokens(a: &Availability) -> bool {
    *a == Availability::Full
}

/// Why the selection has no `what` ("トークン", "ツール呼び出し"): agy does
/// not record it, a missing log says why.
pub fn unavailable_reason(report: &StatsReport, what: &str) -> String {
    let parts: Vec<String> = report
        .per_agent
        .iter()
        .filter_map(|(agent, _, a)| match a {
            Availability::Full => None,
            Availability::NoTokens => Some(format!("agy は{what}を記録しません")),
            Availability::Missing(why) => Some(format!("{}: {why}", agent.display())),
        })
        .collect();
    parts.join(" · ")
}

/// "$612.40", "$612.40 · 一部未計上 (1.2M tokens)", or "—".
pub fn cost_text(c: &Counts, known: bool) -> String {
    match (known, c.cost_usd) {
        (false, _) | (true, None) => DASH.to_owned(),
        (true, Some(v)) if c.unpriced_tokens > 0 => format!(
            "{} · 一部未計上 ({} tokens)",
            usd(v),
            tokens(c.unpriced_tokens)
        ),
        (true, Some(v)) => usd(v),
    }
}

fn or_dash(known: bool, text: String) -> String {
    if known { text } else { DASH.to_owned() }
}

/// (title, value, detail) of each KPI card.
pub fn kpis(report: &StatsReport) -> Vec<(&'static str, String, String)> {
    let t = &report.total;
    let l = logs_known(report);
    let k = tokens_known(report);
    let tk = &t.c.tokens;
    let cost_detail = match (k, t.cost_usd) {
        (true, Some(_)) if t.unpriced_tokens > 0 => {
            format!("API換算\n一部未計上 ({} tokens)", tokens(t.unpriced_tokens))
        }
        (true, None) if t.unpriced_tokens > 0 => {
            format!("API換算\n未計上 {} tokens", tokens(t.unpriced_tokens))
        }
        _ => "API換算".to_owned(),
    };
    vec![
        ("セッション", or_dash(l, count(t.sessions)), String::new()),
        ("ターン", or_dash(l, count(t.c.turns)), String::new()),
        ("メッセージ", or_dash(k, count(t.c.messages)), String::new()),
        (
            "ツール呼び出し",
            or_dash(k, count(t.c.tool_calls)),
            String::new(),
        ),
        (
            "稼働時間",
            or_dash(l, duration(t.c.active_secs)),
            String::new(),
        ),
        (
            "総トークン",
            or_dash(k, tokens(tk.total())),
            if k {
                format!(
                    "入力 {} · 出力 {} · 推論 {}\nキャッシュ読取 {} · キャッシュ書込 {}",
                    tokens(tk.input),
                    tokens(tk.output),
                    tokens(tk.reasoning),
                    tokens(tk.cache_read),
                    tokens(tk.cache_write)
                )
            } else {
                unavailable_reason(report, "トークン")
            },
        ),
        (
            "推定コスト",
            match (k, t.cost_usd) {
                (true, Some(v)) => usd(v),
                _ => DASH.to_owned(),
            },
            cost_detail,
        ),
    ]
}

/// The per-agent table's rows.
pub fn agent_rows(report: &StatsReport) -> Vec<Vec<String>> {
    report
        .per_agent
        .iter()
        .map(|(agent, c, a)| {
            let missing = matches!(a, Availability::Missing(_));
            let k = has_tokens(a);
            let tk = &c.c.tokens;
            let any = |v: String| if missing { DASH.to_owned() } else { v };
            vec![
                agent.display().to_owned(),
                any(count(c.sessions)),
                any(count(c.c.turns)),
                or_dash(k, count(c.c.messages)),
                or_dash(k, count(c.c.tool_calls)),
                any(duration(c.c.active_secs)),
                or_dash(k, tokens(tk.total())),
                or_dash(k, tokens(tk.input)),
                or_dash(k, tokens(tk.output)),
                or_dash(k, tokens(tk.cache_read)),
                or_dash(k, tokens(tk.cache_write)),
                or_dash(k, tokens(tk.reasoning)),
                cost_text(c, k),
                // From the ctl journals, not the logs.
                count(c.workers),
            ]
        })
        .collect()
}

/// Builds the tab under `parent` and returns the tables' scroll views.
/// Everything is reachable at the window's minimum width: rows of cards wrap,
/// and a table wider than its card scrolls sideways.
pub fn build(
    tree: &mut UiTree,
    parent: WidgetId,
    report: &StatsReport,
    period: Period,
    filter: AgentFilter,
) -> Scrolls {
    let mut scrolls = Scrolls::new();
    let head = tree.insert(Some(parent), Container::new(style::row(10.0)));
    heading(
        tree,
        head,
        format!("{} · {}", period_label(period), filter_label(filter)),
    );
    let c = &report.coverage;
    if c.lines_bad + c.lines_skipped > 0 {
        caption(
            tree,
            head,
            format!(
                "読めなかった行 {} · 大きすぎて飛ばした行 {}",
                count(c.lines_bad),
                count(c.lines_skipped)
            ),
        );
    }

    kpi_row(tree, parent, report);

    let charts = tree.insert(Some(parent), Container::new(stretch_row()));
    daily_card(tree, charts, report, period, filter);
    cumulative_card(tree, charts, report, filter);

    let second = tree.insert(Some(parent), Container::new(wrap_row(style::GUTTER)));
    share_card(tree, second, report);
    heatmap_card(tree, second, report);

    agent_table(tree, parent, report, &mut scrolls);
    model_table(tree, parent, report, &mut scrolls);
    let last = tree.insert(Some(parent), Container::new(wrap_row(style::GUTTER)));
    project_table(tree, last, report, &mut scrolls);
    tools_table(tree, last, report, &mut scrolls);
    workers_table(tree, last, report, &mut scrolls);
    scrolls
}

fn stretch_row() -> Style {
    Style {
        flex_direction: FlexDirection::Row,
        gap: gap(style::GUTTER),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

fn flex(grow: f32) -> Style {
    Style {
        flex_grow: grow,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: length(0.0),
        },
        ..Default::default()
    }
}

/// The width a KPI card asks for before the row shares out the rest.
fn kpi_basis(title: &str) -> f32 {
    match title {
        "総トークン" => 290.0,
        "推定コスト" => 200.0,
        "稼働時間" => 150.0,
        _ => 110.0,
    }
}

fn kpi_grow(title: &str) -> f32 {
    match title {
        "総トークン" => 2.6,
        "推定コスト" => 1.8,
        "稼働時間" => 1.3,
        _ => 1.0,
    }
}

/// The counts, then tokens and cost: two groups that share a line where
/// both fit and wrap as wholes where not.
fn kpi_row(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
    let row = tree.insert(Some(parent), Container::new(wrap_row(style::GUTTER)));
    let all = kpis(report);
    for group in [&all[..5], &all[5..]] {
        let basis = group.iter().map(|k| kpi_basis(k.0)).sum::<f32>()
            + style::GUTTER * (group.len() - 1) as f32;
        let line = tree.insert(
            Some(row),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                gap: gap(style::GUTTER),
                ..wrap_item(basis, basis / 100.0)
            }),
        );
        for (title, value, detail) in group {
            let c = card(
                tree,
                line,
                Style {
                    gap: gap(4.0),
                    ..wrap_item(kpi_basis(title), kpi_grow(title))
                },
            );
            caption(tree, c, *title);
            tree.insert(
                Some(c),
                Label::new(value.clone())
                    .font_size(20.0)
                    .weight(700)
                    .wrap(true),
            );
            for text in detail.lines() {
                tree.insert(
                    Some(c),
                    Label::new(text.to_owned())
                        .font_size(11.0)
                        .color(Role::Muted)
                        .wrap(true),
                );
            }
        }
    }
}

fn token_series(
    rows: &[(String, [u64; 3])],
    filter: AgentFilter,
) -> (Vec<String>, Vec<ChartSeries>) {
    let labels = rows.iter().map(|(l, _)| l.clone()).collect();
    let series = [Agent::Claude, Agent::Codex]
        .into_iter()
        .filter(|a| filter.matches(*a))
        .map(|a| {
            ChartSeries::new(
                a.display(),
                rows.iter().map(|(_, v)| v[a.index()] as f64).collect(),
            )
            .color(agent_color(a))
        })
        .collect();
    (labels, series)
}

fn no_tokens(tree: &mut UiTree, card: WidgetId, report: &StatsReport) -> bool {
    if report.days.is_empty() {
        caption(tree, card, "この期間の記録はありません");
        return true;
    }
    if !tokens_known(report) {
        caption(tree, card, unavailable_reason(report, "トークン"));
        return true;
    }
    false
}

fn token_axis(v: f64) -> String {
    tokens(v.max(0.0) as u64)
}

fn daily_card(
    tree: &mut UiTree,
    parent: WidgetId,
    report: &StatsReport,
    period: Period,
    filter: AgentFilter,
) {
    let c = card(tree, parent, flex(1.0));
    // B labels "all" longer than 92 days by ISO week (L1-4.1).
    let unit = match period {
        Period::Today => "時間別",
        Period::All if report.range.1 - report.range.0 > 92 * 86_400 => "週別",
        _ => "日別",
    };
    heading(tree, c, format!("{unit}トークン"));
    if no_tokens(tree, c, report) {
        return;
    }
    let (labels, series) = token_series(&report.days, filter);
    let mut chart = Chart::bar(labels)
        .stacked(true)
        .format(token_axis)
        .height(220.0);
    for s in series {
        chart = chart.series(s);
    }
    tree.insert(Some(c), chart);
}

fn cumulative_card(tree: &mut UiTree, parent: WidgetId, report: &StatsReport, filter: AgentFilter) {
    let c = card(tree, parent, flex(1.0));
    heading(tree, c, "累積トレンド");
    if no_tokens(tree, c, report) {
        return;
    }
    let (labels, series) = token_series(&report.cumulative, filter);
    let mut chart = Chart::line(labels).format(token_axis).height(220.0);
    for s in series {
        chart = chart.series(s);
    }
    tree.insert(Some(c), chart);
}

/// Colours of the models, shades of their agent's colour by rank.
fn model_color(agent: Agent, rank: usize) -> Color {
    let base = agent_color(agent);
    let f = [0.0, 0.35, 0.6, 0.75][rank.min(3)];
    Color::rgba(
        base.r + (1.0 - base.r) * f,
        base.g + (1.0 - base.g) * f,
        base.b + (1.0 - base.b) * f,
        1.0,
    )
}

fn share_card(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
    let c = card(
        tree,
        parent,
        Style {
            gap: gap(10.0),
            ..wrap_item(SHARE_LEGEND + CARD_CHROME, 3.0)
        },
    );
    heading(tree, c, "モデル別シェア");
    let mut rows: Vec<(String, u64, Agent)> = report
        .per_model
        .iter()
        .map(|r| (r.model.clone(), r.counts.c.tokens.total(), r.agent))
        .filter(|(_, t, _)| *t > 0)
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    let folded = fold_shares(
        &rows
            .iter()
            .map(|(m, t, _)| (m.clone(), *t))
            .collect::<Vec<_>>(),
    );
    if folded.is_empty() {
        caption(tree, c, "トークンの記録がありません");
        return;
    }
    let total: u64 = folded.iter().map(|(_, t)| t).sum();
    let mut ranks = [0usize; 3];
    let parts: Vec<(String, u64, Color)> = folded
        .iter()
        .map(|(name, t)| {
            let color = match rows.iter().find(|(m, _, _)| m == name) {
                Some((_, _, agent)) => {
                    let r = ranks[agent.index()];
                    ranks[agent.index()] += 1;
                    model_color(*agent, r)
                }
                None => Color::hex(0x8e8e93),
            };
            (name.clone(), *t, color)
        })
        .collect();
    tree.insert(
        Some(c),
        ShareBar {
            parts: parts.clone(),
        },
    );
    let legend = tree.insert(Some(c), Container::new(style::column(4.0)));
    for (name, t, color) in parts {
        let row = tree.insert(Some(legend), Container::new(style::row(8.0)));
        tree.insert(Some(row), Swatch(color));
        cell(
            tree,
            row,
            LEGEND_NAME,
            false,
            Label::new(name).font_size(12.0),
        );
        cell(
            tree,
            row,
            LEGEND_TOKENS,
            true,
            Label::new(tokens(t)).font_size(12.0).color(Role::Muted),
        );
        cell(
            tree,
            row,
            LEGEND_PERCENT,
            true,
            Label::new(format!("{:.1}%", t as f64 * 100.0 / total as f64))
                .font_size(12.0)
                .weight(600),
        );
    }
}

fn heatmap_card(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
    let c = card(
        tree,
        parent,
        Style {
            gap: gap(8.0),
            ..wrap_item(Heatmap::width() + CARD_CHROME, 1.0)
        },
    );
    let head = tree.insert(Some(c), Container::new(style::row(10.0)));
    heading(tree, head, "時間帯ヒートマップ");
    let max = heat_max(&report.heatmap);
    let at = report
        .heatmap
        .iter()
        .enumerate()
        .flat_map(|(d, row)| row.iter().enumerate().map(move |(h, v)| (d, h, *v)))
        .find(|(_, _, v)| *v == max && max > 0);
    let text = match at {
        Some((d, h, _)) => format!(
            "ターン+メッセージ · 最大 {} ({} {}時)",
            count(max),
            WEEKDAYS_JA[d],
            h
        ),
        None => "ターン+メッセージ · 記録なし".to_owned(),
    };
    caption(tree, head, text);
    tree.insert(Some(c), Heatmap::new(report.heatmap, HEAT_COLOR));
}

/// The style of a card holding a table of `columns` in a wrapping row.
fn table_item(columns: &Columns, grow: f32) -> Style {
    let width: f32 = columns.iter().map(|c| c.1).sum();
    wrap_item(width + SCROLL_TRACK + CARD_CHROME, grow)
}

/// A table of fixed-width columns in a view that scrolls sideways when the
/// card is narrower; the first `left` columns align at the start, the others
/// at the end. Returns the scroll view.
fn table(
    tree: &mut UiTree,
    parent: WidgetId,
    columns: &Columns,
    left: usize,
    rows: &[Vec<String>],
) -> WidgetId {
    let width: f32 = columns.iter().map(|c| c.1).sum();
    // Vertical too, with nothing to scroll that way: a plain wheel then
    // passes on to the page instead of turning sideways.
    let scroll = tree.insert(
        Some(parent),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            ..Default::default()
        })
        .horizontal(true)
        .scroller_style(ScrollerStyle::Legacy),
    );
    let body = tree.insert(
        Some(scroll),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(6.0),
            flex_shrink: 0.0,
            size: LayoutSize {
                width: length(width),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let header = tree.insert(Some(body), Container::new(style::row(0.0)));
    for (i, (title, w)) in columns.iter().enumerate() {
        cell(
            tree,
            header,
            *w,
            i >= left,
            Label::new(*title).font_size(11.0).color(Role::Muted),
        );
    }
    for (r, row) in rows.iter().enumerate() {
        let line = tree.insert(
            Some(body),
            Container::new(Style {
                padding: noa_gpui_core::style::Rect {
                    left: length(0.0),
                    right: length(0.0),
                    top: length(3.0),
                    bottom: length(3.0),
                },
                ..style::row(0.0)
            })
            .background(if r % 2 == 0 {
                Role::SurfaceAlt
            } else {
                Role::Surface
            })
            .radius(4.0),
        );
        for (i, ((_, w), text)) in columns.iter().zip(row).enumerate() {
            cell(
                tree,
                line,
                *w,
                i >= left,
                Label::new(text.clone())
                    .font_size(12.0)
                    .weight(if i == 0 { 600 } else { 400 }),
            );
        }
    }
    scroll
}

fn agent_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport, scrolls: &mut Scrolls) {
    let c = card(tree, parent, Style::default());
    heading(tree, c, "エージェント別");
    if report.per_agent.is_empty() {
        caption(tree, c, "この期間の記録はありません");
        return;
    }
    scrolls.push((
        "agents",
        table(tree, c, &AGENT_COLUMNS, 1, &agent_rows(report)),
    ));
    for (agent, _, a) in &report.per_agent {
        if let Availability::Missing(why) = a {
            caption(tree, c, format!("{}: {why}", agent.display()));
        }
    }
}

fn model_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport, scrolls: &mut Scrolls) {
    let c = card(tree, parent, Style::default());
    heading(tree, c, "モデル別");
    if report.per_model.is_empty() {
        caption(tree, c, "モデルの記録がありません");
        return;
    }
    let rows: Vec<Vec<String>> = report
        .per_model
        .iter()
        .map(|r| {
            let t = &r.counts.c.tokens;
            vec![
                r.agent.display().to_owned(),
                r.model.clone(),
                tokens(t.input),
                tokens(t.output),
                tokens(t.cache_read),
                tokens(t.cache_write),
                tokens(t.reasoning),
                tokens(t.total()),
                count(r.counts.c.messages),
                cost_text(&r.counts, true),
            ]
        })
        .collect();
    scrolls.push(("models", table(tree, c, &MODEL_COLUMNS, 2, &rows)));
}

/// The project table's rows: name, sessions, turns, tokens and the cost,
/// which says what it leaves out (L1-3.5).
pub fn project_rows(report: &StatsReport) -> Vec<Vec<String>> {
    let k = tokens_known(report);
    report
        .per_project
        .iter()
        .take(PROJECTS_SHOWN)
        .map(|r| {
            vec![
                r.key.clone(),
                count(r.counts.sessions),
                count(r.counts.c.turns),
                or_dash(k, tokens(r.counts.c.tokens.total())),
                cost_text(&r.counts, k),
            ]
        })
        .collect()
}

fn project_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport, scrolls: &mut Scrolls) {
    let c = card(tree, parent, table_item(&PROJECT_COLUMNS, 2.2));
    heading(tree, c, "プロジェクト別 (トークン上位)");
    if report.per_project.is_empty() {
        caption(tree, c, "プロジェクトの記録がありません");
        return;
    }
    scrolls.push((
        "projects",
        table(tree, c, &PROJECT_COLUMNS, 1, &project_rows(report)),
    ));
    let rest =
        report.other_projects + report.per_project.len().saturating_sub(PROJECTS_SHOWN) as u64;
    if rest > 0 {
        caption(tree, c, format!("その他 {}件", count(rest)));
    }
}

fn tools_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport, scrolls: &mut Scrolls) {
    let c = card(tree, parent, table_item(&TOOL_COLUMNS, 1.0));
    heading(tree, c, "ツール上位");
    if report.tools.is_empty() {
        let text = if tokens_known(report) {
            "ツール呼び出しの記録がありません".to_owned()
        } else {
            unavailable_reason(report, "ツール呼び出し")
        };
        tree.insert(
            Some(c),
            Label::new(text)
                .font_size(11.0)
                .color(Role::Muted)
                .wrap(true),
        );
        return;
    }
    let rows: Vec<Vec<String>> = report
        .tools
        .iter()
        .take(PROJECTS_SHOWN)
        .map(|(name, n)| vec![name.clone(), count(*n)])
        .collect();
    scrolls.push(("tools", table(tree, c, &TOOL_COLUMNS, 1, &rows)));
}

fn workers_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport, scrolls: &mut Scrolls) {
    let c = card(tree, parent, table_item(&WORKER_COLUMNS, 1.0));
    heading(tree, c, "Noa ワーカー");
    if report.per_agent.is_empty() {
        caption(tree, c, "この期間の起動はありません");
        return;
    }
    let rows: Vec<Vec<String>> = report
        .per_agent
        .iter()
        .map(|(agent, c, _)| vec![agent.display().to_owned(), count(c.workers)])
        .collect();
    scrolls.push(("workers", table(tree, c, &WORKER_COLUMNS, 1, &rows)));
    caption(tree, c, "ctl ジャーナルの spawn から");
}
