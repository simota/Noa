//! Hand-drawn charts noa-gpui has no widget for: the limit gauge with its
//! pace tick, the hour × weekday heatmap and the per-model share bar. Each
//! has a fixed `style()` size (or a fixed height and a flexible width) and
//! draws itself in `paint`; the geometry lives in plain functions so tests
//! can check it without a GPU.

use noa_gpui_core::style::{Dimension, Size as LayoutSize, length, percent};
use noa_gpui_core::{Paint, PaintCx, Role, Style, Widget};
use noa_gpui_scene::{Border, Color, Direction, Point, Rect, TextLayoutId};
use noa_gpui_text::{TextStore, TextStyle};

use agent_usage::history::Series;
use agent_usage::model::Unix;

pub const GAUGE_HEIGHT: f32 = 16.0;
/// Room above the bar for the pace tick's ▽.
const GAUGE_TICK_ROOM: f32 = 6.0;

/// A window's used % as a bar, with the elapsed fraction as a tick.
pub struct Gauge {
    /// 0..=100.
    pub used: f64,
    /// 0..=1, `None` when the window length or reset is unknown.
    pub tick: Option<f64>,
    pub color: Paint,
    /// Stale: the fill and the tick are drawn in the muted colour (the row
    /// around it is drawn at half opacity).
    pub dimmed: bool,
}

/// Where the tick of `tick` (0..=1) falls along a bar.
pub fn tick_x(bar: Rect, tick: f64) -> f32 {
    bar.x + bar.width * tick.clamp(0.0, 1.0) as f32
}

/// The bar's filled width for `used` percent.
pub fn fill_width(bar: Rect, used: f64) -> f32 {
    bar.width * (used / 100.0).clamp(0.0, 1.0) as f32
}

/// The corner radius of a fill `w` wide: round ends, never wider than it.
pub fn fill_radius(bar: Rect, w: f32) -> f32 {
    (bar.height / 2.0).min(w / 2.0)
}

impl Gauge {
    fn bar(bounds: Rect) -> Rect {
        let h = GAUGE_HEIGHT - GAUGE_TICK_ROOM;
        Rect::new(bounds.x, bounds.y + GAUGE_TICK_ROOM, bounds.width, h)
    }
}

impl Widget for Gauge {
    fn style(&self) -> Style {
        Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: length(0.0),
            size: LayoutSize {
                width: Dimension::auto(),
                height: length(GAUGE_HEIGHT),
            },
            min_size: LayoutSize {
                width: length(40.0),
                height: length(GAUGE_HEIGHT),
            },
            ..Default::default()
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let bar = Gauge::bar(cx.bounds());
        let track = cx.color(Role::SurfaceAlt);
        let outline = cx.color(Role::Separator);
        cx.rounded_rect(
            bar,
            bar.height / 2.0,
            track,
            Border {
                width: 1.0,
                color: outline,
            },
        );
        let fill = if self.dimmed {
            cx.color(Role::Muted)
        } else {
            cx.color(self.color)
        };
        // The fill keeps its true width: a sliver under the bar's height gets
        // smaller corners instead of growing to a circle.
        let w = fill_width(bar, self.used);
        if w > 0.0 {
            let filled = Rect::new(bar.x, bar.y, w, bar.height);
            cx.rounded_rect(filled, fill_radius(bar, w), fill, Border::default());
        }
        if let Some(tick) = self.tick {
            let x = tick_x(bar, tick);
            let mark = cx.color(if self.dimmed { Role::Muted } else { Role::Text });
            cx.line(
                Point::new(x, bar.y - 1.0),
                Point::new(x, bar.y + bar.height + 1.0),
                1.5,
                mark,
            );
            cx.triangle(
                Rect::new(x - 4.0, cx.bounds().y, 8.0, GAUGE_TICK_ROOM),
                Direction::Down,
                mark,
            );
        }
    }
}

/// A small status dot: filled when the value is current, an outline when not.
pub struct Dot {
    pub color: Paint,
    pub filled: bool,
}

