use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Overflow, Rect as LayoutRect, Size as LayoutSize, auto, length,
    percent,
};
use noa_gpui_core::widgets::{
    Badge, BrowserPathChanged, BrowserView, Button, ButtonClicked, COMMAND_PALETTE_SHORTCUTS,
    ComboButton, CommandChosen, CommandPalette, Container, ControlSize, EmptyState, FIND_SHORTCUT,
    FileDropZone, FilesDropped, FindBar, Kbd, Label, LogLine, LogView, MenuChosen, MenuItem,
    OutlineItem, PaletteCommand, PathControl, PathItemClicked, RecentCommands, RowSelectionChanged,
    ScrollView, SegmentMode, SegmentedControl, SelectionChanged, SelectionMode, SplitPane,
    SplitView, StatusBar, TABLE_HEADER, TabView, TableColumn, TableData, TableView, Terminal,
    TextArea, Toast, TokenField, TokensChanged, Tone, Truncation, drive_find,
};
use noa_gpui_core::{Message, Role, Shortcut, ShortcutPressed, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Color, Size};
use noa_gpui_text::TextStore;

use crate::live::{Channel, Command, Link, Snapshot};
use crate::model::{
    AgentActivity, ChangedFile, CommitInfo, DiffMode, PaneSummary, Repo, RepoData, Worktree,
    demo_data, now_secs,
};

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
    /// Resolved when chosen, like `SelectFile`.
    SelectCommit {
        root: String,
        cwd: String,
        sha: String,
    },
    FocusPane(u64),
    /// Resolved against the selection when chosen.
    OpenShell,
    OpenEditor,
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

