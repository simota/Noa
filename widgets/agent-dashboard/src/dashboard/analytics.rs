//! Analytics tab: fleet KPIs, phase time breakdown, aggregate output
//! throughput, per-agent/model comparison, and wait-time bottleneck ranking.

use std::collections::BTreeMap;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Rect as LayoutRect, Size as LayoutSize, auto,
    length, percent,
};
use noa_gpui_core::widgets::{
    Chart, ChartSeries, Container, Label, ScrollView, Tag, TagStyle, Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use crate::fleet::{Phase, State, Worker, minutes_text, one_line};

use super::graph::mark;
use super::style::{
    BLUE, GRAY, GREEN, ORANGE, RED, YELLOW, caption, fill_remaining, gap, phase_color, spacer,
};
use super::{Mark, Registry};

/// Summary metrics computed over the currently filtered workers.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FleetMetrics {
    pub(super) total: usize,
    pub(super) running: usize,
    pub(super) waiting: usize,
    pub(super) finished: usize,
    pub(super) error: usize,
    pub(super) quiet: usize,
    pub(super) total_minutes: f64,
    pub(super) waited_minutes: f64,
    pub(super) wait_ratio_pct: f64,
    pub(super) avg_took_minutes: Option<f64>,
    pub(super) results_count: usize,
    pub(super) phase_minutes: [(Phase, f64); 5],
    pub(super) throughput: Vec<f64>,
}

pub(super) fn compute_metrics(workers: &[&Worker]) -> FleetMetrics {
    let total = workers.len();
    let running = workers.iter().filter(|w| w.state == State::Running).count();
    let waiting = workers.iter().filter(|w| w.needs_attention()).count();
    let finished = workers
        .iter()
        .filter(|w| w.state == State::Finished)
        .count();
    let error = workers.iter().filter(|w| w.state == State::Error).count();
    let quiet = workers.iter().filter(|w| w.quiet).count();

    let mut phase_minutes = [
        (Phase::Running, 0.0),
        (Phase::Quiet, 0.0),
        (Phase::Permission, 0.0),
        (Phase::Input, 0.0),
        (Phase::Ended, 0.0),
    ];
    for worker in workers {
        for (phase, mins) in &mut phase_minutes {
            *mins += worker.minutes_where(|p| p == *phase);
        }
    }
    let total_minutes: f64 = phase_minutes.iter().map(|(_, m)| *m).sum();
    let waited_minutes: f64 = phase_minutes
        .iter()
        .filter(|(p, _)| p.waits_on_a_person())
        .map(|(_, m)| *m)
        .sum();
    let wait_ratio_pct = if total_minutes > 0.0 {
        (waited_minutes / total_minutes) * 100.0
    } else {
        0.0
    };

    let tooks: Vec<f64> = workers
        .iter()
        .filter(|w| w.state == State::Finished)
        .filter_map(|w| w.took_minutes)
        .collect();
    let avg_took_minutes =
        (!tooks.is_empty()).then(|| tooks.iter().sum::<f64>() / tooks.len() as f64);
    let results_count = workers.iter().filter(|w| w.has_result).count();

    let max_len = workers
        .iter()
        .map(|w| w.lines_per_tick.len())
        .max()
        .unwrap_or(0);
    let mut throughput = vec![0.0; max_len];
    for worker in workers {
        let offset = max_len.saturating_sub(worker.lines_per_tick.len());
        for (i, v) in worker.lines_per_tick.iter().enumerate() {
            throughput[offset + i] += *v;
        }
    }

    FleetMetrics {
        total,
        running,
        waiting,
        finished,
        error,
        quiet,
        total_minutes,
        waited_minutes,
        wait_ratio_pct,
        avg_took_minutes,
        results_count,
        phase_minutes,
        throughput,
    }
}

