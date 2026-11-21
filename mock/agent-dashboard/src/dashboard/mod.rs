use std::time::{Duration, Instant};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, percent,
};
use noa_gpui_core::widgets::{ButtonClicked, Chart, Container, LevelIndicator, SelectionChanged};
use noa_gpui_core::{
    Appearance, Dirty, Message, Role, ShortcutPressed, Style, Theme, UiTree, WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use crate::fleet::{self, Fleet, Grouping, minutes_text};
use crate::live::{Channel, Command, Link, Snapshot};

mod cards;
mod graph;
mod header;
mod panel;
mod queue;
mod style;
mod timeline;
use cards::{Cards, done, done_caption, quiet_text, running, running_caption};
use graph::graph;
use header::{header, longest, longest_text, waited_text};
use panel::{Panel, empty, history_row, panel};
use queue::{QueueButtons, queue, queue_since};
use style::{GUTTER, fill_remaining, gap, set_label, set_tag};
use timeline::{TimelineParts, fill_lanes, span_text, timeline, timeline_span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct View {
    grouping: Grouping,
    expanded: Option<u64>,
    shown: [bool; 5],
}

impl View {
    fn of(grouping: Grouping) -> Self {
        View {
            grouping,
            expanded: None,
            shown: [true; 5],
        }
    }
}

/// Where the workers come from: the canned fleet (no link) or the live feed.
#[derive(Default)]
struct Feed {
    fleet: Fleet,
    link: Option<Link>,
    synced_secs: u64,
    /// The live feed's `Snapshot::can_focus`.
    can_focus: bool,
    /// Why the last "open" failed, shown in the header instead of the link
    /// state for `NOTICE_FOR`.
    notice: Option<(String, Instant)>,
    channel: Option<Channel>,
}

const NOTICE_FOR: Duration = Duration::from_secs(8);

impl Feed {
    fn demo() -> Self {
        Feed {
            fleet: fleet::canned(),
            ..Feed::default()
        }
    }

    fn status(&self) -> String {
        if let Some((notice, at)) = &self.notice
            && at.elapsed() < NOTICE_FOR
        {
            return notice.clone();
        }
        let n = self.fleet.workers.len();
        match &self.link {
            None => format!("ワーカー {n} · ダミーデータ"),
            Some(Link::Connecting) => "接続中…".to_owned(),
            Some(Link::Live) => format!(
                "ワーカー {n} · 同期 {}前",
                minutes_text(self.synced_secs as f64 / 60.0)
            ),
            Some(Link::Down(reason)) => format!("切断: {reason}"),
        }
    }
}

/// A widget whose content changes as time passes, updated in place by
/// `Dashboard::refresh`; the pane names the worker it shows.
#[derive(Debug, Clone, Copy)]
enum Pulse {
    Status,
    Waited,
    Longest,
    QueueSince(u64),
    QuietTag(u64),
    Output(u64),
    Freshness(u64),
    RunningCaption(u64),
    DoneCaption(u64),
}

pub struct Dashboard {
    tree: UiTree,
    view: View,
    feed: Feed,
    status: WidgetId,
    grouping_control: WidgetId,
    queue_buttons: QueueButtons,
    pulses: Vec<(WidgetId, Pulse)>,
    timeline: Option<TimelineParts>,
    /// The size the last `prepare` laid the screen out at. A rebuilt screen is
    /// laid out at it at once (see `rebuilt`).
    viewport: Option<Size>,
    #[cfg(test)]
    panels: [Option<WidgetId>; 5],
}

impl Dashboard {
    /// The demo: the canned fleet.
    pub fn new(grouping: Grouping) -> Self {
        Self::showing(View::of(grouping))
    }

    fn showing(view: View) -> Self {
        Self::build(view, Theme::for_appearance(Appearance::Dark), Feed::demo())
    }

    /// Fed by the live feed, connecting until its first snapshot arrives.
    pub fn live(grouping: Grouping, channel: Channel) -> Self {
        let feed = Feed {
            link: Some(Link::Connecting),
            channel: Some(channel),
            ..Feed::default()
        };
        Self::build(
            View::of(grouping),
            Theme::for_appearance(Appearance::Dark),
            feed,
        )
    }

    /// One live snapshot, with no feed behind it.
    pub fn of_snapshot(grouping: Grouping, snapshot: Snapshot) -> Self {
        Self::from_snapshot(View::of(grouping), snapshot)
    }

    fn from_snapshot(view: View, snapshot: Snapshot) -> Self {
        let feed = Feed {
            fleet: snapshot.fleet,
            link: Some(snapshot.link),
            synced_secs: snapshot.synced_secs,
            can_focus: snapshot.can_focus,
            notice: None,
            channel: None,
        };
        Self::build(view, Theme::for_appearance(Appearance::Dark), feed)
    }

    /// Takes a snapshot in. One that changes only what time changes (texts,
    /// meters, lanes) updates those widgets in place, so the scroll positions
    /// and the hover survive; any other change rebuilds the screen.
    fn apply(&mut self, snapshot: Snapshot, text: &mut TextStore) {
        let structural = self.feed.link.as_ref() != Some(&snapshot.link)
            || !self.feed.fleet.same_structure(&snapshot.fleet);
        self.feed.fleet = snapshot.fleet;
        self.feed.link = Some(snapshot.link);
        self.feed.synced_secs = snapshot.synced_secs;
        self.feed.can_focus = snapshot.can_focus;
        if structural {
            self.rebuilt(self.view, text);
        } else {
            self.refresh(text);
        }
    }

    fn show_notice(&mut self, notice: String) {
        self.feed.notice = Some((notice, Instant::now()));
        set_label(&mut self.tree, self.status, self.feed.status());
    }

    fn refresh(&mut self, text: &mut TextStore) {
        let (tree, fleet) = (&mut self.tree, &self.feed.fleet);
        for &(id, pulse) in &self.pulses {
            let worker = |pane: u64| fleet.worker(pane).expect("the same structure");
            match pulse {
                Pulse::Status => set_label(tree, id, self.feed.status()),
                Pulse::Waited => set_tag(tree, id, waited_text(&fleet.workers)),
                Pulse::Longest => {
                    if let Some(worker) = longest(&fleet.workers) {
                        set_tag(tree, id, longest_text(worker));
                    }
                }
                Pulse::QueueSince(pane) => {
                    let worker = worker(pane);
                    if let Some(need) = worker.need() {
                        set_label(tree, id, queue_since(worker, need));
                    }
                }
                Pulse::QuietTag(pane) => set_tag(tree, id, quiet_text(worker(pane))),
                Pulse::Output(pane) => {
                    let values = worker(pane).lines_per_tick.clone();
                    tree.update::<Chart, _>(id, |c, cx| c.set_values(cx, 0, values));
                }
                Pulse::Freshness(pane) => {
                    let value = worker(pane).freshness;
                    tree.update::<LevelIndicator, _>(id, |l, cx| l.set_value(cx, value));
                }
                Pulse::RunningCaption(pane) => set_label(tree, id, running_caption(worker(pane))),
                Pulse::DoneCaption(pane) => set_label(tree, id, done_caption(worker(pane))),
            }
        }
        if let Some(parts) = &self.timeline {
            let span = timeline_span(&fleet.workers);
            set_label(tree, parts.span, span_text(span));
            for lane in tree.children(parts.lanes).to_vec() {
                tree.remove(lane, text);
            }
            fill_lanes(tree, parts.lanes, &fleet.workers, self.view.grouping, span);
        }
    }

    /// Builds the same screen again for a changed view, keeping the theme (the
    /// appearance the OS last set) the screen has now. The old tree's widgets
    /// go through `UiTree::remove` first: that is what releases their text
    /// layouts in the `TextStore`. The new tree is laid out at the old size
    /// before this returns: the window delivers input before it lays out for the
    /// next frame, and a tree without rects would let that input hit nothing.
    /// That layout is not a frame the window presented, so the root is marked
    /// for compositing again: the window then requests a redraw (it asks
    /// `needs_update` after input) and presents (`prepare` returns true), and the
    /// list it presents is the new tree's, reassembled from what this layout
    /// painted.
    fn rebuilt(&mut self, view: View, text: &mut TextStore) {
        let theme = self.tree.theme().clone();
        let roots: Vec<WidgetId> = self
            .tree
            .root()
            .into_iter()
            .chain(self.tree.overlays())
            .collect();
        for root in roots {
            self.tree.remove(root, text);
        }
        let viewport = self.viewport;
        let feed = std::mem::take(&mut self.feed);
        *self = Self::build(view, theme, feed);
        if let Some(viewport) = viewport {
            self.prepare(viewport, text);
            if let Some(root) = self.tree.root() {
                self.tree.invalidate(root, Dirty::COMPOSITE);
            }
        }
    }

    fn build(view: View, theme: Theme, feed: Feed) -> Self {
        let grouping = view.grouping;
        let workers = &feed.fleet.workers;
        let mut pulses = Vec::new();
        let mut tree = UiTree::new();
        tree.set_theme(theme);

        let root = tree.insert(
            None,
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            })
            .background(Role::Background),
        );

        let header = header(&mut tree, root, &feed, grouping, &view.shown, &mut pulses);
        for (panel, item) in Panel::ALL.into_iter().zip(header.hints) {
            tree.add_shortcut(panel.shortcut(), item);
        }
        let body = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(GUTTER),
                padding: LayoutRect::length(GUTTER),
                ..fill_remaining()
            }),
        );
        let mut queue_buttons = QueueButtons::default();
        let mut timeline_parts = None;
        #[cfg(test)]
        let mut panels = [None; 5];
        let now_shown = Panel::NOW.iter().any(|p| view.shown[*p as usize]);
        let rows = [
            (Panel::NOW.as_slice(), fill_remaining()),
            (Panel::HISTORY.as_slice(), history_row(now_shown)),
        ];
        let nobody = feed.link.as_ref().filter(|_| workers.is_empty());
        if let Some(link) = nobody {
            empty(&mut tree, body, link);
        }
        // The demo's numbers stand for lines; the live ones count preview
        // updates.
        let output_caption = if feed.link.is_some() {
            "出力の更新（回 / 5 秒）"
        } else {
            "出力（行 / 間隔）"
        };
        for (members, style) in rows {
            if nobody.is_some() {
                break;
            }
            if !members.iter().any(|p| view.shown[*p as usize]) {
                continue;
            }
            let row = tree.insert(
                Some(body),
                Container::new(Style {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::STRETCH,
                    gap: gap(GUTTER),
                    ..style
                }),
            );
            for &member in members.iter().filter(|p| view.shown[**p as usize]) {
                let id = panel(&mut tree, row, member.style(&view.shown));
                #[cfg(test)]
                {
                    panels[member as usize] = Some(id);
                }
                match member {
                    Panel::Queue => {
                        queue_buttons = queue(&mut tree, id, workers, view.expanded, &mut pulses);
                    }
                    Panel::Running => {
                        let cards = Cards {
                            grouping,
                            output_caption,
                        };
                        running(&mut tree, id, workers, cards, &mut pulses);
                    }
                    Panel::Done => done(&mut tree, id, workers, grouping, &mut pulses),
                    Panel::Timeline => {
                        timeline_parts = Some(timeline(&mut tree, id, workers, grouping));
                    }
                    Panel::Graph => graph(&mut tree, id, &feed.fleet),
                }
            }
        }
        if let Some(channel) = &feed.channel {
            channel.send(Command::Expand(queue_buttons.expanded));
        }

        Self {
            tree,
            view,
            feed,
            status: header.status,
            grouping_control: header.grouping_control,
            queue_buttons,
            pulses,
            timeline: timeline_parts,
            viewport: None,
            #[cfg(test)]
            panels,
        }
    }
}

