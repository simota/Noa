//! A setup overview, followed by assets, registration candidates and sources.
//! Search and attention filters apply to every tab; selected items stay pinned
//! by identity across rescans. Writes go through explicit preview actions.
//!
//! Keys outside text inputs: `0` setup, `1`/`2`/`3` the existing tabs,
//! `j`/`k` or arrows move, `Enter` opens detail, `/` searches, `Esc` backs
//! out, `r` rescans, `t` hands a candidate off, `l` links a verification.

pub mod backend;
mod form;
mod setup;
mod style;

use noa_gpui_core::style::{FlexDirection, Rect as LayoutRect, Size as LayoutSize, length};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, Pressable, ScrollView, SegmentMode,
    SegmentedControl, SelectionChanged, Tag, TextArea, TextChanged, TextField, TextSubmitted, Tone,
};
use noa_gpui_core::{
    Appearance, Event, Key, Message, Modifiers, Paint, Role, Style, Theme, UiTree, Widget as _,
    WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Rect, Size};
use noa_gpui_text::TextStore;

use scaffold::model::{
    Asset, Improvement, ImprovementStatus, Subject, VerifyState, check_history, improvement_status,
    rule_label, verification,
};
use scaffold::scan::{REGISTRY, SourceState};
use scaffold::store::LinkKind;
use scaffold::time::{self, Millis};

pub use backend::{Action, Backend, Live, State};
use form::{Form, Kind as FormKind, Outcome};
use style::{DETAIL_WIDTH, Faded, GUTTER, RESOLVED_OPACITY, fill, full, gap, grow, row, wide};

/// The interval of the automatic rescan (the same tick as Tasks).
pub const RESCAN_MS: Millis = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Setup,
    Assets,
    Candidates,
    Sources,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Setup, Tab::Assets, Tab::Candidates, Tab::Sources];

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Setup => "セットアップ",
            Tab::Assets => "資産",
            Tab::Candidates => "改善候補",
            Tab::Sources => "情報源",
        }
    }
}

/// What a list row names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Setup(String),
    Asset(String),
    Candidate(String),
    Source(String),
}

impl Item {
    fn subject(&self) -> Option<Subject> {
        match self {
            Item::Asset(id) => Some(Subject::Asset(id.clone())),
            Item::Candidate(id) => Some(Subject::Improvement(id.clone())),
            Item::Setup(_) | Item::Source(_) => None,
        }
    }
}

/// What a button in the detail pane does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Press {
    RegisterPreview,
    RegisterAssets,
    SetupTaskPreview,
    SetupTask,
    ClosePreview,
    GoTab(Tab),
    ClearFilters,
    CloseDetail,
    ToggleBrief,
    HandOff,
    OpenLink(LinkKind),
    Unlink(LinkKind, String),
    OpenSkip,
    OpenDecide,
}

/// The widgets the screen updates in place.
struct Ids {
    repo: WidgetId,
    commit: WidgetId,
    score: WidgetId,
    scanned: WidgetId,
    rescan: WidgetId,
    tabs: WidgetId,
    notice: WidgetId,
    search: WidgetId,
    filter: WidgetId,
    count: WidgetId,
    list: WidgetId,
    detail: WidgetId,
    info: WidgetId,
    form_host: WidgetId,
}

pub struct Scaffold {
    tree: UiTree,
    backend: Box<dyn Backend>,
    state: State,
    tab: Tab,
    /// The selection of each tab, by what it names (kept across rescans).
    selected: [Option<Item>; 4],
    query: String,
    attention_only: bool,
    setup_preview: Option<setup::Preview>,
    detail_open: bool,
    brief_open: bool,
    form: Option<Form>,
    /// The last action's notice, and whether it is a refusal.
    notice: Option<(String, bool)>,
    /// The text a shortcut's key types is not typed anywhere.
    swallow: Option<char>,
    /// The selected row is scrolled into view after the next layout: the
    /// list keeps its offset while its rows are rebuilt.
    reveal: bool,
    ids: Ids,
    rows: Vec<(WidgetId, Item)>,
    presses: Vec<(WidgetId, Press)>,
}