/// Builds the full-body Analytics tab into `body`.
pub(super) fn build(
    tree: &mut UiTree,
    body: WidgetId,
    workers: &[&Worker],
    output_caption: &str,
    filtering: bool,
    reg: &mut Registry,
) {
    let scroll = tree.insert(
        Some(body),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(12.0),
            ..fill_remaining()
        }),
    );

    if workers.is_empty() {
        let card = surface_card(tree, scroll, Style::default());
        let msg = if filtering {
            "絞り込み条件に一致するワーカーがありません"
        } else {
            "集計対象のワーカーがありません"
        };
        caption(tree, card, msg);
        return;
    }

    let metrics = compute_metrics(workers);

    // Row 1: 4 KPI cards.
    let kpi_row = tree.insert(
        Some(scroll),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::STRETCH,
            gap: gap(12.0),
            flex_shrink: 0.0,
            ..Default::default()
        }),
    );
    kpi_card(
        tree,
        kpi_row,
        "フリート稼働状況",
        format!("{} 体", metrics.total),
        format!(
            "作業中 {} · 要対応 {} · 完了 {} · 失敗 {}",
            metrics.running, metrics.waiting, metrics.finished, metrics.error
        ),
        if metrics.error > 0 { RED } else { GREEN },
    );
    let wait_color = if metrics.wait_ratio_pct >= 35.0 {
        RED
    } else if metrics.wait_ratio_pct >= 15.0 {
        ORANGE
    } else {
        GREEN
    };
    kpi_card(
        tree,
        kpi_row,
        "人待ち率（ボトルネック指標）",
        format!("{:.1}%", metrics.wait_ratio_pct),
        format!(
            "人待ち合計 {} / 延べ {}",
            minutes_text(metrics.waited_minutes),
            minutes_text(metrics.total_minutes)
        ),
        wait_color,
    );
    let avg_text = metrics
        .avg_took_minutes
        .map(minutes_text)
        .unwrap_or_else(|| "—".to_owned());
    kpi_card(
        tree,
        kpi_row,
        "平均完了時間",
        avg_text,
        format!(
            "完了 {} 件中 結果あり {} 件",
            metrics.finished, metrics.results_count
        ),
        BLUE,
    );
    let recent_out: f64 = metrics.throughput.iter().copied().sum();
    kpi_card(
        tree,
        kpi_row,
        "直近出力アクティビティ",
        format!("{recent_out:.0}"),
        format!("稼働中 {} 体 · 静止 {} 体", metrics.running, metrics.quiet),
        if metrics.quiet > 0 { YELLOW } else { BLUE },
    );

    // Row 2: Phase time stacked bar + Aggregate throughput chart.
    let mid_row = tree.insert(
        Some(scroll),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::STRETCH,
            gap: gap(12.0),
            flex_shrink: 0.0,
            ..Default::default()
        }),
    );
    phase_breakdown_card(tree, mid_row, &metrics);
    throughput_card(tree, mid_row, &metrics, output_caption);

    // Row 3: Agent/Model comparison + Wait-time bottleneck ranking.
    let bottom_row = tree.insert(
        Some(scroll),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::STRETCH,
            gap: gap(12.0),
            flex_shrink: 0.0,
            ..Default::default()
        }),
    );
    agent_comparison_card(tree, bottom_row, workers);
    bottleneck_card(tree, bottom_row, workers, reg);
}

fn surface_card(tree: &mut UiTree, parent: WidgetId, extra: Style) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(8.0),
            padding: LayoutRect::length(crate::appearance::space(14.0)),
            ..extra
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(12.0),
    )
}

fn kpi_card(
    tree: &mut UiTree,
    parent: WidgetId,
    title: &str,
    value: String,
    subtitle: String,
    accent: Color,
) {
    let card = surface_card(tree, parent, fill_remaining());
    let head = tree.insert(
        Some(card),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(6.0)),
    );
    tree.insert(
        Some(head),
        Container::row()
            .width(8.0)
            .height(8.0)
            .background(accent)
            .radius(4.0),
    );
    caption(tree, head, title);
    tree.insert(
        Some(card),
        Label::new(value)
            .font_size(crate::appearance::font(24.0))
            .weight(700)
            .color(accent),
    );
    tree.insert(
        Some(card),
        Label::new(subtitle)
            .font_size(crate::appearance::font(11.0))
            .color(Role::Muted)
            .truncation(Truncation::Tail),
    );
}

