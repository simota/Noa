//! The start view: the target (a git diff or the tree as it is), the spec
//! file, the request, the person's own criteria, whether to confirm them,
//! the judge, the verifier panel (rows of agent, model, effort and
//! strategy), the isolation, the round limit, and the list of past
//! verifications to open, resume or set up again.

use std::path::{Path, PathBuf};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Overflow, Point as LayoutPoint, Rect as LayoutRect,
    Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, CheckState, Checkbox, ComboBox, Container, ControlSize, Label, NumberInput,
    PopUpButton, ScrollView, SegmentMode, SegmentedControl, TextArea, TextField, Tone,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use verify::model::types::{
    AgentChoice, AgentKind, AgentSetup, BuiltinStrategy, DEFAULT_MAX_ROUNDS, Head, Isolation,
    MAX_MAX_ROUNDS, MAX_PANEL, MIN_PANEL, Millis, RoundNo, Session, Strategy, StrategyChoice,
    Target, TargetKind, VerifierChoice,
};
use verify::store::Listed;

use super::{Action, caption, clear_children, gap, spacer, tag};
use crate::installed::Installed;
use crate::live::Link;

/// How many past verifications the start view lists.
pub(super) const PAST_LIMIT: usize = 12;

const CARD_WIDTH: f32 = 660.0;
const FIELD_WIDTH: f32 = 612.0;

pub(super) struct Parts {
    pub(super) root: WidgetId,
    pub(super) kind: WidgetId,
    pub(super) repo: WidgetId,
    /// The base ref's column, hidden for a state target.
    base_col: WidgetId,
    pub(super) base: WidgetId,
    pub(super) head: WidgetId,
    pub(super) filter: WidgetId,
    pub(super) spec: WidgetId,
    /// 選択…: picks the spec file in an open panel.
    pub(super) pick_spec: WidgetId,
    pub(super) request: WidgetId,
    pub(super) criteria: WidgetId,
    pub(super) confirm: WidgetId,
    pub(super) isolation: WidgetId,
    pub(super) max_rounds: WidgetId,
    /// Holds the round limit input, made again when its cap changes.
    rounds_row: WidgetId,
    /// The cap the round limit input has.
    round_cap: RoundNo,
    /// The form was filled from a stored verification (同じ設定で再検証):
    /// the agent check keeps its panel and only drops kinds not installed.
    pub(super) refilled: bool,
    /// Holds the judge's kind menu, so it can be rebuilt for other kinds.
    judge_host: WidgetId,
    pub(super) judge: WidgetId,
    /// The kinds the agent menus offer, in menu order: a menu index maps
    /// to a kind through this list, never through `AgentKind::ALL`.
    kinds: Vec<AgentKind>,
    pub(super) judge_setup: SetupParts,
    panel_list: WidgetId,
    pub(super) panel: Vec<PanelRow>,
    pub(super) add_row: WidgetId,
    /// Holds 「自動で割り当て」 (`ui::auto` draws it), in the agents'
    /// section header.
    pub(super) auto_host: WidgetId,
    pub(super) start_button: WidgetId,
    note: WidgetId,
    past: WidgetId,
}

impl Parts {
    pub(super) fn kinds(&self) -> &[AgentKind] {
        &self.kinds
    }

    /// The note under the form, which `ui::auto` adds its lines to.
    pub(super) fn note(&self) -> WidgetId {
        self.note
    }

    /// Each verifier row's identity, in form order: its host widget, whose
    /// id stays with the row while rows around it go and is never reused.
    pub(super) fn row_ids(&self) -> Vec<WidgetId> {
        self.panel.iter().map(|row| row.host).collect()
    }
}

/// Which agents are installed, as far as the screen knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Detection {
    /// The check is still running.
    Pending,
    Done(Installed),
}

impl Detection {
    /// Every kind installed: what the demo and stored views assume, so
    /// they stay deterministic.
    pub(super) fn all_installed() -> Self {
        Self::Done(Installed::Found(AgentKind::ALL.to_vec()))
    }

    /// The kinds the agent menus offer: the installed ones, or all of
    /// them while nothing is known (pending, unknown, none installed; a
    /// menu cannot be empty, and the start button is off where it matters).
    pub(super) fn menu_kinds(&self) -> Vec<AgentKind> {
        match self {
            Self::Done(Installed::Found(kinds)) if !kinds.is_empty() => kinds.clone(),
            _ => AgentKind::ALL.to_vec(),
        }
    }

    /// A verification can be started: the check is done and found an
    /// agent (or could not tell, and all are offered).
    pub(super) fn startable(&self) -> bool {
        match self {
            Self::Pending => false,
            Self::Done(Installed::Found(kinds)) => !kinds.is_empty(),
            Self::Done(Installed::Unknown(_)) => true,
        }
    }

    /// Why the start button is off (also what shows in the note), or the
    /// caveat when it is on.
    fn note(&self) -> Option<String> {
        match self {
            Self::Pending => Some("インストール済みのエージェントを確認しています…".to_owned()),
            Self::Done(Installed::Unknown(reason)) => Some(format!(
                "インストール済みエージェントを確認できませんでした（{reason}）。すべて表示しています"
            )),
            Self::Done(Installed::Found(kinds)) if kinds.is_empty() => Some(
                "claude / codex / agy のいずれもログインシェルの PATH に見つかりません".to_owned(),
            ),
            Self::Done(Installed::Found(_)) => None,
        }
    }

    /// The agents a fix can be asked of: the installed ones (all of them
    /// when the check could not tell), or why none can be yet.
    pub(super) fn fix_kinds(&self) -> Result<Vec<AgentKind>, String> {
        match self {
            Self::Done(Installed::Found(kinds)) if !kinds.is_empty() => Ok(kinds.clone()),
            Self::Done(Installed::Unknown(_)) => Ok(AgentKind::ALL.to_vec()),
            _ => Err(self.note().unwrap_or_default()),
        }
    }

    pub(super) fn blocked_reason(&self) -> String {
        self.note().unwrap_or_default()
    }