impl Widget for Dot {
    fn style(&self) -> Style {
        Style {
            flex_shrink: 0.0,
            size: LayoutSize {
                width: length(10.0),
                height: length(10.0),
            },
            ..Default::default()
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let color = cx.color(self.color);
        let rect = cx.bounds();
        if self.filled {
            cx.rounded_rect(rect, 5.0, color, Border::default());
        } else {
            cx.rounded_rect(rect, 5.0, Color::TRANSPARENT, Border { width: 1.5, color });
        }
    }
}

/// A colour swatch for legends.
pub struct Swatch(pub Color);

impl Widget for Swatch {
    fn style(&self) -> Style {
        Style {
            flex_shrink: 0.0,
            size: LayoutSize {
                width: length(10.0),
                height: length(10.0),
            },
            ..Default::default()
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let rect = cx.bounds();
        cx.rounded_rect(rect, 2.0, self.0, Border::default());
    }
}

/// Text shaped once and kept until the widget goes away.
#[derive(Default)]
struct Texts {
    shaped: Vec<(TextLayoutId, noa_gpui_scene::Size)>,
}

impl Texts {
    fn ensure(&mut self, store: &mut TextStore, texts: &[String], size: f32) {
        if !self.shaped.is_empty() {
            return;
        }
        for t in texts {
            let id = store.create(t, TextStyle::new(size), None);
            self.shaped.push((id, store.size(id)));
        }
    }

    fn release(&mut self, store: &mut TextStore) {
        for (id, _) in self.shaped.drain(..) {
            store.remove(id);
        }
    }
}

pub const HEAT_CELL: f32 = 22.0;
pub const HEAT_GAP: f32 = 2.0;
const HEAT_ROW_LABEL: f32 = 22.0;
const HEAT_COL_LABEL: f32 = 16.0;
const HEAT_FONT: f32 = 10.0;
pub const WEEKDAYS_JA: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];

/// 7 rows (Monday first) × 24 local hours; each cell's colour scales to the
/// largest cell.
pub struct Heatmap {
    pub cells: [[u64; 24]; 7],
    /// Row labels, then the 24 column labels.
    pub labels: Vec<String>,
    pub color: Color,
    texts: Texts,
}

/// A cell's strength 0..=1 against the largest cell (0 when all are empty).
pub fn heat_level(cells: &[[u64; 24]; 7], value: u64) -> f32 {
    let max = cells.iter().flatten().copied().max().unwrap_or(0);
    if max == 0 {
        0.0
    } else {
        value as f32 / max as f32
    }
}

pub fn heat_max(cells: &[[u64; 24]; 7]) -> u64 {
    cells.iter().flatten().copied().max().unwrap_or(0)
}

impl Heatmap {
    pub fn new(cells: [[u64; 24]; 7], color: Color) -> Heatmap {
        let mut labels: Vec<String> = WEEKDAYS_JA.iter().map(|s| s.to_string()).collect();
        labels.extend((0..24).map(|h| {
            if h % 3 == 0 {
                h.to_string()
            } else {
                String::new()
            }
        }));
        Heatmap {
            cells,
            labels,
            color,
            texts: Texts::default(),
        }
    }

    pub fn width() -> f32 {
        HEAT_ROW_LABEL + 24.0 * (HEAT_CELL + HEAT_GAP)
    }

    pub fn height() -> f32 {
        HEAT_COL_LABEL + 7.0 * (HEAT_CELL + HEAT_GAP)
    }

    /// A cell's rectangle relative to the widget's origin.
    pub fn cell_rect(row: usize, col: usize) -> Rect {
        Rect::new(
            HEAT_ROW_LABEL + col as f32 * (HEAT_CELL + HEAT_GAP),
            HEAT_COL_LABEL + row as f32 * (HEAT_CELL + HEAT_GAP),
            HEAT_CELL,
            HEAT_CELL,
        )
    }