fn phase_breakdown_card(tree: &mut UiTree, parent: WidgetId, metrics: &FleetMetrics) {
    let card = surface_card(tree, parent, fill_remaining());
    tree.insert(
        Some(card),
        Label::new("フェーズ別 時間内訳（フリート合算）")
            .font_size(crate::appearance::font(13.0))
            .weight(700),
    );
    let track = tree.insert(
        Some(card),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            size: LayoutSize {
                width: percent(1.0),
                height: length(20.0),
            },
            ..Default::default()
        })
        .background(Role::Background)
        .border(1.0, Role::Separator)
        .radius(6.0)
        .clip(true),
    );
    if metrics.total_minutes > 0.0 {
        let scale = 1000.0 / metrics.total_minutes;
        for (phase, mins) in metrics.phase_minutes {
            if mins <= 0.0 {
                continue;
            }
            tree.insert(
                Some(track),
                Container::new(Style {
                    flex_grow: (mins * scale) as f32,
                    flex_basis: length(0.0),
                    size: LayoutSize {
                        width: auto(),
                        height: percent(1.0),
                    },
                    ..Default::default()
                })
                .background(phase_color(phase)),
            );
        }
    }
    let legend = tree.insert(
        Some(card),
        Container::wrapping_row(14.0, 6.0).align_items(AlignItems::CENTER),
    );
    for (phase, mins) in metrics.phase_minutes {
        let pct = if metrics.total_minutes > 0.0 {
            (mins / metrics.total_minutes) * 100.0
        } else {
            0.0
        };
        let item = tree.insert(
            Some(legend),
            Container::row()
                .align_items(AlignItems::CENTER)
                .gap(crate::appearance::space(6.0)),
        );
        tree.insert(
            Some(item),
            Container::row()
                .width(10.0)
                .height(10.0)
                .background(phase_color(phase))
                .radius(2.0),
        );
        tree.insert(
            Some(item),
            Label::new(format!(
                "{} {} ({pct:.0}%)",
                phase.title(),
                minutes_text(mins)
            ))
            .font_size(crate::appearance::font(12.0)),
        );
    }
}

fn throughput_card(
    tree: &mut UiTree,
    parent: WidgetId,
    metrics: &FleetMetrics,
    output_caption: &str,
) {
    let card = surface_card(tree, parent, fill_remaining());
    let head = tree.insert(
        Some(card),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(8.0)),
    );
    tree.insert(
        Some(head),
        Label::new("フリート全体 出力推移")
            .font_size(crate::appearance::font(13.0))
            .weight(700),
    );
    spacer(tree, head);
    caption(tree, head, output_caption);
    if metrics.throughput.is_empty() {
        caption(tree, card, "稼働中の出力データがありません");
    } else {
        tree.insert(
            Some(card),
            Chart::line(vec![String::new(); metrics.throughput.len()])
                .series(ChartSeries::new("合算", metrics.throughput.clone()))
                .sparkline()
                .width(420.0)
                .height(64.0),
        );
    }
}

