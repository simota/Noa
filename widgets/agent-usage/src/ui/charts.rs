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
    /// Stale: the fill and the tick are drawn in the muted colour.
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
        let w = fill_width(bar, self.used);
        if w > 0.0 {
            let filled = Rect::new(bar.x, bar.y, w.max(bar.height), bar.height);
            cx.rounded_rect(filled, bar.height / 2.0, fill, Border::default());
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

/// Chart categories (labels) and the time each one starts for the limit
/// history of `range`: hourly for a day or less, every 3 h for up to 8
/// days, every 12 h for up to 31 days, then daily.
pub fn history_bins(range: (Unix, Unix)) -> Vec<Unix> {
    let span = (range.1 - range.0).max(3600);
    let step = if span <= 86_400 {
        3600
    } else if span <= 8 * 86_400 {
        3 * 3600
    } else if span <= 31 * 86_400 {
        12 * 3600
    } else {
        86_400
    };
    let mut bins = Vec::new();
    let mut t = range.0;
    while t < range.1 {
        bins.push(t);
        t += step;
    }
    bins
}

/// One value per bin: the last finite point inside it, `NAN` when none (so
/// a gap in the samples breaks the line).
pub fn bin_series(series: &Series, bins: &[Unix], end: Unix) -> Vec<f64> {
    bins.iter()
        .enumerate()
        .map(|(i, start)| {
            let stop = bins.get(i + 1).copied().unwrap_or(end);
            series
                .points
                .iter()
                .filter(|(t, v)| *t >= *start && *t < stop && v.is_finite())
                .map(|(_, v)| *v)
                .next_back()
                .unwrap_or(f64::NAN)
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
        let bins = history_bins((0, 86_400));
        assert_eq!(bins.len(), 24);
        let s = Series {
            label: "Claude 5h".into(),
            points: vec![(100, 10.0), (200, 12.0), (7300, f64::NAN), (7400, 30.0)],
        };
        let v = bin_series(&s, &bins, 86_400);
        assert_eq!(v[0], 12.0);
        assert!(v[1].is_nan());
        assert_eq!(v[2], 30.0);
        assert_eq!(history_bins((0, 7 * 86_400)).len(), 56);
    }
}
