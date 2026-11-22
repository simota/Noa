use std::collections::HashMap;
use std::path::PathBuf;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Overflow, Rect as LayoutRect, Size as LayoutSize, auto, length,
    percent,
};
use noa_gpui_core::widgets::{
    Badge, BrowserPathChanged, BrowserView, Button, ButtonClicked, COMMAND_PALETTE_SHORTCUTS,
    CommandChosen, CommandPalette, Container, ControlSize, EmptyState, FIND_SHORTCUT, FileDropZone,
    FilesDropped, FindBar, Kbd, Label, LogLine, LogView, OutlineItem, PaletteCommand, PathControl,
    PathItemClicked, RecentCommands, ScrollView, SegmentMode, SegmentedControl, SelectionChanged,
    SplitPane, SplitView, StatusBar, TabView, Terminal, TextArea, Toast, TokenField, TokensChanged,
    Tone, Truncation, drive_find,
};
use noa_gpui_core::{Message, Role, Shortcut, ShortcutPressed, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Color, Size};
use noa_gpui_text::TextStore;

use crate::live::{Channel, Command, Link, Snapshot};
use crate::model::{AgentActivity, ChangedFile, DiffMode, Repo, RepoData, Worktree, demo_data};

const DIFF_ADD_FG: Color = Color::rgba(0.45, 0.90, 0.56, 1.0);
const DIFF_ADD_BG: Color = Color::rgba(0.14, 0.36, 0.20, 0.42);
const DIFF_DEL_FG: Color = Color::rgba(0.98, 0.52, 0.52, 1.0);
const DIFF_DEL_BG: Color = Color::rgba(0.42, 0.14, 0.16, 0.42);
const DIFF_HUNK_FG: Color = Color::rgba(0.46, 0.80, 0.98, 1.0);
const DIFF_HUNK_BG: Color = Color::rgba(0.14, 0.25, 0.38, 0.40);
const DIFF_META_FG: Color = Color::rgba(0.84, 0.76, 0.46, 1.0);

#[derive(Debug, Clone)]
enum PaletteAction {
    /// Resolved when chosen: the palette stays open across refreshes.
    SelectFile {
        root: String,
        cwd: String,
        path: String,
    },
    FocusPane(u64),
    SwitchDiffMode(DiffMode),
    ToggleInspector,
    Refresh,
}

#[derive(Debug, Clone)]
enum NodeKind {
    Repo(usize),
    Worktree(usize, usize),
    File(usize, usize, usize),
}

enum Source {
    Demo,
    /// A one-off render of a live snapshot (`--snapshot` without `--demo`).
    Snapshot,
    Live(Channel),
}

pub struct RepoBrowser {
    tree: UiTree,
    source: Source,
    link: Link,
    can_focus: bool,
    can_send: bool,
    diff_mode: DiffMode,
    raw_data: RepoData,
    filtered: RepoData,
    tokens: Vec<String>,
    diff_cache: HashMap<(String, String), String>,
    /// The file the live feed was last asked to keep refreshing.
    requested_diff: Option<(String, String)>,
    /// The diff body the log view holds, so an unchanged refresh keeps its
    /// scroll position and line selection.
    shown_diff: Option<String>,

    // Current selection into `filtered`.
    sel_repo: usize,
    sel_wt: usize,
    sel_file: usize,

    // Map from BrowserView flat item index -> NodeKind.
    node_map: Vec<NodeKind>,

    // Key widget IDs for incremental updates and event routing.
    path_bar_host: WidgetId,
    path_control: WidgetId,
    token_field: WidgetId,
    diff_mode_seg: WidgetId,
    palette_btn: WidgetId,
    refresh_btn: WidgetId,
    focus_btn: WidgetId,
    toggle_side_btn: WidgetId,
    outer_split: WidgetId,
    browser_host: WidgetId,
    browser: WidgetId,
    find_bar: WidgetId,
    diff_header: WidgetId,
    diff_log: WidgetId,
    drop_zone: WidgetId,
    side_tabs: WidgetId,
    terminal: WidgetId,
    quote_btn: WidgetId,
    prompt_area: WidgetId,
    send_btn: WidgetId,
    status_leading: WidgetId,
    status_trailing: WidgetId,

    // Active CommandPalette overlay state.
    active_palette: Option<(WidgetId, Vec<PaletteAction>)>,
    recent_commands: RecentCommands,
    last_terminal_key: Option<(u64, String)>,
    pending_browser_path: Option<Vec<usize>>,
    /// Every toast shown, for tests to inspect.
    #[cfg(test)]
    toasts: Vec<String>,
}

impl RepoBrowser {
    pub fn demo() -> Self {
        let data = demo_data(DiffMode::Unstaged);
        Self::build(Source::Demo, Link::Live, true, true, data)
    }

    pub fn live(channel: Channel) -> Self {
        Self::build(
            Source::Live(channel),
            Link::Connecting,
            false,
            false,
            RepoData::default(),
        )
    }

    pub fn of_snapshot(snapshot: Snapshot) -> Self {
        let mut screen = Self::build(
            Source::Snapshot,
            snapshot.link,
            snapshot.can_focus,
            snapshot.can_send,
            snapshot.data,
        );
        screen.diff_mode = snapshot.diff_mode;
        if let Some((cwd, path, diff)) = snapshot.selected_diff {
            screen.diff_cache.insert((cwd, path), diff);
        }
        let mut text = TextStore::new(Default::default());
        screen.sync_views(&mut text);
        screen
    }

    fn build(
        source: Source,
        link: Link,
        can_focus: bool,
        can_send: bool,
        raw_data: RepoData,
    ) -> Self {
        let mut tree = UiTree::new();
        tree.set_theme(Theme::dark());
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

        // ── Top toolbar ───────────────────────────────────────────────────────
        let toolbar = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::CENTER,
                flex_shrink: 0.0,
                gap: LayoutSize {
                    width: length(8.0),
                    height: length(0.0),
                },
                padding: LayoutRect {
                    left: length(10.0),
                    right: length(10.0),
                    top: length(6.0),
                    bottom: length(6.0),
                },
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .background(Role::SurfaceAlt),
        );