/// What the live terminal tab shows, so an unchanged refresh keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalShown {
    /// The target pane (picked, else the worktree's primary) and its tail.
    Pane(u64, String),
    /// A worktree listed by `git worktree list` with no pane in it.
    NoPane {
        branch: String,
        cwd: String,
    },
    Nothing,
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
    /// The commit (worktree cwd, sha) whose diff the diff view shows instead of
    /// a file's; `None` while a file is shown.
    sel_commit: Option<(String, String)>,
    /// `git show` text of commits already loaded, by (worktree cwd, sha).
    /// A commit never changes, so entries stay valid across refreshes.
    commit_diffs: HashMap<(String, String), String>,
    /// The commit the live feed was last asked to load.
    requested_commit: Option<(String, String)>,
    /// What the commit table holds: its rows and the sha selected in it.
    shown_commits: Option<CommitTableState>,
    /// The line-number gutter in front of each line of `shown_diff`, when it
    /// is a unified diff.
    gutter: Option<Gutter>,
    /// The browser's height and column width, sized to its pane: BrowserView
    /// only takes fixed sizes, so it is rebuilt when its pane resizes.
    browser_size: (f32, f32),

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
    commit_head: WidgetId,
    commit_table: WidgetId,
    quote_btn: WidgetId,
    clear_btn: WidgetId,
    /// Shows which pane receives the prompt; rebuilt when the panes change.
    target_host: WidgetId,
    target_seg: Option<WidgetId>,
    prompt_area: WidgetId,
    send_btn: WidgetId,
    status_leading: WidgetId,
    status_trailing: WidgetId,

    // Active CommandPalette overlay state.
    active_palette: Option<(WidgetId, Vec<PaletteAction>)>,
    recent_commands: RecentCommands,
    last_terminal_key: TerminalShown,
    pending_browser_path: Option<Vec<usize>>,
    /// The pane the user picked to receive the prompt, while the selected
    /// worktree stays `target_wt`; `None` means the worktree's primary pane.
    target_pane: Option<u64>,
    target_wt: Option<String>,
    /// What the target display was last built from.
    target_key: String,
    /// The worktree (cwd) the prompt was started for.
    prompt_origin: Option<String>,
    /// The prompt (and its origin) the last "クリア" emptied. TextArea drops
    /// its undo history on `set_text`, so the screen keeps one level itself.
    cleared_prompt: Option<(String, Option<String>)>,
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
            TokenField::new("フィルタ (dirty, merge-conflict, no-pane…)", 280.0).completions([
                "dirty",
                "conflict",
                "merge-conflict",
                "no-pane",
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
            ComboButton::new(
                "ペインを開く",
                vec![
                    MenuItem::new("ここでシェルを開く").key(SHELL_SHORTCUT),
                    MenuItem::new("エディタで開く").key(EDITOR_SHORTCUT),
                ],
            )
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
            BrowserView::new(Vec::new()).height(BROWSER_HEIGHT),
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
            TabView::new(["ライブ端末", "レビュー指示", "コミット"]),
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
                "ファイル・行範囲を何度でも引用して指示を組み立て、選んだペインへ送ります (⌘L で引用)。",
            )
            .font_size(12.0)
            .color(Role::Muted)
            .wrap(true),
        );
        let target_host = tree.insert(
            Some(composer_page),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_shrink: 0.0,
                gap: LayoutSize {
                    width: length(0.0),
                    height: length(6.0),
                },
                ..Default::default()
            }),
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
        let clear_btn = tree.insert(
            Some(composer_actions),
            Button::new(CLEAR_LABEL).control_size(ControlSize::Small),
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
                min_size: LayoutSize {
                    width: auto(),
                    height: length(PROMPT_MIN_HEIGHT),
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

        // Tab 2: the worktree's commits beyond the base branch
        let commit_page = tree.insert(
            Some(side_tabs),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                gap: LayoutSize {
                    width: length(0.0),
                    height: length(6.0),
                },
                padding: LayoutRect::length(6.0),
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            }),
        );
        let commit_head = tree.insert(
            Some(commit_page),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                flex_shrink: 0.0,
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        let commit_table = tree.insert(
            Some(commit_page),
            TableView::new(
                vec![TableColumn::new("コミット", 360.0).flex(1.0)],
                CommitRows::default(),
            )
            .selection_mode(SelectionMode::Single)
            .row_height(COMMIT_ROW)
            .height(commit_table_height(0)),
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
        tree.add_shortcut(SHELL_SHORTCUT, focus_btn);
        tree.add_shortcut(EDITOR_SHORTCUT, focus_btn);
        tree.add_shortcut(QUOTE_SHORTCUT, quote_btn);

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
            sel_commit: None,
            commit_diffs: HashMap::new(),
            requested_commit: None,
            shown_commits: None,
            gutter: None,
            browser_size: (BROWSER_HEIGHT, BROWSER_COLUMN),
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
            commit_head,
            commit_table,
            quote_btn,
            clear_btn,
            target_host,
            target_seg: None,
            prompt_area,
            send_btn,
            status_leading: status_parts.leading,
            status_trailing: status_parts.trailing,
            active_palette: None,
            recent_commands: RecentCommands::default(),
            last_terminal_key: TerminalShown::Nothing,
            pending_browser_path: None,
            target_pane: None,
            target_wt: None,
            target_key: String::new(),
            prompt_origin: None,
            cleared_prompt: None,
            #[cfg(test)]
            toasts: Vec::new(),
        };
        let mut text = TextStore::new(Default::default());
        screen.sync_views(&mut text);
        screen
    }

    /// Opens the review tab with the selected lines quoted (for `--composer`
    /// snapshots).
    pub fn show_composer(&mut self) {
        self.quote_selected_diff();
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

    /// The commit the diff view shows, when it is one of the current
    /// worktree's.
    fn current_commit(&self) -> Option<&CommitInfo> {
        let (cwd, sha) = self.sel_commit.as_ref()?;
        let wt = self.current_worktree()?;
        if wt.cwd != *cwd {
            return None;
        }
        wt.commits.iter().find(|c| c.sha == *sha)
    }

    /// Back to showing the selected file's diff.
    fn leave_commit(&mut self) {
        self.sel_commit = None;
        self.requested_commit = None;
    }

    /// Shows the commit `sha` of the current worktree in the diff view.
    fn select_commit(&mut self, sha: String, text: &mut TextStore) {
        let Some(cwd) = self.current_worktree().map(|w| w.cwd.clone()) else {
            return;
        };
        if self.sel_commit.as_ref() == Some(&(cwd.clone(), sha.clone())) {
            return;
        }
        self.sel_commit = Some((cwd, sha));
        // The live feed drops its file when asked for a commit and only picks
        // it up again on a new request, so the file must be requested afresh
        // once the user goes back to it.
        self.requested_diff = None;
        self.sync_views(text);
    }

    /// A row of the commit table was chosen.
    fn select_commit_row(&mut self, row: usize, text: &mut TextStore) {
        let sha = self
            .current_worktree()
            .and_then(|w| w.commits.get(row))
            .map(|c| c.sha.clone());
        if let Some(sha) = sha {
            self.select_commit(sha, text);
        }
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
        if self.sel_commit.is_some() && self.current_commit().is_none() {
            self.leave_commit();
        }
        self.rebuild_browser(text);
        self.rebuild_path_control(text);
        self.rebuild_diff_view(text);
        self.rebuild_commits(text);
        self.refresh_target(text);
        self.rebuild_status_bar(text);
    }

    /// The pane that receives the prompt: the picked one while it is still in
    /// the selected worktree, else the worktree's primary pane.
    fn target(&self) -> Option<&PaneSummary> {
        let wt = self.current_worktree()?;
        self.target_pane
            .and_then(|id| wt.panes.iter().find(|p| p.pane_id == id))
            .or_else(|| wt.primary_pane())
    }

    /// Drops the pick when the worktree changed or its pane disappeared, pins
    /// the pane shown instead, then brings the target display and the
    /// terminal tab up to date.
    fn refresh_target(&mut self, text: &mut TextStore) {
        let cwd = self.current_worktree().map(|w| w.cwd.clone());
        if cwd != self.target_wt {
            self.target_wt = cwd;
            self.target_pane = None;
        } else if let Some(id) = self.target_pane
            && !self
                .current_worktree()
                .is_some_and(|w| w.panes.iter().any(|p| p.pane_id == id))
        {
            self.target_pane = None;
        }
        // The pane first shown stays the target: a later change of
        // `primary_pane()` (an agent finishing, another asking) must never
        // redirect the prompt silently.
        if self.target_pane.is_none() {
            self.target_pane = self
                .current_worktree()
                .and_then(Worktree::primary_pane)
                .map(|p| p.pane_id);
        }
        self.rebuild_target(text);
        self.update_terminal();
    }

    /// Picks the `idx`-th pane of the selected worktree as the prompt's target.
    fn choose_target(&mut self, idx: usize, text: &mut TextStore) {
        self.target_pane = self
            .current_worktree()
            .and_then(|w| w.panes.get(idx))
            .map(|p| p.pane_id);
        self.rebuild_target(text);
        self.update_terminal();
    }

    fn rebuild_target(&mut self, text: &mut TextStore) {
        let panes: Vec<PaneSummary> = self
            .current_worktree()
            .map(|w| w.panes.clone())
            .unwrap_or_default();
        let chosen = self.target().map(|p| p.pane_id);
        // Only what the display shows: a pane's output changing must not
        // rebuild the control under a click.
        let shown: Vec<_> = panes
            .iter()
            .map(|p| (p.pane_id, &p.name, &p.process, p.activity))
            .collect();
        let key = format!("{chosen:?}|{shown:?}");
        if key == self.target_key {
            return;
        }
        self.target_key = key;
        self.clear_children(self.target_host, text);
        self.target_seg = None;

        let row = self.tree.insert(
            Some(self.target_host),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::CENTER,
                flex_shrink: 0.0,
                gap: LayoutSize {
                    width: length(6.0),
                    height: length(0.0),
                },
                ..Default::default()
            }),
        );
        self.tree.insert(
            Some(row),
            Label::new("送信先:").font_size(12.0).color(Role::Muted),
        );
        let Some(pane) = panes.iter().find(|p| Some(p.pane_id) == chosen) else {
            self.tree.insert(
                Some(row),
                Label::new("(ペインなし)")
                    .font_size(12.0)
                    .color(Role::Muted),
            );
            return;
        };
        let process = pane
            .process
            .as_deref()
            .map(|p| format!(" · {p}"))
            .unwrap_or_default();
        self.tree.insert(
            Some(row),
            Label::new(format!("#{} {}{process}", pane.pane_id, pane.name))
                .font_size(12.0)
                .weight(600)
                .truncation(Truncation::Tail),
        );
        let tone = match pane.activity {
            AgentActivity::None => None,
            AgentActivity::Running => Some(Tone::Accent),
            AgentActivity::Waiting => Some(Tone::Warning),
            AgentActivity::Done => Some(Tone::Success),
        };
        if let Some(tone) = tone {
            self.tree
                .insert(Some(row), Badge::text(pane.activity.badge()).tone(tone));
        }
        if panes.len() > 1 {
            let selected = panes.iter().position(|p| p.pane_id == pane.pane_id);
            let labels = panes.iter().map(|p| format!("#{} {}", p.pane_id, p.name));
            let seg = SegmentedControl::new(labels, SegmentMode::SelectOne)
                .control_size(ControlSize::Small)
                .with_selected(&selected.into_iter().collect::<Vec<_>>());
            self.target_seg = Some(self.tree.insert(Some(self.target_host), seg));
        }
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
                let merge = if wt.merge_conflicts.is_empty() {
                    String::new()
                } else {
                    format!(" ⛔マージ衝突 {}", wt.merge_conflicts.len())
                };
                let no_pane = if wt.has_pane() {
                    ""
                } else {
                    " · ペインなし"
                };
                let ab = if wt.ahead > 0 || wt.behind > 0 {
                    format!(" ↑{}↓{}", wt.ahead, wt.behind)
                } else {
                    String::new()
                };
                let mut wt_item =
                    OutlineItem::new(format!("{}{ab}{merge}{no_pane}{act_str}{conf}", wt.branch));

                for (fi, file) in wt.files.iter().enumerate() {
                    let file_id = next_id;
                    next_id += 1;
                    self.node_map.push(NodeKind::File(ri, wi, fi));
                    let c = if file.conflict_worktrees.is_empty() {
                        ""
                    } else {
                        " ⚠"
                    };
                    let m = if wt.is_merge_conflict(&file.path) {
                        " ⛔"
                    } else {
                        ""
                    };
                    let title = format!(
                        "{} {} (+{}/-{}){m}{c}",
                        file.status, file.path, file.added, file.deleted
                    );
                    wt_item = wt_item.child(OutlineItem::new(title));
                    // While a commit is shown the worktree is the highlighted
                    // node, so choosing a file is a change of selection again.
                    if ri == self.sel_repo
                        && wi == self.sel_wt
                        && fi == self.sel_file
                        && self.sel_commit.is_none()
                    {
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
            BrowserView::new(items)
                .column_width(self.browser_size.1)
                .height(self.browser_size.0),
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
                if let Some(commit) = self.current_commit() {
                    crumbs.push(commit.short.clone());
                } else if let Some(file) = self.current_file() {
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

    /// The shown diff body: the commit's `git show`, else the selected file's
    /// inline diff, else the live cache.
    fn current_diff_text(&self) -> Option<String> {
        let wt = self.current_worktree()?;
        if let Some(commit) = self.current_commit() {
            if !commit.diff.is_empty() {
                return Some(commit.diff.clone());
            }
            return self
                .commit_diffs
                .get(&(wt.cwd.clone(), commit.sha.clone()))
                .cloned();
        }
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
            (Some(wt), Some(file)) if file.diff.is_empty() && self.sel_commit.is_none() => {
                Some((wt.cwd.clone(), file.path.clone()))
            }
            _ => None,
        };
        let wanted_commit = self
            .current_commit()
            .zip(self.current_worktree())
            .filter(|(c, _)| c.diff.is_empty())
            .map(|(c, wt)| (wt.cwd.clone(), c.sha.clone()));
        if let (Source::Live(ch), Some(key)) = (&self.source, wanted_commit)
            && self.requested_commit.as_ref() != Some(&key)
        {
            ch.send(Command::LoadCommit {
                cwd: key.0.clone(),
                sha: key.1.clone(),
            });
            self.requested_commit = Some(key);
        }
        if let (Source::Live(ch), Some(key)) = (&self.source, wanted)
            && self.requested_diff.as_ref() != Some(&key)
        {
            ch.send(Command::LoadDiff {
                cwd: key.0.clone(),
                path: key.1.clone(),
            });
            self.requested_diff = Some(key);
        }

        let merge_note = match (self.current_worktree(), self.current_file()) {
            (Some(wt), Some(file))
                if self.current_commit().is_none() && wt.is_merge_conflict(&file.path) =>
            {
                Some(format!(
                    "⛔ ベースへのマージで衝突: {}",
                    wt.merge_conflicts.join(", ")
                ))
            }
            _ => None,
        };
        let commit_view = self.current_commit().map(|commit| {
            let text = self
                .current_diff_text()
                .unwrap_or_else(|| "コミットを取得しています…".into());
            (
                format!("{}  ·  コミット  ({} files)", commit.title(), commit.files),
                text,
            )
        });
        let (header_text, conflict_note, diff_text) = if let Some((header, text)) = commit_view {
            (header, None, text)
        } else {
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
            }
        };

        self.tree.insert(
            Some(self.diff_header),
            Label::new(header_text)
                .font_size(12.5)
                .weight(600)
                .truncation(Truncation::Tail),
        );
        if let Some(note) = merge_note {
            self.tree.insert(
                Some(self.diff_header),
                Badge::text(note).tone(Tone::Critical),
            );
        }
        if let Some(note) = conflict_note {
            self.tree.insert(
                Some(self.diff_header),
                Badge::text(note).tone(Tone::Warning),
            );
        }

        if self.shown_diff.as_deref() != Some(diff_text.as_str()) {
            self.gutter = Gutter::of(&diff_text);
            let lines: Vec<LogLine> = match &self.gutter {
                Some(gutter) => diff_text
                    .lines()
                    .zip(&gutter.numbers)
                    .map(|(line, &numbers)| gutter.line(line, numbers))
                    .collect(),
                None => diff_text.lines().map(colorize_diff_line).collect(),
            };
            self.tree
                .update::<LogView, _>(self.diff_log, |log, cx| log.set_lines(cx, lines));
            self.shown_diff = Some(diff_text);
        }
    }

    /// Fills the commit tab with the current worktree's commits.
    fn rebuild_commits(&mut self, text: &mut TextStore) {
        let now = now_secs();
        let (rows, summary) = match self.current_worktree() {
            Some(wt) => {
                let rows: Vec<(String, String)> = wt
                    .commits
                    .iter()
                    .map(|c| (c.title(), c.detail(now)))
                    .collect();
                let summary = if wt.commits.is_empty() {
                    "ベースブランチより先のコミットはありません".to_owned()
                } else if wt.commits.len() >= crate::git::COMMIT_CAP {
                    format!(
                        "{}: ベースより先のコミット（新しい順・最新 {} 件まで）",
                        wt.branch,
                        wt.commits.len()
                    )
                } else {
                    format!(
                        "{}: ベースより先のコミット {} 件（新しい順）",
                        wt.branch,
                        wt.commits.len()
                    )
                };
                (rows, summary)
            }
            None => (Vec::new(), "ワークツリーが選択されていません".into()),
        };
        self.clear_children(self.commit_head, text);
        self.tree.insert(
            Some(self.commit_head),
            Label::new(summary)
                .font_size(12.0)
                .color(Role::Muted)
                .wrap(true),
        );

        let selected = self.current_commit().map(|c| c.sha.clone());
        let height = commit_table_height(rows.len());
        self.tree
            .update::<TableView, _>(self.commit_table, |table, cx| {
                table.set_height(cx, Some(height))
            });
        let state = (rows, selected);
        if self.shown_commits.as_ref() == Some(&state) {
            return;
        }
        let (rows, selected) = state.clone();
        let at = selected.as_ref().and_then(|sha| {
            self.current_worktree()
                .and_then(|w| w.commits.iter().position(|c| c.sha == *sha))
        });
        self.tree
            .update::<TableView, _>(self.commit_table, |table, cx| {
                table.set_data(cx, CommitRows { rows });
                if let Some(at) = at {
                    table.select_rows(cx, [at]);
                }
            });
        self.shown_commits = Some(state);
    }

    fn update_terminal(&mut self) {
        let next = match self.current_worktree() {
            Some(wt) => match self.target() {
                Some(p) => TerminalShown::Pane(p.pane_id, p.terminal_tail.clone()),
                None => TerminalShown::NoPane {
                    branch: wt.branch.clone(),
                    cwd: wt.cwd.clone(),
                },
            },
            None => TerminalShown::Nothing,
        };
        if next == self.last_terminal_key {
            return;
        }
        self.last_terminal_key = next.clone();
        self.tree.update::<Terminal, _>(self.terminal, |term, cx| {
            let _ = term.feed(cx, b"\x1b[2J\x1b[H");
            match &next {
                TerminalShown::Pane(_, tail) if !tail.is_empty() => {
                    let _ = term.feed(cx, tail.as_bytes());
                }
                TerminalShown::Pane(id, _) => {
                    let msg = format!("Pane #{id} (出力なし)\r\n");
                    let _ = term.feed(cx, msg.as_bytes());
                }
                TerminalShown::NoPane { branch, cwd } => {
                    let msg = format!(
                        "\x1b[1;33mペインなし\x1b[0m: {branch}\r\n\
                         この worktree を開いている Noa ペインはありません。\r\n\
                         ペインを閉じても worktree とブランチは残っています:\r\n  {cwd}\r\n\
                         差分の確認はできますが、指示の送信とフォーカスはできません。\r\n"
                    );
                    let _ = term.feed(cx, msg.as_bytes());
                }
                TerminalShown::Nothing => {
                    let _ = term.feed(cx, "(紐づくペインがありません)\r\n".as_bytes());
                }
            }
        });
    }

    /// The toast when the selection has no pane to `act` on: a worktree
    /// listed without a pane says so by name.
    fn no_pane_message(&self, act: &str) -> String {
        match self.current_worktree() {
            Some(wt) if !wt.has_pane() => format!(
                "{} はペインなしの worktree です（開いているペインがないため{act}できません）",
                wt.branch
            ),
            _ => format!("{act}先のペインがありません"),
        }
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
        let merge_conflicts = self.filtered.merge_conflict_count();
        if merge_conflicts > 0 {
            self.tree.insert(
                Some(self.status_leading),
                Badge::text(format!("⛔ マージ衝突 {merge_conflicts}")).tone(Tone::Critical),
            );
        }

        for (sc, text) in [
            (Shortcut::command('k'), "検索"),
            (Shortcut::command('f'), "差分検索"),
            (SHELL_SHORTCUT, "シェル"),
            (EDITOR_SHORTCUT, "エディタ"),
            (QUOTE_SHORTCUT, "引用"),
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

        for repo in &self.filtered.repos {
            for wt in &repo.worktrees {
                for commit in &wt.commits {
                    commands.push(
                        PaletteCommand::new(format!("コミット: {}", commit.title()))
                            .detail(format!("{} / {}", repo.name, wt.branch))
                            .keywords(format!(
                                "commit log {} {} {}",
                                commit.sha, commit.author, wt.branch
                            )),
                    );
                    actions.push(PaletteAction::SelectCommit {
                        root: repo.root.clone(),
                        cwd: wt.cwd.clone(),
                        sha: commit.sha.clone(),
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
        if let Some(wt) = self.current_worktree() {
            commands.push(
                PaletteCommand::new(format!("ここでシェルを開く: {}", wt.branch))
                    .detail(wt.cwd.clone())
                    .shortcut(SHELL_SHORTCUT)
                    .keywords("open shell terminal new tab worktree"),
            );
            actions.push(PaletteAction::OpenShell);
        }
        // While a commit is shown, the file of its selected diff line.
        let editor_path = if self.current_commit().is_some() {
            self.editor_target().ok().map(|(path, _)| path)
        } else {
            self.current_file().map(|f| f.path.clone())
        };
        if let Some(path) = editor_path {
            commands.push(
                PaletteCommand::new(format!("エディタで開く: {path}"))
                    .shortcut(EDITOR_SHORTCUT)
                    .keywords("open editor edit vim code"),
            );
            actions.push(PaletteAction::OpenEditor);
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
        let Some(pane) = self.target() else {
            self.toast(self.no_pane_message("フォーカス"));
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

    /// A new Noa tab with a shell in the selected worktree's root. Works for
    /// a worktree without panes.
    fn open_shell_here(&mut self) {
        let Some(cwd) = self.current_worktree().map(|wt| wt.cwd.clone()) else {
            self.toast("worktree が選択されていません");
            return;
        };
        match &self.source {
            Source::Demo | Source::Snapshot => {
                self.toast(format!("[DEMO] {cwd} で新しいタブのシェルを開きます"))
            }
            Source::Live(ch) => {
                if !self.can_focus {
                    self.toast("server-scopes に control が必要です");
                    return;
                }
                ch.send(Command::OpenShell { cwd });
            }
        }
    }

    /// A new Noa tab in the selected worktree running the editor on the
    /// selected file (the commit's file of the selected line while a commit
    /// is shown), at the diff selection's line.
    fn open_in_editor(&mut self) {
        let (path, line) = match self.editor_target() {
            Ok(target) => target,
            Err(why) => return self.toast(why),
        };
        // A target always lies in the selected worktree.
        let Some(cwd) = self.current_worktree().map(|wt| wt.cwd.clone()) else {
            return;
        };
        match &self.source {
            Source::Demo | Source::Snapshot => self.toast(format!(
                "[DEMO] 新しいタブのエディタで {path}:{line} を開きます"
            )),
            Source::Live(ch) => {
                // The editor is a command, which takes `input` on top of `control`.
                if !self.can_focus {
                    self.toast("server-scopes に control が必要です");
                    return;
                }
                if !self.can_send {
                    self.toast("server-scopes に input が必要です");
                    return;
                }
                ch.send(Command::OpenEditor { cwd, path, line });
            }
        }
    }

    /// The (path, line) the editor opens, or why there is nothing to open.
    /// While a commit is shown it is never the file list's selection.
    fn editor_target(&self) -> Result<(String, u32), String> {
        if self.current_commit().is_some() {
            return self.commit_editor_target();
        }
        let file = self
            .current_file()
            .ok_or_else(|| NO_FILE_TO_OPEN.to_owned())?;
        if file.status == "D" {
            return Err(format!("{} は削除済みのため開けません", file.path));
        }
        let lines = self.shown_diff.as_deref().map_or(0, |d| d.lines().count());
        Ok((file.path.clone(), self.editor_line(0..lines)))
    }

    /// The commit's file the selected diff line belongs to (from the nearest
    /// `diff --git` line above it), else its first file, with the line.
    fn commit_editor_target(&self) -> Result<(String, u32), String> {
        if self.current_diff_text().is_none() {
            return Err("コミットの差分を取得しています…".into());
        }
        let lines: Vec<&str> = self.shown_diff.as_deref().unwrap_or("").lines().collect();
        let files: Vec<usize> = (0..lines.len())
            .filter(|&i| lines[i].starts_with("diff --git "))
            .collect();
        let selected = self
            .tree
            .get::<LogView>(self.diff_log)
            .and_then(LogView::selected_lines);
        let start = selected
            .and_then(|sel| files.iter().rev().find(|&&i| i <= sel.start).copied())
            .or_else(|| files.first().copied())
            .ok_or_else(|| "このコミットに開けるファイルがありません".to_owned())?;
        let end = files
            .iter()
            .find(|&&i| i > start)
            .copied()
            .unwrap_or(lines.len());
        let path = diff_git_new_path(lines[start])
            .ok_or_else(|| format!("開くファイルのパスを読めません: {}", lines[start]))?;
        let deleted = lines[start..end]
            .iter()
            .take_while(|l| !l.starts_with("@@"))
            .any(|l| l.starts_with("deleted file mode") || *l == "+++ /dev/null");
        if deleted {
            return Err(format!("{path} はこのコミットで削除されたため開けません"));
        }
        Ok((path, self.editor_line(start..end)))
    }

    /// The line the editor opens at, looking only at `section` (line indices
    /// of the shown diff): the first selected line there that has a new-side
    /// number, else the section's first hunk's new start, else 1.
    fn editor_line(&self, section: std::ops::Range<usize>) -> u32 {
        let selected = self
            .tree
            .get::<LogView>(self.diff_log)
            .and_then(LogView::selected_lines);
        let from_selection = selected
            .map(|sel| sel.start.max(section.start)..sel.end.min(section.end))
            .filter(|lines| !lines.is_empty())
            .zip(self.gutter.as_ref())
            .and_then(|(lines, gutter)| gutter.first_new_line(lines));
        let first_hunk = || {
            self.shown_diff
                .as_deref()?
                .lines()
                .skip(section.start)
                .take(section.len())
                .find_map(parse_hunk_header)
                .map(|(_, new)| new)
        };
        from_selection.or_else(first_hunk).unwrap_or(1).max(1)
    }

    fn quote_selected_diff(&mut self) {
        let log = self.tree.get::<LogView>(self.diff_log);
        let selected = log.and_then(LogView::selected_lines);
        // A commit diff spans several files, so a bare line number would not
        // say which one it is in: the range is left out there.
        let range = selected
            .clone()
            .zip(self.gutter.as_ref())
            .filter(|_| self.current_commit().is_none())
            .and_then(|(lines, gutter)| gutter.range_label(lines))
            .map(|label| format!(" {label}"))
            .unwrap_or_default();
        let header = match (
            self.current_worktree(),
            self.current_commit(),
            self.current_file(),
        ) {
            (Some(wt), Some(commit), _) => {
                format!("[{} / commit {}{range}]", wt.branch, commit.short)
            }
            (Some(wt), None, Some(f)) => format!("[{} / {}{range}]", wt.branch, f.path),
            _ => "[diff]".into(),
        };
        let snippet = log
            .and_then(LogView::copy_text)
            .map(|text| match &self.gutter {
                Some(gutter) => gutter.strip(&text),
                None => text,
            })
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                self.current_diff_text()
                    .map(|d| d.lines().take(10).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default()
            });
        let block = format!("{header}\n```diff\n{snippet}\n```\n");
        self.tree
            .update::<TabView, _>(self.side_tabs, |tabs, cx| tabs.set_selected(cx, 1));
        self.tree.set_focus(Some(self.prompt_area));

        let current = self.prompt_text();
        if current.contains(&block) {
            self.toast("同じ範囲はすでに引用済みです");
            return;
        }
        let cwd = self.current_worktree().map(|w| w.cwd.clone());
        let mut next = if current.trim().is_empty() {
            self.prompt_origin = cwd;
            format!("{QUOTE_LEAD}\n")
        } else {
            let mixed = self.prompt_origin.is_some() && self.prompt_origin != cwd;
            if self.prompt_origin.is_none() {
                self.prompt_origin = cwd;
            }
            if mixed {
                let origin = self.prompt_origin_name().unwrap_or_default();
                self.toast(format!(
                    "この指示は複数の worktree の引用を含みます。送信できるのは最初に引用した worktree {origin} からです"
                ));
            }
            let mut text = current;
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text
        };
        next.push_str(&block);
        self.tree
            .update::<TextArea, _>(self.prompt_area, |area, cx| area.set_text(cx, next));
    }

    fn prompt_text(&self) -> String {
        self.tree
            .get::<TextArea>(self.prompt_area)
            .map(|a| a.text().to_owned())
            .unwrap_or_default()
    }

    fn set_prompt_text(&mut self, text: String) {
        self.tree
            .update::<TextArea, _>(self.prompt_area, |area, cx| area.set_text(cx, text));
    }

    /// Empties the prompt without asking; pressed again, it brings the text
    /// back (TextArea has no undo across `set_text`, so one level is kept here).
    fn clear_or_restore_prompt(&mut self) {
        if let Some((prev, origin)) = self.cleared_prompt.take() {
            let current = self.prompt_text();
            let restored = if current.trim().is_empty() {
                prev
            } else {
                format!("{}\n{current}", prev.trim_end())
            };
            self.set_prompt_text(restored);
            self.prompt_origin = origin;
        } else {
            let current = self.prompt_text();
            if current.is_empty() {
                return;
            }
            self.cleared_prompt = Some((current, self.prompt_origin.take()));
            self.set_prompt_text(String::new());
        }
        self.sync_clear_label();
    }

    /// The prompt left the screen for good (sent): nothing to restore.
    fn reset_prompt_state(&mut self) {
        self.set_prompt_text(String::new());
        self.prompt_origin = None;
        self.cleared_prompt = None;
        self.sync_clear_label();
    }

    fn sync_clear_label(&mut self) {
        let label = if self.cleared_prompt.is_some() {
            RESTORE_LABEL
        } else {
            CLEAR_LABEL
        };
        self.tree
            .update::<Button, _>(self.clear_btn, |b, cx| b.set_label(cx, label));
    }

    /// The branch of the worktree the prompt's quotes were taken from (its
    /// cwd when it is no longer listed).
    fn prompt_origin_name(&self) -> Option<String> {
        let origin = self.prompt_origin.as_ref()?;
        let branch = self
            .raw_data
            .repos
            .iter()
            .flat_map(|r| &r.worktrees)
            .find(|w| w.cwd == *origin)
            .map(|w| w.branch.clone());
        Some(branch.unwrap_or_else(|| origin.clone()))
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
        // The quotes describe another worktree's diff: its agent, not this
        // one's, is the one to receive them.
        if self.prompt_origin.is_some()
            && self.prompt_origin.as_ref() != self.current_worktree().map(|w| &w.cwd)
        {
            let origin = self.prompt_origin_name().unwrap_or_default();
            self.toast(format!(
                "この指示は worktree {origin} の引用から作られています。{origin} に戻って送信するか、指示をクリアしてください"
            ));
            return;
        }
        let Some(pane) = self.target() else {
            self.toast(self.no_pane_message("送信"));
            return;
        };
        let pane_id = pane.pane_id;
        // A shell would run each line of the prompt (and the diff in it) as
        // a command.
        if let Some(shell) = pane.plain_shell() {
            self.toast(format!(
                "ペイン #{pane_id} はシェル ({shell}) です。指示はコマンドとして実行されてしまうため送信しません。エージェントのペインを送信先に選んでください"
            ));
            return;
        }
        match &self.source {
            Source::Demo | Source::Snapshot => {
                self.toast(format!("[DEMO] ペイン #{pane_id} へ指示を送信しました"));
                self.reset_prompt_state();
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
        let Some(pane) = self.target() else {
            self.toast(format!("{}: {joined}", self.no_pane_message("送信")));
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
                self.reset_prompt_state();
            }
        }
        if let Some(snap) = latest {
            self.link = snap.link;
            self.can_focus = snap.can_focus;
            self.can_send = snap.can_send;
            // A commit's text does not depend on the diff mode, so a snapshot
            // built in another mode still carries a usable one.
            let got_commit = snap.selected_commit.is_some();
            if let Some((cwd, sha, diff)) = snap.selected_commit {
                self.commit_diffs.insert((cwd, sha), diff);
            }
            // The UI is the only source of mode changes; a snapshot in another
            // mode was built before the feed saw our SetDiffMode.
            if snap.diff_mode != self.diff_mode {
                if got_commit {
                    self.rebuild_diff_view(text);
                }
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
                self.leave_commit();
                self.sync_views(text);
            }
            PaletteAction::SelectCommit { root, cwd, sha } => {
                let found = self.filtered.repos.iter().enumerate().find_map(|(ri, r)| {
                    if r.root != root {
                        return None;
                    }
                    let wi = r.worktrees.iter().position(|w| w.cwd == cwd)?;
                    r.worktrees[wi]
                        .commits
                        .iter()
                        .any(|c| c.sha == sha)
                        .then_some((ri, wi))
                });
                let Some((ri, wi)) = found else {
                    self.toast(format!(
                        "コミット {} はもう一覧にありません",
                        short_sha(&sha)
                    ));
                    return;
                };
                self.sel_repo = ri;
                self.sel_wt = wi;
                self.sel_file = 0;
                self.sel_commit = None;
                self.select_commit(sha, text);
                self.tree
                    .update::<TabView, _>(self.side_tabs, |tabs, cx| tabs.set_selected(cx, 2));
            }
            PaletteAction::FocusPane(id) => match &self.source {
                Source::Demo | Source::Snapshot => {
                    self.toast(format!("[DEMO] ペイン #{id} にフォーカス"))
                }
                Source::Live(ch) => ch.send(Command::Focus(id)),
            },
            PaletteAction::OpenShell => self.open_shell_here(),
            PaletteAction::OpenEditor => self.open_in_editor(),
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
            if *sc == SHELL_SHORTCUT {
                self.open_shell_here();
                return;
            }
            if *sc == EDITOR_SHORTCUT {
                self.open_in_editor();
                return;
            }
            if *sc == QUOTE_SHORTCUT {
                self.quote_selected_diff();
                return;
            }
        }

        if message.source == self.focus_btn
            && let Some(MenuChosen(idx)) = message.downcast_ref::<MenuChosen>()
        {
            match idx {
                0 => self.open_shell_here(),
                _ => self.open_in_editor(),
            }
            return;
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
                self.leave_commit();
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
                self.rebuild_commits(text);
                self.refresh_target(text);
            }
            return;
        }

        if message.source == self.commit_table
            && let Some(RowSelectionChanged {
                lead: Some(row), ..
            }) = message.downcast_ref::<RowSelectionChanged>()
        {
            self.select_commit_row(*row, text);
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
            if *idx <= 1 {
                self.leave_commit();
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

        if let Some(seg) = self.target_seg
            && message.source == seg
            && let Some(SelectionChanged(idx)) = message.downcast_ref::<SelectionChanged>()
        {
            self.choose_target(*idx, text);
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
            } else if message.source == self.clear_btn {
                self.clear_or_restore_prompt();
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
        if let Some(host) = self.tree.layout(self.browser_host) {
            let size = browser_size_for(host.rect.width, host.rect.height);
            if size != self.browser_size {
                self.browser_size = size;
                self.rebuild_browser(text);
                changed |= self.tree.prepare(viewport, text).is_some();
            }
        }
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

const COMMIT_ROW: f32 = 38.0;

/// A tab page is only as tall as its content, so the table is sized to its
/// rows (header included) up to a cap, beyond which it scrolls.
fn commit_table_height(rows: usize) -> f32 {
    (TABLE_HEADER + COMMIT_ROW * rows as f32).clamp(TABLE_HEADER + COMMIT_ROW, 640.0)
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Shell-escapes one word: single-quotes it (embedded `'` as `'\''`) unless
/// it only holds characters no shell treats specially. Mirrors noa-app's
/// drop/paste escaping.
pub(crate) fn shell_escape(s: &str) -> String {
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

/// Open a shell in the selected worktree / the selected file in the editor.
const SHELL_SHORTCUT: Shortcut = Shortcut::command('t');
const EDITOR_SHORTCUT: Shortcut = Shortcut::command('e');
const NO_FILE_TO_OPEN: &str = "開くファイルが選択されていません";
/// Quotes the selected diff lines into the prompt.
const QUOTE_SHORTCUT: Shortcut = Shortcut::command('l');
const QUOTE_LEAD: &str = "以下の箇所を修正してください:";
/// The prompt box would otherwise collapse to one line in the tab page.
const PROMPT_MIN_HEIGHT: f32 = 220.0;
const CLEAR_LABEL: &str = "クリア";
const RESTORE_LABEL: &str = "元に戻す";

const BROWSER_HEIGHT: f32 = 240.0;
const BROWSER_COLUMN: f32 = 310.0;
/// The browser shows three columns (repository, worktree, file) when its
/// pane is wide enough, and never squeezes a column below this.
const BROWSER_MIN_COLUMN: f32 = 200.0;

/// The browser's (height, column width) for a pane of `width` x `height`,
/// rounded to whole pixels so sub-pixel layout jitter does not rebuild it.
fn browser_size_for(width: f32, height: f32) -> (f32, f32) {
    (
        height.round().max(1.0),
        (width / 3.0).floor().max(BROWSER_MIN_COLUMN),
    )
}

/// The commit table's rows (title, detail) and the sha selected in it.
type CommitTableState = (Vec<(String, String)>, Option<String>);

/// The commit table's rows: a title line and a detail line each.
#[derive(Default)]
struct CommitRows {
    rows: Vec<(String, String)>,
}

impl TableData for CommitRows {
    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn cell(&self, row: usize, _column: usize) -> Cow<'_, str> {
        Cow::Borrowed(self.rows.get(row).map_or("", |r| r.0.as_str()))
    }

    fn cell_detail(&self, row: usize, _column: usize) -> Option<Cow<'_, str>> {
        self.rows.get(row).map(|r| Cow::Borrowed(r.1.as_str()))
    }
}

/// (old, new) line numbers of one diff line; `None` on a side the line is not on.
type LineNumbers = (Option<u32>, Option<u32>);

/// The line-number gutter shown in front of a unified diff.
struct Gutter {
    /// One entry per line of the diff; both `None` outside a hunk.
    numbers: Vec<LineNumbers>,
    /// Digits of each of the two number columns.
    digits: usize,
}

impl Gutter {
    /// `None` when `diff` has no hunk (a note such as "no changes").
    fn of(diff: &str) -> Option<Self> {
        let mut numbers = Vec::new();
        let mut cursor: Option<(u32, u32)> = None;
        let mut seen_hunk = false;
        for line in diff.lines() {
            if let Some(start) = parse_hunk_header(line) {
                cursor = Some(start);
                seen_hunk = true;
                numbers.push((None, None));
                continue;
            }
            if line.starts_with("diff --git") {
                cursor = None;
            }
            let Some((old, new)) = cursor.as_mut() else {
                numbers.push((None, None));
                continue;
            };
            numbers.push(match line.as_bytes().first() {
                Some(b'+') => {
                    *new += 1;
                    (None, Some(*new - 1))
                }
                Some(b'-') => {
                    *old += 1;
                    (Some(*old - 1), None)
                }
                Some(b'\\') => (None, None),
                _ => {
                    *old += 1;
                    *new += 1;
                    (Some(*old - 1), Some(*new - 1))
                }
            });
        }
        if !seen_hunk {
            return None;
        }
        let widest = numbers
            .iter()
            .flat_map(|&(o, n)| [o, n])
            .flatten()
            .max()
            .unwrap_or(0);
        Some(Self {
            numbers,
            digits: widest.to_string().len().max(3),
        })
    }

    /// Characters the gutter puts in front of every line.
    fn width(&self) -> usize {
        2 * self.digits + 4
    }

    fn line(&self, raw: &str, (old, new): LineNumbers) -> LogLine {
        let num = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        let text = format!("{:>w$} {:>w$} │ {raw}", num(old), num(new), w = self.digits);
        let style = match (old, new) {
            (None, Some(_)) => ADDED,
            (Some(_), None) => REMOVED,
            (Some(_), Some(_)) => PLAIN,
            // Outside a hunk the line is a header, or a hunk's `@@` line.
            (None, None) => diff_line_style(raw),
        };
        styled_line(text, style)
    }

    /// `text` (lines copied from the log view) without the gutter.
    fn strip(&self, text: &str) -> String {
        text.lines()
            .map(|line| line.chars().skip(self.width()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The first new-side line number among `lines` (a selection).
    fn first_new_line(&self, lines: std::ops::Range<usize>) -> Option<u32> {
        self.numbers.get(lines)?.iter().find_map(|&(_, new)| new)
    }

    /// "L12-L18" for the new-side lines in `lines`, or "旧L12-L18" when the
    /// selection only holds removed lines; `None` without numbered lines.
    fn range_label(&self, lines: std::ops::Range<usize>) -> Option<String> {
        let picked = self.numbers.get(lines)?;
        let span = |side: &dyn Fn(&LineNumbers) -> Option<u32>| {
            let mut it = picked.iter().filter_map(side);
            let first = it.next()?;
            Some((first, it.last().unwrap_or(first)))
        };
        let (prefix, (lo, hi)) = match span(&|n| n.1) {
            Some(range) => ("", range),
            None => ("旧", span(&|n| n.0)?),
        };
        Some(if lo == hi {
            format!("{prefix}L{lo}")
        } else {
            format!("{prefix}L{lo}-L{hi}")
        })
    }
}

/// The new-side path of a `diff --git a/<old> b/<new>` line, unquoted.
fn diff_git_new_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git ")?;
    if let Some(unclosed) = rest.strip_suffix('"') {
        // A C-quoted `"b/…"`; a `"` inside it is escaped as `\"`, so the
        // last ` "b/` opens it.
        let open = unclosed.rfind(" \"b/")?;
        return c_unquote(&rest[open + 1..])?
            .strip_prefix("b/")
            .map(str::to_owned);
    }
    // `a/<p> b/<p>`: the two halves are the same path unless renamed.
    let half = rest.len().checked_sub(5).filter(|n| n % 2 == 0)? / 2;
    if let (Some("a/"), Some(old), Some(" b/"), Some(new)) = (
        rest.get(..2),
        rest.get(2..2 + half),
        rest.get(2 + half..5 + half),
        rest.get(5 + half..),
    ) && old == new
    {
        return Some(new.to_owned());
    }
    rest.rfind(" b/").map(|i| rest[i + 3..].to_owned())
}

/// A path git C-quoted (`"…"` with `\"`, `\\`, `\t`, octal bytes, …).
fn c_unquote(quoted: &str) -> Option<String> {
    let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = Vec::new();
    let mut bytes = inner.bytes();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let escaped = bytes.next()?;
        out.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 0x0b,
            b'f' => 0x0c,
            b'r' => b'\r',
            b'0'..=b'3' => {
                let digit = |d: u8| (b'0'..=b'7').contains(&d).then(|| d - b'0');
                let (mid, low) = (digit(bytes.next()?)?, digit(bytes.next()?)?);
                ((escaped - b'0') << 6) | (mid << 3) | low
            }
            other => other,
        });
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// The (old, new) start lines of a `@@ -a,b +c,d @@` hunk header.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.strip_prefix('+')?.split(' ').next()?;
    let start = |range: &str| range.split(',').next()?.parse::<u32>().ok();
    Some((start(old)?, start(new)?))
}

/// A diff line's (text color, background).
type LineStyle = (Option<Color>, Option<Color>);

const PLAIN: LineStyle = (None, None);
const ADDED: LineStyle = (Some(DIFF_ADD_FG), Some(DIFF_ADD_BG));
const REMOVED: LineStyle = (Some(DIFF_DEL_FG), Some(DIFF_DEL_BG));

/// The style of a diff line told apart by its prefix alone.
fn diff_line_style(line: &str) -> LineStyle {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff --git") {
        (Some(DIFF_META_FG), None)
    } else if line.starts_with("@@") {
        (Some(DIFF_HUNK_FG), Some(DIFF_HUNK_BG))
    } else if line.starts_with('+') {
        ADDED
    } else if line.starts_with('-') {
        REMOVED
    } else {
        PLAIN
    }
}

fn styled_line(text: String, (color, background): LineStyle) -> LogLine {
    let mut line = LogLine::new(text);
    if let Some(color) = color {
        line = line.color(color);
    }
    if let Some(fill) = background {
        line = line.background(fill);
    }
    line
}

fn colorize_diff_line(line: &str) -> LogLine {
    styled_line(line.to_owned(), diff_line_style(line))
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

    const CWD: &str = "/repo";

    const C_NEW: &str = "aaaaaaa1111111111111111111111111111111aa";
    const C_OLD: &str = "bbbbbbb2222222222222222222222222222222bb";
    const COMMIT_SHOW: &str = "commit aaaaaaa1\nAuthor:     t <t@example.com>\n\n    newer change\n\ndiff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -10,3 +10,4 @@ fn f\n ctx\n-old\n+new1\n+new2\n ctx2\n";

    fn commit(sha: &str, subject: &str) -> CommitInfo {
        CommitInfo {
            sha: sha.into(),
            short: sha[..7].into(),
            subject: subject.into(),
            author: "t".into(),
            time: now_secs() - 3 * 3600,
            files: 2,
            diff: String::new(),
        }
    }

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
                    merge_conflicts: Vec::new(),
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
                    commits: vec![commit(C_NEW, "newer change"), commit(C_OLD, "older change")],
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
            selected_commit: None,
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

    const NUMBERED: &str = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -10,3 +10,4 @@ fn f\n ctx\n-old\n+new1\n+new2\n ctx2\n";

    #[test]
    fn gutter_numbers_follow_hunk_headers() {
        let gutter = Gutter::of(NUMBERED).unwrap();
        assert_eq!(
            gutter.numbers,
            vec![
                (None, None),
                (None, None),
                (None, None),
                (None, None),
                (Some(10), Some(10)),
                (Some(11), None),
                (None, Some(11)),
                (None, Some(12)),
                (Some(12), Some(13)),
            ]
        );
        assert!(Gutter::of("(このワークツリーに差分はありません)").is_none());
    }

    #[test]
    fn quoting_numbered_lines_strips_the_gutter_and_names_the_range() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", NUMBERED))));
        assert!(
            h.diff_lines()[6].ends_with("11 │ +new1"),
            "{:?}",
            h.diff_lines()
        );
        let diff_log = h.screen.diff_log;
        h.screen
            .tree
            .update::<LogView, _>(diff_log, |log, cx| log.select_lines(cx, 5..8));
        h.screen.quote_selected_diff();
        assert_eq!(
            prompt_text(&h.screen),
            "以下の箇所を修正してください:\n[main / a.rs L11-L12]\n```diff\n-old\n+new1\n+new2\n```\n"
        );

        h.screen
            .tree
            .update::<LogView, _>(diff_log, |log, cx| log.select_lines(cx, 5..6));
        h.screen.quote_selected_diff();
        assert!(prompt_text(&h.screen).ends_with("[main / a.rs 旧L11]\n```diff\n-old\n```\n"));
    }

    fn pane(id: u64, name: &str, tail: &str) -> PaneSummary {
        PaneSummary {
            pane_id: id,
            name: name.into(),
            label: None,
            process: None,
            activity: AgentActivity::None,
            busy: false,
            attention: false,
            terminal_tail: tail.into(),
        }
    }

    /// A snapshot whose only worktree has `panes`.
    fn snapshot_with_panes(panes: Vec<PaneSummary>) -> Snapshot {
        let mut snap = snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1")));
        snap.data.repos[0].worktrees[0].panes = panes;
        snap
    }

    fn select_lines(h: &mut LiveHarness, lines: std::ops::Range<usize>) {
        let diff_log = h.screen.diff_log;
        h.screen
            .tree
            .update::<LogView, _>(diff_log, |log, cx| log.select_lines(cx, lines));
    }

    fn sent(h: &LiveHarness) -> Vec<(u64, String)> {
        h.drain()
            .into_iter()
            .filter_map(|c| match c {
                Command::SendText { pane_id, text, .. } => Some((pane_id, text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn quotes_append_in_order_and_duplicates_are_ignored() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", NUMBERED))));
        select_lines(&mut h, 5..6);
        h.screen.quote_selected_diff();
        select_lines(&mut h, 6..8);
        h.screen.quote_selected_diff();
        let two = "以下の箇所を修正してください:\n\
                   [main / a.rs 旧L11]\n```diff\n-old\n```\n\
                   [main / a.rs L11-L12]\n```diff\n+new1\n+new2\n```\n";
        assert_eq!(prompt_text(&h.screen), two);

        h.screen.toasts.clear();
        select_lines(&mut h, 5..6);
        h.screen.quote_selected_diff();
        assert_eq!(prompt_text(&h.screen), two);
        assert_eq!(h.screen.toasts, vec!["同じ範囲はすでに引用済みです"]);
    }

    #[test]
    fn quote_starts_on_a_fresh_line_after_a_typed_remark() {
        let mut h = LiveHarness::new();
        set_prompt(&mut h.screen, "メモ");
        h.screen.quote_selected_diff();
        assert_eq!(
            prompt_text(&h.screen),
            "メモ\n[main / a.rs]\n```diff\n+a v1\n```\n"
        );
    }

    fn clear_label(h: &LiveHarness) -> String {
        h.screen
            .tree()
            .get::<Button>(h.screen.clear_btn)
            .unwrap()
            .label()
            .to_owned()
    }

    #[test]
    fn clear_empties_the_prompt_and_restores_it_once() {
        let mut h = LiveHarness::new();
        h.screen.quote_selected_diff();
        let quoted = prompt_text(&h.screen);
        assert_eq!(clear_label(&h), "クリア");
        h.screen.clear_or_restore_prompt();
        assert_eq!(prompt_text(&h.screen), "");
        assert_eq!(clear_label(&h), "元に戻す");
        // Quoting after a clear starts a new prompt; restoring merges.
        h.screen.quote_selected_diff();
        h.screen.clear_or_restore_prompt();
        assert_eq!(
            prompt_text(&h.screen),
            format!("{}\n{quoted}", quoted.trim_end())
        );
        assert_eq!(clear_label(&h), "クリア");
    }

    #[test]
    fn restore_after_clear_brings_back_the_text() {
        let mut h = LiveHarness::new();
        set_prompt(&mut h.screen, "直して");
        h.screen.clear_or_restore_prompt();
        assert_eq!(prompt_text(&h.screen), "");
        h.screen.clear_or_restore_prompt();
        assert_eq!(prompt_text(&h.screen), "直して");
        // Clearing an empty prompt does nothing.
        set_prompt(&mut h.screen, "");
        h.screen.clear_or_restore_prompt();
        assert_eq!(clear_label(&h), "クリア");
    }

    #[test]
    fn chosen_pane_receives_the_prompt_and_the_terminal_follows() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot_with_panes(vec![
            pane(7, "zsh", "one\r\n"),
            pane(8, "claude", "two\r\n"),
        ]));
        assert_eq!(h.screen.target().unwrap().pane_id, 7);
        assert!(matches!(
            h.screen.last_terminal_key,
            TerminalShown::Pane(7, _)
        ));
        assert!(h.screen.target_seg.is_some());

        h.screen.choose_target(1, &mut h.text);
        assert_eq!(h.screen.target().unwrap().pane_id, 8);
        assert!(matches!(
            h.screen.last_terminal_key,
            TerminalShown::Pane(8, _)
        ));

        // The pick survives a refresh of the same worktree.
        h.deliver(snapshot_with_panes(vec![
            pane(7, "zsh", "one\r\n"),
            pane(8, "claude", "two\r\nmore\r\n"),
        ]));
        assert_eq!(h.screen.target().unwrap().pane_id, 8);

        set_prompt(&mut h.screen, "直して");
        h.drain();
        h.screen.send_review_prompt();
        assert_eq!(sent(&h), vec![(8, "直して".to_owned())]);

        // Focusing and dropped files go to the same pane as the prompt.
        h.drain();
        h.screen.focus_selected_pane();
        h.screen
            .send_dropped_files(&[PathBuf::from("/tmp/plain.txt")]);
        let targets: Vec<u64> = h
            .drain()
            .into_iter()
            .filter_map(|c| match c {
                Command::Focus(id) | Command::SendText { pane_id: id, .. } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(targets, vec![8, 8]);
    }

    #[test]
    fn target_resets_when_its_pane_disappears() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot_with_panes(vec![
            pane(7, "zsh", ""),
            pane(8, "claude", "two\r\n"),
        ]));
        h.screen.choose_target(1, &mut h.text);
        assert_eq!(h.screen.target_pane, Some(8));

        h.deliver(snapshot_with_panes(vec![pane(7, "zsh", "")]));
        // The pick is dropped and the pane now shown is pinned instead.
        assert_eq!(h.screen.target_pane, Some(7));
        assert_eq!(h.screen.target().unwrap().pane_id, 7);
        assert!(matches!(
            h.screen.last_terminal_key,
            TerminalShown::Pane(7, _)
        ));
        assert!(h.screen.target_seg.is_none());

        // It does not come back when the pane reappears.
        h.deliver(snapshot_with_panes(vec![
            pane(7, "zsh", ""),
            pane(8, "claude", ""),
        ]));
        assert_eq!(h.screen.target().unwrap().pane_id, 7);
    }

    #[test]
    fn target_resets_when_another_worktree_is_selected() {
        let mut h = LiveHarness::new();
        let mut snap = snapshot_with_panes(vec![pane(7, "zsh", ""), pane(8, "claude", "")]);
        let mut other = snap.data.repos[0].worktrees[0].clone();
        other.cwd = "/repo-wt-b".into();
        other.branch = "b".into();
        other.panes = vec![pane(9, "zsh", ""), pane(8, "claude", "")];
        snap.data.repos[0].worktrees.push(other);
        h.deliver(snap);
        h.screen.choose_target(1, &mut h.text);
        assert_eq!(h.screen.target_pane, Some(8));
        h.screen.sel_wt = 1;
        h.screen.sync_views(&mut h.text);
        // Pane 8 is in both worktrees here, but the pick belonged to the
        // first: the second's primary pane is pinned instead.
        assert_eq!(h.screen.target_pane, Some(9));
    }

    #[test]
    fn quoting_from_another_worktree_warns_that_the_prompt_is_mixed() {
        let mut h = LiveHarness::new();
        let mut snap = snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1")));
        let mut other = snap.data.repos[0].worktrees[0].clone();
        other.cwd = "/repo-wt-b".into();
        other.branch = "b".into();
        other.panes[0].pane_id = 9;
        snap.data.repos[0].worktrees.push(other);
        h.deliver(snap);
        h.screen.toasts.clear();
        h.screen.quote_selected_diff();
        assert!(h.screen.toasts.is_empty());

        h.screen.sel_wt = 1;
        h.screen.sync_views(&mut h.text);
        h.screen.quote_selected_diff();
        assert_eq!(h.screen.toasts.len(), 1, "{:?}", h.screen.toasts);
        assert!(h.screen.toasts[0].contains("複数の worktree"));
        assert!(
            h.screen.toasts[0].contains("worktree main"),
            "{:?}",
            h.screen.toasts
        );
        assert!(prompt_text(&h.screen).contains("[main / a.rs]"));
        assert!(prompt_text(&h.screen).contains("[b / a.rs]"));
    }

    #[test]
    fn quote_shortcut_is_shown_in_the_status_bar() {
        let h = LiveHarness::new();
        let hints: Vec<String> = h
            .screen
            .tree()
            .children(h.screen.status_trailing)
            .iter()
            .filter_map(|id| {
                h.screen
                    .tree()
                    .get::<Label>(*id)
                    .map(|l| l.text().to_owned())
            })
            .collect();
        assert!(hints.contains(&"引用".to_owned()), "{hints:?}");
    }

    #[test]
    fn browser_follows_its_pane_size() {
        let mut screen = RepoBrowser::demo();
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
        let host = screen.tree().layout(screen.browser_host).unwrap().rect;
        assert_eq!(
            screen.browser_size,
            browser_size_for(host.width, host.height)
        );
        let browser = screen.tree().layout(screen.browser).unwrap().rect;
        assert_eq!(browser.height, host.height.round());
    }

    #[test]
    fn dropped_paths_toast_only_the_send_result() {
        let mut h = LiveHarness::new();
        h.screen.toasts.clear();
        h.screen
            .send_dropped_files(&[PathBuf::from("/tmp/plain.txt")]);
        assert!(h.screen.toasts.is_empty(), "{:?}", h.screen.toasts);
    }

    // ── Open a shell / the editor in a worktree ──────────────────────────────

    fn open_commands(cmds: Vec<Command>) -> Vec<Command> {
        cmds.into_iter()
            .filter(|c| matches!(c, Command::OpenShell { .. } | Command::OpenEditor { .. }))
            .collect()
    }

    #[test]
    fn shell_opens_in_the_worktree_root() {
        let mut h = LiveHarness::new();
        h.drain();
        h.screen.open_shell_here();
        let sent = open_commands(h.drain());
        assert!(
            matches!(sent.as_slice(), [Command::OpenShell { cwd }] if cwd == CWD),
            "{sent:?}"
        );
    }

    #[test]
    fn shell_opens_for_a_worktree_without_panes() {
        let mut h = LiveHarness::new();
        let mut snap = snapshot(DiffMode::Unstaged, None);
        snap.data.repos[0].worktrees[0].panes.clear();
        h.deliver(snap);
        h.drain();
        h.screen.open_shell_here();
        assert!(matches!(
            open_commands(h.drain()).as_slice(),
            [Command::OpenShell { .. }]
        ));
    }

    #[test]
    fn editor_opens_the_selected_file_at_the_selected_line() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", NUMBERED))));
        select_lines(&mut h, 5..8);
        h.drain();
        h.screen.open_in_editor();
        let sent = open_commands(h.drain());
        assert!(
            matches!(
                sent.as_slice(),
                [Command::OpenEditor { cwd, path, line: 11 }] if cwd == CWD && path == "a.rs"
            ),
            "{sent:?}"
        );
    }

    #[test]
    fn editor_line_falls_back_to_the_first_hunk_then_one() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", NUMBERED))));
        // Nothing selected: the first hunk's new start.
        assert_eq!(h.screen.editor_target().unwrap().1, 10);
        // Only a removed line selected: no new-side number, so the hunk start.
        select_lines(&mut h, 5..6);
        assert_eq!(h.screen.editor_target().unwrap().1, 10);
        // The first selected line with a new-side number wins.
        select_lines(&mut h, 4..9);
        assert_eq!(h.screen.editor_target().unwrap().1, 10);
        select_lines(&mut h, 7..9);
        assert_eq!(h.screen.editor_target().unwrap().1, 12);
        // A diff without hunks (a note, or still loading): line 1.
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", "(差分なし)"))));
        assert_eq!(h.screen.editor_target().unwrap().1, 1);
    }

    #[test]
    fn gutter_picks_the_first_new_side_line_of_a_selection() {
        let gutter = Gutter::of(NUMBERED).unwrap();
        assert_eq!(gutter.first_new_line(5..8), Some(11));
        assert_eq!(gutter.first_new_line(5..6), None);
        assert_eq!(gutter.first_new_line(0..4), None);
    }

    #[test]
    fn palette_offers_shell_and_editor_and_runs_them() {
        let mut h = LiveHarness::new();
        h.screen.open_command_palette();
        let (_, actions) = h.screen.active_palette.clone().unwrap();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, PaletteAction::OpenShell))
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, PaletteAction::OpenEditor))
        );
        h.drain();
        h.screen
            .run_palette_action(PaletteAction::OpenShell, &mut h.text);
        h.screen
            .run_palette_action(PaletteAction::OpenEditor, &mut h.text);
        let sent = open_commands(h.drain());
        assert!(matches!(
            sent.as_slice(),
            [
                Command::OpenShell { .. },
                Command::OpenEditor { line: 1, .. }
            ]
        ));
    }

    #[test]
    fn missing_scopes_toast_instead_of_sending() {
        let mut h = LiveHarness::new();
        let mut snap = snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1")));
        snap.can_focus = false;
        h.deliver(snap);
        h.drain();
        h.screen.toasts.clear();
        h.screen.open_shell_here();
        h.screen.open_in_editor();
        assert!(open_commands(h.drain()).is_empty());
        assert_eq!(
            h.screen.toasts,
            vec!["server-scopes に control が必要です"; 2]
        );

        // control without input: a shell opens, the editor command cannot.
        let mut snap = snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1")));
        snap.can_send = false;
        h.deliver(snap);
        h.drain();
        h.screen.toasts.clear();
        h.screen.open_shell_here();
        h.screen.open_in_editor();
        assert!(matches!(
            open_commands(h.drain()).as_slice(),
            [Command::OpenShell { .. }]
        ));
        assert_eq!(h.screen.toasts, vec!["server-scopes に input が必要です"]);
    }

    #[test]
    fn deleted_files_are_not_opened_in_the_editor() {
        let mut h = LiveHarness::new();
        let mut snap = snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1")));
        snap.data.repos[0].worktrees[0].files[0].status = "D".into();
        h.deliver(snap);
        h.drain();
        h.screen.toasts.clear();
        h.screen.open_in_editor();
        assert!(open_commands(h.drain()).is_empty());
        assert_eq!(h.screen.toasts, vec!["a.rs は削除済みのため開けません"]);
    }

    #[test]
    fn server_errors_from_opening_reach_the_toasts() {
        let mut h = LiveHarness::new();
        h.screen.toasts.clear();
        h.feed
            .notices
            .send("シェルを開けませんでした: boom".into())
            .unwrap();
        h.deliver(snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1"))));
        assert_eq!(h.screen.toasts, vec!["シェルを開けませんでした: boom"]);
    }

    #[test]
    fn demo_shows_a_toast_instead_of_opening() {
        let mut screen = RepoBrowser::demo();
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
        screen.open_shell_here();
        screen.open_in_editor();
        assert_eq!(screen.toasts.len(), 2, "{:?}", screen.toasts);
        assert!(
            screen.toasts[0].starts_with("[DEMO] "),
            "{:?}",
            screen.toasts
        );
        assert!(
            screen.toasts[1].starts_with("[DEMO] "),
            "{:?}",
            screen.toasts
        );
        assert!(screen.toasts[1].contains(':'), "{:?}", screen.toasts);
    }

    /// The live snapshot plus a worktree with no pane, `orch/foo`, that also
    /// edits `a.rs` and is predicted to conflict on it when merged.
    fn snapshot_with_paneless_worktree() -> Snapshot {
        let mut snap = snapshot(DiffMode::Unstaged, None);
        let mut foo = snap.data.repos[0].worktrees[0].clone();
        foo.cwd = "/repo-wt/foo".into();
        foo.branch = "orch/foo".into();
        foo.name = "orch/foo".into();
        foo.panes.clear();
        foo.files.truncate(1);
        foo.merge_conflicts = vec!["a.rs".into()];
        snap.data.repos[0].worktrees.push(foo);
        snap.data.compute_conflicts();
        snap
    }

    fn badge_labels(screen: &RepoBrowser, parent: WidgetId) -> Vec<String> {
        screen
            .tree()
            .children(parent)
            .iter()
            .filter_map(|id| screen.tree().get::<Badge>(*id).and_then(Badge::label))
            .collect()
    }

    fn browser_title(screen: &RepoBrowser, kind: impl Fn(&NodeKind) -> bool) -> String {
        let item = screen.node_map.iter().position(kind).unwrap();
        let browser = screen.tree().get::<BrowserView>(screen.browser).unwrap();
        browser.title(item).to_owned()
    }

    #[test]
    fn paneless_worktree_is_shown_and_refuses_send_and_focus() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot_with_paneless_worktree());
        h.screen.sel_wt = 1;
        h.screen.sel_file = 0;
        h.screen.sync_views(&mut h.text);

        let row = browser_title(&h.screen, |k| matches!(k, NodeKind::Worktree(0, 1)));
        assert!(row.contains("ペインなし"), "{row}");
        assert!(row.contains("⛔マージ衝突 1"), "{row}");
        assert!(row.contains("⚠衝突"), "same-file marking kept: {row}");
        let file = browser_title(&h.screen, |k| matches!(k, NodeKind::File(0, 1, 0)));
        assert!(file.contains('⛔') && file.contains('⚠'), "{file}");
        // The worktree with a pane predicts no merge conflict on its a.rs.
        let main_file = browser_title(&h.screen, |k| matches!(k, NodeKind::File(0, 0, 0)));
        assert!(!main_file.contains('⛔'), "{main_file}");

        let header = badge_labels(&h.screen, h.screen.diff_header);
        assert!(
            header.iter().any(|b| b.contains("マージで衝突: a.rs")),
            "{header:?}"
        );
        let status = badge_labels(&h.screen, h.screen.status_leading);
        assert!(status.contains(&"⛔ マージ衝突 1".to_owned()), "{status:?}");

        let term = h.screen.tree().get::<Terminal>(h.screen.terminal).unwrap();
        let shown: Vec<String> = (0..6).map(|row| term.line_text(row)).collect();
        assert!(shown[0].contains("ペインなし: orch/foo"), "{shown:?}");
        assert!(
            shown.iter().any(|l| l.contains("/repo-wt/foo")),
            "{shown:?}"
        );

        h.drain();
        h.screen.toasts.clear();
        set_prompt(&mut h.screen, "直して");
        h.screen.send_review_prompt();
        h.screen.focus_selected_pane();
        h.screen
            .send_dropped_files(&[PathBuf::from("/tmp/plain.txt")]);
        let sent: Vec<Command> = h
            .drain()
            .into_iter()
            .filter(|c| matches!(c, Command::SendText { .. } | Command::Focus(_)))
            .collect();
        assert!(sent.is_empty(), "{sent:?}");
        assert_eq!(h.screen.toasts.len(), 3, "{:?}", h.screen.toasts);
        for toast in &h.screen.toasts {
            assert!(
                toast.contains("orch/foo はペインなしの worktree"),
                "{toast}"
            );
        }
        assert!(h.screen.toasts[0].contains("送信できません"));
        assert!(h.screen.toasts[1].contains("フォーカスできません"));
        assert_eq!(prompt_text(&h.screen), "直して");
    }

    #[test]
    fn no_pane_token_narrows_the_browser() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot_with_paneless_worktree());
        h.screen.tokens = vec!["no-pane".into()];
        h.screen.sync_views(&mut h.text);
        let wts = &h.screen.filtered.repos[0].worktrees;
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].branch, "orch/foo");
        h.screen.tokens = vec!["merge-conflict".into()];
        h.screen.sync_views(&mut h.text);
        assert_eq!(h.screen.filtered.repos[0].worktrees[0].branch, "orch/foo");
    }

    fn commit_loads(cmds: &[Command]) -> Vec<String> {
        cmds.iter()
            .filter_map(|c| match c {
                Command::LoadCommit { sha, .. } => Some(sha.clone()),
                _ => None,
            })
            .collect()
    }

    fn snapshot_with_commit(sha: &str, show: &str) -> Snapshot {
        let mut snap = snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1")));
        snap.selected_commit = Some((CWD.into(), sha.into(), show.into()));
        snap
    }

    #[test]
    fn selecting_a_commit_loads_its_diff_through_the_feed() {
        let mut h = LiveHarness::new();
        h.drain();
        h.screen.select_commit_row(1, &mut h.text);
        assert_eq!(commit_loads(&h.drain()), vec![C_OLD]);
        assert_eq!(h.diff_lines(), vec!["コミットを取得しています…"]);
        h.deliver(snapshot_with_commit(C_OLD, COMMIT_SHOW));
        assert!(
            h.diff_lines()[0].contains("commit aaaaaaa1"),
            "{:?}",
            h.diff_lines()
        );
        // The gutter numbers the commit's hunks like a file diff's.
        assert!(
            h.diff_lines()[11].ends_with("11 │ +new1"),
            "{:?}",
            h.diff_lines()
        );
        // A periodic snapshot must not ask for the same commit again.
        h.deliver(snapshot_with_commit(C_OLD, COMMIT_SHOW));
        assert!(commit_loads(&h.drain()).is_empty());
        let table = h
            .screen
            .tree()
            .get::<TableView>(h.screen.commit_table)
            .unwrap();
        assert_eq!(table.selected_rows(), vec![1]);
    }

    #[test]
    fn a_commit_text_from_a_stale_mode_snapshot_is_kept() {
        let mut h = LiveHarness::new();
        h.screen.select_commit_row(0, &mut h.text);
        h.screen.set_diff_mode(DiffMode::Staged, &mut h.text);
        h.deliver(snapshot_with_commit(C_NEW, COMMIT_SHOW));
        assert_eq!(h.screen.diff_mode, DiffMode::Staged);
        assert!(
            h.diff_lines().iter().any(|l| l.ends_with("+new2")),
            "{:?}",
            h.diff_lines()
        );
    }

    #[test]
    fn quoting_from_a_commit_names_its_short_sha() {
        let mut h = LiveHarness::new();
        h.screen.select_commit_row(0, &mut h.text);
        h.deliver(snapshot_with_commit(C_NEW, COMMIT_SHOW));
        let diff_log = h.screen.diff_log;
        // Lines 10-11 of the log: `-old`, `+new1`.
        h.screen
            .tree
            .update::<LogView, _>(diff_log, |log, cx| log.select_lines(cx, 10..12));
        h.screen.quote_selected_diff();
        assert_eq!(
            prompt_text(&h.screen),
            "以下の箇所を修正してください:\n[main / commit aaaaaaa]\n```diff\n-old\n+new1\n```\n"
        );
    }

    #[test]
    fn choosing_a_file_returns_from_the_commit_to_the_file_diff() {
        let mut h = LiveHarness::new();
        h.screen.select_commit_row(0, &mut h.text);
        h.deliver(snapshot_with_commit(C_NEW, COMMIT_SHOW));
        assert!(h.screen.current_commit().is_some());
        h.drain();
        let action = PaletteAction::SelectFile {
            root: CWD.into(),
            cwd: CWD.into(),
            path: "a.rs".into(),
        };
        h.screen.run_palette_action(action, &mut h.text);
        assert!(h.screen.current_commit().is_none());
        // The feed is asked for the file again, and the commit tab is cleared.
        assert_eq!(load_diff_paths(&h.drain()), vec!["a.rs"]);
        assert_eq!(h.diff_lines(), vec!["+a v1"]);
        let table = h
            .screen
            .tree()
            .get::<TableView>(h.screen.commit_table)
            .unwrap();
        assert!(table.selected_rows().is_empty());
    }

    #[test]
    fn palette_lists_each_commit_and_jumps_to_it() {
        let mut h = LiveHarness::new();
        h.screen.open_command_palette();
        let (_, actions) = h.screen.active_palette.clone().unwrap();
        let shas: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                PaletteAction::SelectCommit { sha, .. } => Some(sha.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(shas, vec![C_NEW, C_OLD]);
        let choose_old = actions
            .iter()
            .find(|a| matches!(a, PaletteAction::SelectCommit { sha, .. } if sha == C_OLD))
            .cloned()
            .unwrap();
        h.drain();
        h.screen.run_palette_action(choose_old, &mut h.text);
        assert_eq!(h.screen.current_commit().unwrap().subject, "older change");
        assert_eq!(commit_loads(&h.drain()), vec![C_OLD]);
        let tabs = h.screen.tree().get::<TabView>(h.screen.side_tabs).unwrap();
        assert_eq!(tabs.selected(), 2);
    }

    #[test]
    fn a_commit_that_vanishes_falls_back_to_the_file() {
        let mut h = LiveHarness::new();
        h.screen.select_commit_row(0, &mut h.text);
        let mut snap = snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1")));
        snap.data.repos[0].worktrees[0].commits.clear();
        h.deliver(snap);
        assert!(h.screen.sel_commit.is_none());
        assert_eq!(h.diff_lines(), vec!["+a v1"]);
    }

    #[test]
    fn demo_commits_show_their_inline_diff_without_the_feed() {
        let mut screen = RepoBrowser::demo();
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
        assert_eq!(screen.current_worktree().unwrap().commits.len(), 3);
        screen.select_commit_row(0, &mut text);
        let commit = screen.current_commit().unwrap().clone();
        let log = screen.tree().get::<LogView>(screen.diff_log).unwrap();
        assert!(
            log.all_text().contains(&commit.subject),
            "{}",
            log.all_text()
        );
        assert!(log.all_text().contains("+    guard.drop_removed"));
    }

    // ── Review findings: sending and opening go where they claim ─────────────

    /// The live snapshot with a second worktree `b` (pane #9) of the repo.
    fn snapshot_with_two_worktrees() -> Snapshot {
        let mut snap = snapshot(DiffMode::Unstaged, Some(("a.rs", "+a v1")));
        let mut other = snap.data.repos[0].worktrees[0].clone();
        other.cwd = "/repo-wt-b".into();
        other.branch = "b".into();
        other.panes[0].pane_id = 9;
        snap.data.repos[0].worktrees.push(other);
        snap
    }

    #[test]
    fn a_prompt_quoted_in_one_worktree_is_not_sent_from_another() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot_with_two_worktrees());
        h.screen.quote_selected_diff();
        h.screen.sel_wt = 1;
        h.screen.sync_views(&mut h.text);
        h.drain();
        h.screen.toasts.clear();
        h.screen.send_review_prompt();
        assert!(sent(&h).is_empty());
        assert_eq!(h.screen.toasts.len(), 1, "{:?}", h.screen.toasts);
        assert!(
            h.screen.toasts[0].contains("worktree main の引用"),
            "{:?}",
            h.screen.toasts
        );
        // Back in its worktree it goes to that worktree's pane.
        h.screen.sel_wt = 0;
        h.screen.sync_views(&mut h.text);
        h.screen.send_review_prompt();
        assert_eq!(sent(&h).len(), 1);
        assert_eq!(h.screen.target().unwrap().pane_id, 7);
        // Once cleared, a prompt typed in `b` is sent there.
        h.screen.clear_or_restore_prompt();
        h.screen.sel_wt = 1;
        h.screen.sync_views(&mut h.text);
        set_prompt(&mut h.screen, "直して");
        h.screen.send_review_prompt();
        assert_eq!(sent(&h), vec![(9, "直して".to_owned())]);
    }

    fn agent(id: u64, process: &str, activity: AgentActivity) -> PaneSummary {
        PaneSummary {
            process: Some(process.into()),
            activity,
            ..pane(id, process, "")
        }
    }

    #[test]
    fn the_target_shown_first_stays_when_the_primary_pane_changes() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot_with_panes(vec![agent(
            8,
            "claude",
            AgentActivity::Running,
        )]));
        h.deliver(snapshot_with_panes(vec![
            agent(7, "zsh", AgentActivity::None),
            agent(8, "claude", AgentActivity::Running),
        ]));
        assert_eq!(h.screen.target().unwrap().pane_id, 8);
        // claude finishes: the primary pane is now the zsh in front.
        h.deliver(snapshot_with_panes(vec![
            agent(7, "zsh", AgentActivity::None),
            agent(8, "claude", AgentActivity::Done),
        ]));
        assert_eq!(h.screen.target_pane, Some(8));
        set_prompt(&mut h.screen, "直して");
        h.drain();
        h.screen.send_review_prompt();
        assert_eq!(sent(&h), vec![(8, "直して".to_owned())]);
    }

    #[test]
    fn a_review_prompt_is_never_submitted_to_a_plain_shell() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot_with_panes(vec![agent(
            7,
            "zsh",
            AgentActivity::None,
        )]));
        set_prompt(&mut h.screen, "```diff\n rm -rf build\n```");
        h.drain();
        h.screen.toasts.clear();
        h.screen.send_review_prompt();
        assert!(sent(&h).is_empty());
        assert_eq!(h.screen.toasts.len(), 1, "{:?}", h.screen.toasts);
        assert!(
            h.screen.toasts[0].contains("#7 はシェル (zsh)")
                && h.screen.toasts[0].contains("コマンドとして実行"),
            "{:?}",
            h.screen.toasts
        );
        assert_eq!(prompt_text(&h.screen), "```diff\n rm -rf build\n```");
        // Dropped file paths are typed, not submitted, so they still go.
        h.screen
            .send_dropped_files(&[PathBuf::from("/tmp/plain.txt")]);
        assert!(matches!(
            h.drain().as_slice(),
            [Command::SendText {
                pane_id: 7,
                submit: false,
                ..
            }]
        ));
    }

    #[test]
    fn pane_output_does_not_rebuild_the_target_control() {
        let mut h = LiveHarness::new();
        h.deliver(snapshot_with_panes(vec![
            pane(7, "zsh", "one\r\n"),
            pane(8, "claude", "two\r\n"),
        ]));
        let seg = h.screen.target_seg.unwrap();
        h.deliver(snapshot_with_panes(vec![
            pane(7, "zsh", "one\r\nmore\r\n"),
            pane(8, "claude", "two\r\nmore\r\n"),
        ]));
        assert_eq!(h.screen.target_seg, Some(seg));
        // The terminal tab still follows the output.
        assert!(matches!(
            &h.screen.last_terminal_key,
            TerminalShown::Pane(7, tail) if tail.contains("more")
        ));
        // A pane that changes state is shown again.
        let mut busy = pane(8, "claude", "");
        busy.activity = AgentActivity::Running;
        h.deliver(snapshot_with_panes(vec![pane(7, "zsh", ""), busy]));
        assert_ne!(h.screen.target_seg, Some(seg));
    }

    /// `git show` of a commit touching `x` (lines 5-13), `src/y.rs` (14-21)
    /// and deleting `gone.rs` (22-28).
    const COMMIT_THREE_FILES: &str = "commit aaaaaaa1\nAuthor:     t <t@example.com>\n\n    newer change\n\n\
diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -10,3 +10,4 @@ fn f\n ctx\n-old\n+new1\n+new2\n ctx2\n\
diff --git a/src/y.rs b/src/y.rs\nindex 1..2 100644\n--- a/src/y.rs\n+++ b/src/y.rs\n@@ -1,2 +3,3 @@\n a\n+b\n c\n\
diff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\nindex 1..0\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";

    fn editor_opens(h: &mut LiveHarness) -> Vec<(String, u32)> {
        h.drain();
        h.screen.open_in_editor();
        open_commands(h.drain())
            .into_iter()
            .filter_map(|c| match c {
                Command::OpenEditor { path, line, .. } => Some((path, line)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_editor_opens_the_commit_file_of_the_selected_line() {
        let mut h = LiveHarness::new();
        // b.rs is selected in the file list; it must never be what opens.
        h.select_file(1);
        h.screen.select_commit_row(0, &mut h.text);
        h.deliver(snapshot_with_commit(C_NEW, COMMIT_THREE_FILES));
        assert_eq!(h.screen.sel_file, 1);
        // No selection: the first file at its first hunk.
        assert_eq!(editor_opens(&mut h), vec![("x".to_owned(), 10)]);
        // A line in the second file, at its new-side number.
        select_lines(&mut h, 20..21);
        assert_eq!(editor_opens(&mut h), vec![("src/y.rs".to_owned(), 4)]);
        // Its hunk header alone: that file's first hunk.
        select_lines(&mut h, 18..19);
        assert_eq!(editor_opens(&mut h), vec![("src/y.rs".to_owned(), 3)]);
        // A selection running into the next file stays in the file it starts in.
        select_lines(&mut h, 11..21);
        assert_eq!(editor_opens(&mut h), vec![("x".to_owned(), 11)]);
        // The commit header: the first file.
        select_lines(&mut h, 0..2);
        assert_eq!(editor_opens(&mut h), vec![("x".to_owned(), 10)]);
        // The palette entry names the same file.
        select_lines(&mut h, 20..21);
        h.screen.open_command_palette();
        let (palette, _) = h.screen.active_palette.clone().unwrap();
        let titles: Vec<String> = h
            .screen
            .tree()
            .get::<CommandPalette>(palette)
            .map(|p| p.commands().iter().map(|c| c.title().to_owned()).collect())
            .unwrap_or_default();
        assert!(
            titles.iter().any(|t| t == "エディタで開く: src/y.rs"),
            "{titles:?}"
        );
        assert!(
            !titles.iter().any(|t| t == "エディタで開く: b.rs"),
            "{titles:?}"
        );
    }

    #[test]
    fn a_file_the_commit_deleted_is_not_opened() {
        let mut h = LiveHarness::new();
        h.screen.select_commit_row(0, &mut h.text);
        h.deliver(snapshot_with_commit(C_NEW, COMMIT_THREE_FILES));
        select_lines(&mut h, 28..29);
        h.screen.toasts.clear();
        assert!(editor_opens(&mut h).is_empty());
        assert_eq!(
            h.screen.toasts,
            vec!["gone.rs はこのコミットで削除されたため開けません"]
        );
        // Still loading: nothing to open yet.
        h.screen.select_commit_row(1, &mut h.text);
        h.screen.toasts.clear();
        assert!(editor_opens(&mut h).is_empty());
        assert_eq!(h.screen.toasts, vec!["コミットの差分を取得しています…"]);
    }

    #[test]
    fn diff_git_lines_name_the_new_path() {
        let new = |line: &str| diff_git_new_path(line);
        assert_eq!(new("diff --git a/x b/x").as_deref(), Some("x"));
        assert_eq!(new("diff --git a/a b/c b/a b/c").as_deref(), Some("a b/c"));
        assert_eq!(
            new("diff --git a/old name.rs b/new name.rs").as_deref(),
            Some("new name.rs")
        );
        assert_eq!(
            new(r#"diff --git "a/\346\227\245 \"q\".rs" "b/\346\227\245 \"q\".rs""#).as_deref(),
            Some("日 \"q\".rs")
        );
        assert_eq!(
            new(r#"diff --git a/plain "b/t\tab""#).as_deref(),
            Some("t\tab")
        );
        assert_eq!(new("--- a/x"), None);
    }

    #[test]
    fn commit_age_reads_in_japanese() {
        use crate::model::relative_age;
        assert_eq!(relative_age(1000, 1030), "たった今");
        assert_eq!(relative_age(0, 25 * 60), "25分前");
        assert_eq!(relative_age(0, 3 * 3600 + 5), "3時間前");
        assert_eq!(relative_age(0, 2 * 86_400), "2日前");
        assert_eq!(relative_age(0, 60 * 86_400), "2か月前");
        assert_eq!(relative_age(0, 800 * 86_400), "2年前");
    }
}
