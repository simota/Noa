//! Floating panes (`docs/specs/floating-panes.md`): the pure model.
//!
//! A float is a terminal pane drawn above the split layout. This module owns
//! its geometry, display state, stacking and focus-cycling rules and knows
//! nothing about windows, ptys or the GPU — `app::floating` wires it to them.

use crate::split_tree::{DIVIDER_HIT_ZONE_PX, PaneId, Point, Rect};

/// REQ-001: a new float's content area, in cells.
pub const DEFAULT_COLS: u32 = 80;
pub const DEFAULT_ROWS: u32 = 24;
/// REQ-004: the smallest content area a float may be resized to, in cells.
pub const MIN_COLS: u32 = 20;
pub const MIN_ROWS: u32 = 3;
/// REQ-019: floats one anchor may hold.
pub const MAX_PER_ANCHOR: usize = 8;
/// REQ-001: a new float never exceeds this fraction of its anchor per side.
const INITIAL_MAX_FRACTION: f64 = 0.9;
/// Cells each title-strip button occupies.
const STRIP_BUTTON_CELLS: u32 = 2;

/// What a float belongs to (REQ-005).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatAnchor {
    Pane(PaneId),
    Tab,
    Window,
}

impl FloatAnchor {
    /// Stacking layer, back to front (REQ-008).
    const fn layer(self) -> u8 {
        match self {
            Self::Pane(_) => 0,
            Self::Tab => 1,
            Self::Window => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatState {
    Normal,
    Collapsed,
    Maximized,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Float {
    pub id: PaneId,
    pub anchor: FloatAnchor,
    /// Top-left of the normal-state rect, as a fraction of the anchor rect.
    pub origin: (f64, f64),
    /// Normal-state size including the title strip, as a fraction of the
    /// anchor rect.
    pub size: (f64, f64),
    pub state: FloatState,
    /// What a collapsed float expands back to.
    pub maximized_before_collapse: bool,
    /// The strip was dragged since it collapsed, so it sits where `origin`
    /// says rather than at the normal rect's corner.
    moved_while_collapsed: bool,
    z: u64,
    seq: u64,
    focus_stamp: u64,
}

/// Cell metrics plus the grid padding a pane's content area carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloatMetrics {
    pub cell_w: u32,
    pub cell_h: u32,
    pub pad_w: u32,
    pub pad_h: u32,
    /// The left share of `pad_w`: where a pane's first cell column starts.
    pub pad_left: u32,
}

impl FloatMetrics {
    /// The title strip is one cell row high, plus the grid padding its row
    /// is drawn inside.
    pub const fn strip_h(self) -> u32 {
        self.cell_h + self.pad_h
    }

    const fn min_w(self) -> u32 {
        MIN_COLS * self.cell_w + self.pad_w
    }

    const fn min_h(self) -> u32 {
        MIN_ROWS * self.cell_h + self.pad_h + self.strip_h()
    }
}

/// The geometry floats are resolved against for one tab.
#[derive(Clone, Debug)]
pub struct FloatLayout<'a> {
    /// The tab's split area.
    pub tab: Rect,
    /// The window's terminal area.
    pub window: Rect,
    /// Every split pane of the tab with its laid-out rect.
    pub panes: &'a [(PaneId, Rect)],
    pub zoomed: Option<PaneId>,
}

impl FloatLayout<'_> {
    fn anchor_rect(&self, anchor: FloatAnchor) -> Option<Rect> {
        match anchor {
            FloatAnchor::Pane(pane) => self
                .panes
                .iter()
                .find_map(|(id, rect)| (*id == pane).then_some(*rect)),
            FloatAnchor::Tab => Some(self.tab),
            FloatAnchor::Window => Some(self.window),
        }
    }

    fn anchor_shown(&self, anchor: FloatAnchor) -> bool {
        match anchor {
            FloatAnchor::Pane(pane) => self.zoomed.is_none_or(|zoomed| zoomed == pane),
            FloatAnchor::Tab | FloatAnchor::Window => true,
        }
    }
}

/// A visible float's pixel rects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedFloat {
    pub id: PaneId,
    pub outer: Rect,
    pub strip: Rect,
    /// `None` while collapsed.
    pub content: Option<Rect>,
    pub state: FloatState,
    pub anchor: FloatAnchor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripButton {
    Anchor,
    Maximize,
    Collapse,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatHitPart {
    Resize(Edges),
    Button(StripButton),
    Strip,
    Content,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloatHit {
    pub id: PaneId,
    pub part: FloatHitPart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatDragKind {
    Move,
    Resize(Edges),
}

/// An in-progress title-strip or edge drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloatDrag {
    pub id: PaneId,
    pub kind: FloatDragKind,
    start_point: Point,
    start_rect: Rect,
}

/// REQ-019: the target anchor already holds [`MAX_PER_ANCHOR`] floats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorFull;

/// A float as the session file stores it (REQ-017). Where the record sits in
/// the file names its anchor.
#[derive(Clone, Debug, PartialEq)]
pub struct FloatRecord {
    pub origin: (f64, f64),
    pub size: (f64, f64),
    pub collapsed: bool,
    pub seq: u64,
    pub z: u64,
    pub cwd: Option<String>,
    /// A collapsed strip dragged below where the full rect fits (REQ-003):
    /// `origin` then names the strip's corner, not the full rect's.
    pub strip_moved: bool,
}

impl FloatRecord {
    /// L2.4: finite, origin within `0.0..=1.0`, size within `(0.0, 1.0]`.
    pub fn is_valid(&self) -> bool {
        let unit = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
        let extent = |v: f64| v.is_finite() && v > 0.0 && v <= 1.0;
        unit(self.origin.0) && unit(self.origin.1) && extent(self.size.0) && extent(self.size.1)
    }
}

/// One tab's floats, in creation order.
#[derive(Clone, Debug, Default)]
pub struct FloatSet {
    floats: Vec<Float>,
}

/// Source of every float's creation, stacking and focus stamps. One clock
/// for the whole process, so the stamps of floats that move between tabs
/// (a window float on a tab switch, a pane's floats on a pane move) stay
/// comparable with those of the tab they arrive in.
static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn tick() -> u64 {
    CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

fn round_px(fraction: f64, extent: u32) -> u32 {
    (fraction * f64::from(extent)).round().max(0.0) as u32
}

fn ratio(offset: u32, extent: u32) -> f64 {
    if extent == 0 {
        0.0
    } else {
        f64::from(offset) / f64::from(extent)
    }
}

/// The normal-state rect: ratios applied, the minimum enforced, then clamped
/// into the anchor (REQ-007). `None` when the anchor cannot hold the minimum.
fn resolve_normal(float: &Float, anchor: Rect, metrics: FloatMetrics) -> Option<Rect> {
    if metrics.min_w() > anchor.w || metrics.min_h() > anchor.h {
        return None;
    }
    let w = round_px(float.size.0, anchor.w)
        .max(metrics.min_w())
        .min(anchor.w);
    let h = round_px(float.size.1, anchor.h)
        .max(metrics.min_h())
        .min(anchor.h);
    let x = (anchor.x + round_px(float.origin.0, anchor.w)).min(anchor.right() - w);
    let y = (anchor.y + round_px(float.origin.1, anchor.h)).min(anchor.bottom() - h);
    Some(Rect::new(x, y, w, h))
}

fn place(float: &Float, anchor: Rect, metrics: FloatMetrics) -> Option<PlacedFloat> {
    let normal = resolve_normal(float, anchor, metrics)?;
    let strip_h = metrics.strip_h();
    let (outer, content) = match float.state {
        FloatState::Normal => (normal, true),
        FloatState::Maximized => (anchor, true),
        FloatState::Collapsed if float.maximized_before_collapse => {
            (Rect::new(anchor.x, anchor.y, anchor.w, strip_h), false)
        }
        // REQ-009: the strip keeps the corner the float had. Only a drag of
        // the strip itself (REQ-003) takes it below where the full rect fits.
        FloatState::Collapsed if float.moved_while_collapsed => {
            let y = (anchor.y + round_px(float.origin.1, anchor.h)).min(anchor.bottom() - strip_h);
            (Rect::new(normal.x, y, normal.w, strip_h), false)
        }
        FloatState::Collapsed => (Rect::new(normal.x, normal.y, normal.w, strip_h), false),
    };
    let strip = Rect::new(outer.x, outer.y, outer.w, strip_h);
    Some(PlacedFloat {
        id: float.id,
        outer,
        strip,
        content: content.then(|| {
            Rect::new(
                outer.x,
                outer.y + strip_h,
                outer.w,
                outer.h.saturating_sub(strip_h),
            )
        }),
        state: float.state,
        anchor: float.anchor,
    })
}

fn set_from_rect(float: &mut Float, rect: Rect, anchor: Rect) {
    float.origin = (
        ratio(rect.x - anchor.x, anchor.w),
        ratio(rect.y - anchor.y, anchor.h),
    );
    float.size = (ratio(rect.w, anchor.w), ratio(rect.h, anchor.h));
}

/// The title strip's buttons, each [`STRIP_BUTTON_CELLS`] wide: the anchor
/// icon at the left end, then maximize, collapse and close at the right end.
///
/// The strip is drawn as a one-row grid inside the pane's padding, so the
/// rects follow its cell columns: the first and last buttons also take the
/// padding beside them.
pub fn strip_button_rect(strip: Rect, button: StripButton, metrics: FloatMetrics) -> Rect {
    let cols = strip.w.saturating_sub(metrics.pad_w) / metrics.cell_w.max(1);
    let column = |col: u32| (strip.x + metrics.pad_left + col * metrics.cell_w).min(strip.right());
    let from_right = |slot: u32| column(cols.saturating_sub(STRIP_BUTTON_CELLS * (slot + 1)));
    let span = |from: u32, to: u32| Rect::new(from, strip.y, to.saturating_sub(from), strip.h);
    match button {
        StripButton::Anchor => span(strip.x, column(STRIP_BUTTON_CELLS.min(cols))),
        StripButton::Close => span(from_right(0), strip.right()),
        StripButton::Collapse => span(from_right(1), from_right(0)),
        StripButton::Maximize => span(from_right(2), from_right(1)),
    }
}

impl FloatSet {
    pub fn is_empty(&self) -> bool {
        self.floats.is_empty()
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.floats.iter().any(|float| float.id == id)
    }

    pub fn get(&self, id: PaneId) -> Option<&Float> {
        self.floats.iter().find(|float| float.id == id)
    }

    fn get_mut(&mut self, id: PaneId) -> Option<&mut Float> {
        self.floats.iter_mut().find(|float| float.id == id)
    }

    pub fn floats(&self) -> &[Float] {
        &self.floats
    }

    pub fn count_on(&self, anchor: FloatAnchor) -> usize {
        self.floats
            .iter()
            .filter(|float| float.anchor == anchor)
            .count()
    }

    /// REQ-001: add a float anchored to `pane`, sized and placed by the
    /// default rule. The caller focuses it.
    pub fn create(
        &mut self,
        id: PaneId,
        pane: PaneId,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Result<(), AnchorFull> {
        let anchor = FloatAnchor::Pane(pane);
        if self.count_on(anchor) >= MAX_PER_ANCHOR {
            return Err(AnchorFull);
        }
        let rect = layout.anchor_rect(anchor).map(|anchor_rect| {
            default_rect(
                anchor_rect,
                self.floats_over(anchor_rect, layout, metrics),
                metrics,
            )
        });
        let seq = tick();
        let z = tick();
        let mut float = Float {
            id,
            anchor,
            origin: (0.0, 0.0),
            size: (1.0, 1.0),
            state: FloatState::Normal,
            maximized_before_collapse: false,
            moved_while_collapsed: false,
            z,
            seq,
            focus_stamp: 0,
        };
        if let (Some(rect), Some(anchor_rect)) = (rect, layout.anchor_rect(anchor)) {
            set_from_rect(&mut float, rect, anchor_rect);
        }
        self.floats.push(float);
        Ok(())
    }

    /// The content rect [`FloatSet::create`] would give a new float on
    /// `pane`, for sizing its pty before it exists.
    pub fn default_content_rect(
        &self,
        pane: PaneId,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Option<Rect> {
        let anchor_rect = layout.anchor_rect(FloatAnchor::Pane(pane))?;
        let rect = default_rect(
            anchor_rect,
            self.floats_over(anchor_rect, layout, metrics),
            metrics,
        );
        Some(Rect::new(
            rect.x,
            rect.y + metrics.strip_h(),
            rect.w,
            rect.h.saturating_sub(metrics.strip_h()),
        ))
    }

    /// Visible floats, on any anchor, that overlap `area` — the ones a new
    /// float placed there must step clear of (REQ-001).
    fn floats_over(&self, area: Rect, layout: &FloatLayout<'_>, metrics: FloatMetrics) -> u32 {
        self.draw_order(layout, metrics)
            .into_iter()
            .filter(|placed| {
                let outer = placed.outer;
                outer.x < area.right()
                    && area.x < outer.right()
                    && outer.y < area.bottom()
                    && area.y < outer.bottom()
            })
            .count() as u32
    }

    pub fn remove(&mut self, id: PaneId) -> Option<Float> {
        let index = self.floats.iter().position(|float| float.id == id)?;
        Some(self.floats.remove(index))
    }

    /// Detach every float matching `anchor`, keeping creation order — for a
    /// closed anchor (REQ-013) or one moving to another tab (L2.3).
    pub fn take_anchor(&mut self, anchor: FloatAnchor) -> Vec<Float> {
        let (taken, kept) = std::mem::take(&mut self.floats)
            .into_iter()
            .partition(|float| float.anchor == anchor);
        self.floats = kept;
        taken
    }

    /// L2.3: the anchors whose floats leave with split pane `pane` when it
    /// moves to another tab.
    ///
    /// Its own always. When it was the tab's last split pane the tab closes
    /// behind it, so the tab's floats go along rather than end unasked — and
    /// the window's too, when no other tab remains to hold them.
    pub fn anchors_following_pane(
        pane: PaneId,
        last_split_pane: bool,
        window_floats_survive: bool,
    ) -> Vec<FloatAnchor> {
        let mut anchors = vec![FloatAnchor::Pane(pane)];
        if last_split_pane {
            anchors.push(FloatAnchor::Tab);
            if !window_floats_survive {
                anchors.push(FloatAnchor::Window);
            }
        }
        anchors
    }

    /// REQ-013: the floats that close when split pane `pane` closes.
    /// Closing the tab's last split pane closes the tab.
    pub fn closing_with_pane(
        &self,
        pane: PaneId,
        last_split_pane: bool,
        window_floats_survive: bool,
    ) -> Vec<PaneId> {
        if last_split_pane {
            return self.closing_with_tab(window_floats_survive);
        }
        self.floats
            .iter()
            .filter(|float| float.anchor == FloatAnchor::Pane(pane))
            .map(|float| float.id)
            .collect()
    }

    /// REQ-013: the floats that close when this set's tab closes. Window
    /// floats outlive it while another tab of the window remains (L2.3).
    pub fn closing_with_tab(&self, window_floats_survive: bool) -> Vec<PaneId> {
        self.floats
            .iter()
            .filter(|float| !(window_floats_survive && float.anchor == FloatAnchor::Window))
            .map(|float| float.id)
            .collect()
    }

    /// Attach floats detached from another set. Their creation, stacking and
    /// focus stamps carry over unchanged, so they slot back into creation
    /// order and keep their place in the stack and in the focus history.
    pub fn adopt(&mut self, floats: Vec<Float>) {
        self.floats.extend(floats);
        self.floats.sort_by_key(|float| float.seq);
    }

    pub fn placed(
        &self,
        id: PaneId,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Option<PlacedFloat> {
        let float = self.get(id)?;
        if !layout.anchor_shown(float.anchor) {
            return None;
        }
        place(float, layout.anchor_rect(float.anchor)?, metrics)
    }

    pub fn is_visible(&self, id: PaneId, layout: &FloatLayout<'_>, metrics: FloatMetrics) -> bool {
        self.placed(id, layout, metrics).is_some()
    }

    /// REQ-009/REQ-024: only a float on screen and expanded takes the
    /// keyboard.
    pub fn accepts_focus(
        &self,
        id: PaneId,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> bool {
        self.placed(id, layout, metrics)
            .is_some_and(|placed| placed.state != FloatState::Collapsed)
    }

    /// Visible floats, back to front (REQ-008). Hidden floats are absent
    /// (CFR-003).
    pub fn draw_order(&self, layout: &FloatLayout<'_>, metrics: FloatMetrics) -> Vec<PlacedFloat> {
        let mut order: Vec<&Float> = self.floats.iter().collect();
        order.sort_by_key(|float| (float.anchor.layer(), float.z));
        order
            .into_iter()
            .filter_map(|float| self.placed(float.id, layout, metrics))
            .collect()
    }

    /// The front-most float under `point`, with the part hit (REQ-004,
    /// REQ-011). A resizable float's hit area extends
    /// [`DIVIDER_HIT_ZONE_PX`] outside its rect.
    pub fn hit_test(
        &self,
        point: Point,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Option<FloatHit> {
        self.draw_order(layout, metrics)
            .into_iter()
            .rev()
            .find_map(|placed| {
                hit_part(placed, point, metrics).map(|part| FloatHit {
                    id: placed.id,
                    part,
                })
            })
    }

    /// REQ-008/REQ-011: bring `id` to the front of its layer.
    pub fn raise(&mut self, id: PaneId) {
        let z = tick();
        if let Some(float) = self.get_mut(id) {
            float.z = z;
        }
    }

    /// Record that `id` took keyboard focus, and raise it.
    pub fn note_focus(&mut self, id: PaneId) {
        let stamp = tick();
        if let Some(float) = self.get_mut(id) {
            float.focus_stamp = stamp;
        }
        self.raise(id);
    }

    /// REQ-011: visible, non-collapsed floats in creation order.
    pub fn cycle_targets(&self, layout: &FloatLayout<'_>, metrics: FloatMetrics) -> Vec<PaneId> {
        self.floats
            .iter()
            .filter(|float| float.state != FloatState::Collapsed)
            .filter(|float| self.is_visible(float.id, layout, metrics))
            .map(|float| float.id)
            .collect()
    }

    /// REQ-011: the next cycle target after `current` (a float, or `None`
    /// for a split pane, which starts at the first/last target).
    pub fn cycle_from(
        &self,
        current: Option<PaneId>,
        forward: bool,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Option<PaneId> {
        let targets = self.cycle_targets(layout, metrics);
        let len = targets.len();
        if len == 0 {
            return None;
        }
        let position = current.and_then(|id| targets.iter().position(|target| *target == id));
        let index = match (position, forward) {
            (Some(index), true) => (index + 1) % len,
            (Some(index), false) => (index + len - 1) % len,
            (None, true) => 0,
            (None, false) => len - 1,
        };
        Some(targets[index])
    }

    /// REQ-011: where `float.toggle-focus` goes from a split pane — the
    /// last-focused cycle target, or the newest when none ever had focus.
    pub fn toggle_focus_target(
        &self,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Option<PaneId> {
        let targets = self.cycle_targets(layout, metrics);
        targets
            .iter()
            .filter_map(|id| self.get(*id))
            .max_by_key(|float| (float.focus_stamp, float.seq))
            .map(|float| float.id)
    }

    /// L2.2: the float a collapse/maximize/cycle-anchor action applies to —
    /// the focused one, else the visible float focused most recently.
    pub fn action_target(
        &self,
        focused: Option<PaneId>,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Option<PaneId> {
        if let Some(id) = focused.filter(|id| self.contains(*id)) {
            return Some(id);
        }
        self.floats
            .iter()
            .filter(|float| float.focus_stamp > 0)
            .filter(|float| self.is_visible(float.id, layout, metrics))
            .max_by_key(|float| float.focus_stamp)
            .map(|float| float.id)
    }

    /// REQ-009. Returns the new state.
    pub fn toggle_collapse(&mut self, id: PaneId) -> Option<FloatState> {
        let float = self.get_mut(id)?;
        float.state = match float.state {
            FloatState::Collapsed if float.maximized_before_collapse => FloatState::Maximized,
            FloatState::Collapsed => FloatState::Normal,
            state => {
                float.maximized_before_collapse = state == FloatState::Maximized;
                FloatState::Collapsed
            }
        };
        float.moved_while_collapsed = false;
        Some(float.state)
    }

    /// REQ-010. A collapsed float ignores it.
    pub fn toggle_maximize(&mut self, id: PaneId) -> Option<FloatState> {
        let float = self.get_mut(id)?;
        float.state = match float.state {
            FloatState::Normal => FloatState::Maximized,
            FloatState::Maximized => FloatState::Normal,
            FloatState::Collapsed => FloatState::Collapsed,
        };
        Some(float.state)
    }

    /// REQ-006: pane → tab → window → pane, keeping the normal-state rect
    /// where it is on screen (shrunk, then clamped, into the new anchor).
    pub fn cycle_anchor(
        &mut self,
        id: PaneId,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Result<Option<FloatAnchor>, AnchorFull> {
        let Some(float) = self.get(id) else {
            return Ok(None);
        };
        let Some(old_anchor) = layout.anchor_rect(float.anchor) else {
            return Ok(None);
        };
        let Some(normal) = resolve_normal(float, old_anchor, metrics) else {
            return Ok(None);
        };
        let next = match float.anchor {
            FloatAnchor::Pane(_) => FloatAnchor::Tab,
            FloatAnchor::Tab => FloatAnchor::Window,
            FloatAnchor::Window => {
                let centre = Point::new(normal.x + normal.w / 2, normal.y + normal.h / 2);
                match pane_under(centre, layout) {
                    Some(pane) => FloatAnchor::Pane(pane),
                    None => return Ok(None),
                }
            }
        };
        if self.count_on(next) >= MAX_PER_ANCHOR {
            return Err(AnchorFull);
        }
        let Some(new_anchor) = layout.anchor_rect(next) else {
            return Ok(None);
        };
        let w = normal.w.min(new_anchor.w);
        let h = normal.h.min(new_anchor.h);
        let x = normal.x.clamp(new_anchor.x, new_anchor.right() - w);
        let y = normal.y.clamp(new_anchor.y, new_anchor.bottom() - h);
        // A strip dragged while collapsed sits where the drag left it, which
        // can be below where the full rect fits: keep that row too.
        let strip_y = (float.state == FloatState::Collapsed && float.moved_while_collapsed)
            .then(|| place(float, old_anchor, metrics))
            .flatten()
            .map(|placed| {
                let strip_h = metrics.strip_h().min(new_anchor.h);
                placed
                    .outer
                    .y
                    .clamp(new_anchor.y, new_anchor.bottom() - strip_h)
            });
        let z = tick();
        let float = self.get_mut(id).expect("float checked above");
        float.anchor = next;
        float.z = z;
        set_from_rect(float, Rect::new(x, y, w, h), new_anchor);
        if let Some(strip_y) = strip_y {
            float.origin.1 = ratio(strip_y - new_anchor.y, new_anchor.h);
        }
        Ok(Some(next))
    }

    /// Start a drag on `id` at `point`. `None` when the float's state does
    /// not accept it (REQ-003, REQ-004, REQ-010).
    pub fn begin_drag(
        &self,
        id: PaneId,
        kind: FloatDragKind,
        point: Point,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) -> Option<FloatDrag> {
        let float = self.get(id)?;
        let accepted = match (float.state, kind) {
            (FloatState::Normal, _) => true,
            (FloatState::Collapsed, FloatDragKind::Move) => !float.maximized_before_collapse,
            _ => false,
        };
        if !accepted {
            return None;
        }
        Some(FloatDrag {
            id,
            kind,
            start_point: point,
            start_rect: self.placed(id, layout, metrics)?.outer,
        })
    }

    /// Apply `drag` with the pointer now at `point`.
    pub fn drag_to(
        &mut self,
        drag: &FloatDrag,
        point: Point,
        layout: &FloatLayout<'_>,
        metrics: FloatMetrics,
    ) {
        let Some(anchor) = self
            .get(drag.id)
            .and_then(|float| layout.anchor_rect(float.anchor))
        else {
            return;
        };
        let dx = i64::from(point.x) - i64::from(drag.start_point.x);
        let dy = i64::from(point.y) - i64::from(drag.start_point.y);
        // The anchor may have moved or shrunk since the drag began (REQ-007):
        // work from where the start rect sits inside it now.
        let start = {
            let rect = drag.start_rect;
            let w = rect.w.min(anchor.w);
            let h = rect.h.min(anchor.h);
            Rect::new(
                rect.x.clamp(anchor.x, anchor.right() - w),
                rect.y.clamp(anchor.y, anchor.bottom() - h),
                w,
                h,
            )
        };
        let shift = |base: u32, delta: i64, lo: u32, hi: u32| -> u32 {
            (i64::from(base) + delta).clamp(i64::from(lo), i64::from(hi.max(lo))) as u32
        };
        let Some(float) = self.get_mut(drag.id) else {
            return;
        };
        match drag.kind {
            FloatDragKind::Move => {
                let x = shift(
                    start.x,
                    dx,
                    anchor.x,
                    anchor.right().saturating_sub(start.w),
                );
                let y = shift(
                    start.y,
                    dy,
                    anchor.y,
                    anchor.bottom().saturating_sub(start.h),
                );
                float.origin = (ratio(x - anchor.x, anchor.w), ratio(y - anchor.y, anchor.h));
                float.moved_while_collapsed = float.state == FloatState::Collapsed;
            }
            FloatDragKind::Resize(edges) => {
                let (mut left, mut right) = (start.x, start.right());
                let (mut top, mut bottom) = (start.y, start.bottom());
                if edges.left {
                    left = shift(left, dx, anchor.x, right.saturating_sub(metrics.min_w()));
                }
                if edges.right {
                    right = shift(right, dx, left + metrics.min_w(), anchor.right());
                }
                if edges.top {
                    top = shift(top, dy, anchor.y, bottom.saturating_sub(metrics.min_h()));
                }
                if edges.bottom {
                    bottom = shift(bottom, dy, top + metrics.min_h(), anchor.bottom());
                }
                set_from_rect(
                    float,
                    Rect::new(left, top, right - left, bottom - top),
                    anchor,
                );
            }
        }
    }

    /// REQ-017: this set's floats on `anchor`, as session records.
    pub fn records_on(
        &self,
        anchor: FloatAnchor,
        cwd: impl Fn(PaneId) -> Option<String>,
    ) -> Vec<FloatRecord> {
        self.floats
            .iter()
            .filter(|float| float.anchor == anchor)
            .map(|float| FloatRecord {
                origin: float.origin,
                size: float.size,
                collapsed: float.state == FloatState::Collapsed,
                seq: float.seq,
                z: float.z,
                cwd: cwd(float.id),
                strip_moved: float.state == FloatState::Collapsed && float.moved_while_collapsed,
            })
            .collect()
    }

    /// REQ-017: rebuild a float from a valid record. A float saved while
    /// maximized, or collapsed from maximized, restores over its normal rect.
    pub fn restore(&mut self, id: PaneId, anchor: FloatAnchor, record: &FloatRecord) {
        CLOCK.fetch_max(
            record.seq.max(record.z),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.floats.push(Float {
            id,
            anchor,
            origin: record.origin,
            size: record.size,
            state: if record.collapsed {
                FloatState::Collapsed
            } else {
                FloatState::Normal
            },
            maximized_before_collapse: false,
            moved_while_collapsed: record.collapsed && record.strip_moved,
            z: record.z,
            seq: record.seq,
            focus_stamp: 0,
        });
        self.floats.sort_by_key(|float| float.seq);
    }
}

fn default_rect(anchor: Rect, existing: u32, metrics: FloatMetrics) -> Rect {
    let cap = |extent: u32| (f64::from(extent) * INITIAL_MAX_FRACTION).floor() as u32;
    let w = (DEFAULT_COLS * metrics.cell_w + metrics.pad_w).min(cap(anchor.w));
    let h = (DEFAULT_ROWS * metrics.cell_h + metrics.pad_h + metrics.strip_h()).min(cap(anchor.h));
    let offset = metrics.strip_h() * existing;
    let x = anchor
        .right()
        .saturating_sub(w + offset)
        .clamp(anchor.x, anchor.right().saturating_sub(w).max(anchor.x));
    let y = anchor
        .bottom()
        .saturating_sub(h + offset)
        .clamp(anchor.y, anchor.bottom().saturating_sub(h).max(anchor.y));
    Rect::new(x, y, w, h)
}

fn pane_under(point: Point, layout: &FloatLayout<'_>) -> Option<PaneId> {
    let shown = |pane: &PaneId| layout.zoomed.is_none_or(|zoomed| zoomed == *pane);
    layout
        .panes
        .iter()
        .filter(|(pane, _)| shown(pane))
        .find(|(_, rect)| rect.contains(point))
        .or_else(|| {
            // The point sits on a divider: take the nearest pane.
            layout
                .panes
                .iter()
                .filter(|(pane, _)| shown(pane))
                .min_by_key(|(_, rect)| {
                    let cx = i64::from(rect.x + rect.w / 2) - i64::from(point.x);
                    let cy = i64::from(rect.y + rect.h / 2) - i64::from(point.y);
                    cx * cx + cy * cy
                })
        })
        .map(|(pane, _)| *pane)
}

fn hit_part(placed: PlacedFloat, point: Point, metrics: FloatMetrics) -> Option<FloatHitPart> {
    let outer = placed.outer;
    let zone = if placed.state == FloatState::Normal {
        DIVIDER_HIT_ZONE_PX
    } else {
        0
    };
    let within = |value: u32, lo: u32, hi: u32| value + zone >= lo && value < hi + zone;
    if !within(point.x, outer.x, outer.right()) || !within(point.y, outer.y, outer.bottom()) {
        return None;
    }
    if zone > 0 {
        let near = |value: u32, edge: u32| value.abs_diff(edge) <= zone;
        let edges = Edges {
            left: near(point.x, outer.x),
            right: near(point.x + 1, outer.right()),
            top: near(point.y, outer.y),
            bottom: near(point.y + 1, outer.bottom()),
        };
        if edges.left || edges.right || edges.top || edges.bottom {
            return Some(FloatHitPart::Resize(edges));
        }
    }
    if placed.strip.contains(point) {
        let button = [
            StripButton::Anchor,
            StripButton::Maximize,
            StripButton::Collapse,
            StripButton::Close,
        ]
        .into_iter()
        .find(|button| strip_button_rect(placed.strip, *button, metrics).contains(point));
        return Some(button.map_or(FloatHitPart::Strip, FloatHitPart::Button));
    }
    Some(FloatHitPart::Content)
}

#[cfg(test)]
mod tests;