        let path_bar_host = tree.insert(
            Some(toolbar),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::CENTER,
                flex_grow: 1.0,
                min_size: LayoutSize {
                    width: length(180.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        let path_control = tree.insert(Some(path_bar_host), PathControl::new(["リポジトリ未選択"]));

        let token_field = tree.insert(
            Some(toolbar),
            TokenField::new("フィルタ (dirty, conflict, agent:waiting…)", 280.0).completions([
                "dirty",
                "conflict",
                "agent:running",
                "agent:waiting",
                "agent:done",
            ]),
        );

        let diff_mode_seg = tree.insert(
            Some(toolbar),
            SegmentedControl::new(
                DiffMode::ALL.iter().map(|m| m.label()),
                SegmentMode::SelectOne,
            )
            .control_size(ControlSize::Small),
        );

        let palette_btn = tree.insert(
            Some(toolbar),
            Button::new("⌘K 検索").control_size(ControlSize::Small),
        );
        let refresh_btn = tree.insert(
            Some(toolbar),
            Button::new("更新").control_size(ControlSize::Small),
        );
        let focus_btn = tree.insert(
            Some(toolbar),
            Button::new("ペインを開く")
                .default_button(true)
                .control_size(ControlSize::Small),
        );
        let toggle_side_btn = tree.insert(
            Some(toolbar),
            Button::new("右パネル").control_size(ControlSize::Small),
        );

        // ── Main body: horizontal SplitView (Explorer+Diff | Live Terminal+Composer) ──
        let body = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(0.0),
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );

        let outer_split = tree.insert(
            Some(body),
            SplitView::horizontal()
                .pane(SplitPane::flexible().min(520.0))
                .pane(SplitPane::fixed(400.0).min(260.0).rail(36.0)),
        );

        // Left pane: vertical SplitView (BrowserView | diff view)
        let left_split = tree.insert(
            Some(outer_split),
            SplitView::vertical()
                .pane(SplitPane::fixed(250.0).min(140.0))
                .pane(SplitPane::flexible().min(200.0)),
        );

        let browser_host = tree.insert(
            Some(left_split),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            })
            .background(Role::Surface),
        );
        let browser = tree.insert(
            Some(browser_host),
            BrowserView::new(Vec::new()).height(240.0),
        );