fn agent_comparison_card(tree: &mut UiTree, parent: WidgetId, workers: &[&Worker]) {
    let card = surface_card(tree, parent, fill_remaining());
    tree.insert(
        Some(card),
        Label::new("エージェント・モデル別 内訳")
            .font_size(crate::appearance::font(13.0))
            .weight(700),
    );
    let mut by_key: BTreeMap<String, (usize, usize, usize, f64, f64)> = BTreeMap::new();
    for worker in workers {
        let key = if worker.is_agent() {
            format!("{} / {}", one_line(&worker.kind), worker.model_line())
        } else {
            one_line(&worker.kind)
        };
        let entry = by_key.entry(key).or_insert((0, 0, 0, 0.0, 0.0));
        entry.0 += 1;
        if worker.state == State::Finished {
            entry.1 += 1;
        }
        if worker.state == State::Error {
            entry.2 += 1;
        }
        entry.3 += worker.total_minutes();
        entry.4 += worker.minutes_where(Phase::waits_on_a_person);
    }
    let max_total = by_key
        .values()
        .map(|(_, _, _, total, _)| *total)
        .fold(0.0_f64, f64::max)
        .max(1.0);
    for (name, (count, done, err, total_min, wait_min)) in by_key {
        let row = tree.insert(
            Some(card),
            Container::column().gap(crate::appearance::space(4.0)),
        );
        let top = tree.insert(
            Some(row),
            Container::row()
                .align_items(AlignItems::CENTER)
                .gap(crate::appearance::space(8.0)),
        );
        let name_slot = tree.insert(Some(top), Container::new(fill_remaining()));
        tree.insert(
            Some(name_slot),
            Label::new(name)
                .font_size(crate::appearance::font(12.0))
                .weight(600)
                .truncation(Truncation::Tail),
        );
        tree.insert(
            Some(top),
            Tag::new(format!("{count} 体")).style(TagStyle::Outlined),
        );
        if done > 0 {
            tree.insert(
                Some(top),
                Tag::new(format!("完了 {done}")).tone(Tone::Success),
            );
        }
        if err > 0 {
            tree.insert(
                Some(top),
                Tag::new(format!("失敗 {err}")).tone(Tone::Critical),
            );
        }
        caption(
            tree,
            top,
            format!(
                "稼働 {} / 人待ち {}",
                minutes_text(total_min),
                minutes_text(wait_min)
            ),
        );

        let bar = tree.insert(
            Some(row),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                size: LayoutSize {
                    width: percent(1.0),
                    height: length(8.0),
                },
                ..Default::default()
            })
            .background(Role::Background)
            .radius(4.0)
            .clip(true),
        );
        let work_min = (total_min - wait_min).max(0.0);
        let rest_min = (max_total - total_min).max(0.0);
        for (mins, color) in [
            (work_min, GREEN),
            (wait_min, ORANGE),
            (rest_min, Color::TRANSPARENT),
        ] {
            if mins <= 0.0 {
                continue;
            }
            tree.insert(
                Some(bar),
                Container::new(Style {
                    flex_grow: (mins / max_total * 1000.0) as f32,
                    flex_basis: length(0.0),
                    size: LayoutSize {
                        width: auto(),
                        height: percent(1.0),
                    },
                    ..Default::default()
                })
                .background(color),
            );
        }
    }
}

fn bottleneck_card(tree: &mut UiTree, parent: WidgetId, workers: &[&Worker], reg: &mut Registry) {
    let card = surface_card(tree, parent, fill_remaining());
    let head = tree.insert(
        Some(card),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(8.0)),
    );
    tree.insert(
        Some(head),
        Label::new("人待ち・停滞ランキング（クリックで選択）")
            .font_size(crate::appearance::font(13.0))
            .weight(700),
    );

    let mut ranked: Vec<(&Worker, f64)> = workers
        .iter()
        .copied()
        .map(|w| {
            let waited = w
                .minutes_where(Phase::waits_on_a_person)
                .max(w.wait_minutes);
            (w, waited)
        })
        .collect();
    ranked.sort_by(|(a, aw), (b, bw)| {
        bw.total_cmp(aw)
            .then_with(|| b.total_minutes().total_cmp(&a.total_minutes()))
    });

    for (worker, waited) in ranked.into_iter().take(8) {
        let (symbol, color) = mark(worker);
        let row = tree.insert(
            Some(card),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::CENTER,
                justify_content: JustifyContent::SPACE_BETWEEN,
                gap: gap(8.0),
                padding: LayoutRect {
                    left: length(8.0),
                    right: length(8.0),
                    top: length(6.0),
                    bottom: length(6.0),
                },
                ..Default::default()
            })
            .background(Role::SurfaceAlt)
            .border(1.0, Role::Separator)
            .radius(6.0),
        );
        reg.marks
            .push((row, worker.pane, Mark::Node { normal: color }));
        tree.insert(
            Some(row),
            Label::new(symbol)
                .font_size(crate::appearance::font(12.0))
                .weight(700)
                .color(color),
        );
        let name_slot = tree.insert(Some(row), Container::new(fill_remaining()));
        tree.insert(
            Some(name_slot),
            Label::new(one_line(&worker.label))
                .font_size(crate::appearance::font(12.0))
                .weight(600)
                .truncation(Truncation::Tail),
        );
        tree.insert(
            Some(row),
            Tag::new(one_line(&worker.kind)).tone(Tone::Accent),
        );
        let wait_color = if waited >= 5.0 {
            ORANGE
        } else if waited > 0.0 {
            YELLOW
        } else {
            GRAY
        };
        tree.insert(
            Some(row),
            Label::new(format!("人待ち {}", minutes_text(waited)))
                .font_size(crate::appearance::font(12.0))
                .weight(600)
                .color(wait_color),
        );
    }
}