impl Screen for Dashboard {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        self.viewport = Some(viewport);
        self.tree.prepare(viewport, text).is_some()
    }

    fn before_frame(&mut self, text: &mut TextStore) {
        let latest = self
            .feed
            .channel
            .as_ref()
            .and_then(|channel| channel.snapshots.try_iter().last());
        if let Some(snapshot) = latest {
            self.apply(snapshot, text);
        }
        let notice = self
            .feed
            .channel
            .as_ref()
            .and_then(|channel| channel.notices.try_iter().last());
        if let Some(notice) = notice {
            self.show_notice(notice);
        }
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if let Some(ShortcutPressed(pressed)) = message.downcast_ref::<ShortcutPressed>() {
            if let Some(panel) = Panel::ALL.into_iter().find(|p| p.shortcut() == *pressed) {
                let mut shown = self.view.shown;
                shown[panel as usize] = !shown[panel as usize];
                self.rebuilt(View { shown, ..self.view }, text);
            }
            return;
        }
        if let Some(SelectionChanged(index)) = message.downcast_ref::<SelectionChanged>()
            && message.source == self.grouping_control
        {
            self.rebuilt(
                View {
                    grouping: Grouping::ALL[*index],
                    ..self.view
                },
                text,
            );
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        if let Some(pane) = QueueButtons::worker(&self.queue_buttons.details, message.source) {
            self.rebuilt(
                View {
                    expanded: Some(pane),
                    ..self.view
                },
                text,
            );
        } else if let Some(pane) = QueueButtons::worker(&self.queue_buttons.open, message.source)
            && let Some(worker) = self.feed.fleet.worker(pane)
        {
            let status = match &self.feed.channel {
                Some(_) if self.feed.link != Some(Link::Live) => {
                    Err("ペインへ移動できません: Noa に接続していません".to_owned())
                }
                Some(_) if !self.feed.can_focus => Err(
                    "ペインへ移動できません: Noa の server-scopes に control がありません"
                        .to_owned(),
                ),
                Some(channel) => {
                    channel.send(Command::Focus(pane));
                    Ok(format!("{} のペインへ移動します", worker.label))
                }
                None => Ok(format!("モック: {} のペインへ移動します", worker.label)),
            };
            match status {
                Ok(status) => set_label(&mut self.tree, self.status, status),
                Err(notice) => self.show_notice(notice),
            }
        }
    }
}

#[cfg(test)]
mod tests;