        // Bottom-left: diff view column
        let diff_col = tree.insert(
            Some(left_split),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                gap: LayoutSize {
                    width: length(0.0),
                    height: length(4.0),
                },
                padding: LayoutRect::length(6.0),
                ..Default::default()
            })
            .background(Role::Background),
        );

        let diff_header = tree.insert(
            Some(diff_col),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::CENTER,
                flex_shrink: 0.0,
                gap: LayoutSize {
                    width: length(8.0),
                    height: length(0.0),
                },
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );

        let find_bar = tree.insert(Some(diff_col), FindBar::new());
        tree.set_hidden(find_bar, true);

        let diff_log = tree.insert(
            Some(diff_col),
            LogView::new()
                .font_size(12.5)
                .follows_tail(false)
                .fill_height(),
        );

        let drop_zone = tree.insert(
            Some(diff_col),
            FileDropZone::new()
                .title("ここにファイルをドロップして選択ペインへパスを送信")
                .message("外部スクショやログの共有に使えます")
                .height(76.0),
        );

        // Right pane: TabView (Live Terminal | Review Composer)
        let right_wrap = tree.insert(
            Some(outer_split),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                padding: LayoutRect::length(6.0),
                ..Default::default()
            })
            .background(Role::SurfaceAlt),
        );

        let side_tabs = tree.insert(
            Some(right_wrap),
            TabView::new(["ライブ端末", "レビュー指示"]),
        );

        // Tab 0: Live Terminal
        let term_page = tree.insert(
            Some(side_tabs),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                gap: LayoutSize {
                    width: length(0.0),
                    height: length(6.0),
                },
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            }),
        );
        let terminal = tree.insert(Some(term_page), Terminal::new().font_size(12.0).rows(36));

        // Tab 1: Review Composer
        let composer_page = tree.insert(
            Some(side_tabs),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                gap: LayoutSize {
                    width: length(0.0),
                    height: length(8.0),
                },
                padding: LayoutRect::length(6.0),
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            }),
        );
        tree.insert(
            Some(composer_page),
            Label::new(
                "選択中のファイル・行範囲を引用し、対象ペインのエージェントへ修正指示を送ります。",
            )
            .font_size(12.0)
            .color(Role::Muted)
            .wrap(true),
        );
        let composer_actions = tree.insert(
            Some(composer_page),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                gap: LayoutSize {
                    width: length(6.0),
                    height: length(0.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            }),
        );
        let quote_btn = tree.insert(
            Some(composer_actions),
            Button::new("選択行を引用").control_size(ControlSize::Small),
        );
        let send_btn = tree.insert(
            Some(composer_actions),
            Button::new("ペインへ送信")
                .default_button(true)
                .control_size(ControlSize::Small),
        );
        let prompt_scroll = tree.insert(
            Some(composer_page),
            ScrollView::new(Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(0.0),
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                overflow: noa_gpui_core::style::Point {
                    x: Overflow::Hidden,
                    y: Overflow::Scroll,
                },
                ..Default::default()
            }),
        );
        let prompt_area = tree.insert(
            Some(prompt_scroll),
            TextArea::new("例: このハンクのエラー処理を Result に統一してテストを追加してください")
                .min_lines(8),
        );

        // ── Bottom StatusBar ──────────────────────────────────────────────────
        let status_parts = StatusBar::insert(&mut tree, root);

        // Register global shortcuts on tree.
        for sc in COMMAND_PALETTE_SHORTCUTS {
            tree.add_shortcut(sc, palette_btn);
        }
        tree.add_shortcut(FIND_SHORTCUT, find_bar);
        tree.add_shortcut(Shortcut::command('r'), refresh_btn);
        tree.add_shortcut(Shortcut::command('b'), toggle_side_btn);

        let filtered = raw_data.clone();
        let mut screen = Self {
            tree,
            source,
            link,
            can_focus,
            can_send,
            diff_mode: DiffMode::Unstaged,
            raw_data,
            filtered,
            tokens: Vec::new(),
            diff_cache: HashMap::new(),
            requested_diff: None,
            shown_diff: None,
            sel_repo: 0,
            sel_wt: 0,
            sel_file: 0,
            node_map: Vec::new(),
            path_bar_host,
            path_control,
            token_field,
            diff_mode_seg,
            palette_btn,
            refresh_btn,
            focus_btn,
            toggle_side_btn,
            outer_split,
            browser_host,
            browser,
            find_bar,
            diff_header,
            diff_log,
            drop_zone,
            side_tabs,
            terminal,
            quote_btn,
            prompt_area,
            send_btn,
            status_leading: status_parts.leading,
            status_trailing: status_parts.trailing,
            active_palette: None,
            recent_commands: RecentCommands::default(),
            last_terminal_key: None,
            pending_browser_path: None,
            #[cfg(test)]
            toasts: Vec::new(),
        };
        let mut text = TextStore::new(Default::default());
        screen.sync_views(&mut text);
        screen
    }

    fn current_repo(&self) -> Option<&Repo> {
        self.filtered.repos.get(self.sel_repo)
    }

    fn current_worktree(&self) -> Option<&Worktree> {
        self.current_repo()?.worktrees.get(self.sel_wt)
    }

    fn current_file(&self) -> Option<&ChangedFile> {
        self.current_worktree()?.files.get(self.sel_file)
    }

    fn clamp_selection(&mut self) {
        if self.filtered.repos.is_empty() {
            self.sel_repo = 0;
            self.sel_wt = 0;
            self.sel_file = 0;
            return;
        }
        self.sel_repo = self.sel_repo.min(self.filtered.repos.len() - 1);
        let wts = &self.filtered.repos[self.sel_repo].worktrees;
        if wts.is_empty() {
            self.sel_wt = 0;
            self.sel_file = 0;
            return;
        }
        self.sel_wt = self.sel_wt.min(wts.len() - 1);
        let files = &wts[self.sel_wt].files;
        if files.is_empty() {
            self.sel_file = 0;
        } else {
            self.sel_file = self.sel_file.min(files.len() - 1);
        }
    }

    /// (repo root, worktree cwd, file path) of the current selection.
    fn selection_key(&self) -> (Option<String>, Option<String>, Option<String>) {
        (
            self.current_repo().map(|r| r.root.clone()),
            self.current_worktree().map(|w| w.cwd.clone()),
            self.current_file().map(|f| f.path.clone()),
        )
    }

    /// Points the selection indices back at the same repo, worktree and file
    /// after `filtered` was rebuilt; whatever is gone keeps its old index.
    fn restore_selection(&mut self, key: (Option<String>, Option<String>, Option<String>)) {
        let (root, cwd, path) = key;
        let Some(ri) =
            root.and_then(|root| self.filtered.repos.iter().position(|r| r.root == root))
        else {
            return;
        };
        self.sel_repo = ri;
        let worktrees = &self.filtered.repos[ri].worktrees;
        let Some(wi) = cwd.and_then(|cwd| worktrees.iter().position(|w| w.cwd == cwd)) else {
            return;
        };
        self.sel_wt = wi;
        if let Some(fi) =
            path.and_then(|path| worktrees[wi].files.iter().position(|f| f.path == path))
        {
            self.sel_file = fi;
        }
    }

    fn sync_views(&mut self, text: &mut TextStore) {
        let key = self.selection_key();
        self.filtered = self.raw_data.filtered(&self.tokens);
        self.restore_selection(key);
        self.clamp_selection();
        self.rebuild_browser(text);
        self.rebuild_path_control(text);
        self.rebuild_diff_view(text);
        self.update_terminal();
        self.rebuild_status_bar(text);
    }

    fn rebuild_browser(&mut self, text: &mut TextStore) {
        self.clear_children(self.browser_host, text);
        self.node_map.clear();

        if self.filtered.repos.is_empty() {
            let msg = match &self.link {
                Link::Connecting => "Noa サーバーに接続しています…",
                Link::Down(why) => why.as_str(),
                Link::Live => "条件に一致するリポジトリ／worktree がありません",
            };
            EmptyState::new("表示できるリポジトリがありません")
                .message(msg)
                .insert(&mut self.tree, self.browser_host);
            self.browser = self.browser_host;
            return;
        }

        let mut items = Vec::new();
        let mut selected_path = Vec::new();
        let mut next_id = 0usize;

        for (ri, repo) in self.filtered.repos.iter().enumerate() {
            let repo_id = next_id;
            next_id += 1;
            self.node_map.push(NodeKind::Repo(ri));
            let conflict_tag = if repo.conflict_count() > 0 {
                format!(" ⚠{}", repo.conflict_count())
            } else {
                String::new()
            };
            let mut repo_item = OutlineItem::new(format!(
                "{} ({} files){conflict_tag}",
                repo.name,
                repo.total_files()
            ));

            for (wi, wt) in repo.worktrees.iter().enumerate() {
                let wt_id = next_id;
                next_id += 1;
                self.node_map.push(NodeKind::Worktree(ri, wi));
                let act = wt.activity();
                let act_str = if act == AgentActivity::None {
                    String::new()
                } else {
                    format!(" [{}]", act.badge())
                };
                let conf = if wt.has_conflict() { " ⚠衝突" } else { "" };
                let ab = if wt.ahead > 0 || wt.behind > 0 {
                    format!(" ↑{}↓{}", wt.ahead, wt.behind)
                } else {
                    String::new()
                };
                let mut wt_item = OutlineItem::new(format!("{}{ab}{act_str}{conf}", wt.branch));

                for (fi, file) in wt.files.iter().enumerate() {
                    let file_id = next_id;
                    next_id += 1;
                    self.node_map.push(NodeKind::File(ri, wi, fi));
                    let c = if file.conflict_worktrees.is_empty() {
                        ""
                    } else {
                        " ⚠"
                    };
                    let title = format!(
                        "{} {} (+{}/-{}){c}",
                        file.status, file.path, file.added, file.deleted
                    );
                    wt_item = wt_item.child(OutlineItem::new(title));
                    if ri == self.sel_repo && wi == self.sel_wt && fi == self.sel_file {
                        selected_path = vec![repo_id, wt_id, file_id];
                    }
                }
                if ri == self.sel_repo && wi == self.sel_wt && selected_path.is_empty() {
                    selected_path = vec![repo_id, wt_id];
                }
                repo_item = repo_item.child(wt_item);
            }
            if ri == self.sel_repo && selected_path.is_empty() {
                selected_path = vec![repo_id];
            }
            items.push(repo_item);
        }

        self.browser = self.tree.insert(
            Some(self.browser_host),
            BrowserView::new(items).column_width(310.0).height(240.0),
        );
        self.pending_browser_path = (!selected_path.is_empty()).then_some(selected_path);
    }

    fn rebuild_path_control(&mut self, text: &mut TextStore) {
        self.clear_children(self.path_bar_host, text);
        let mut crumbs = Vec::new();
        if let Some(repo) = self.current_repo() {
            crumbs.push(repo.name.clone());
            if let Some(wt) = self.current_worktree() {
                crumbs.push(wt.branch.clone());
                if let Some(file) = self.current_file() {
                    crumbs.push(file.path.clone());
                }
            }
        } else {
            crumbs.push("リポジトリ未選択".into());
        }
        self.path_control = self
            .tree
            .insert(Some(self.path_bar_host), PathControl::new(crumbs));
    }

    /// The selected file's diff body: its inline diff, else the live cache.
    fn current_diff_text(&self) -> Option<String> {
        let wt = self.current_worktree()?;
        let file = self.current_file()?;
        if !file.diff.is_empty() {
            return Some(file.diff.clone());
        }
        self.diff_cache
            .get(&(wt.cwd.clone(), file.path.clone()))
            .cloned()
    }

    fn rebuild_diff_view(&mut self, text: &mut TextStore) {
        self.clear_children(self.diff_header, text);

        // The live feed refreshes only the last requested file, so re-request
        // on every selection change, cached or not.
        let wanted = match (self.current_worktree(), self.current_file()) {
            (Some(wt), Some(file)) if file.diff.is_empty() => {
                Some((wt.cwd.clone(), file.path.clone()))
            }
            _ => None,
        };
        if let (Source::Live(ch), Some(key)) = (&self.source, wanted)
            && self.requested_diff.as_ref() != Some(&key)
        {
            ch.send(Command::LoadDiff {
                cwd: key.0.clone(),
                path: key.1.clone(),
            });
            self.requested_diff = Some(key);
        }

        let (header_text, conflict_note, diff_text) =
            match (self.current_worktree(), self.current_file()) {
                (Some(_), Some(file)) => {
                    let text = self
                        .current_diff_text()
                        .unwrap_or_else(|| "差分を取得しています…".into());
                    let note = if file.conflict_worktrees.is_empty() {
                        None
                    } else {
                        Some(format!(
                            "競合注意: {} でも編集中",
                            file.conflict_worktrees.join(", ")
                        ))
                    };
                    (
                        format!(
                            "{}  ·  {}  (+{} / -{})",
                            file.path,
                            self.diff_mode.label(),
                            file.added,
                            file.deleted
                        ),
                        note,
                        text,
                    )
                }
                (Some(wt), None) => (
                    format!("{} ({}) — 変更ファイルなし", wt.branch, wt.cwd),
                    None,
                    wt.note
                        .clone()
                        .unwrap_or_else(|| "(このワークツリーに差分はありません)".into()),
                ),
                _ => (
                    "差分ビュー".into(),
                    None,
                    "(左のカラムブラウザからファイルを選択してください)".into(),
                ),
            };

        self.tree.insert(
            Some(self.diff_header),
            Label::new(header_text)
                .font_size(12.5)
                .weight(600)
                .truncation(Truncation::Tail),
        );
        if let Some(note) = conflict_note {
            self.tree.insert(
                Some(self.diff_header),
                Badge::text(note).tone(Tone::Warning),
            );
        }

        if self.shown_diff.as_deref() != Some(diff_text.as_str()) {
            let lines: Vec<LogLine> = diff_text.lines().map(colorize_diff_line).collect();
            self.tree
                .update::<LogView, _>(self.diff_log, |log, cx| log.set_lines(cx, lines));
            self.shown_diff = Some(diff_text);
        }
    }

    fn update_terminal(&mut self) {
        let next = self.current_worktree().and_then(|wt| {
            wt.primary_pane()
                .map(|p| (p.pane_id, p.terminal_tail.clone()))
        });
        if next == self.last_terminal_key {
            return;
        }
        self.last_terminal_key = next.clone();
        self.tree.update::<Terminal, _>(self.terminal, |term, cx| {
            let _ = term.feed(cx, b"\x1b[2J\x1b[H");
            match &next {
                Some((_, tail)) if !tail.is_empty() => {
                    let _ = term.feed(cx, tail.as_bytes());
                }
                Some((id, _)) => {
                    let msg = format!("Pane #{id} (出力なし)\r\n");
                    let _ = term.feed(cx, msg.as_bytes());
                }
                None => {
                    let _ = term.feed(cx, "(紐づくペインがありません)\r\n".as_bytes());
                }
            }
        });
    }

    fn rebuild_status_bar(&mut self, text: &mut TextStore) {
        self.clear_children(self.status_leading, text);
        self.clear_children(self.status_trailing, text);

        let (badge_label, tone) = match (&self.source, &self.link) {
            (Source::Demo, _) => ("DEMO", Tone::Accent),
            (_, Link::Live) => ("LIVE", Tone::Success),
            (_, Link::Connecting) => ("接続中", Tone::Warning),
            (_, Link::Down(_)) => ("オフライン", Tone::Critical),
        };
        self.tree.insert(
            Some(self.status_leading),
            Badge::text(badge_label).tone(tone),
        );

        let (repos, wts, files, conflicts) = self.filtered.totals();
        self.tree.insert(
            Some(self.status_leading),
            Label::new(format!("{repos} repos · {wts} worktrees · {files} files"))
                .font_size(11.5)
                .color(Role::Muted),
        );
        if conflicts > 0 {
            self.tree.insert(
                Some(self.status_leading),
                Badge::text(format!("⚠ 衝突 {conflicts}")).tone(Tone::Warning),
            );
        }

        for (sc, text) in [
            (Shortcut::command('k'), "検索"),
            (Shortcut::command('f'), "差分検索"),
            (Shortcut::command('b'), "右パネル"),
        ] {
            self.tree
                .insert(Some(self.status_trailing), Kbd::shortcut(sc));
            self.tree.insert(
                Some(self.status_trailing),
                Label::new(text).font_size(11.0).color(Role::Muted),
            );
        }
    }

    fn open_command_palette(&mut self) {
        let mut commands = Vec::new();
        let mut actions = Vec::new();

        for repo in &self.filtered.repos {
            for wt in &repo.worktrees {
                if let Some(pane) = wt.primary_pane() {
                    commands.push(
                        PaletteCommand::new(format!("ペインへ移動: {} ({})", wt.branch, pane.name))
                            .detail(format!("{} · #{}", repo.name, pane.pane_id))
                            .keywords(format!("focus pane {} {}", repo.name, wt.branch)),
                    );
                    actions.push(PaletteAction::FocusPane(pane.pane_id));
                }
                for file in &wt.files {
                    commands.push(
                        PaletteCommand::new(format!("{} {}", file.status, file.path))
                            .detail(format!("{} / {}", repo.name, wt.branch))
                            .keywords(format!("diff file {} {}", repo.name, wt.branch)),
                    );
                    actions.push(PaletteAction::SelectFile {
                        root: repo.root.clone(),
                        cwd: wt.cwd.clone(),
                        path: file.path.clone(),
                    });
                }
            }
        }

        for mode in DiffMode::ALL {
            commands.push(
                PaletteCommand::new(format!("差分モード切替: {}", mode.label()))
                    .keywords("diff mode staged unstaged main"),
            );
            actions.push(PaletteAction::SwitchDiffMode(mode));
        }
        commands.push(
            PaletteCommand::new("右パネルを開閉")
                .shortcut(Shortcut::command('b'))
                .keywords("toggle side inspector terminal"),
        );
        actions.push(PaletteAction::ToggleInspector);
        commands.push(
            PaletteCommand::new("今すぐ再取得")
                .shortcut(Shortcut::command('r'))
                .keywords("refresh reload git"),
        );
        actions.push(PaletteAction::Refresh);

        let palette_id = CommandPalette::new(commands)
            .recent(&self.recent_commands)
            .placeholder("ファイル・ペイン・アクションを検索…")
            .open(&mut self.tree);
        self.active_palette = Some((palette_id, actions));
    }

    fn toggle_inspector(&mut self) {
        let collapsed = self
            .tree
            .get::<SplitView>(self.outer_split)
            .is_some_and(|s| s.is_collapsed(1));
        self.tree.update::<SplitView, _>(self.outer_split, |s, cx| {
            s.set_collapsed(cx, 1, !collapsed);
        });
    }

    fn open_find_bar(&mut self) {
        self.tree.set_hidden(self.find_bar, false);
        self.tree.set_focus(Some(self.find_bar));
    }

    fn focus_selected_pane(&mut self) {
        let Some(pane) = self.current_worktree().and_then(Worktree::primary_pane) else {
            self.toast("フォーカス対象のペインがありません");
            return;
        };
        let pane_id = pane.pane_id;
        let pane_name = pane.name.clone();
        match &self.source {
            Source::Demo | Source::Snapshot => {
                self.toast(format!("[DEMO] ペイン #{pane_id} ({pane_name}) を開きます"))
            }
            Source::Live(ch) => {
                if !self.can_focus {
                    self.toast("server-scopes に control が必要です");
                    return;
                }
                ch.send(Command::Focus(pane_id));
            }
        }
    }

    fn quote_selected_diff(&mut self) {
        let header = match (self.current_worktree(), self.current_file()) {
            (Some(wt), Some(f)) => format!("[{} / {}]", wt.branch, f.path),
            _ => "[diff]".into(),
        };
        let snippet = self
            .tree
            .get::<LogView>(self.diff_log)
            .and_then(LogView::copy_text)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                self.current_diff_text()
                    .map(|d| d.lines().take(10).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default()
            });
        let quote = format!("{header}\n```diff\n{snippet}\n```\n修正依頼: ");
        self.tree
            .update::<TextArea, _>(self.prompt_area, |area, cx| area.set_text(cx, quote));
        self.tree
            .update::<TabView, _>(self.side_tabs, |tabs, cx| tabs.set_selected(cx, 1));
        self.tree.set_focus(Some(self.prompt_area));
    }

    fn send_review_prompt(&mut self) {
        let text = self
            .tree
            .get::<TextArea>(self.prompt_area)
            .map(|a| a.text().trim().to_owned())
            .unwrap_or_default();
        if text.is_empty() {
            self.toast("送信する指示が空です");
            return;
        }
        let Some(pane) = self.current_worktree().and_then(Worktree::primary_pane) else {
            self.toast("送信先のペインがありません");
            return;
        };
        let pane_id = pane.pane_id;
        match &self.source {
            Source::Demo | Source::Snapshot => {
                self.toast(format!("[DEMO] ペイン #{pane_id} へ指示を送信しました"));
                self.tree
                    .update::<TextArea, _>(self.prompt_area, |area, cx| area.set_text(cx, ""));
            }
            Source::Live(ch) => {
                if !self.can_send {
                    self.toast("server-scopes に input が必要です");
                    return;
                }
                ch.send(Command::SendText {
                    pane_id,
                    text,
                    submit: true,
                });
            }
        }
    }

    fn send_dropped_files(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let joined = paths
            .iter()
            .map(|p| shell_escape(&p.display().to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        let Some(pane) = self.current_worktree().and_then(Worktree::primary_pane) else {
            self.toast(format!("送信先ペインなし: {joined}"));
            return;
        };
        let pane_id = pane.pane_id;
        match &self.source {
            Source::Demo | Source::Snapshot => {
                self.toast(format!("[DEMO] #{pane_id} へ送信: {joined}"))
            }
            Source::Live(ch) => {
                if !self.can_send {
                    self.toast("server-scopes に input が必要です");
                    return;
                }
                // The live feed toasts the outcome once the pane answered.
                ch.send(Command::SendText {
                    pane_id,
                    text: joined.clone(),
                    submit: false,
                });
            }
        }
    }

    fn set_diff_mode(&mut self, mode: DiffMode, text: &mut TextStore) {
        if self.diff_mode == mode {
            return;
        }
        self.diff_mode = mode;
        self.diff_cache.clear();
        let idx = DiffMode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
        self.tree
            .update::<SegmentedControl, _>(self.diff_mode_seg, |seg, cx| {
                seg.set_selected(cx, &[idx])
            });
        match &self.source {
            Source::Demo => {
                self.raw_data = demo_data(mode);
                self.sync_views(text);
            }
            Source::Snapshot => self.rebuild_diff_view(text),
            Source::Live(ch) => {
                ch.send(Command::SetDiffMode(mode));
                self.rebuild_diff_view(text);
            }
        }
    }

    fn toast(&mut self, message: impl Into<String>) {
        let message = message.into();
        #[cfg(test)]
        self.toasts.push(message.clone());
        Toast::new(message).show(&mut self.tree);
    }

    fn clear_children(&mut self, parent: WidgetId, text: &mut TextStore) {
        let children: Vec<WidgetId> = self.tree.children(parent).to_vec();
        for child in children {
            self.tree.remove(child, text);
        }
    }

    fn poll_live(&mut self, text: &mut TextStore) {
        let mut latest = None;
        let mut notices = Vec::new();
        let mut delivered = Vec::new();
        if let Source::Live(ch) = &self.source {
            while let Ok(snap) = ch.snapshots.try_recv() {
                latest = Some(snap);
            }
            while let Ok(msg) = ch.notices.try_recv() {
                notices.push(msg);
            }
            while let Ok(sent) = ch.delivered.try_recv() {
                delivered.push(sent);
            }
        }
        // Clear the prompt only once the pane took it, and only if it was not
        // edited meanwhile; a failed send leaves it for another try.
        for sent in delivered {
            let current = self
                .tree
                .get::<TextArea>(self.prompt_area)
                .map(|a| a.text().trim().to_owned());
            if current.as_deref() == Some(sent.as_str()) {
                self.tree
                    .update::<TextArea, _>(self.prompt_area, |area, cx| area.set_text(cx, ""));
            }
        }
        if let Some(snap) = latest {
            self.link = snap.link;
            self.can_focus = snap.can_focus;
            self.can_send = snap.can_send;
            // The UI is the only source of mode changes; a snapshot in another
            // mode was built before the feed saw our SetDiffMode.
            if snap.diff_mode != self.diff_mode {
                self.rebuild_status_bar(text);
                return self.show_notices(notices);
            }
            self.raw_data = snap.data;
            if let Some((cwd, path, diff)) = snap.selected_diff {
                self.diff_cache.insert((cwd, path), diff);
            }
            self.sync_views(text);
        }
        self.show_notices(notices);
    }

    fn show_notices(&mut self, notices: Vec<String>) {
        for notice in notices {
            self.toast(notice);
        }
    }

    fn run_palette_action(&mut self, action: PaletteAction, text: &mut TextStore) {
        match action {
            PaletteAction::SelectFile { root, cwd, path } => {
                let found = self.filtered.repos.iter().enumerate().find_map(|(ri, r)| {
                    if r.root != root {
                        return None;
                    }
                    let wi = r.worktrees.iter().position(|w| w.cwd == cwd)?;
                    let fi = r.worktrees[wi].files.iter().position(|f| f.path == path)?;
                    Some((ri, wi, fi))
                });
                let Some((ri, wi, fi)) = found else {
                    self.toast(format!("{path} はもう一覧にありません"));
                    return;
                };
                self.sel_repo = ri;
                self.sel_wt = wi;
                self.sel_file = fi;
                self.sync_views(text);
            }
            PaletteAction::FocusPane(id) => match &self.source {
                Source::Demo | Source::Snapshot => {
                    self.toast(format!("[DEMO] ペイン #{id} にフォーカス"))
                }
                Source::Live(ch) => ch.send(Command::Focus(id)),
            },
            PaletteAction::SwitchDiffMode(m) => self.set_diff_mode(m, text),
            PaletteAction::ToggleInspector => self.toggle_inspector(),
            PaletteAction::Refresh => {
                if let Source::Live(ch) = &self.source {
                    ch.send(Command::Refresh);
                }
            }
        }
    }

    fn handle_message(&mut self, message: &Message, text: &mut TextStore) {
        if drive_find(&mut self.tree, self.find_bar, self.diff_log, message) {
            return;
        }

        if let Some(ShortcutPressed(sc)) = message.downcast_ref::<ShortcutPressed>() {
            if COMMAND_PALETTE_SHORTCUTS.contains(sc) {
                self.open_command_palette();
                return;
            }
            if *sc == FIND_SHORTCUT {
                self.open_find_bar();
                return;
            }
            if *sc == Shortcut::command('r') {
                if let Source::Live(ch) = &self.source {
                    ch.send(Command::Refresh);
                }
                self.toast("ワークツリーと差分を再取得しました");
                return;
            }
            if *sc == Shortcut::command('b') {
                self.toggle_inspector();
                return;
            }
        }

        if let Some((palette_id, actions)) = &self.active_palette
            && message.source == *palette_id
            && let Some(CommandChosen(idx)) = message.downcast_ref::<CommandChosen>()
        {
            let action = actions.get(*idx).cloned();
            self.active_palette = None;
            if let Some(action) = action {
                self.run_palette_action(action, text);
            }
            return;
        }

        if message.source == self.browser
            && let Some(BrowserPathChanged(path)) = message.downcast_ref::<BrowserPathChanged>()
        {
            if let Some(&last) = path.last()
                && let Some(kind) = self.node_map.get(last).cloned()
            {
                match kind {
                    NodeKind::Repo(ri) => {
                        self.sel_repo = ri;
                        self.sel_wt = 0;
                        self.sel_file = 0;
                    }
                    NodeKind::Worktree(ri, wi) => {
                        self.sel_repo = ri;
                        self.sel_wt = wi;
                        self.sel_file = 0;
                    }
                    NodeKind::File(ri, wi, fi) => {
                        self.sel_repo = ri;
                        self.sel_wt = wi;
                        self.sel_file = fi;
                    }
                }
                self.rebuild_path_control(text);
                self.rebuild_diff_view(text);
                self.update_terminal();
            }
            return;
        }

        if message.source == self.path_control
            && let Some(PathItemClicked(idx)) = message.downcast_ref::<PathItemClicked>()
        {
            if *idx == 0 {
                self.sel_wt = 0;
                self.sel_file = 0;
            } else if *idx == 1 {
                self.sel_file = 0;
            }
            self.sync_views(text);
            return;
        }

        if message.source == self.token_field
            && let Some(TokensChanged(tokens)) = message.downcast_ref::<TokensChanged>()
        {
            self.tokens = tokens.clone();
            self.sync_views(text);
            return;
        }

        if message.source == self.diff_mode_seg
            && let Some(SelectionChanged(idx)) = message.downcast_ref::<SelectionChanged>()
        {
            self.set_diff_mode(DiffMode::from_index(*idx), text);
            return;
        }

        if message.source == self.drop_zone
            && let Some(FilesDropped(paths)) = message.downcast_ref::<FilesDropped>()
        {
            self.send_dropped_files(paths);
            return;
        }

        if message.downcast_ref::<ButtonClicked>().is_some() {
            if message.source == self.palette_btn {
                self.open_command_palette();
            } else if message.source == self.refresh_btn {
                if let Source::Live(ch) = &self.source {
                    ch.send(Command::Refresh);
                }
                self.toast("再取得をリクエストしました");
            } else if message.source == self.focus_btn {
                self.focus_selected_pane();
            } else if message.source == self.toggle_side_btn {
                self.toggle_inspector();
            } else if message.source == self.quote_btn {
                self.quote_selected_diff();
            } else if message.source == self.send_btn {
                self.send_review_prompt();
            }
        }
    }
}

impl Screen for RepoBrowser {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        self.poll_live(text);
        let mut changed = self.tree.prepare(viewport, text).is_some();
        if let Some(path) = self.pending_browser_path.take()
            && self.browser != self.browser_host
        {
            self.tree
                .update::<BrowserView, _>(self.browser, |b, cx| b.set_path(cx, path));
            changed |= self.tree.prepare(viewport, text).is_some();
        }
        changed
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        self.handle_message(&message, text);
    }
}