impl Scaffold {
    pub fn new(backend: Box<dyn Backend>) -> Scaffold {
        let mut tree = UiTree::new();
        let mut theme = Theme::for_appearance(Appearance::Dark);
        // Keep secondary text and status colors readable on the selected row.
        theme.selection = noa_gpui_core::theme::mix(theme.surface, theme.accent, 0.12);
        tree.set_theme(theme);
        let root = tree.insert(
            None,
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: full(),
                ..Default::default()
            })
            .background(Role::Background),
        );
        let header = tree.insert(
            Some(root),
            Container::new(Style {
                padding: LayoutRect::length(GUTTER),
                ..style::column(10.0)
            })
            .background(Role::Surface),
        );
        let identity = tree.insert(Some(header), Container::new(row(12.0)));
        tree.insert(
            Some(identity),
            Label::new("足場").font_size(20.0).weight(700),
        );
        let repo_host = tree.insert(Some(identity), Container::new(grow()));
        let repo = tree.insert(
            Some(repo_host),
            Label::new("")
                .font_size(12.0)
                .max_lines(1)
                .color(Role::Muted),
        );
        let rescan = tree.insert(Some(identity), Button::new("再スキャン (R)"));
        let status = tree.insert(Some(header), Container::new(row(10.0)));
        let score = tree.insert(Some(status), Tag::new(""));
        let commit = tree.insert(Some(status), Tag::new(""));
        let scanned = tree.insert(
            Some(status),
            Label::new("").font_size(11.0).color(Role::Muted),
        );
        let navigation = tree.insert(Some(header), Container::new(row(12.0)));
        let tabs = tree.insert(
            Some(navigation),
            SegmentedControl::new(Tab::ALL.map(Tab::label), SegmentMode::SelectOne)
                .with_selected(&[0]),
        );
        let search = tree.insert(
            Some(navigation),
            TextField::new("名称・パスを検索 (/)", 230.0),
        );
        let filter = tree.insert(
            Some(navigation),
            SegmentedControl::new(["すべて", "要対応"], SegmentMode::SelectOne)
                .control_size(ControlSize::Small)
                .with_selected(&[0]),
        );
        let count_host = tree.insert(Some(navigation), Container::new(grow()));
        let count = tree.insert(
            Some(count_host),
            Label::new("")
                .font_size(11.0)
                .color(Role::Muted)
                .max_lines(1),
        );
        let notice_host = tree.insert(
            Some(root),
            Container::new(Style {
                padding: LayoutRect {
                    left: length(GUTTER),
                    right: length(GUTTER),
                    top: length(0.0),
                    bottom: length(0.0),
                },
                ..style::column(0.0)
            }),
        );
        let notice = tree.insert(Some(notice_host), Label::new("").font_size(12.0).wrap(true));
        let body = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                gap: gap(GUTTER),
                padding: LayoutRect::length(GUTTER),
                ..fill()
            }),
        );
        let list = tree.insert(
            Some(body),
            ScrollView::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(6.0),
                ..fill()
            }),
        );
        let detail = tree.insert(
            Some(body),
            ScrollView::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(10.0),
                flex_shrink: 0.0,
                size: LayoutSize {
                    width: length(DETAIL_WIDTH),
                    height: noa_gpui_core::style::percent(1.0),
                },
                ..Default::default()
            }),
        );
        let info = tree.insert(Some(detail), Container::new(style::column(8.0)));
        let form_host = tree.insert(Some(detail), Container::new(style::column(8.0)));

        let footer = tree.insert(
            Some(root),
            Container::new(Style {
                padding: LayoutRect {
                    left: length(GUTTER),
                    right: length(GUTTER),
                    top: length(8.0),
                    bottom: length(8.0),
                },
                ..style::column(0.0)
            })
            .background(Role::Surface),
        );
        tree.insert(Some(footer), Label::new(
            "0 セットアップ · 1 資産 · 2 改善候補 · 3 情報源    ↑↓ 選択 · Enter 詳細 · / 検索 · Esc 戻る · R 再スキャン"
        ).font_size(10.0).color(Role::Muted).max_lines(1));

        let mut backend = backend;
        let state = backend.scan();
        let mut screen = Scaffold {
            tree,
            backend,
            state,
            tab: Tab::Setup,
            selected: [None, None, None, None],
            query: String::new(),
            attention_only: false,
            setup_preview: None,
            detail_open: false,
            brief_open: false,
            form: None,
            notice: None,
            swallow: None,
            reveal: false,
            ids: Ids {
                repo,
                commit,
                score,
                scanned,
                rescan,
                tabs,
                notice,
                search,
                filter,
                count,
                list,
                detail,
                info,
                form_host,
            },
            rows: Vec::new(),
            presses: Vec::new(),
        };
        // Nothing is laid out yet: any store serves the first build.
        screen.rebuild(&mut TextStore::new(Default::default()));
        screen
    }

    /// The selected item of the shown tab.
    pub fn selection(&self) -> Option<Item> {
        let items = self.items(self.tab);
        let wanted = self.selected[self.tab.index()].as_ref();
        wanted
            .filter(|w| items.contains(w))
            .cloned()
            .or_else(|| items.first().cloned())
    }

    /// The visible rows of `tab`, in order, sharing the toolbar's filters.
    pub fn items(&self, tab: Tab) -> Vec<Item> {
        let terms: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.all_items(tab)
            .into_iter()
            .filter(|item| {
                (!self.attention_only || self.needs_attention(item))
                    && terms
                        .iter()
                        .all(|term| self.search_text(item).to_lowercase().contains(term))
            })
            .collect()
    }

    fn all_items(&self, tab: Tab) -> Vec<Item> {
        match tab {
            Tab::Setup => self
                .state
                .setup
                .checks
                .iter()
                .map(|check| Item::Setup(check.id.clone()))
                .collect(),
            Tab::Assets => self
                .state
                .registry
                .as_ref()
                .map(|l| {
                    l.registry
                        .assets
                        .iter()
                        .map(|a| Item::Asset(a.id.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            Tab::Candidates => self
                .candidates()
                .into_iter()
                .map(|i| Item::Candidate(i.id.clone()))
                .collect(),
            Tab::Sources => self
                .state
                .report
                .sources
                .iter()
                .map(|s| Item::Source(s.path.clone()))
                .collect(),
        }
    }

    fn search_text(&self, item: &Item) -> String {
        match item {
            Item::Setup(id) => self
                .state
                .setup
                .checks
                .iter()
                .find(|c| &c.id == id)
                .map(|c| {
                    format!(
                        "{} {} {} {} {}",
                        c.id,
                        c.title,
                        c.description,
                        c.next_step,
                        c.evidence
                            .iter()
                            .map(|e| e.path.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                })
                .unwrap_or_default(),
            Item::Asset(id) => self
                .asset(id)
                .map(|a| {
                    format!(
                        "{} {} {} {} {} {}",
                        a.id,
                        a.title,
                        a.path,
                        a.kind.label(),
                        a.lifecycle().label(),
                        self.verification_of(id).label()
                    )
                })
                .unwrap_or_default(),
            Item::Candidate(id) => self
                .improvement(id)
                .map(|i| {
                    format!(
                        "{} {} {} {} {} {}",
                        i.widget,
                        i.target,
                        i.rule,
                        rule_label(&i.rule),
                        self.status_of(i).label(),
                        i.evidence
                            .iter()
                            .map(|e| e.path.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                })
                .unwrap_or_default(),
            Item::Source(path) => self
                .state
                .report
                .sources
                .iter()
                .find(|s| &s.path == path)
                .map(|s| format!("{} {}", s.path, s.error.as_deref().unwrap_or("ok")))
                .unwrap_or_default(),
        }
    }

    fn needs_attention(&self, item: &Item) -> bool {
        match item {
            Item::Setup(id) => self
                .state
                .setup
                .checks
                .iter()
                .find(|c| &c.id == id)
                .is_some_and(|c| c.state != scaffold::setup::CheckState::Found),
            Item::Asset(id) => self.asset(id).is_some_and(|a| {
                check_history(a).is_err()
                    || (!matches!(a.lifecycle(), scaffold::model::Lifecycle::Retired)
                        && !matches!(
                            self.verification_of(id),
                            VerifyState::Verified | VerifyState::Skipped
                        ))
            }),
            Item::Candidate(id) => self
                .improvement(id)
                .is_some_and(|i| self.status_of(i) == ImprovementStatus::Open),
            Item::Source(path) => self
                .state
                .report
                .sources
                .iter()
                .find(|s| &s.path == path)
                .is_some_and(|s| s.state == SourceState::Unknown),
        }
    }

    fn filters_changed(&mut self, text: &mut TextStore) {
        if self.selected[self.tab.index()]
            .as_ref()
            .is_some_and(|s| !self.items(self.tab).contains(s))
        {
            self.detail_open = false;
            self.brief_open = false;
            self.close_form(text);
        }
        self.reveal = true;
        self.rebuild(text);
    }

    fn clear_filters(&mut self, text: &mut TextStore) {
        self.query.clear();
        self.attention_only = false;
        self.tree
            .update::<TextField, _>(self.ids.search, |f, cx| f.set_text(cx, ""));
        self.tree
            .update::<SegmentedControl, _>(self.ids.filter, |s, cx| s.set_selected(cx, &[0]));
        self.filters_changed(text);
    }

    fn focus_search(&mut self) {
        self.tree.set_focus(Some(self.ids.search));
        self.tree
            .update::<TextField, _>(self.ids.search, |f, cx| f.select(cx, 0..f.text().len()));
    }

    /// Open candidates first (by widget, target, rule), then resolved ones.
    fn candidates(&self) -> Vec<&Improvement> {
        if self.state.unregistered() {
            return Vec::new();
        }
        let mut list: Vec<&Improvement> = self.state.improvements.iter().collect();
        list.sort_by(|a, b| {
            (a.resolved.is_some(), &a.widget, &a.target, &a.rule).cmp(&(
                b.resolved.is_some(),
                &b.widget,
                &b.target,
                &b.rule,
            ))
        });
        list
    }

    fn asset(&self, id: &str) -> Option<&Asset> {
        self.state
            .registry
            .as_ref()
            .ok()?
            .registry
            .assets
            .iter()
            .find(|a| a.id == id)
    }

    fn improvement(&self, id: &str) -> Option<&Improvement> {
        self.state.improvements.iter().find(|i| i.id == id)
    }

    /// 「整合 n/m」, or why there is no score.
    pub fn score_label(&self) -> String {
        if !self.state.setup.registration_applicable {
            return format!(
                "構成を検出 {}/{}",
                self.state.setup.found_count(),
                self.state.setup.checks.len()
            );
        }
        if self.state.unregistered() {
            return "スコアなし（registry なし）".into();
        }
        match self.state.report.score() {
            Some((n, m)) => format!("整合 {n}/{m}"),
            None => "スコアなし（不明な情報源あり）".into(),
        }
    }

    pub fn verification_of(&self, asset: &str) -> VerifyState {
        let subject = Subject::Asset(asset.to_owned());
        verification(&self.state.facts_of(&subject), self.state.commit.as_deref())
    }

    pub fn status_of(&self, imp: &Improvement) -> ImprovementStatus {
        improvement_status(imp, &self.state.observations)
    }

    fn stamp(&self, at: Millis) -> String {
        time::stamp(at, self.backend.offset(at))
    }

    /// Scans again and shows the result.
    pub fn rescan(&mut self, text: &mut TextStore) {
        let state = self.backend.scan();
        self.set_state(state, text);
        self.rebuild(text);
    }

    /// Takes a new scan. Each tab's selection follows its item by id; when
    /// the shown tab's item is gone, its detail closes rather than show
    /// whichever row took its place.
    fn set_state(&mut self, state: State, text: &mut TextStore) {
        self.state = state;
        for tab in Tab::ALL {
            let i = tab.index();
            let gone = self.selected[i]
                .as_ref()
                .is_some_and(|item| !self.items(tab).contains(item));
            if !gone {
                continue;
            }
            self.selected[i] = None;
            if tab == self.tab {
                self.detail_open = false;
                self.brief_open = false;
                self.close_form(text);
            }
        }
    }

    /// `text` must be the store the tree was laid out with: removed widgets
    /// release their text layouts in it.
    fn rebuild(&mut self, text: &mut TextStore) {
        self.update_header();
        clear(&mut self.tree, self.ids.list, text);
        clear(&mut self.tree, self.ids.info, text);
        self.rows.clear();
        self.presses.clear();
        let selection = self.selection();
        // Pinned by id, the first row's fallback included: a rescan that
        // adds a row before it does not move the selection or the detail.
        self.selected[self.tab.index()] = selection.clone();
        match self.tab {
            Tab::Setup => self.build_setup(selection.as_ref()),
            Tab::Assets => self.build_assets(selection.as_ref()),
            Tab::Candidates => self.build_candidates(selection.as_ref()),
            Tab::Sources => self.build_sources(selection.as_ref()),
        }
        let show_detail = self.detail_open && selection.is_some();
        self.tree.set_hidden(self.ids.detail, !show_detail);
        if show_detail && let Some(item) = &selection {
            self.build_detail(item);
        }
        if !show_detail && self.form.is_some() {
            self.close_form(text);
        }
    }

    fn update_header(&mut self) {
        let repo = self.state.repo.display().to_string();
        let commit = match &self.state.commit {
            Some(c) => format!("commit {}", c.chars().take(7).collect::<String>()),
            None => "commit 不明".to_owned(),
        };
        let score = self.score_label();
        let scanned = if self.backend.is_demo() {
            format!(
                "デモ表示 · 最終スキャン {}",
                self.stamp(self.state.scanned_at)
            )
        } else {
            format!("最終スキャン {}", self.stamp(self.state.scanned_at))
        };
        let tab = self.tab.index();
        let count = format!(
            "{} / {} 件",
            self.items(self.tab).len(),
            self.all_items(self.tab).len()
        );
        let tree = &mut self.tree;
        tree.update::<Label, _>(self.ids.repo, |l, cx| l.set_text(cx, repo));
        tree.update::<Tag, _>(self.ids.commit, |t, cx| t.set_text(cx, commit));
        tree.update::<Tag, _>(self.ids.score, |t, cx| t.set_text(cx, score));
        tree.update::<Label, _>(self.ids.scanned, |l, cx| l.set_text(cx, scanned));
        tree.update::<Label, _>(self.ids.count, |l, cx| l.set_text(cx, count));
        tree.update::<SegmentedControl, _>(self.ids.tabs, |s, cx| {
            if s.selected().get(tab) != Some(&true) {
                s.set_selected(cx, &[tab]);
            }
        });
        let (notice, color) = match (&self.notice, &self.state.problem) {
            (Some((n, true)), _) => (n.clone(), Paint::Fixed(style::RED)),
            (Some((n, false)), _) => (n.clone(), Paint::Role(Role::Accent)),
            (None, Some(p)) => (p.clone(), Paint::Fixed(style::RED)),
            (None, None) => (String::new(), Paint::Role(Role::Muted)),
        };
        let hidden = notice.is_empty();
        tree.update::<Label, _>(self.ids.notice, |l, cx| {
            l.set_text(cx, notice);
            l.set_color(cx, color);
        });
        tree.set_hidden(self.ids.notice, hidden);
    }

    /// A row frame; its pressable holds the row's labels.
    fn row(&mut self, item: Item, selected: bool, faded: bool) -> WidgetId {
        let label = match &item {
            Item::Setup(id) => self
                .state
                .setup
                .checks
                .iter()
                .find(|c| &c.id == id)
                .map(|c| format!("{}、{}", c.title, c.state.label()))
                .unwrap_or_default(),
            Item::Asset(id) => self
                .asset(id)
                .map(|a| {
                    format!(
                        "{}、{}、{}",
                        a.title,
                        a.lifecycle().label(),
                        self.verification_of(id).label()
                    )
                })
                .unwrap_or_default(),
            Item::Candidate(id) => self
                .improvement(id)
                .map(|i| {
                    format!(
                        "{} {}、{}、{}",
                        i.widget,
                        i.target,
                        rule_label(&i.rule),
                        self.status_of(i).label()
                    )
                })
                .unwrap_or_default(),
            Item::Source(path) => self
                .state
                .report
                .sources
                .iter()
                .find(|s| &s.path == path)
                .map(|s| {
                    format!(
                        "{}、{}",
                        s.path,
                        if s.state == SourceState::Ok {
                            "読込済み"
                        } else {
                            "確認できません"
                        }
                    )
                })
                .unwrap_or_default(),
        };
        let parent = if faded {
            self.tree.insert(
                Some(self.ids.list),
                Faded {
                    style: wide(style::column(0.0)),
                    opacity: RESOLVED_OPACITY,
                },
            )
        } else {
            self.ids.list
        };
        let frame = self.tree.insert(
            Some(parent),
            Container::new(wide(Style {
                flex_shrink: 0.0,
                ..Default::default()
            }))
            .background(Role::Surface)
            .border(
                if selected { 2.0 } else { 1.0 },
                if selected {
                    Role::Accent
                } else {
                    Role::Separator
                },
            )
            .radius(6.0)
            .clip(true),
        );
        let pressable = self.tree.insert(
            Some(frame),
            Pressable::new(wide(Style {
                padding: LayoutRect {
                    left: length(10.0),
                    right: length(10.0),
                    top: length(10.0),
                    bottom: length(10.0),
                },
                ..style::column(5.0)
            }))
            .label(label)
            .selected(selected)
            .radius(6.0),
        );
        self.rows.push((pressable, item));
        pressable
    }

    fn note(&mut self, parent: WidgetId, text: impl Into<String>) {
        self.tree.insert(
            Some(parent),
            Label::new(text)
                .font_size(12.0)
                .color(Role::Muted)
                .wrap(true),
        );
    }

    fn empty(&mut self, title: &str, description: impl Into<String>) {
        let card = self.tree.insert(
            Some(self.ids.list),
            Container::new(wide(style::column(10.0)))
                .background(Role::Surface)
                .border(1.0, Role::Separator)
                .radius(8.0)
                .padding(18.0),
        );
        self.tree.insert(
            Some(card),
            Label::new(title).font_size(15.0).weight(600).wrap(true),
        );
        self.note(card, description);
    }

    fn filtered_empty(&mut self) {
        self.empty(
            "一致する項目がありません",
            "検索語を短くするか、「すべて」に戻して確認できます。",
        );
        self.button(self.ids.list, "検索・絞り込みを解除", Press::ClearFilters);
    }

    fn build_assets(&mut self, selection: Option<&Item>) {
        match &self.state.registry {
            Err(scaffold::registry::LoadError::Absent) => {
                self.empty("開発資産を登録してはじめましょう", "未登録リポジトリです。セットアップで見つかった手順書・ルール・検証設定を確認し、資産として登録できます。");
                self.button(self.ids.list, "セットアップへ", Press::GoTab(Tab::Setup));
                return;
            }
            Err(e) => {
                let error = e.to_string();
                self.empty(
                    "資産台帳を読み込めません",
                    format!(
                        "{error}。情報源で原因を確認し、ファイルを修正して再スキャンしてください。"
                    ),
                );
                self.button(self.ids.list, "情報源を確認", Press::GoTab(Tab::Sources));
                return;
            }
            Ok(loaded) if loaded.registry.assets.is_empty() => {
                self.empty("登録済みの資産はまだありません", "セットアップで検出した開発資産を登録すると、判断と検証の記録をここに残せます。");
                self.button(self.ids.list, "セットアップへ", Press::GoTab(Tab::Setup));
                return;
            }
            Ok(_) => {}
        }
        let items = self.items(Tab::Assets);
        if items.is_empty() {
            self.filtered_empty();
            return;
        }
        for item in items {
            let Item::Asset(id) = &item else { continue };
            let Some(a) = self.asset(id).cloned() else {
                continue;
            };
            let verify = self.verification_of(&a.id);
            let r = self.row(item.clone(), selection == Some(&item), false);
            let top = self.tree.insert(Some(r), Container::new(wide(row(8.0))));
            let host = self.tree.insert(Some(top), Container::new(grow()));
            self.tree.insert(
                Some(host),
                Label::new(&a.title)
                    .font_size(14.0)
                    .weight(600)
                    .max_lines(1),
            );
            self.tree.insert(
                Some(top),
                Tag::new(verify.label()).tone(verify_tone(verify)),
            );
            let bottom = self.tree.insert(Some(r), Container::new(wide(row(8.0))));
            let host = self.tree.insert(Some(bottom), Container::new(grow()));
            self.tree.insert(
                Some(host),
                Label::new(format!("{} · {}", a.kind.label(), a.path))
                    .font_size(11.0)
                    .max_lines(1)
                    .color(Role::Muted),
            );
            self.tree.insert(
                Some(bottom),
                Tag::new(a.lifecycle().label()).tone(Tone::Accent),
            );
            if check_history(&a).is_err() {
                self.tree
                    .insert(Some(bottom), Tag::new("記録に矛盾").tone(Tone::Critical));
            }
        }
    }

    fn build_candidates(&mut self, selection: Option<&Item>) {
        if self.state.unregistered() {
            self.empty(
                "未登録リポジトリです",
                "改善候補を記録するには、セットアップで開発資産を確認して登録してください。",
            );
            self.button(self.ids.list, "セットアップへ", Press::GoTab(Tab::Setup));
            return;
        }
        let items = self.items(Tab::Candidates);
        if items.is_empty() {
            if !self.all_items(Tab::Candidates).is_empty() {
                self.filtered_empty();
            } else if self
                .state
                .report
                .sources
                .iter()
                .any(|s| s.state == SourceState::Unknown)
            {
                self.empty("確認できない情報源があります", "改善候補はありませんが、すべての情報源を確認できていません。読込状況を確認してください。");
                self.button(self.ids.list, "情報源を確認", Press::GoTab(Tab::Sources));
            } else {
                self.empty("改善候補はありません", "登録の不整合は見つかりませんでした。開発の準備状況はセットアップで確認できます。");
                self.button(self.ids.list, "セットアップへ", Press::GoTab(Tab::Setup));
            }
            return;
        }
        for item in items {
            let Item::Candidate(id) = &item else { continue };
            let Some(imp) = self.improvement(id).cloned() else {
                continue;
            };
            let status = self.status_of(&imp);
            let r = self.row(
                item.clone(),
                selection == Some(&item),
                status == ImprovementStatus::Resolved,
            );
            let top = self.tree.insert(Some(r), Container::new(wide(row(8.0))));
            let host = self.tree.insert(Some(top), Container::new(grow()));
            self.tree.insert(
                Some(host),
                Label::new(format!("{}/{}", imp.widget, imp.target))
                    .font_size(14.0)
                    .weight(600)
                    .max_lines(1),
            );
            self.tree.insert(
                Some(top),
                Tag::new(status.label()).tone(status_tone(&status)),
            );
            let evidence = imp
                .evidence
                .first()
                .map(|e| format!(" · {}:{}", e.path, e.line))
                .unwrap_or_default();
            self.tree.insert(
                Some(r),
                Label::new(format!("{}{evidence}", rule_label(&imp.rule)))
                    .font_size(11.0)
                    .max_lines(1)
                    .color(Role::Muted),
            );
        }
    }

    fn build_sources(&mut self, selection: Option<&Item>) {
        let items = self.items(Tab::Sources);
        if items.is_empty() {
            if self.state.report.sources.is_empty() {
                self.empty(
                    "読み込んだ情報源はまだありません",
                    "開発用の設定や手順を追加したら、再スキャンして確認できます。",
                );
            } else {
                self.filtered_empty();
            }
            return;
        }
        for item in items {
            let Item::Source(path) = &item else { continue };
            let Some(source) = self
                .state
                .report
                .sources
                .iter()
                .find(|s| &s.path == path)
                .cloned()
            else {
                continue;
            };
            let r = self.row(item.clone(), selection == Some(&item), false);
            let top = self.tree.insert(Some(r), Container::new(wide(row(8.0))));
            let host = self.tree.insert(Some(top), Container::new(grow()));
            self.tree.insert(
                Some(host),
                Label::new(&source.path)
                    .font_size(13.0)
                    .weight(600)
                    .max_lines(1),
            );
            let ok = source.state == SourceState::Ok;
            self.tree.insert(
                Some(top),
                Tag::new(if ok { "ok" } else { "不明" }).tone(if ok {
                    Tone::Success
                } else {
                    Tone::Critical
                }),
            );
            self.tree.insert(
                Some(r),
                Label::new(
                    source
                        .error
                        .unwrap_or_else(|| "スキャンの根拠として読み込みました".into()),
                )
                .font_size(11.0)
                .max_lines(2)
                .color(if ok {
                    Paint::Role(Role::Muted)
                } else {
                    Paint::Fixed(style::RED)
                }),
            );
        }
    }

    fn heading(&mut self, text: impl Into<String>) {
        self.tree.insert(
            Some(self.ids.info),
            Label::new(text).font_size(14.0).weight(700).wrap(true),
        );
    }

    fn section(&mut self, text: &str) {
        self.tree.insert(
            Some(self.ids.info),
            Label::new(text)
                .font_size(11.0)
                .weight(600)
                .color(Role::Muted),
        );
    }

    fn line(&mut self, text: impl Into<String>) {
        self.tree.insert(
            Some(self.ids.info),
            Label::new(text).font_size(12.0).wrap(true).selectable(true),
        );
    }

    fn button(&mut self, parent: WidgetId, label: &str, press: Press) {
        let id = self.tree.insert(
            Some(parent),
            Button::new(label).control_size(ControlSize::Small),
        );
        self.presses.push((id, press));
    }

    fn links(&mut self, subject: &Subject) {
        self.section("関連");
        let links = self.state.links(subject);
        if links.is_empty() {
            self.line("なし");
        }
        for (kind, id, path) in links {
            let r = self
                .tree
                .insert(Some(self.ids.info), Container::new(row(8.0)));
            let host = self.tree.insert(Some(r), Container::new(grow()));
            self.tree.insert(
                Some(host),
                Label::new(format!("{} {id} · {}", kind.label(), path.display()))
                    .font_size(12.0)
                    .wrap(true),
            );
            self.button(r, "外す", Press::Unlink(kind, id));
        }
    }

    fn build_detail(&mut self, item: &Item) {
        self.button(self.ids.info, "詳細を閉じる (Esc)", Press::CloseDetail);
        match item {
            Item::Setup(id) => self.setup_detail(id),
            Item::Asset(id) => self.asset_detail(id),
            Item::Candidate(id) => self.candidate_detail(id),
            Item::Source(path) => {
                let Some(s) = self
                    .state
                    .report
                    .sources
                    .iter()
                    .find(|s| &s.path == path)
                    .cloned()
                else {
                    return;
                };
                self.heading(s.path.clone());
                self.line(match s.state {
                    SourceState::Ok => "状態: ok".to_owned(),
                    SourceState::Unknown => "状態: 不明".to_owned(),
                });
                if let Some(e) = s.error {
                    self.line(format!("理由: {e}"));
                }
            }
        }
    }

    fn asset_detail(&mut self, id: &str) {
        let Some(a) = self.asset(id).cloned() else {
            return;
        };
        let line = self
            .state
            .registry
            .as_ref()
            .ok()
            .and_then(|l| l.id_line(id));
        let verify = self.verification_of(id);
        self.heading(a.title.clone());
        self.line(format!("id: {}", a.id));
        self.line(format!("種別: {}", a.kind.label()));
        self.line(format!("パス: {}", a.path));
        if let Some(line) = line {
            self.line(format!("根拠: {REGISTRY}:{line}"));
        }
        self.line(format!("ライフサイクル: {}", a.lifecycle().label()));
        self.line(format!("検証状態: {}", verify.label()));
        if let Err((i, e)) = check_history(&a) {
            self.line(format!("記録に矛盾: {} 件目の決定 — {e}", i + 1));
        }
        self.section("決定の記録");
        if a.decisions.is_empty() {
            self.line("なし");
        }
        for d in &a.decisions {
            let refs: Vec<String> = d.evidence.iter().map(|e| e.r#ref.clone()).collect();
            let refs = if refs.is_empty() {
                String::new()
            } else {
                format!("（根拠: {}）", refs.join(", "))
            };
            self.line(format!("{} {}: {}{refs}", d.at, d.to.label(), d.reason));
        }
        let subject = Subject::Asset(a.id.clone());
        self.links(&subject);
        let skips: Vec<String> = self
            .state
            .observations
            .iter()
            .filter_map(|o| match o {
                scaffold::model::Observation::Skipped {
                    subject: s,
                    reason,
                    at,
                    ..
                } if *s == subject => Some(format!("{} 省略: {reason}", self.stamp(*at))),
                _ => None,
            })
            .collect();
        for s in skips {
            self.line(s);
        }
        let actions = self
            .tree
            .insert(Some(self.ids.info), Container::new(row(6.0)));
        self.button(
            actions,
            "検証を関連付け (L)",
            Press::OpenLink(LinkKind::Verify),
        );
        self.button(
            actions,
            "レビューを関連付け",
            Press::OpenLink(LinkKind::Review),
        );
        let actions = self
            .tree
            .insert(Some(self.ids.info), Container::new(row(6.0)));
        self.button(actions, "省略にする", Press::OpenSkip);
        self.button(actions, "決定を記録", Press::OpenDecide);
    }

    fn candidate_detail(&mut self, id: &str) {
        let Some(imp) = self.improvement(id).cloned() else {
            return;
        };
        let status = self.status_of(&imp);
        self.heading(format!(
            "{} {}: {}",
            imp.widget,
            imp.target,
            rule_label(&imp.rule)
        ));
        self.line(format!("id: {}", imp.id));
        self.line(format!("規則: {}", imp.rule));
        self.line(format!("状態: {}", status.label()));
        if let Some(at) = imp.resolved {
            self.line(format!("解消: {}", self.stamp(at)));
        }
        self.section("根拠");
        for e in &imp.evidence {
            self.line(format!("{}:{}", e.path, e.line));
        }
        let subject = Subject::Improvement(imp.id.clone());
        self.links(&subject);
        let actions = self
            .tree
            .insert(Some(self.ids.info), Container::new(row(6.0)));
        self.button(
            actions,
            if self.brief_open {
                "作業指示を隠す"
            } else {
                "作業指示を表示"
            },
            Press::ToggleBrief,
        );
        match &status {
            ImprovementStatus::Open => self.button(actions, "タスクにする (T)", Press::HandOff),
            ImprovementStatus::HandedOff(task) => {
                self.line(format!(
                    "タスク {task} に引き渡し済みです。View → Tasks で作業を続けられます。"
                ));
            }
            ImprovementStatus::Resolved => {
                self.line("この候補は解消しています。関連する作業と検証の記録を確認できます。");
            }
        }
        let actions = self
            .tree
            .insert(Some(self.ids.info), Container::new(row(6.0)));
        self.button(
            actions,
            "検証を関連付け (L)",
            Press::OpenLink(LinkKind::Verify),
        );
        self.button(
            actions,
            "レビューを関連付け",
            Press::OpenLink(LinkKind::Review),
        );
        if self.brief_open {
            let brief = scaffold::handoff::task_text(&imp, &backend::related(&self.state, &imp.id));
            self.tree.insert(
                Some(self.ids.info),
                Label::new(brief)
                    .font_size(11.0)
                    .wrap(true)
                    .selectable(true),
            );
        }
    }

    fn set_notice(&mut self, notice: Result<String, String>) {
        self.notice = Some(match notice {
            Ok(n) => (n, false),
            Err(e) => (e, true),
        });
    }

    fn act(&mut self, action: Action, text: &mut TextStore) -> bool {
        let result = self.backend.act(action, &self.state);
        let ok = result.is_ok();
        self.set_notice(result);
        if ok {
            let state = self.backend.scan();
            self.set_state(state, text);
        }
        self.rebuild(text);
        ok
    }

    /// Hands candidate `id` off as a task, once: a candidate already handed
    /// off shows its task instead of getting another.
    fn hand_off(&mut self, id: String, text: &mut TextStore) {
        self.detail_open = true;
        let status = self.improvement(&id).map(|i| self.status_of(i));
        if status == Some(ImprovementStatus::Resolved) {
            self.set_notice(Ok(
                "この候補は解消済みです。作業と検証の記録を確認できます。".into(),
            ));
            self.rebuild(text);
            return;
        }
        if let Some(ImprovementStatus::HandedOff(task)) = status {
            self.set_notice(Err(format!(
                "引き渡し済みです（タスク {task}）。新しいタスクは作りません"
            )));
            self.rebuild(text);
            return;
        }
        self.act(Action::HandOff { improvement: id }, text);
    }

    fn set_tab(&mut self, tab: Tab, text: &mut TextStore) {
        if tab != self.tab {
            self.tab = tab;
            self.brief_open = false;
            self.close_form(text);
            self.setup_preview = None;
        }
        self.reveal = true;
        self.rebuild(text);
    }

    fn select(&mut self, item: Item, text: &mut TextStore) {
        if self.selection().as_ref() != Some(&item) {
            self.brief_open = false;
            self.close_form(text);
            self.tree
                .update::<ScrollView, _>(self.ids.detail, |s, cx| s.set_offset(cx, 0.0));
        }
        self.selected[self.tab.index()] = Some(item);
        self.rebuild(text);
    }

    fn step(&mut self, by: isize, text: &mut TextStore) {
        let items = self.items(self.tab);
        if items.is_empty() {
            return;
        }
        let at = self
            .selection()
            .and_then(|s| items.iter().position(|i| *i == s))
            .unwrap_or(0) as isize;
        let next = (at + by).clamp(0, items.len() as isize - 1) as usize;
        self.reveal = true;
        self.select(items[next].clone(), text);
    }

    /// Scrolls the list the least that shows the selected row; whether it
    /// moved. Needs the rows laid out.
    fn reveal_selection(&mut self) -> bool {
        let Some(selection) = self.selection() else {
            return false;
        };
        let Some(&(row, _)) = self.rows.iter().find(|(_, item)| *item == selection) else {
            return false;
        };
        let list = self.ids.list;
        let tree = &mut self.tree;
        let (Some(frame), Some(origin), Some(layout), Some(offset)) = (
            tree.parent(row).and_then(|f| tree.window_rect(f)),
            tree.window_origin(list),
            tree.layout(list),
            tree.get::<ScrollView>(list)
                .map(ScrollView::scroll_position),
        ) else {
            return false;
        };
        // In the coordinates of the unscrolled rows, as `UiTree` reveals a
        // focused widget.
        let rect = Rect::new(
            frame.x - origin.x + offset.x,
            frame.y - origin.y + offset.y,
            frame.width,
            frame.height,
        );
        let size = Size::new(layout.rect.width, layout.rect.height);
        tree.update::<ScrollView, _>(list, |s, cx| s.reveal(cx, size, layout.content_size, rect))
            .unwrap_or(false)
    }

    fn open_form(&mut self, kind: FormKind, text: &mut TextStore) {
        self.close_form(text);
        self.detail_open = true;
        self.rebuild(text);
        let mut form = Form::new(kind);
        form.insert(&mut self.tree, self.ids.form_host);
        if let Some(field) = form.first_field() {
            self.tree.set_focus(Some(field));
        }
        self.form = Some(form);
    }

    fn close_form(&mut self, text: &mut TextStore) {
        if self.form.take().is_some() {
            clear(&mut self.tree, self.ids.form_host, text);
            self.tree.set_focus(None);
        }
    }

    /// The selected candidate, if the candidates tab is shown.
    fn selected_candidate(&self) -> Option<String> {
        match (self.tab, self.selection()) {
            (Tab::Candidates, Some(Item::Candidate(id))) => Some(id),
            _ => None,
        }
    }

    fn do_press(&mut self, press: Press, text: &mut TextStore) {
        if self.setup_press(&press, text) {
            return;
        }
        match &press {
            Press::GoTab(tab) => {
                self.clear_filters(text);
                self.set_tab(*tab, text);
                return;
            }
            Press::ClearFilters => {
                self.clear_filters(text);
                return;
            }
            Press::CloseDetail => {
                self.detail_open = false;
                self.brief_open = false;
                self.close_form(text);
                self.rebuild(text);
                return;
            }
            _ => {}
        }
        let Some(item) = self.selection() else {
            return;
        };
        match press {
            Press::RegisterPreview
            | Press::RegisterAssets
            | Press::SetupTaskPreview
            | Press::SetupTask
            | Press::ClosePreview
            | Press::GoTab(_)
            | Press::ClearFilters
            | Press::CloseDetail => {}
            Press::ToggleBrief => {
                self.brief_open = !self.brief_open;
                self.rebuild(text);
            }
            Press::HandOff => {
                if let Item::Candidate(id) = item {
                    self.hand_off(id, text);
                }
            }
            Press::OpenLink(kind) => {
                if let Some(subject) = item.subject() {
                    self.open_form(FormKind::Link { subject, kind }, text);
                }
            }
            Press::Unlink(kind, id) => {
                if let Some(subject) = item.subject() {
                    self.act(Action::Unlink { subject, kind, id }, text);
                }
            }
            Press::OpenSkip => {
                if let Some(subject) = item.subject() {
                    self.open_form(FormKind::Skip { subject }, text);
                }
            }
            Press::OpenDecide => {
                let Item::Asset(id) = item else { return };
                let Some(asset) = self.asset(&id) else { return };
                let choices = asset.lifecycle().next().to_vec();
                if choices.is_empty() {
                    self.set_notice(Err("廃止された資産には決定を記録できません".into()));
                    self.rebuild(text);
                    return;
                }
                let links = self
                    .state
                    .links(&Subject::Asset(id.clone()))
                    .into_iter()
                    .map(|(k, r, _)| (k, r))
                    .collect();
                self.open_form(
                    FormKind::Decide {
                        asset: id,
                        choices,
                        links,
                    },
                    text,
                );
            }
        }
    }

    /// A shortcut key; whether it was one.
    pub fn press(&mut self, key: &Key, text: &mut TextStore) -> bool {
        if self.setup_preview.is_some()
            && matches!(
                key,
                Key::Enter | Key::ArrowDown | Key::ArrowUp | Key::Char('j' | 'k')
            )
        {
            return false;
        }
        match key {
            Key::Char('0') => self.set_tab(Tab::Setup, text),
            Key::Char('/') => self.focus_search(),
            Key::Char('1') => self.set_tab(Tab::Assets, text),
            Key::Char('2') => self.set_tab(Tab::Candidates, text),
            Key::Char('3') => self.set_tab(Tab::Sources, text),
            Key::Char('j') | Key::ArrowDown => self.step(1, text),
            Key::Char('k') | Key::ArrowUp => self.step(-1, text),
            Key::Enter => {
                if self.selection().is_none() {
                    return false;
                }
                self.detail_open = true;
                self.rebuild(text);
            }
            Key::Char('r') => {
                self.notice = None;
                self.reveal = true;
                self.rescan(text);
            }
            Key::Char('t') => {
                // `KeyDown` does not say whether the key repeats: a held
                // `t` is kept from making more tasks by `hand_off`.
                let Some(id) = self.selected_candidate() else {
                    return false;
                };
                self.hand_off(id, text);
            }
            Key::Char('l') => {
                let Some(subject) = self.selection().and_then(|i| i.subject()) else {
                    return false;
                };
                self.open_form(
                    FormKind::Link {
                        subject,
                        kind: LinkKind::Verify,
                    },
                    text,
                );
            }
            _ => return false,
        }
        true
    }

    /// Escape: the form, then the detail; whether it closed one.
    fn escape(&mut self, text: &mut TextStore) -> bool {
        if self.form.is_some() {
            self.close_form(text);
            return true;
        }
        if self.tree.focused() == Some(self.ids.search) {
            if !self.query.is_empty() {
                self.query.clear();
                self.tree
                    .update::<TextField, _>(self.ids.search, |f, cx| f.set_text(cx, ""));
                self.filters_changed(text);
            } else {
                self.tree.set_focus(None);
            }
            return true;
        }
        if self.setup_preview.take().is_some() {
            self.rebuild(text);
            return true;
        }
        if self.detail_open {
            self.detail_open = false;
            self.brief_open = false;
            self.rebuild(text);
            return true;
        }
        false
    }

    /// Whether keys go to a text input: one has focus, or a form is open.
    fn typing(&self) -> bool {
        self.form.is_some()
            || self.tree.focused().is_some_and(|id| {
                self.tree.get::<TextField>(id).is_some() || self.tree.get::<TextArea>(id).is_some()
            })
    }

    /// Wakes on the tick: rescans once 30 s have passed (live only).
    pub fn tick(&mut self, text: &mut TextStore) {
        if self.backend.is_demo() {
            return;
        }
        if self.backend.now() - self.state.scanned_at >= RESCAN_MS - 1_000 {
            self.rescan(text);
        }
    }
}

fn verify_tone(v: VerifyState) -> Tone {
    match v {
        VerifyState::Verified => Tone::Success,
        VerifyState::Failed => Tone::Critical,
        VerifyState::Stale | VerifyState::Unknown | VerifyState::NotRunnable => Tone::Warning,
        VerifyState::NotRun | VerifyState::Skipped => Tone::Neutral,
    }
}

fn status_tone(s: &ImprovementStatus) -> Tone {
    match s {
        ImprovementStatus::Open => Tone::Warning,
        ImprovementStatus::HandedOff(_) => Tone::Accent,
        ImprovementStatus::Resolved => Tone::Success,
    }
}

fn clear(tree: &mut UiTree, parent: WidgetId, text: &mut TextStore) {
    for child in tree.children(parent).to_vec() {
        tree.remove(child, text);
    }
}

fn shortcut_modifiers(key: &Key, m: &Modifiers) -> bool {
    let shifted_symbol = m.shift && matches!(key, Key::Char(c) if !c.is_alphabetic());
    !(m.ctrl || m.alt || m.meta || m.command || shifted_symbol)
}

impl Screen for Scaffold {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn woken(&mut self, text: &mut TextStore) {
        self.tick(text);
    }

    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        let changed = self.tree.prepare(viewport, text).is_some();
        if std::mem::take(&mut self.reveal) && self.reveal_selection() {
            self.tree.prepare(viewport, text);
            return true;
        }
        changed
    }

    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        let swallow = self.swallow.take();
        match &event {
            Event::Text(typed)
                if swallow.is_some_and(|c| typed.to_lowercase() == c.to_string()) =>
            {
                return;
            }
            Event::KeyDown {
                key: Key::Escape, ..
            } if self.escape(text) => return,
            Event::KeyDown {
                key: Key::Char('f' | 'F'),
                modifiers,
            } if (modifiers.ctrl || modifiers.meta || modifiers.command) && !modifiers.alt => {
                self.focus_search();
                return;
            }
            Event::KeyDown { key, modifiers }
                if !shortcut_modifiers(key, modifiers) || self.typing() => {}
            Event::KeyDown { key, .. } => {
                let key = match key {
                    Key::Char(c) => Key::Char(c.to_ascii_lowercase()),
                    other => *other,
                };
                if self.press(&key, text) {
                    if let Key::Char(c) = key {
                        self.swallow = Some(c);
                    }
                    return;
                }
            }
            _ => {}
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if let Some(form) = &self.form {
            match form.handle(&self.tree, &message) {
                Outcome::Pending => {}
                Outcome::Cancel => {
                    self.close_form(text);
                    return;
                }
                Outcome::Invalid(why) => {
                    self.set_notice(Err(why));
                    self.update_header();
                    return;
                }
                Outcome::Submit(action) => {
                    if self.act(action, text) {
                        self.close_form(text);
                    }
                    return;
                }
            }
        }
        let source = message.source;
        if let Some(TextChanged(typed)) = message.downcast_ref::<TextChanged>() {
            if source == self.ids.search {
                self.query = typed.clone();
                self.filters_changed(text);
            }
            return;
        }
        if message.downcast_ref::<TextSubmitted>().is_some() && source == self.ids.search {
            self.tree.set_focus(None);
            self.reveal = true;
            return;
        }
        if let Some(SelectionChanged(i)) = message.downcast_ref::<SelectionChanged>() {
            if source == self.ids.tabs
                && let Some(tab) = Tab::ALL.get(*i)
            {
                self.set_tab(*tab, text);
            } else if source == self.ids.filter {
                self.attention_only = *i == 1;
                self.filters_changed(text);
            }
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        if source == self.ids.rescan {
            self.notice = None;
            self.reveal = true;
            self.rescan(text);
        } else if let Some((_, item)) = self.rows.iter().find(|(id, _)| *id == source).cloned() {
            self.detail_open = true;
            self.select(item, text);
        } else if let Some((_, press)) = self.presses.iter().find(|(id, _)| *id == source).cloned()
        {
            self.do_press(press, text);
        }
    }
}

#[cfg(test)]
mod tests;