    /// Why `session` cannot be started again (resumed or given rounds),
    /// when the check has not finished or an agent it uses is not
    /// installed; `None` when it can, or when the check could not tell.
    pub(super) fn resume_blocker(&self, session: &Session) -> Option<String> {
        match self {
            Self::Pending => self.note(),
            Self::Done(Installed::Unknown(_)) => None,
            Self::Done(Installed::Found(installed)) => {
                let mut missing = Vec::new();
                for part in std::iter::once(&session.judge).chain(&session.verifiers) {
                    if !installed.contains(&part.kind) && !missing.contains(&part.kind) {
                        missing.push(part.kind);
                    }
                }
                (!missing.is_empty()).then(|| {
                    format!(
                        "この検証は {} を使いますが、インストールされていないため再開できません",
                        missing
                            .iter()
                            .map(|k| k.command())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
            }
        }
    }
}

/// "インストールされていないため除外: codex, agy".
pub(super) fn left_out_message(kinds: &[AgentKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|k| k.command()).collect();
    format!("インストールされていないため除外: {}", names.join(", "))
}

/// One verifier row: agent kind, model, effort, strategy, custom strategy
/// text, and its remove button.
pub(super) struct PanelRow {
    host: WidgetId,
    pub(super) kind: WidgetId,
    pub(super) setup: SetupParts,
    pub(super) strategy: WidgetId,
    pub(super) custom: WidgetId,
    pub(super) remove: WidgetId,
}

/// What the start form holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Form {
    pub kind: TargetKind,
    pub repo: String,
    /// Empty: the remote default branch, then `main`, then `master`.
    pub base: String,
    /// Empty: the work tree.
    pub head: String,
    pub filter: String,
    /// The spec file, relative to the repository or absolute inside it.
    pub spec: String,
    pub request: String,
    /// The person's criteria, one per line.
    pub criteria: String,
    /// 基準を確認してから検証.
    pub confirm: bool,
    pub isolation: Isolation,
    pub max_rounds: RoundNo,
    /// The highest limit the form takes: `MAX_MAX_ROUNDS`, or a stored
    /// verification's limit after +N when it is set up again.
    pub round_cap: RoundNo,
    pub judge: AgentChoice,
    pub panel: Vec<VerifierChoice>,
}

impl Default for Form {
    /// The clean form while every kind is on offer.
    fn default() -> Self {
        Self::for_kinds(&AgentKind::ALL)
    }
}

/// The strategies a clean panel gives its rows, in order (L1-9's default
/// panel: 仕様適合, 反証, 回帰).
const DEFAULT_STRATEGIES: [BuiltinStrategy; 3] = [
    BuiltinStrategy::Conformance,
    BuiltinStrategy::Falsify,
    BuiltinStrategy::Regression,
];

/// The first built-in strategy none of `panel` holds.
fn unheld_strategy(panel: &[VerifierChoice]) -> BuiltinStrategy {
    BuiltinStrategy::ALL
        .into_iter()
        .find(|s| {
            !panel
                .iter()
                .any(|row| row.strategy == StrategyChoice::Builtin(*s))
        })
        .unwrap_or(BuiltinStrategy::Conformance)
}

/// A row of `kind` on the first strategy `panel` lacks.
fn next_row(kind: AgentKind, panel: &[VerifierChoice]) -> VerifierChoice {
    VerifierChoice {
        agent: kind.into(),
        strategy: StrategyChoice::Builtin(unheld_strategy(panel)),
    }
}

impl Form {
    /// The clean form for the available `kinds` (not empty): the judge is
    /// the first; the panel is L1-9's default (claude 仕様適合, codex 反証,
    /// claude 回帰) where those kinds are offered, else one row per kind,
    /// padded to the minimum by repeating the first.
    pub(crate) fn for_kinds(kinds: &[AgentKind]) -> Self {
        let first = kinds.first().copied().unwrap_or(AgentKind::ALL[0]);
        let default_panel = [AgentKind::Claude, AgentKind::Codex, AgentKind::Claude];
        let mut panel: Vec<VerifierChoice> = if default_panel.iter().all(|k| kinds.contains(k)) {
            default_panel
                .iter()
                .zip(DEFAULT_STRATEGIES)
                .map(|(&kind, strategy)| VerifierChoice {
                    agent: kind.into(),
                    strategy: StrategyChoice::Builtin(strategy),
                })
                .collect()
        } else {
            kinds
                .iter()
                .zip(DEFAULT_STRATEGIES)
                .map(|(&kind, strategy)| VerifierChoice {
                    agent: kind.into(),
                    strategy: StrategyChoice::Builtin(strategy),
                })
                .collect()
        };
        while panel.len() < MIN_PANEL {
            let row = next_row(first, &panel);
            panel.push(row);
        }
        Self {
            kind: TargetKind::Diff,
            repo: String::new(),
            base: String::new(),
            head: String::new(),
            filter: String::new(),
            spec: String::new(),
            request: String::new(),
            criteria: String::new(),
            confirm: true,
            isolation: Isolation::Worktree,
            max_rounds: DEFAULT_MAX_ROUNDS,
            round_cap: MAX_MAX_ROUNDS,
            judge: first.into(),
            panel,
        }
    }

    /// This form with only the available `kinds` (not empty): verifiers of
    /// another kind are dropped (the panel is padded back to the minimum
    /// with the first kind), a judge of another kind becomes the first kind
    /// on its defaults. Also the kinds left out, in order, once each.
    fn restricted(mut self, kinds: &[AgentKind]) -> (Self, Vec<AgentKind>) {
        let mut left_out = Vec::new();
        let mut note = |kind: AgentKind| {
            if !left_out.contains(&kind) {
                left_out.push(kind);
            }
        };
        if !kinds.contains(&self.judge.kind) {
            note(self.judge.kind);
            self.judge = kinds[0].into();
        }
        self.panel.retain(|row| {
            let keep = kinds.contains(&row.agent.kind);
            if !keep {
                note(row.agent.kind);
            }
            keep
        });
        while self.panel.len() < MIN_PANEL {
            let row = next_row(kinds[0], &self.panel);
            self.panel.push(row);
        }
        (self, left_out)
    }
}

/// Where the note a verification set up again adds to its request starts:
/// an earlier one is cut there, so notes do not pile up run after run.
pub(super) const AGAIN_MARK: &str = "（前回の検証 ";
/// The line under the note's heading.
const AGAIN_ASK: &str = "前回は次の基準と不具合が合格を妨げました。直っているか、直し方に新しい問題がないかを確かめてください:";
/// The line a note [`again_note`] writes ends with: what the widget wrote
/// stops there, so a line the person adds after it stays theirs.
const AGAIN_END: &str = "（前回の検証の要対応はここまで）";

/// Where in `request` a note [`again_note`] wrote lies: from its heading
/// alone on a line, with the line [`AGAIN_ASK`] right under it, through the
/// line [`AGAIN_END`], with the blank line before it. Without that closing
/// line it is not one (nothing of the person's is taken for it).
fn again_block(request: &str) -> Option<std::ops::Range<usize>> {
    let mut at = 0;
    let lines: Vec<(usize, &str)> = request
        .split_inclusive('\n')
        .map(|line| {
            let start = at;
            at += line.len();
            (start, line)
        })
        .collect();
    let bare = |i: usize| lines.get(i).map(|(_, l)| l.trim_end_matches('\n'));
    let heading = |i: usize| {
        bare(i).is_some_and(|head| {
            head.starts_with(AGAIN_MARK) && head.ends_with('）') && head.contains(" の総合判定: ")
        }) && bare(i + 1) == Some(AGAIN_ASK)
    };
    for (i, (start, _)) in lines.iter().enumerate() {
        if !heading(i) {
            continue;
        }
        let Some(close) = (i + 2..lines.len())
            .take_while(|&j| !heading(j))
            .find(|&j| bare(j) == Some(AGAIN_END))
        else {
            continue;
        };
        let end = close + 1;
        let from = match i.checked_sub(1) {
            Some(prev) if lines[prev].1.trim().is_empty() => lines[prev].0,
            _ => *start,
        };
        let to = lines.get(end).map_or(request.len(), |(at, _)| *at);
        return Some(from..to);
    }
    None
}

/// What a verification set up again asks on top of `session`'s request:
/// its verdict and every blocking criterion and defect, to be checked
/// again (L1-7 item 3). `None` without a verdict.
pub(super) fn again_note(session: &Session) -> Option<String> {
    let verdict = session.verdict()?;
    let mut out = format!(
        "{AGAIN_MARK}{} の総合判定: {}）\n{AGAIN_ASK}",
        session.id,
        verdict.decision.label()
    );
    for id in &verdict.blocking {
        let line = if let Some(c) = session.criterion(id) {
            let ruling = session
                .ruling(id)
                .map(|r| r.kind.label())
                .unwrap_or("未判定");
            format!("\n- {id} [{ruling}] {}", c.statement)
        } else if let Some(d) = session.defect(id) {
            format!("\n- {id} [{}] {}", d.severity.label(), d.claim)
        } else {
            format!("\n- {id}")
        };
        out.push_str(&line);
    }
    if verdict.blocking.is_empty() {
        out.push_str("\n- （なし）");
    }
    out.push('\n');
    out.push_str(AGAIN_END);
    Some(out)
}

impl Form {
    /// The form that starts `session` again: the same target, spec,
    /// panel, isolation, round limit and request, the frozen criteria as
    /// the person's (so they are kept word for word), and the request
    /// followed by [`again_note`] (an earlier such note is dropped first).
    pub(super) fn from_session(session: &Session) -> Self {
        let target = &session.target;
        let mut form = Self {
            kind: target.kind,
            repo: target.repo.display().to_string(),
            base: target.base.clone().unwrap_or_default(),
            head: match &target.head {
                Head::Worktree => String::new(),
                Head::Ref(head) => head.clone(),
            },
            filter: target.paths.join("\n"),
            spec: target.spec.clone().unwrap_or_default(),
            request: String::new(),
            criteria: if session.criteria.is_empty() {
                session.user_criteria.join("\n")
            } else {
                session
                    .criteria
                    .iter()
                    .map(|c| c.statement.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            confirm: session.confirm,
            isolation: session.isolation,
            max_rounds: session.max_rounds,
            round_cap: session.max_rounds.max(MAX_MAX_ROUNDS),
            judge: AgentChoice {
                kind: session.judge.kind,
                setup: session.judge.setup.clone(),
            },
            panel: session
                .verifiers
                .iter()
                .map(|part| VerifierChoice {
                    agent: AgentChoice {
                        kind: part.kind,
                        setup: part.setup.clone(),
                    },
                    strategy: match &part.strategy {
                        Some(Strategy::Custom { text, .. }) => StrategyChoice::Custom(text.clone()),
                        Some(Strategy::Builtin { id }) => StrategyChoice::Builtin(*id),
                        None => StrategyChoice::Builtin(BuiltinStrategy::Conformance),
                    },
                })
                .collect(),
        };
        let request = session.request.as_str();
        form.request = match (again_block(request), again_note(session)) {
            (_, None) => request.to_owned(),
            (Some(block), Some(note)) => {
                let before = request[..block.start].trim_end();
                let after = &request[block.end..];
                let mut out = before.to_owned();
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&note);
                if !after.is_empty() {
                    out.push('\n');
                    out.push_str(after);
                }
                out
            }
            (None, Some(note)) => match request.trim_end() {
                "" => note,
                asked => format!("{asked}\n\n{note}"),
            },
        };
        form
    }
}

/// One entry per line, blank lines left out: a path may hold spaces.
pub(super) fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Form {
    /// The target the form describes, with the typed repository checked.
    pub(crate) fn target(&self) -> Result<Target, String> {
        let spec = match self.spec.trim() {
            "" => None,
            typed if typed.starts_with('~') => Some(expand(typed)?.display().to_string()),
            typed => Some(typed.to_owned()),
        };
        Ok(Target {
            kind: self.kind,
            repo: dir_path(&self.repo, "リポジトリ")?,
            base: match self.kind {
                TargetKind::Diff => Some(self.base.trim().to_owned()).filter(|b| !b.is_empty()),
                TargetKind::State => None,
            },
            head: match self.head.trim() {
                "" => Head::Worktree,
                other => Head::Ref(other.to_owned()),
            },
            paths: lines(&self.filter),
            spec,
        })
    }

    /// The person's criteria, one per line as typed (blank lines dropped).
    pub(crate) fn user_criteria(&self) -> Vec<String> {
        lines(&self.criteria)
    }
}

fn card_container() -> Container {
    Container::new(Style {
        flex_direction: FlexDirection::Column,
        flex_shrink: 0.0,
        gap: gap(12.0),
        padding: LayoutRect::length(24.0),
        size: LayoutSize {
            width: length(CARD_WIDTH),
            height: auto(),
        },
        max_size: LayoutSize {
            width: percent(1.0),
            height: auto(),
        },
        ..Default::default()
    })
    .background(Role::Surface)
    .border(1.0, Role::Separator)
    .radius(10.0)
}

const PAST_WIDTH: f32 = 380.0;
const PAST_HEIGHT: f32 = 600.0;

/// The card beside the form that lists the past runs; returns the list's
/// column.
fn insert_past_card(tree: &mut UiTree, parent: WidgetId) -> WidgetId {
    let card = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 1.0,
            size: LayoutSize {
                width: length(PAST_WIDTH),
                height: length(PAST_HEIGHT),
            },
            min_size: LayoutSize {
                width: length(0.0),
                height: length(0.0),
            },
            max_size: LayoutSize {
                width: percent(1.0),
                height: percent(1.0),
            },
            ..Default::default()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(10.0),
    );
    super::heading(tree, card, "過去の検証");
    super::scroll_column(tree, card, 12.0)
}

fn section(tree: &mut UiTree, parent: WidgetId, title: &str) -> WidgetId {
    let col = tree.insert(Some(parent), Container::column().gap(6.0));
    tree.insert(Some(col), Label::new(title).font_size(13.0).weight(600));
    col
}

fn field(tree: &mut UiTree, parent: WidgetId, label: &str, placeholder: &str) -> WidgetId {
    caption(tree, parent, label);
    tree.insert(Some(parent), TextField::new(placeholder, FIELD_WIDTH))
}

pub(super) fn insert(tree: &mut UiTree, parent: WidgetId) -> Parts {
    let root = tree.insert(
        Some(parent),
        ScrollView::new(Style {
            overflow: LayoutPoint {
                x: Overflow::Hidden,
                y: Overflow::Scroll,
            },
            ..super::fill()
        }),
    );
    let centered = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::CENTER,
            padding: LayoutRect::length(24.0),
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            min_size: LayoutSize {
                width: auto(),
                height: percent(1.0),
            },
            ..Default::default()
        }),
    );
    let cards = tree.insert(
        Some(centered),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::START,
            gap: gap(16.0),
            margin: LayoutRect {
                left: length(0.0),
                right: length(0.0),
                top: auto(),
                bottom: auto(),
            },
            max_size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let card = tree.insert(Some(cards), card_container());
    tree.insert(
        Some(card),
        Label::new("新しい検証").font_size(17.0).weight(600),
    );
    tree.insert(
        Some(card),
        Label::new(
            "手法の違う検証者が、確認した受け入れ基準に沿って変更を各自の worktree で実際に動かして\
             確かめ、次のラウンドで互いの結果を追試します。判定役が保存・ハッシュした証跡から基準ごとに判定し、\
             総合判定とレポートを出します。",
        )
        .font_size(12.0)
        .color(Role::Muted)
        .wrap(true),
    );

    let target = section(tree, card, "対象");
    let kind = tree.insert(
        Some(target),
        SegmentedControl::new(
            TargetKind::ALL.iter().map(|k| k.label()),
            SegmentMode::SelectOne,
        )
        .with_selected(&[0]),
    );
    let repo = field(
        tree,
        target,
        "リポジトリ（作業ツリー内のパス）",
        "例: ~/repos/my-app",
    );
    let refs = tree.insert(Some(target), Container::row().gap(12.0));
    let base_col = tree.insert(Some(refs), Container::column().gap(6.0));
    caption(tree, base_col, "比較元");
    let base = tree.insert(
        Some(base_col),
        TextField::new("自動（origin の既定ブランチ）", 300.0),
    );
    let head_col = tree.insert(Some(refs), Container::column().gap(6.0));
    caption(tree, head_col, "検証する版");
    let head = tree.insert(
        Some(head_col),
        TextField::new("作業ツリー（未コミットを含む）", 300.0),
    );
    caption(tree, target, "パスの絞り込み（任意・1行に1つ）");
    let filter = tree.insert(
        Some(target),
        TextArea::new("例: crates/noa-app\ndocs/release notes.md").min_lines(2),
    );
    tree.insert(
        Some(target),
        Label::new(
            "開始時に対象をスナップショットとして保存します（リポジトリ内の .noa/verify/ に置かれ、git には載りません）。git の作業ツリーでのみ検証できます。",
        )
        .font_size(11.0)
        .color(Role::Muted)
        .wrap(true),
    );

    let spec_col = section(tree, card, "仕様ファイル（任意）");
    let spec_row = tree.insert(
        Some(spec_col),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let spec = tree.insert(
        Some(spec_row),
        TextField::new("例: docs/specs/cwd-inherit.md（リポジトリ内）", 520.0),
    );
    let pick_spec = tree.insert(
        Some(spec_row),
        Button::new("選択…").control_size(ControlSize::Small),
    );
    caption(
        tree,
        spec_col,
        "判定役が受け入れ基準を作るときの主な拠り所になります（テキスト・1 MiB まで）",
    );

    let ask = section(tree, card, "依頼");
    let request = tree.insert(
        Some(ask),
        TextArea::new(
            "例: 新しいタブがフォーカス中のペインの作業ディレクトリを引き継ぐようにした変更です",
        )
        .min_lines(2),
    );

    let criteria_col = section(tree, card, "受け入れ基準（任意・1行に1つ）");
    let criteria = tree.insert(
        Some(criteria_col),
        TextArea::new("空欄なら判定役が依頼と仕様から作成").min_lines(3),
    );
    let confirm = tree.insert(
        Some(criteria_col),
        Checkbox::new("基準を確認してから検証").checked(true),
    );
    caption(
        tree,
        criteria_col,
        "書いた行はそのままの文言で基準になります。オンにすると、判定役の基準を確かめて直してから検証を始めます",
    );

    let judge_col = tree.insert(Some(card), Container::column().gap(6.0));
    let judge_head = tree.insert(
        Some(judge_col),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    tree.insert(
        Some(judge_head),
        Label::new("判定役").font_size(13.0).weight(600),
    );
    spacer(tree, judge_head);
    let auto_host = tree.insert(
        Some(judge_head),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let judge_row = tree.insert(
        Some(judge_col),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    // Nothing is known of what is installed yet: every kind is offered
    // until the check reports ([`apply_kinds`]).
    let kinds = AgentKind::ALL.to_vec();
    let judge_host = tree.insert(Some(judge_row), Container::row());
    let judge = insert_kind_menu(tree, judge_host, &kinds, kinds[0]);
    let judge_setup = insert_setup(tree, judge_row, kinds[0]);
    caption(
        tree,
        judge_col,
        "基準を作り、毎ラウンド各基準を判定し、最後に総合判定を出します（自分では何も実行しません）",
    );

    let panel_col = section(
        tree,
        card,
        &format!("検証者（{MIN_PANEL}〜{MAX_PANEL}体）・手法"),
    );
    let panel_list = tree.insert(Some(panel_col), Container::column().gap(8.0));
    let panel = Form::for_kinds(&kinds)
        .panel
        .iter()
        .map(|choice| insert_row(tree, panel_list, &kinds, choice))
        .collect();
    let panel_foot = tree.insert(
        Some(panel_col),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let add_row = tree.insert(
        Some(panel_foot),
        Button::new("＋ 検証者を追加").control_size(ControlSize::Small),
    );
    tree.insert(
        Some(panel_foot),
        Label::new("同じエージェントの2体目以降は claude-2 … として参加します")
            .font_size(11.0)
            .color(Role::Muted),
    );

    let isolation_col = section(tree, card, "隔離");
    let isolation = tree.insert(
        Some(isolation_col),
        SegmentedControl::new(
            Isolation::ALL.iter().map(|i| i.label()),
            SegmentMode::SelectOne,
        )
        .with_selected(&[0]),
    );
    caption(
        tree,
        isolation_col,
        "個別 worktree: 検証者ごとに対象を別の worktree に作り、そこで自由に動かします。共有: 全員がこのチェックアウトで読み取りだけ行います（再実行なし）",
    );

    let rounds_col = section(tree, card, "ラウンド上限");
    let rounds_row = tree.insert(
        Some(rounds_col),
        Container::row().align_items(AlignItems::CENTER).gap(10.0),
    );
    let max_rounds = tree.insert(
        Some(rounds_row),
        rounds_input(MAX_MAX_ROUNDS, DEFAULT_MAX_ROUNDS),
    );
    caption(
        tree,
        rounds_row,
        "R1 は独立検証、R2 以降は追試。最終ラウンドで判定役が総合判定を出します",
    );

    let foot = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let note = tree.insert(Some(foot), Container::column().grow(1.0));
    let start_button = tree.insert(Some(foot), Button::new("検証を開始").default_button(true));
    let past = insert_past_card(tree, cards);
    let mut parts = Parts {
        root,
        kind,
        repo,
        base_col,
        base,
        head,
        filter,
        spec,
        pick_spec,
        request,
        criteria,
        confirm,
        isolation,
        max_rounds,
        rounds_row,
        round_cap: MAX_MAX_ROUNDS,
        refilled: false,
        judge_host,
        judge,
        kinds,
        judge_setup,
        panel_list,
        panel,
        add_row,
        auto_host,
        start_button,
        note,
        past,
    };
    show_kind(tree, &parts, TargetKind::Diff);
    sync_row_buttons(tree, &mut parts);
    parts
}

/// The round limit input, 1 to `cap`.
fn rounds_input(cap: RoundNo, value: RoundNo) -> NumberInput {
    NumberInput::new(1.0, f64::from(cap), f64::from(value))
        .step(1.0)
        .decimals(0)
        .stepper(true)
        .width(80.0)
}

/// Brings the top of the filled form into view and focuses the
/// repository field.
pub(super) fn show_top(tree: &mut UiTree, parts: &Parts) {
    tree.update::<ScrollView, _>(parts.root, |s, cx| s.set_offset(cx, 0.0));
    tree.set_focus(Some(parts.repo));
}

/// A state target has no base.
fn show_kind(tree: &mut UiTree, parts: &Parts, kind: TargetKind) {
    tree.set_hidden(parts.base_col, kind == TargetKind::State);
}

fn selected_segment(tree: &UiTree, id: WidgetId) -> usize {
    tree.get::<SegmentedControl>(id)
        .and_then(|s| s.selected().iter().position(|on| *on))
        .unwrap_or(0)
}

fn selected_target(tree: &UiTree, parts: &Parts) -> TargetKind {
    TargetKind::ALL
        .get(selected_segment(tree, parts.kind))
        .copied()
        .unwrap_or(TargetKind::Diff)
}

fn selected_isolation(tree: &UiTree, parts: &Parts) -> Isolation {
    Isolation::ALL
        .get(selected_segment(tree, parts.isolation))
        .copied()
        .unwrap_or_default()
}

/// The target kind changed: its fields show; `false` when `source` is not
/// the kind control.
pub(super) fn target_kind_changed(tree: &mut UiTree, parts: &Parts, source: WidgetId) -> bool {
    if source != parts.kind {
        return false;
    }
    let kind = selected_target(tree, parts);
    show_kind(tree, parts, kind);
    true
}

/// The model box and effort menu of one agent.
pub(super) struct SetupParts {
    host: WidgetId,
    pub(super) model: WidgetId,
    pub(super) effort: WidgetId,
}

const DEFAULT_MODEL: &str = "既定のモデル";
const DEFAULT_EFFORT: &str = "既定のエフォート";

fn insert_setup(tree: &mut UiTree, parent: WidgetId, kind: AgentKind) -> SetupParts {
    let host = tree.insert(
        Some(parent),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let (model, effort) = insert_setup_controls(tree, host, kind);
    SetupParts {
        host,
        model,
        effort,
    }
}

fn insert_setup_controls(
    tree: &mut UiTree,
    host: WidgetId,
    kind: AgentKind,
) -> (WidgetId, WidgetId) {
    let model = tree.insert(
        Some(host),
        ComboBox::new(kind.models().iter().copied(), 190.0).placeholder(DEFAULT_MODEL),
    );
    let effort = tree.insert(
        Some(host),
        PopUpButton::new(std::iter::once(DEFAULT_EFFORT).chain(kind.efforts().iter().copied())),
    );
    (model, effort)
}

fn set_setup(tree: &mut UiTree, parts: &SetupParts, kind: AgentKind, setup: &AgentSetup) {
    let model = setup.model.clone().unwrap_or_default();
    tree.update::<ComboBox, _>(parts.model, |c, cx| c.set_text(cx, model));
    let effort = setup
        .effort
        .as_deref()
        .and_then(|e| kind.efforts().iter().position(|k| *k == e))
        .map_or(0, |i| i + 1);
    tree.update::<PopUpButton, _>(parts.effort, |p, cx| p.set_selected(cx, effort));
}

/// The strategy menu's entries: the built-in strategies, then カスタム.
fn strategy_labels() -> Vec<&'static str> {
    BuiltinStrategy::ALL
        .iter()
        .map(|s| s.label())
        .chain(std::iter::once("カスタム"))
        .collect()
}

/// A menu of `kinds`, `selected` picked (the first when it is not one).
fn insert_kind_menu(
    tree: &mut UiTree,
    host: WidgetId,
    kinds: &[AgentKind],
    selected: AgentKind,
) -> WidgetId {
    tree.insert(
        Some(host),
        PopUpButton::new(kinds.iter().map(|k| k.command()))
            .selected_index(kind_index(kinds, selected)),
    )
}

/// Where `kind` is in the menu of `kinds`; the first when it is not.
fn kind_index(kinds: &[AgentKind], kind: AgentKind) -> usize {
    kinds.iter().position(|&k| k == kind).unwrap_or(0)
}

fn insert_row(
    tree: &mut UiTree,
    list: WidgetId,
    kinds: &[AgentKind],
    choice: &VerifierChoice,
) -> PanelRow {
    let host = tree.insert(
        Some(list),
        Container::column()
            .gap(6.0)
            .padding(8.0)
            .background(Role::SurfaceAlt)
            .radius(6.0),
    );
    let top = tree.insert(
        Some(host),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let kind = insert_kind_menu(tree, top, kinds, choice.agent.kind);
    let setup = insert_setup(tree, top, choice.agent.kind);
    set_setup(tree, &setup, choice.agent.kind, &choice.agent.setup);
    spacer(tree, top);
    let remove = tree.insert(
        Some(top),
        Button::new("削除").control_size(ControlSize::Small),
    );
    let bottom = tree.insert(
        Some(host),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    caption(tree, bottom, "手法");
    let (index, custom_text) = match &choice.strategy {
        StrategyChoice::Builtin(s) => (
            BuiltinStrategy::ALL
                .iter()
                .position(|b| b == s)
                .unwrap_or(0),
            String::new(),
        ),
        StrategyChoice::Custom(text) => (BuiltinStrategy::ALL.len(), text.clone()),
    };
    let strategy = tree.insert(
        Some(bottom),
        PopUpButton::new(strategy_labels()).selected_index(index),
    );
    let custom = tree.insert(
        Some(bottom),
        TextField::new("手法を自由に記述（例: 全ロケールで表示を確かめる）", 380.0),
    );
    tree.update::<TextField, _>(custom, |f, cx| f.set_text(cx, custom_text));
    tree.set_hidden(custom, index < BuiltinStrategy::ALL.len());
    PanelRow {
        host,
        kind,
        setup,
        strategy,
        custom,
        remove,
    }
}

fn sync_row_buttons(tree: &mut UiTree, parts: &mut Parts) {
    let removable = parts.panel.len() > MIN_PANEL;
    for row in &parts.panel {
        tree.set_enabled(row.remove, removable);
    }
    tree.set_enabled(parts.add_row, parts.panel.len() < MAX_PANEL);
}

/// Adds a row of the last row's kind on the first strategy nobody holds
/// yet.
pub(super) fn add_row(tree: &mut UiTree, parts: &mut Parts) {
    if parts.panel.len() >= MAX_PANEL {
        return;
    }
    let kind = parts.panel.last().map_or(parts.kinds[0], |row| {
        selected_kind(tree, row.kind, &parts.kinds)
    });
    let held: Vec<usize> = parts
        .panel
        .iter()
        .filter_map(|row| {
            tree.get::<PopUpButton>(row.strategy)
                .map(PopUpButton::selected)
        })
        .collect();
    let strategy = BuiltinStrategy::ALL
        .iter()
        .enumerate()
        .find(|(i, _)| !held.contains(i))
        .map_or(BuiltinStrategy::Conformance, |(_, s)| *s);
    let row = insert_row(
        tree,
        parts.panel_list,
        &parts.kinds,
        &VerifierChoice {
            agent: kind.into(),
            strategy: StrategyChoice::Builtin(strategy),
        },
    );
    parts.panel.push(row);
    sync_row_buttons(tree, parts);
}

pub(super) fn remove_row(tree: &mut UiTree, parts: &mut Parts, index: usize, text: &mut TextStore) {
    if parts.panel.len() <= MIN_PANEL || index >= parts.panel.len() {
        return;
    }
    let row = parts.panel.remove(index);
    tree.remove(row.host, text);
    sync_row_buttons(tree, parts);
}

/// Handles a pop-up change in a verifier row (its kind, or its strategy);
/// `false` when `source` is in no row.
pub(super) fn row_changed(
    tree: &mut UiTree,
    parts: &mut Parts,
    source: WidgetId,
    text: &mut TextStore,
) -> bool {
    if let Some(row) = parts.panel.iter().find(|row| row.strategy == source) {
        let custom = tree
            .get::<PopUpButton>(row.strategy)
            .is_some_and(|p| p.selected() >= BuiltinStrategy::ALL.len());
        tree.set_hidden(row.custom, !custom);
        return true;
    }
    let Some(row) = parts.panel.iter_mut().find(|row| row.kind == source) else {
        return false;
    };
    let kind = selected_kind(tree, row.kind, &parts.kinds);
    clear_children(tree, row.setup.host, text);
    let (model, effort) = insert_setup_controls(tree, row.setup.host, kind);
    row.setup.model = model;
    row.setup.effort = effort;
    true
}

pub(super) fn judge_kind_changed(tree: &mut UiTree, parts: &mut Parts, text: &mut TextStore) {
    let kind = selected_kind(tree, parts.judge, &parts.kinds);
    clear_children(tree, parts.judge_setup.host, text);
    let (model, effort) = insert_setup_controls(tree, parts.judge_setup.host, kind);
    parts.judge_setup.model = model;
    parts.judge_setup.effort = effort;
}

/// Puts `judge` and `rows` (one per verifier row, in order) into the agent
/// rows, as picking them by hand would: the kind, then the model and
/// effort. Everything else of the form stays as it is. Each kind is one the
/// menus offer.
pub(super) fn set_agents(
    tree: &mut UiTree,
    parts: &mut Parts,
    judge: &AgentChoice,
    rows: &[AgentChoice],
    text: &mut TextStore,
) {
    let index = kind_index(&parts.kinds, judge.kind);
    tree.update::<PopUpButton, _>(parts.judge, |p, cx| p.set_selected(cx, index));
    judge_kind_changed(tree, parts, text);
    set_setup(tree, &parts.judge_setup, judge.kind, &judge.setup);
    for (row, choice) in parts.panel.iter_mut().zip(rows) {
        let index = kind_index(&parts.kinds, choice.kind);
        tree.update::<PopUpButton, _>(row.kind, |p, cx| p.set_selected(cx, index));
        clear_children(tree, row.setup.host, text);
        let (model, effort) = insert_setup_controls(tree, row.setup.host, choice.kind);
        row.setup.model = model;
        row.setup.effort = effort;
        set_setup(tree, &row.setup, choice.kind, &choice.setup);
    }
}

/// Puts `form` into the start view. Entries of a kind the menus do not
/// offer are left out; those kinds come back.
pub(super) fn fill(
    tree: &mut UiTree,
    parts: &mut Parts,
    form: &Form,
    text: &mut TextStore,
) -> Vec<AgentKind> {
    let kind = TargetKind::ALL
        .iter()
        .position(|k| *k == form.kind)
        .unwrap_or(0);
    tree.update::<SegmentedControl, _>(parts.kind, |s, cx| s.set_selected(cx, &[kind]));
    show_kind(tree, parts, form.kind);
    let isolation = Isolation::ALL
        .iter()
        .position(|i| *i == form.isolation)
        .unwrap_or(0);
    tree.update::<SegmentedControl, _>(parts.isolation, |s, cx| s.set_selected(cx, &[isolation]));
    for (id, value) in [
        (parts.repo, &form.repo),
        (parts.base, &form.base),
        (parts.head, &form.head),
        (parts.spec, &form.spec),
    ] {
        let value = value.clone();
        tree.update::<TextField, _>(id, |f, cx| f.set_text(cx, value));
    }
    for (id, value) in [
        (parts.filter, &form.filter),
        (parts.request, &form.request),
        (parts.criteria, &form.criteria),
    ] {
        let value = value.clone();
        tree.update::<TextArea, _>(id, |a, cx| a.set_text(cx, value));
    }
    let confirm = if form.confirm {
        CheckState::On
    } else {
        CheckState::Off
    };
    tree.update::<Checkbox, _>(parts.confirm, |c, cx| c.set_state(cx, confirm));
    if parts.round_cap != form.round_cap {
        // An input's range is fixed once made: a new one takes the cap.
        tree.remove(parts.max_rounds, text);
        parts.max_rounds = tree.insert(
            Some(parts.rounds_row),
            rounds_input(form.round_cap, form.max_rounds),
        );
        tree.move_child(parts.max_rounds, 0);
        parts.round_cap = form.round_cap;
    }
    let rounds = f64::from(form.max_rounds);
    tree.update::<NumberInput, _>(parts.max_rounds, |n, cx| n.set_value(cx, rounds));
    let (form, left_out) = form.clone().restricted(&parts.kinds);
    let judge = kind_index(&parts.kinds, form.judge.kind);
    tree.update::<PopUpButton, _>(parts.judge, |p, cx| p.set_selected(cx, judge));
    judge_kind_changed(tree, parts, text);
    set_setup(tree, &parts.judge_setup, form.judge.kind, &form.judge.setup);
    for row in std::mem::take(&mut parts.panel) {
        tree.remove(row.host, text);
    }
    parts.panel = form
        .panel
        .iter()
        .take(MAX_PANEL)
        .map(|choice| insert_row(tree, parts.panel_list, &parts.kinds, choice))
        .collect();
    sync_row_buttons(tree, parts);
    left_out
}

/// Makes the agent menus offer `kinds` (rebuilt: a menu's entries are
/// fixed once made) and shows `form` in them; the kinds it had to leave
/// out come back.
pub(super) fn apply_kinds(
    tree: &mut UiTree,
    parts: &mut Parts,
    kinds: Vec<AgentKind>,
    form: &Form,
    text: &mut TextStore,
) -> Vec<AgentKind> {
    parts.kinds = kinds;
    clear_children(tree, parts.judge_host, text);
    parts.judge = insert_kind_menu(tree, parts.judge_host, &parts.kinds, parts.kinds[0]);
    fill(tree, parts, form, text)
}

/// Puts the file chosen in the open panel in the spec file field.
pub(super) fn set_spec_file(tree: &mut UiTree, parts: &Parts, path: &Path) {
    let path = path.display().to_string();
    tree.update::<TextField, _>(parts.spec, |f, cx| f.set_text(cx, path));
}

pub(super) fn actions(parts: &Parts, actions: &mut Vec<(WidgetId, Action)>) {
    actions.push((parts.start_button, Action::Start));
    actions.push((parts.add_row, Action::AddPanelRow));
    actions.push((parts.pick_spec, Action::PickSpec));
    for (index, row) in parts.panel.iter().enumerate() {
        actions.push((row.remove, Action::RemovePanelRow(index)));
    }
}

/// The kind `popup` shows: its index into the menu's `kinds`.
fn selected_kind(tree: &UiTree, popup: WidgetId, kinds: &[AgentKind]) -> AgentKind {
    tree.get::<PopUpButton>(popup)
        .and_then(|p| kinds.get(p.selected()).copied())
        .unwrap_or(kinds[0])
}

fn read_setup(tree: &UiTree, parts: &SetupParts, kind: AgentKind) -> AgentSetup {
    let model = tree
        .get::<ComboBox>(parts.model)
        .map(|c| c.text().trim().to_owned())
        .filter(|m| !m.is_empty());
    let effort = tree
        .get::<PopUpButton>(parts.effort)
        .and_then(|p| p.selected().checked_sub(1))
        .and_then(|i| kind.efforts().get(i))
        .map(|e| (*e).to_owned());
    AgentSetup { model, effort }
}

pub(super) fn rebuild_note(
    tree: &mut UiTree,
    parts: &Parts,
    link: &Link,
    detection: &Detection,
    text: &mut TextStore,
) {
    clear_children(tree, parts.note, text);
    tree.set_enabled(parts.start_button, detection.startable());
    let notes = [
        match link {
            Link::Live => None,
            Link::Connecting => Some("Noa に接続しています…".to_owned()),
            Link::Down(why) => Some(format!("Noa に接続できません: {why}")),
        },
        detection.note(),
    ];
    for note in notes.into_iter().flatten() {
        super::note(tree, parts.note, note);
    }
}

/// Shows why the last start was refused under the form.
pub(super) fn show_refusal(tree: &mut UiTree, parts: &Parts, why: &str, text: &mut TextStore) {
    clear_children(tree, parts.note, text);
    let row = tree.insert(
        Some(parts.note),
        Container::row().align_items(AlignItems::START).gap(6.0),
    );
    tag(tree, row, "開始できません", Tone::Critical);
    tree.insert(
        Some(row),
        Label::new(why.to_owned()).font_size(11.5).wrap(true),
    );
}

pub(super) fn read(tree: &UiTree, parts: &Parts) -> Form {
    let field = |id: WidgetId| {
        tree.get::<TextField>(id)
            .map(|f| f.text().trim().to_owned())
            .unwrap_or_default()
    };
    // A path is kept as typed (a folder's name may end in a space); only
    // whether it is blank looks past spaces ([`dir_path`], [`expand`]).
    let path = |id: WidgetId| {
        tree.get::<TextField>(id)
            .map(|f| f.text().to_owned())
            .unwrap_or_default()
    };
    let area = |id: WidgetId| {
        tree.get::<TextArea>(id)
            .map(|a| a.text().to_owned())
            .unwrap_or_default()
    };
    let max_rounds = tree
        .get::<NumberInput>(parts.max_rounds)
        .map_or(DEFAULT_MAX_ROUNDS, |n| {
            n.value().round().clamp(1.0, f64::from(parts.round_cap)) as RoundNo
        });
    let kind = selected_kind(tree, parts.judge, &parts.kinds);
    let judge = AgentChoice {
        kind,
        setup: read_setup(tree, &parts.judge_setup, kind),
    };
    let panel = parts
        .panel
        .iter()
        .map(|row| {
            let kind = selected_kind(tree, row.kind, &parts.kinds);
            let index = tree
                .get::<PopUpButton>(row.strategy)
                .map_or(0, PopUpButton::selected);
            let strategy = match BuiltinStrategy::ALL.get(index) {
                Some(s) => StrategyChoice::Builtin(*s),
                None => StrategyChoice::Custom(field(row.custom)),
            };
            VerifierChoice {
                agent: AgentChoice {
                    kind,
                    setup: read_setup(tree, &row.setup, kind),
                },
                strategy,
            }
        })
        .collect();
    Form {
        kind: selected_target(tree, parts),
        repo: path(parts.repo),
        base: field(parts.base),
        head: field(parts.head),
        filter: area(parts.filter),
        spec: path(parts.spec),
        request: area(parts.request).trim().to_owned(),
        criteria: area(parts.criteria),
        confirm: tree
            .get::<Checkbox>(parts.confirm)
            .is_none_or(|c| c.state() == CheckState::On),
        isolation: selected_isolation(tree, parts),
        round_cap: parts.round_cap,
        max_rounds,
        judge,
        panel,
    }
}

/// `検証済 4 · 不合格 1 · 判定不能 1` from a summary's ruling counts
/// (verified, failed, inconclusive, not applicable, unruled); empty when
/// nothing is ruled.
pub(super) fn tally(rulings: &[usize; 5]) -> String {
    let labels = ["検証済", "不合格", "判定不能", "対象外", "未判定"];
    let ruled = rulings[..4].iter().sum::<usize>() > 0;
    if !ruled {
        return String::new();
    }
    labels
        .iter()
        .zip(rulings)
        .take(3)
        .map(|(label, n)| format!("{label} {n}"))
        .chain(
            labels
                .iter()
                .zip(rulings)
                .skip(3)
                .filter(|(_, n)| **n > 0)
                .map(|(label, n)| format!("{label} {n}")),
        )
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Rebuilds the list of past verifications, newest first: open read-only
/// (finished, or running in another window), or resume (unfinished).
pub(super) fn rebuild_past(
    tree: &mut UiTree,
    parts: &Parts,
    past: &[Listed],
    total: usize,
    now: Millis,
    actions: &mut Vec<(WidgetId, Action)>,
    text: &mut TextStore,
) {
    clear_children(tree, parts.past, text);
    if past.is_empty() {
        super::note(tree, parts.past, "まだ検証はありません");
        return;
    }
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    for listed in past {
        let row = tree.insert(
            Some(parts.past),
            Container::row().align_items(AlignItems::CENTER).gap(10.0),
        );
        let info = tree.insert(
            Some(row),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                gap: gap(3.0),
                ..Default::default()
            }),
        );
        let summary = match &listed.summary {
            Ok(summary) => summary,
            Err(why) => {
                tree.insert(
                    Some(info),
                    Label::new(dir_name(&listed.dir))
                        .font_size(12.5)
                        .weight(600),
                );
                super::note(tree, info, format!("読めません: {why}"));
                continue;
            }
        };
        tree.insert(
            Some(info),
            Label::new(summary.label.clone())
                .font_size(12.5)
                .weight(600)
                .wrap(true),
        );
        let tags = tree.insert(
            Some(info),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        let (status, tone) = match (summary.closed, summary.busy) {
            (true, _) => ("完了", Tone::Neutral),
            (false, true) => ("他のウィンドウで実行中", Tone::Success),
            (false, false) => ("中断", Tone::Warning),
        };
        tag(tree, tags, status, tone);
        tag(tree, tags, summary.kind.label(), Tone::Neutral);
        match summary.decision {
            Some(decision) => tag(tree, tags, decision.label(), super::decision_tone(decision)),
            None if summary.closed => tag(tree, tags, "判定なし", Tone::Neutral),
            None => {}
        }
        let counts = tally(&summary.rulings);
        if !counts.is_empty() {
            caption(tree, tags, counts);
        }
        caption(
            tree,
            tags,
            format!("{}/{} ラウンド", summary.rounds, summary.max_rounds),
        );
        caption(tree, tags, age(now, summary.created_at));

        let buttons = tree.insert(Some(row), Container::row().gap(6.0));
        let dir: PathBuf = listed.dir.clone();
        let open = if !summary.closed && !summary.busy {
            let resume = tree.insert(Some(buttons), small("再開").default_button(true));
            (resume, Action::ResumePast(dir.clone()))
        } else {
            (
                tree.insert(Some(buttons), small("表示")),
                Action::OpenPast(dir.clone()),
            )
        };
        actions.push(open);
        if !summary.closed && !summary.busy {
            let view = tree.insert(Some(buttons), small("表示"));
            actions.push((view, Action::OpenPast(dir.clone())));
        }
        let again = tree.insert(
            Some(buttons),
            small("再検証").tooltip(
                "同じ対象・基準・パネルで開始ビューを埋めます（前回の総合判定と要対応を依頼に添えます）",
            ),
        );
        tree.set_accessible_label(again, format!("{} を同じ設定で再検証", summary.label));
        actions.push((again, Action::VerifyAgain(Some(dir))));
    }
    if total > past.len() {
        let more = tree.insert(
            Some(parts.past),
            Container::row().align_items(AlignItems::CENTER).gap(10.0),
        );
        caption(
            tree,
            more,
            format!("全 {total} 件のうち新しい {} 件", past.len()),
        );
        let button = tree.insert(
            Some(more),
            small(&format!(
                "さらに {} 件表示",
                PAST_LIMIT.min(total - past.len())
            )),
        );
        actions.push((button, Action::MorePast));
    }
}

fn dir_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// How long ago `at` was: `たった今`, `5分前`, `3時間前`, `2日前`.
pub(super) fn age(now: Millis, at: Millis) -> String {
    let minutes = now.saturating_sub(at) / 60_000;
    match minutes {
        0 => "たった今".to_owned(),
        1..60 => format!("{minutes}分前"),
        60..1440 => format!("{}時間前", minutes / 60),
        _ => format!("{}日前", minutes / 1440),
    }
}

/// `typed` with a leading `~` expanded; it must be absolute.
fn expand(typed: &str) -> Result<PathBuf, String> {
    let path = match typed.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let Some(home) = std::env::var_os("HOME") else {
                return Err("ホームディレクトリが分かりません".into());
            };
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(typed),
    };
    if !path.is_absolute() {
        return Err(format!("{typed} は絶対パスではありません"));
    }
    Ok(path)
}

/// A typed directory, which must exist.
fn dir_path(typed: &str, what: &str) -> Result<PathBuf, String> {
    if typed.trim().is_empty() {
        return Err(format!("{what}を指定してください"));
    }
    let path = expand(typed)?;
    if !path.is_dir() {
        return Err(format!("{} はフォルダではありません", path.display()));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_restricted_to_installed_kinds_says_what_it_left_out() {
        let row = |kind: AgentKind, s| VerifierChoice {
            agent: kind.into(),
            strategy: StrategyChoice::Builtin(s),
        };
        let form = Form {
            judge: AgentKind::Claude.into(),
            panel: vec![
                row(AgentKind::Codex, BuiltinStrategy::Falsify),
                row(AgentKind::Claude, BuiltinStrategy::Conformance),
                row(AgentKind::Codex, BuiltinStrategy::Regression),
            ],
            ..Form::default()
        };
        let (form, left_out) = form.restricted(&[AgentKind::Codex]);
        assert_eq!(left_out, [AgentKind::Claude]);
        assert_eq!(form.judge.kind, AgentKind::Codex);
        assert_eq!(form.panel.len(), 2);
        assert!(form.panel.iter().all(|r| r.agent.kind == AgentKind::Codex));
    }

    #[test]
    fn a_past_verification_shows_its_age_and_tally() {
        let now = 10 * 86_400_000;
        assert_eq!(age(now, now - 30_000), "たった今");
        assert_eq!(age(now, now - 5 * 60_000), "5分前");
        assert_eq!(age(now, now - 3 * 3_600_000), "3時間前");
        assert_eq!(age(now, now - 2 * 86_400_000), "2日前");
        assert_eq!(age(now, now + 1), "たった今");
        assert_eq!(tally(&[4, 1, 1, 0, 0]), "検証済 4 · 不合格 1 · 判定不能 1");
        assert_eq!(
            tally(&[2, 1, 1, 1, 0]),
            "検証済 2 · 不合格 1 · 判定不能 1 · 対象外 1"
        );
        assert_eq!(tally(&[0, 0, 0, 0, 5]), "");
    }

    #[test]
    fn the_clean_panel_is_the_default_panel_of_l1_9() {
        let form = Form::default();
        let rows: Vec<(AgentKind, StrategyChoice)> = form
            .panel
            .iter()
            .map(|r| (r.agent.kind, r.strategy.clone()))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    AgentKind::Claude,
                    StrategyChoice::Builtin(BuiltinStrategy::Conformance)
                ),
                (
                    AgentKind::Codex,
                    StrategyChoice::Builtin(BuiltinStrategy::Falsify)
                ),
                (
                    AgentKind::Claude,
                    StrategyChoice::Builtin(BuiltinStrategy::Regression)
                ),
            ]
        );
        assert!(form.confirm);
        assert_eq!(form.max_rounds, DEFAULT_MAX_ROUNDS);
        assert_eq!(form.isolation, Isolation::Worktree);
    }

    #[test]
    fn the_form_turns_into_each_kind_of_target() {
        let dir = std::env::temp_dir();
        let dir_text = dir.display().to_string();
        let diff = Form {
            repo: dir_text.clone(),
            base: "main".into(),
            head: "feat/x".into(),
            filter: "src/\n\n docs/release notes.md \n".into(),
            spec: "docs/spec.md".into(),
            ..Form::default()
        };
        let target = diff.target().unwrap();
        assert_eq!(target.kind, TargetKind::Diff);
        assert_eq!(target.head, Head::Ref("feat/x".into()));
        assert_eq!(target.base.as_deref(), Some("main"));
        assert_eq!(target.paths, ["src/", "docs/release notes.md"]);
        assert_eq!(target.spec.as_deref(), Some("docs/spec.md"));
        let state = Form {
            kind: TargetKind::State,
            ..diff
        };
        let target = state.target().unwrap();
        assert_eq!(target.base, None, "a state target has no base");
        assert!(
            Form::default()
                .target()
                .unwrap_err()
                .contains("リポジトリを指定")
        );
        let criteria = Form {
            criteria: " a \n\n b\n".into(),
            ..Form::default()
        };
        assert_eq!(criteria.user_criteria(), ["a", "b"]);
    }
}