/// Shell-escapes one word: single-quotes it (embedded `'` as `'\''`) unless
/// it only holds characters no shell treats specially. Mirrors noa-app's
/// drop/paste escaping.
fn shell_escape(s: &str) -> String {
    let is_safe = !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'%' | b'+' | b'='
                )
        });
    if is_safe {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn colorize_diff_line(line: &str) -> LogLine {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff --git") {
        LogLine::new(line).color(DIFF_META_FG)
    } else if line.starts_with("@@") {
        LogLine::new(line)
            .color(DIFF_HUNK_FG)
            .background(DIFF_HUNK_BG)
    } else if line.starts_with('+') {
        LogLine::new(line)
            .color(DIFF_ADD_FG)
            .background(DIFF_ADD_BG)
    } else if line.starts_with('-') {
        LogLine::new(line)
            .color(DIFF_DEL_FG)
            .background(DIFF_DEL_BG)
    } else {
        LogLine::new(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_screen_lays_out_and_switches_diff_mode() {
        let mut screen = RepoBrowser::demo();
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
        assert!(screen.tree().layout(screen.outer_split).is_some());
        assert_eq!(screen.filtered.repos.len(), 2);

        screen.set_diff_mode(DiffMode::Staged, &mut text);
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
        assert_eq!(screen.diff_mode, DiffMode::Staged);
    }

    #[test]
    fn command_palette_opens_and_quotes_diff() {
        let mut screen = RepoBrowser::demo();
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);

        screen.open_command_palette();
        assert!(screen.active_palette.is_some());

        screen.quote_selected_diff();
        let prompt = screen
            .tree()
            .get::<TextArea>(screen.prompt_area)
            .unwrap()
            .text();
        assert!(prompt.contains("crates/noa-ipc/src/push.rs"));
    }

    // ── Live-mode regression tests driven through a fake channel ─────────────

    use crate::live::{TestFeed, test_channel};
    use crate::model::PaneSummary;

    const CWD: &str = "/repo";

    fn live_data() -> RepoData {
        let file = |path: &str| ChangedFile {
            path: path.into(),
            status: "M".into(),
            added: 1,
            deleted: 0,
            diff: String::new(),
            conflict_worktrees: Vec::new(),
        };
        RepoData {
            repos: vec![Repo {
                name: "repo".into(),
                root: CWD.into(),
                worktrees: vec![Worktree {
                    name: "main".into(),
                    cwd: CWD.into(),
                    branch: "main".into(),
                    ahead: 0,
                    behind: 0,
                    note: None,
                    panes: vec![PaneSummary {
                        pane_id: 7,
                        name: "zsh".into(),
                        label: None,
                        process: None,
                        activity: AgentActivity::None,
                        busy: false,
                        attention: false,
                        terminal_tail: String::new(),
                    }],
                    files: vec![file("a.rs"), file("b.rs")],
                }],
            }],
        }
    }

    fn snapshot(mode: DiffMode, selected: Option<(&str, &str)>) -> Snapshot {
        Snapshot {
            data: live_data(),
            link: Link::Live,
            can_focus: true,
            can_send: true,
            diff_mode: mode,
            selected_diff: selected.map(|(path, diff)| (CWD.into(), path.into(), diff.into())),
        }
    }

    struct LiveHarness {
        screen: RepoBrowser,
        text: TextStore,
        feed: TestFeed,
    }

    impl LiveHarness {
        fn new() -> Self {
            let (channel, feed) = test_channel();
            let mut h = LiveHarness {
                screen: RepoBrowser::live(channel),
                text: TextStore::new(Default::default()),
                feed,
            };
            h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1"))));
            h
        }

        fn deliver(&mut self, snap: Snapshot) {
            self.feed.snapshots.send(snap).unwrap();
            self.screen
                .prepare(Size::new(1440.0, 900.0), &mut self.text);
        }

        fn drain(&self) -> Vec<Command> {
            self.feed.commands.try_iter().collect()
        }

        fn select_file(&mut self, fi: usize) {
            self.screen.sel_file = fi;
            self.screen.rebuild_diff_view(&mut self.text);
        }

        fn diff_lines(&self) -> Vec<String> {
            let log = self
                .screen
                .tree()
                .get::<LogView>(self.screen.diff_log)
                .unwrap();
            log.all_text().lines().map(str::to_owned).collect()
        }
    }

    fn load_diff_paths(cmds: &[Command]) -> Vec<String> {
        cmds.iter()
            .filter_map(|c| match c {
                Command::LoadDiff { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn returning_to_cached_file_requests_fresh_diff() {
        let mut h = LiveHarness::new();
        h.drain();
        h.select_file(1);
        assert_eq!(load_diff_paths(&h.drain()), vec!["b.rs"]);
        h.select_file(0);
        assert_eq!(load_diff_paths(&h.drain()), vec!["a.rs"]);
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v2"))));
        assert_eq!(h.diff_lines(), vec!["+a v2"]);
        // A periodic snapshot for the same selection must not re-request it.
        assert!(load_diff_paths(&h.drain()).is_empty());
    }

    #[test]
    fn quote_without_selection_uses_live_diff() {
        let mut h = LiveHarness::new();
        h.screen.quote_selected_diff();
        let prompt = h
            .screen
            .tree()
            .get::<TextArea>(h.screen.prompt_area)
            .unwrap()
            .text()
            .to_owned();
        assert!(prompt.contains("```diff\n+a v1\n```"), "{prompt}");
    }

    #[test]
    fn stale_snapshot_from_previous_mode_is_ignored() {
        let mut h = LiveHarness::new();
        h.screen.set_diff_mode(DiffMode::Staged, &mut h.text);
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", "+unstaged"))));
        assert_eq!(h.screen.diff_mode, DiffMode::Staged);
        assert!(h.screen.diff_cache.is_empty());
        h.deliver(snapshot(DiffMode::Staged, Some(("a.rs", "+staged"))));
        assert_eq!(h.diff_lines(), vec!["+staged"]);
    }

    #[test]
    fn review_prompt_is_sent_as_paste_then_submitted() {
        let mut h = LiveHarness::new();
        h.screen
            .tree
            .update::<TextArea, _>(h.screen.prompt_area, |a, cx| {
                a.set_text(cx, "[main / a.rs]\n```diff\n+x\n```\n修正依頼: 直して")
            });
        h.drain();
        h.screen.send_review_prompt();
        let sends: Vec<_> = h
            .drain()
            .into_iter()
            .filter_map(|c| match c {
                Command::SendText {
                    pane_id,
                    text,
                    submit,
                } => Some((pane_id, text, submit)),
                _ => None,
            })
            .collect();
        assert_eq!(
            sends,
            vec![(
                7,
                "[main / a.rs]\n```diff\n+x\n```\n修正依頼: 直して".to_owned(),
                true
            )]
        );
    }

    #[test]
    fn dropped_paths_are_shell_escaped() {
        let mut h = LiveHarness::new();
        h.drain();
        h.screen.send_dropped_files(&[
            PathBuf::from("/Users/x/Desktop/Screen Shot 1.png"),
            PathBuf::from("/tmp/plain.txt"),
            PathBuf::from("/tmp/it's.txt"),
        ]);
        let texts: Vec<String> = h
            .drain()
            .into_iter()
            .filter_map(|c| match c {
                Command::SendText { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["'/Users/x/Desktop/Screen Shot 1.png' /tmp/plain.txt '/tmp/it'\\''s.txt'"]
        );
    }

    fn prompt_text(screen: &RepoBrowser) -> String {
        screen
            .tree()
            .get::<TextArea>(screen.prompt_area)
            .unwrap()
            .text()
            .to_owned()
    }

    fn set_prompt(screen: &mut RepoBrowser, text: &str) {
        let text = text.to_owned();
        screen
            .tree
            .update::<TextArea, _>(screen.prompt_area, |a, cx| a.set_text(cx, text));
    }

    #[test]
    fn missing_base_branch_is_shown_instead_of_no_changes() {
        let mut h = LiveHarness::new();
        let mut snap = snapshot(DiffMode::VsMain, None);
        let wt = &mut snap.data.repos[0].worktrees[0];
        wt.files.clear();
        wt.note = Some("比較元ブランチ (main / master / origin/HEAD) が見つかりません".into());
        h.screen.set_diff_mode(DiffMode::VsMain, &mut h.text);
        h.deliver(snap);
        assert_eq!(
            h.diff_lines(),
            vec!["比較元ブランチ (main / master / origin/HEAD) が見つかりません"]
        );
    }

    #[test]
    fn selection_follows_the_file_when_earlier_files_appear() {
        let mut h = LiveHarness::new();
        h.select_file(1);
        let mut snap = snapshot(DiffMode::Unstaged, Some(("b.rs", "+b")));
        let files = &mut snap.data.repos[0].worktrees[0].files;
        let mut first = files[0].clone();
        first.path = "0.rs".into();
        files.insert(0, first);
        h.deliver(snap);
        assert_eq!(h.screen.current_file().unwrap().path, "b.rs");
    }

    #[test]
    fn selection_follows_the_worktree_when_an_earlier_one_appears() {
        let mut h = LiveHarness::new();
        let mut snap = snapshot(DiffMode::Unstaged, None);
        let mut other = snap.data.repos[0].worktrees[0].clone();
        other.cwd = "/repo-wt-aaa".into();
        other.branch = "aaa".into();
        other.panes[0].pane_id = 99;
        snap.data.repos[0].worktrees.insert(0, other);
        h.deliver(snap);
        assert_eq!(h.screen.current_worktree().unwrap().cwd, CWD);
        set_prompt(&mut h.screen, "直して");
        h.drain();
        h.screen.send_review_prompt();
        let targets: Vec<u64> = h
            .drain()
            .into_iter()
            .filter_map(|c| match c {
                Command::SendText { pane_id, .. } => Some(pane_id),
                _ => None,
            })
            .collect();
        assert_eq!(targets, vec![7]);
    }

    #[test]
    fn unchanged_periodic_snapshot_keeps_diff_selection() {
        let mut h = LiveHarness::new();
        let diff_log = h.screen.diff_log;
        h.screen
            .tree
            .update::<LogView, _>(diff_log, |log, cx| log.select_lines(cx, 0..1));
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1"))));
        let log = h.screen.tree().get::<LogView>(diff_log).unwrap();
        assert_eq!(log.selected_lines(), Some(0..1));
    }

    #[test]
    fn review_prompt_is_kept_until_the_pane_accepts_it() {
        let mut h = LiveHarness::new();
        set_prompt(&mut h.screen, "直して");
        h.screen.send_review_prompt();
        assert_eq!(prompt_text(&h.screen), "直して");
        h.feed.notices.send("送信失敗: refused".into()).unwrap();
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1"))));
        assert_eq!(prompt_text(&h.screen), "直して");
        h.feed.delivered.send("直して".into()).unwrap();
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1"))));
        assert_eq!(prompt_text(&h.screen), "");
    }

    #[test]
    fn diff_mode_from_palette_updates_toolbar_segment() {
        let mut screen = RepoBrowser::demo();
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
        screen.run_palette_action(PaletteAction::SwitchDiffMode(DiffMode::Staged), &mut text);
        let seg = screen
            .tree()
            .get::<SegmentedControl>(screen.diff_mode_seg)
            .unwrap();
        assert_eq!(seg.selected(), &[false, true, false]);
    }

    /// The palette action that selects `path` in the palette as it was opened.
    fn palette_file_action(screen: &RepoBrowser, path: &str) -> PaletteAction {
        let (_, actions) = screen.active_palette.as_ref().unwrap();
        actions
            .iter()
            .find(|a| matches!(a, PaletteAction::SelectFile { path: p, .. } if p == path))
            .cloned()
            .unwrap()
    }

    #[test]
    fn palette_file_choice_survives_a_refresh_while_open() {
        let mut h = LiveHarness::new();
        h.screen.open_command_palette();
        let choose_b = palette_file_action(&h.screen, "b.rs");
        let mut snap = snapshot(DiffMode::Unstaged, None);
        let mut other = snap.data.repos[0].worktrees[0].clone();
        other.cwd = "/repo-wt-aaa".into();
        other.branch = "aaa".into();
        snap.data.repos[0].worktrees.insert(0, other);
        let files = &mut snap.data.repos[0].worktrees[1].files;
        let mut first = files[0].clone();
        first.path = "0.rs".into();
        files.insert(0, first);
        h.deliver(snap);
        h.screen.run_palette_action(choose_b, &mut h.text);
        assert_eq!(h.screen.current_worktree().unwrap().cwd, CWD);
        assert_eq!(h.screen.current_file().unwrap().path, "b.rs");
    }

    #[test]
    fn live_snapshot_render_shows_link_state() {
        let screen = RepoBrowser::of_snapshot(snapshot(DiffMode::Unstaged, None));
        let badges: Vec<String> = screen
            .tree()
            .children(screen.status_leading)
            .iter()
            .filter_map(|id| screen.tree().get::<Badge>(*id).and_then(Badge::label))
            .collect();
        assert_eq!(
            badges.first().map(String::as_str),
            Some("LIVE"),
            "{badges:?}"
        );
    }

    #[test]
    fn dropped_paths_toast_only_the_send_result() {
        let mut h = LiveHarness::new();
        h.screen.toasts.clear();
        h.screen
            .send_dropped_files(&[PathBuf::from("/tmp/plain.txt")]);
        assert!(h.screen.toasts.is_empty(), "{:?}", h.screen.toasts);
    }
}
