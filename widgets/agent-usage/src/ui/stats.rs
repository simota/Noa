//! The 統計 tab: KPIs for the selection, daily and cumulative tokens, the
//! per-model share, the hour × weekday heatmap, and tables per agent, model,
//! project, tool and Noa worker. A value an agent cannot provide is "—".

use noa_gpui_core::style::{FlexDirection, Size as LayoutSize, length};
use noa_gpui_core::widgets::{Chart, ChartSeries, Container, Label};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use agent_usage::model::Agent;
use agent_usage::stats::types::{AgentFilter, Availability, Counts, Period, StatsReport};

use super::charts::{Heatmap, ShareBar, Swatch, WEEKDAYS_JA, fold_shares, heat_max};
use super::limits::{filter_label, period_label};
use super::style::{
    self, agent_color, caption, card, cell, count, duration, gap, heading, tokens, usd,
};

pub const DASH: &str = "—";
const PROJECTS_SHOWN: usize = 15;
const HEAT_COLOR: Color = Color::hex(0x0a84ff);

/// Whether the selection has anything with token counts (agy alone has none).
pub fn tokens_known(report: &StatsReport) -> bool {
    report.per_agent.is_empty()
        || report
            .per_agent
            .iter()
            .any(|(_, _, a)| *a == Availability::Full)
}

fn has_tokens(a: &Availability) -> bool {
    *a == Availability::Full
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
        ("セッション", count(t.sessions), String::new()),
        ("ターン", count(t.c.turns), String::new()),
        ("メッセージ", or_dash(k, count(t.c.messages)), String::new()),
        (
            "ツール呼び出し",
            or_dash(k, count(t.c.tool_calls)),
            String::new(),
        ),
        ("稼働時間", duration(t.c.active_secs), String::new()),
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
                "agy はトークンを記録しません".to_owned()
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
                any(count(c.workers)),
            ]
        })
        .collect()
}

pub fn build(
    tree: &mut UiTree,
    parent: WidgetId,
    report: &StatsReport,
    period: Period,
    filter: AgentFilter,
) {
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

    let second = tree.insert(Some(parent), Container::new(stretch_row()));
    share_card(tree, second, report);
    heatmap_card(tree, second, report);

    agent_table(tree, parent, report);
    model_table(tree, parent, report);
    let last = tree.insert(Some(parent), Container::new(stretch_row()));
    project_table(tree, last, report);
    tools_table(tree, last, report);
    workers_table(tree, last, report);
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

fn kpi_row(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
    let row = tree.insert(Some(parent), Container::new(stretch_row()));
    for (title, value, detail) in kpis(report) {
        let grow = match title {
            "総トークン" => 2.6,
            "推定コスト" => 1.8,
            "稼働時間" => 1.3,
            _ => 1.0,
        };
        let c = card(
            tree,
            row,
            Style {
                gap: gap(4.0),
                ..flex(grow)
            },
        );
        caption(tree, c, title);
        tree.insert(Some(c), Label::new(value).font_size(20.0).weight(700));
        for line in detail.lines() {
            caption(tree, c, line.to_owned());
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
        caption(tree, card, "agy はトークンを記録しません");
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
            ..flex(1.0)
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
        cell(tree, row, 190.0, false, Label::new(name).font_size(12.0));
        cell(
            tree,
            row,
            80.0,
            true,
            Label::new(tokens(t)).font_size(12.0).color(Role::Muted),
        );
        cell(
            tree,
            row,
            56.0,
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
            ..Default::default()
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

/// A table of fixed-width columns; the first `left` columns align at the
/// start, the others at the end.
fn table(
    tree: &mut UiTree,
    parent: WidgetId,
    columns: &[(&str, f32)],
    left: usize,
    rows: &[Vec<String>],
) {
    let header = tree.insert(Some(parent), Container::new(style::row(0.0)));
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
            Some(parent),
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
}

fn agent_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
    let c = card(tree, parent, Style::default());
    heading(tree, c, "エージェント別");
    if report.per_agent.is_empty() {
        caption(tree, c, "この期間の記録はありません");
        return;
    }
    table(
        tree,
        c,
        &[
            ("エージェント", 110.0),
            ("セッション", 72.0),
            ("ターン", 66.0),
            ("メッセージ", 76.0),
            ("ツール", 66.0),
            ("稼働時間", 92.0),
            ("総トークン", 86.0),
            ("入力", 76.0),
            ("出力", 76.0),
            ("キャッシュ読取", 96.0),
            ("キャッシュ書込", 96.0),
            ("推論", 70.0),
            ("推定コスト", 180.0),
            ("ワーカー", 62.0),
        ],
        1,
        &agent_rows(report),
    );
    for (agent, _, a) in &report.per_agent {
        if let Availability::Missing(why) = a {
            caption(tree, c, format!("{}: {why}", agent.display()));
        }
    }
}

fn model_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
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
    table(
        tree,
        c,
        &[
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
        ],
        2,
        &rows,
    );
}

fn project_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
    let c = card(tree, parent, flex(2.2));
    heading(tree, c, "プロジェクト別 (トークン上位)");
    if report.per_project.is_empty() {
        caption(tree, c, "プロジェクトの記録がありません");
        return;
    }
    let k = tokens_known(report);
    let rows: Vec<Vec<String>> = report
        .per_project
        .iter()
        .take(PROJECTS_SHOWN)
        .map(|r| {
            vec![
                r.key.clone(),
                count(r.counts.sessions),
                count(r.counts.c.turns),
                or_dash(k, tokens(r.counts.c.tokens.total())),
                match r.counts.cost_usd {
                    Some(v) if k => usd(v),
                    _ => DASH.to_owned(),
                },
            ]
        })
        .collect();
    table(
        tree,
        c,
        &[
            ("プロジェクト", 230.0),
            ("セッション", 72.0),
            ("ターン", 66.0),
            ("トークン", 86.0),
            ("推定コスト", 86.0),
        ],
        1,
        &rows,
    );
    let rest =
        report.other_projects + report.per_project.len().saturating_sub(PROJECTS_SHOWN) as u64;
    if rest > 0 {
        caption(tree, c, format!("その他 {}件", count(rest)));
    }
}

fn tools_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
    let c = card(tree, parent, flex(1.0));
    heading(tree, c, "ツール上位");
    if report.tools.is_empty() {
        caption(
            tree,
            c,
            if tokens_known(report) {
                "ツール呼び出しの記録がありません"
            } else {
                "agy はツール呼び出しを記録しません"
            },
        );
        return;
    }
    let rows: Vec<Vec<String>> = report
        .tools
        .iter()
        .take(PROJECTS_SHOWN)
        .map(|(name, n)| vec![name.clone(), count(*n)])
        .collect();
    table(tree, c, &[("ツール", 150.0), ("回数", 70.0)], 1, &rows);
}

fn workers_table(tree: &mut UiTree, parent: WidgetId, report: &StatsReport) {
    let c = card(tree, parent, flex(1.0));
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
    table(
        tree,
        c,
        &[("エージェント", 130.0), ("起動数", 70.0)],
        1,
        &rows,
    );
    caption(tree, c, "ctl ジャーナルの spawn から");
}