    /// The fill of a cell with strength `level`: empty cells take the
    /// alternate surface, others the colour from 15 % to full opacity.
    pub fn cell_color(&self, level: f32, empty: Color) -> Color {
        if level <= 0.0 {
            empty
        } else {
            self.color.with_alpha(0.15 + 0.85 * level)
        }
    }
}

impl Widget for Heatmap {
    fn style(&self) -> Style {
        Style {
            flex_shrink: 0.0,
            size: LayoutSize {
                width: length(Heatmap::width()),
                height: length(Heatmap::height()),
            },
            ..Default::default()
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let origin = cx.bounds();
        let empty = cx.color(Role::SurfaceAlt);
        let muted = cx.color(Role::Muted);
        for (row, values) in self.cells.iter().enumerate() {
            for (col, value) in values.iter().enumerate() {
                let r = Heatmap::cell_rect(row, col);
                let color = self.cell_color(heat_level(&self.cells, *value), empty);
                cx.rounded_rect(
                    Rect::new(origin.x + r.x, origin.y + r.y, r.width, r.height),
                    3.0,
                    color,
                    Border::default(),
                );
            }
        }
        self.texts.ensure(cx.text_store(), &self.labels, HEAT_FONT);
        for (i, (id, size)) in self.texts.shaped.clone().into_iter().enumerate() {
            let rect = if i < 7 {
                let cell = Heatmap::cell_rect(i, 0);
                Rect::new(
                    origin.x + (HEAT_ROW_LABEL - size.width) / 2.0 - 2.0,
                    origin.y + cell.y + (HEAT_CELL - size.height) / 2.0,
                    size.width,
                    size.height,
                )
            } else {
                let cell = Heatmap::cell_rect(0, i - 7);
                Rect::new(
                    origin.x + cell.x + (HEAT_CELL - size.width) / 2.0,
                    origin.y,
                    size.width,
                    size.height,
                )
            };
            cx.text(rect, id, muted);
        }
    }

    fn removed(&mut self, text: &mut TextStore) {
        self.texts.release(text);
    }
}

/// One horizontal 100 % bar split into parts.
pub struct ShareBar {
    pub parts: Vec<(String, u64, Color)>,
}

pub const SHARE_HEIGHT: f32 = 18.0;

/// Models under 2 % of the total fold into one "その他" part at the end.
/// The input is (name, tokens); the output keeps the order of the input.
pub fn fold_shares(rows: &[(String, u64)]) -> Vec<(String, u64)> {
    let total: u64 = rows.iter().map(|(_, t)| t).sum();
    if total == 0 {
        return Vec::new();
    }
    let mut kept = Vec::new();
    let mut other = 0;
    for (name, t) in rows {
        if (*t as f64) < total as f64 * 0.02 {
            other += t;
        } else {
            kept.push((name.clone(), *t));
        }
    }
    if other > 0 {
        kept.push(("その他".to_owned(), other));
    }
    kept
}

/// The parts' widths along a bar of `width`, in order.
pub fn share_widths(parts: &[u64], width: f32) -> Vec<f32> {
    let total: u64 = parts.iter().sum();
    if total == 0 {
        return vec![0.0; parts.len()];
    }
    parts
        .iter()
        .map(|p| width * (*p as f64 / total as f64) as f32)
        .collect()
}

impl Widget for ShareBar {
    fn style(&self) -> Style {
        Style {
            flex_shrink: 0.0,
            size: LayoutSize {
                width: percent(1.0),
                height: length(SHARE_HEIGHT),
            },
            ..Default::default()
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let bounds = cx.bounds();
        let track = cx.color(Role::SurfaceAlt);
        cx.rounded_rect(bounds, 4.0, track, Border::default());
        let tokens: Vec<u64> = self.parts.iter().map(|(_, t, _)| *t).collect();
        let widths = share_widths(&tokens, bounds.width);
        let mut x = bounds.x;
        let last = widths.len().saturating_sub(1);
        for (i, ((_, _, color), w)) in self.parts.iter().zip(widths).enumerate() {
            let r = Rect::new(
                x,
                bounds.y,
                (w - if i < last { 1.0 } else { 0.0 }).max(0.0),
                bounds.height,
            );
            cx.fill_rect(r, *color);
            x += w;
        }
    }
}

/// At most this many history bins: about one per two pixels of the chart.
pub const MAX_HISTORY_BINS: i64 = 400;

/// Bin lengths to choose from, shortest first: 5 min (half the history's
/// gap of two polls, so every gap leaves an empty bin) up to a day.
const HISTORY_STEPS: [i64; 11] = [
    300, 600, 900, 1200, 1800, 3600, 7200, 10_800, 21_600, 43_200, 86_400,
];

/// The bin length for `range`: the shortest step that keeps the bins within
/// [`MAX_HISTORY_BINS`] (whole days past the last step).
pub fn history_step(range: (Unix, Unix)) -> i64 {
    let span = (range.1 - range.0).max(1);
    let bins = |step: i64| (span + step - 1) / step;
    HISTORY_STEPS
        .into_iter()
        .find(|step| bins(*step) <= MAX_HISTORY_BINS)
        .unwrap_or_else(|| (bins(MAX_HISTORY_BINS) + 86_399) / 86_400 * 86_400)
}

/// The start of each chart category of the limit history over `range`.
pub fn history_bins(range: (Unix, Unix)) -> Vec<Unix> {
    let step = history_step(range);
    let mut bins = Vec::new();
    let mut t = range.0;
    while t < range.1 {
        bins.push(t);
        t += step;
    }
    bins
}

/// One value per bin, from the series' points in time order: the last
/// finite point in the bin; for a bin with no point, the line between its
/// neighbours where they are finite and at most `max_gap` apart; `NAN`
/// otherwise, so the line breaks at a gap. A gap is either the `NAN` marker
/// `history::series` puts into it or neighbours more than `max_gap` apart;
/// with bins of half `max_gap` (see [`history_step`]) every gap leaves at
/// least one empty bin. The last bin includes `end`, the newest sample.
pub fn bin_series(series: &Series, bins: &[Unix], end: Unix, max_gap: i64) -> Vec<f64> {
    let points = &series.points;
    bins.iter()
        .enumerate()
        .map(|(i, start)| {
            let stop = bins.get(i + 1).copied();
            let lo = points.partition_point(|(t, _)| t < start);
            let hi = match stop {
                Some(stop) => points.partition_point(|(t, _)| *t < stop),
                None => points.partition_point(|(t, _)| *t <= end),
            };
            let inside = &points[lo..hi];
            if let Some((_, v)) = inside.iter().rev().find(|(_, v)| v.is_finite()) {
                return *v;
            }
            let (Some(stop), true) = (stop, inside.is_empty()) else {
                return f64::NAN;
            };
            match (lo.checked_sub(1).map(|j| points[j]), points.get(hi)) {
                (Some((pt, pv)), Some(&(qt, qv)))
                    if pv.is_finite() && qv.is_finite() && qt - pt <= max_gap =>
                {
                    pv + (qv - pv) * (stop - pt) as f64 / (qt - pt) as f64
                }
                _ => f64::NAN,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_tick_position_matches_elapsed() {
        let bar = Rect::new(100.0, 0.0, 200.0, 10.0);
        assert_eq!(tick_x(bar, 0.0), 100.0);
        assert_eq!(tick_x(bar, 0.25), 150.0);
        assert_eq!(tick_x(bar, 1.0), 300.0);
        assert_eq!(tick_x(bar, 1.5), 300.0);
        assert_eq!(fill_width(bar, 61.0), 122.0);
        // Painted: the tick's line sits at the elapsed fraction of the bar.
        let mut tree = noa_gpui_core::UiTree::new();
        let host = tree.insert(
            None,
            noa_gpui_core::widgets::Container::new(Style {
                size: LayoutSize {
                    width: length(200.0),
                    height: length(40.0),
                },
                ..Default::default()
            }),
        );
        tree.insert(
            Some(host),
            Gauge {
                used: 30.0,
                tick: Some(0.4),
                color: Paint::Role(Role::Accent),
                dimmed: false,
            },
        );
        let mut text = TextStore::new(Default::default());
        let list = tree
            .prepare(noa_gpui_scene::Size::new(200.0, 40.0), &mut text)
            .unwrap();
        let xs: Vec<f32> = list
            .commands()
            .iter()
            .filter_map(|c| match c {
                noa_gpui_scene::DrawCommand::Shape {
                    shape: noa_gpui_scene::Shape::Line { from, to, .. },
                    ..
                } if from.x == to.x => Some(from.x),
                _ => None,
            })
            .collect();
        assert_eq!(xs, vec![80.0]);
    }

    #[test]
    fn heatmap_scales_to_max_cell() {
        let mut cells = [[0u64; 24]; 7];
        cells[0][9] = 40;
        cells[4][14] = 10;
        assert_eq!(heat_max(&cells), 40);
        assert_eq!(heat_level(&cells, 40), 1.0);
        assert_eq!(heat_level(&cells, 10), 0.25);
        assert_eq!(heat_level(&cells, 0), 0.0);
        assert_eq!(heat_level(&[[0; 24]; 7], 0), 0.0);
        let map = Heatmap::new(cells, Color::hex(0x0a84ff));
        let empty = Color::hex(0x222222);
        assert_eq!(map.cell_color(0.0, empty), empty);
        assert_eq!(map.cell_color(1.0, empty).a, 1.0);
        assert!((map.cell_color(0.25, empty).a - 0.3625).abs() < 1e-6);
        assert_eq!(map.labels.len(), 7 + 24);
        assert_eq!(map.labels[0], "月");
        // Cells line up in rows of 24 and columns of 7.
        let a = Heatmap::cell_rect(0, 0);
        let b = Heatmap::cell_rect(6, 23);
        assert_eq!(b.x - a.x, 23.0 * (HEAT_CELL + HEAT_GAP));
        assert_eq!(b.y - a.y, 6.0 * (HEAT_CELL + HEAT_GAP));
        assert!(b.x + b.width <= Heatmap::width());
        assert!(b.y + b.height <= Heatmap::height());
    }

    #[test]
    fn share_bar_folds_small_models() {
        let rows = vec![
            ("opus".to_owned(), 700),
            ("haiku".to_owned(), 15),
            ("sonnet".to_owned(), 270),
            ("review".to_owned(), 15),
        ];
        let folded = fold_shares(&rows);
        assert_eq!(
            folded,
            vec![
                ("opus".to_owned(), 700),
                ("sonnet".to_owned(), 270),
                ("その他".to_owned(), 30),
            ]
        );
        assert_eq!(fold_shares(&[]), Vec::new());
        let widths = share_widths(&[700, 270, 30], 1000.0);
        assert_eq!(widths, vec![700.0, 270.0, 30.0]);
    }

    #[test]
    fn history_bins_break_where_samples_are_missing() {
        // Two polls of 300 s.
        const MAX_GAP: i64 = 600;
        // "Today" at 02:00: 10 % at 00:55, the history's gap marker at
        // 01:05, 20 % at 01:15 and the newest sample, 90 %, at 02:00.
        let range = (0, 2 * 3600);
        let bins = history_bins(range);
        assert_eq!(bins.len(), 24, "5-minute bins");
        let s = Series {
            label: "Claude 5h".into(),
            points: vec![
                (55 * 60, 10.0),
                (65 * 60, f64::NAN),
                (75 * 60, 20.0),
                (120 * 60, 90.0),
            ],
        };
        let v = bin_series(&s, &bins, range.1, MAX_GAP);
        let finite: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
        assert_eq!(finite, vec![10.0, 20.0, 90.0], "the newest sample is kept");
        let at = |minute: usize| v[minute / 5];
        assert_eq!(at(55), 10.0);
        // The 20 minutes between 00:55 and 01:15 break the line.
        assert!(at(60).is_nan() && at(65).is_nan() && at(70).is_nan());
        assert_eq!(at(75), 20.0);
        assert!(at(80).is_nan());
        assert_eq!(*v.last().unwrap(), 90.0);
        // Any gap over two polls (600 s) leaves an empty bin for a day.
        for t0 in (0..3600).step_by(37) {
            let s = Series {
                label: "x".into(),
                points: vec![(t0, 1.0), (t0 + 601, 2.0)],
            };
            let v = bin_series(&s, &history_bins((0, 86_400)), 86_400, MAX_GAP);
            let first = v.iter().position(|x| *x == 1.0).unwrap();
            let second = v.iter().position(|x| *x == 2.0).unwrap();
            assert!(v[first + 1].is_nan(), "gap at {t0} not broken: {v:?}");
            assert!(second - first >= 2, "gap at {t0} not broken: {v:?}");
        }
        // Samples closer than that never break the line, though they leave
        // bins without a sample of their own: 10-minute samples, and a poll
        // that came late.
        let s = Series {
            label: "x".into(),
            points: (0..=12)
                .map(|k| (k * 600, k as f64))
                .chain([(7799, 13.0), (8101, 14.0)])
                .collect(),
        };
        let v = bin_series(&s, &history_bins((0, 3 * 3600)), 3 * 3600, MAX_GAP);
        let last = v.iter().rposition(|x| x.is_finite()).unwrap();
        assert!(v[..=last].iter().all(|x| x.is_finite()), "{v:?}");
        // The empty bin takes the line's value at its end.
        assert_eq!(v[1], 1.0);
        assert_eq!(v[2], 1.0);
        assert_eq!(v[3], 2.0);
        // 7800..8100 has no sample: 13 at 7799 and 14 at 8101, read at 8100.
        assert!((v[26] - (13.0 + 301.0 / 302.0)).abs() < 1e-9, "{}", v[26]);
        // Longer periods take longer bins, never more than the cap.
        assert_eq!(history_step((0, 86_400)), 300);
        assert_eq!(history_step((0, 7 * 86_400)), 1800);
        assert_eq!(history_step((0, 30 * 86_400)), 7200);
        assert_eq!(history_step((0, 400 * 86_400)), 86_400);
        assert_eq!(history_step((0, 1000 * 86_400)), 3 * 86_400);
        for days in [1, 7, 30, 400, 1000] {
            assert!(history_bins((0, days * 86_400)).len() as i64 <= MAX_HISTORY_BINS);
        }
    }

    #[test]
    fn low_gauge_fill_keeps_its_width() {
        // 12 % of the 40 px minimum gauge is 4.8 px, not the bar's height.
        let mut tree = noa_gpui_core::UiTree::new();
        let host = tree.insert(
            None,
            noa_gpui_core::widgets::Container::new(Style {
                size: LayoutSize {
                    width: length(40.0),
                    height: length(GAUGE_HEIGHT),
                },
                ..Default::default()
            }),
        );
        tree.insert(
            Some(host),
            Gauge {
                used: 12.0,
                tick: None,
                color: Paint::Fixed(Color::hex(0x0a84ff)),
                dimmed: false,
            },
        );
        let mut text = TextStore::new(Default::default());
        let list = tree
            .prepare(noa_gpui_scene::Size::new(40.0, GAUGE_HEIGHT), &mut text)
            .unwrap();
        let fills: Vec<(f32, f32)> = list
            .commands()
            .iter()
            .filter_map(|c| match c {
                noa_gpui_scene::DrawCommand::Quad {
                    rect,
                    radius,
                    color,
                    ..
                } if *color == Color::hex(0x0a84ff) => Some((rect.width, radius[0])),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 1, "{fills:?}");
        assert!((fills[0].0 - 4.8).abs() < 1e-4, "{fills:?}");
        assert!(fills[0].1 <= 2.4 + 1e-4, "{fills:?}");
    }
}
