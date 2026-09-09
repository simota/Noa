//! Pure auto-approve prompt detection and state transitions.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use noa_core::Point;
use noa_grid::{Cell, Terminal};

use crate::sidebar::AgentKind;

pub(crate) const USER_INPUT_SUPPRESSION: Duration = Duration::from_secs(3);
pub(crate) const APPROVAL_WINDOW: Duration = Duration::from_secs(60);
pub(crate) const APPROVAL_LIMIT: usize = 6;

pub(crate) type RowText = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PromptKind {
    Edit,
    Write,
    Read,
    Command,
    AskUserQuestion,
    EnterConfirm,
}

impl PromptKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::Write => "Write",
            Self::Read => "Read",
            Self::Command => "Command",
            Self::AskUserQuestion => "Question",
            Self::EnterConfirm => "Enter",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutoApproveSignature {
    ClaudeEdit,
    ClaudeWrite,
    ClaudeRead,
    ClaudeAskUserQuestion,
    ClaudeEnterConfirm,
    CodexCommand,
    AgyAskUserQuestion,
    AgyCommand,
}

impl AutoApproveSignature {
    pub(crate) fn kind(self) -> PromptKind {
        signature(self).kind
    }

    pub(crate) fn agent(self) -> AgentKind {
        signature(self).agent
    }

    pub(crate) fn bytes(self) -> &'static [u8] {
        signature(self).bytes
    }

    pub(crate) fn label(self) -> &'static str {
        self.kind().label()
    }
}

#[derive(Clone, Copy)]
struct Signature {
    id: AutoApproveSignature,
    agent: AgentKind,
    kind: PromptKind,
    anchors: &'static [&'static str],
    yes_label: Option<&'static str>,
    requires_marker: bool,
    bytes: &'static [u8],
}

const SIGNATURES: &[Signature] = &[
    Signature {
        id: AutoApproveSignature::ClaudeEdit,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::Edit,
        anchors: &["claude wants to edit"],
        yes_label: Some("1. Yes"),
        requires_marker: true,
        bytes: b"1\r",
    },
    Signature {
        id: AutoApproveSignature::ClaudeWrite,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::Write,
        anchors: &["claude wants to write", "claude wants to create"],
        yes_label: Some("1. Yes"),
        requires_marker: true,
        bytes: b"1\r",
    },
    Signature {
        id: AutoApproveSignature::ClaudeRead,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::Read,
        anchors: &["claude wants to read"],
        yes_label: Some("1. Yes"),
        requires_marker: true,
        bytes: b"1\r",
    },
    Signature {
        id: AutoApproveSignature::ClaudeAskUserQuestion,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::AskUserQuestion,
        anchors: &["claude has a question", "claude asks"],
        yes_label: Some("1."),
        requires_marker: true,
        bytes: b"1\r",
    },
    Signature {
        id: AutoApproveSignature::ClaudeEnterConfirm,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::EnterConfirm,
        anchors: &["press enter to continue"],
        yes_label: None,
        requires_marker: false,
        bytes: b"\r",
    },
    Signature {
        id: AutoApproveSignature::CodexCommand,
        agent: AgentKind::Codex,
        kind: PromptKind::Command,
        anchors: &["would you like to run the following command?"],
        yes_label: Some("1. Yes, proceed (y)"),
        requires_marker: true,
        bytes: b"\r",
    },
    Signature {
        id: AutoApproveSignature::AgyAskUserQuestion,
        agent: AgentKind::Agy,
        kind: PromptKind::AskUserQuestion,
        anchors: &["question"],
        yes_label: Some("1. (Recommended) "),
        requires_marker: true,
        bytes: b"\r",
    },
    Signature {
        id: AutoApproveSignature::AgyCommand,
        agent: AgentKind::Agy,
        kind: PromptKind::Command,
        anchors: &["command"],
        yes_label: Some("1. Yes"),
        requires_marker: true,
        bytes: b"\r",
    },
];

fn signature(id: AutoApproveSignature) -> &'static Signature {
    SIGNATURES
        .iter()
        .find(|candidate| candidate.id == id)
        .expect("signature id must exist in signature table")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AutoApproveInputGuards {
    pub(crate) ime_preedit_active: bool,
    pub(crate) paste_suppressed_until: Option<Instant>,
    pub(crate) last_user_input_at: Option<Instant>,
}

impl AutoApproveInputGuards {
    pub(crate) fn mark_user_input(&mut self, now: Instant) {
        self.last_user_input_at = Some(now);
    }

    pub(crate) fn mark_paste(&mut self, now: Instant) {
        self.last_user_input_at = Some(now);
        self.paste_suppressed_until = Some(now + USER_INPUT_SUPPRESSION);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DetectContext {
    pub(crate) now: Instant,
    pub(crate) alt_screen: bool,
    pub(crate) scrollback_offset: usize,
    pub(crate) guards: AutoApproveInputGuards,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SuppressReason {
    Disabled,
    #[cfg(test)]
    UnknownAgent,
    ViewportNotLive,
    ImePreedit,
    PasteActive,
    RecentUserInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Fire {
        signature: AutoApproveSignature,
        region_hash: u64,
        disable_after: bool,
    },
    Hold,
    Suppressed(SuppressReason),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AutoApproveState {
    last_match: Option<MatchKey>,
    match_count: u8,
    pending_fire: Option<PendingPrompt>,
    awaiting_change: Option<ConsumedPrompt>,
    approvals: VecDeque<Instant>,
    disabled_by_runaway: bool,
}

impl AutoApproveState {
    pub(crate) fn reset_for_mode_off(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn needs_static_rescan(&self) -> bool {
        !self.disabled_by_runaway
            && self.pending_fire.is_none()
            && self.last_match.is_some_and(|key| {
                self.awaiting_change.is_none_or(|consumed| {
                    consumed.signature != key.signature || consumed.region_hash != key.region_hash
                })
            })
    }

    pub(crate) fn apply_feedback(
        &mut self,
        signature: AutoApproveSignature,
        region_hash: u64,
        accepted: bool,
        now: Instant,
    ) {
        self.approvals
            .retain(|at| now.saturating_duration_since(*at) <= APPROVAL_WINDOW);

        let Some(pending) = self.pending_fire else {
            return;
        };
        if pending.signature != signature || pending.region_hash != region_hash {
            return;
        }

        self.pending_fire = None;
        if !accepted {
            return;
        }

        self.approvals.push_back(now);
        self.awaiting_change = Some(ConsumedPrompt {
            signature,
            region_hash,
        });
        self.last_match = None;
        self.match_count = 0;
        self.disabled_by_runaway = pending.disable_after;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MatchKey {
    signature: AutoApproveSignature,
    region_hash: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingPrompt {
    signature: AutoApproveSignature,
    region_hash: u64,
    disable_after: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConsumedPrompt {
    signature: AutoApproveSignature,
    region_hash: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MatchedPrompt {
    pub(crate) signature: AutoApproveSignature,
    pub(crate) region_hash: u64,
    pub(crate) region: RangeInclusive<usize>,
}

#[cfg(test)]
pub(crate) fn detect(
    rows: &[RowText],
    cursor: Point,
    agent: AgentKind,
    ctx: DetectContext,
    state: &AutoApproveState,
) -> Decision {
    if agent == AgentKind::Generic {
        return Decision::Suppressed(SuppressReason::UnknownAgent);
    }
    detect_inner(rows, cursor, Some(agent), ctx, state)
}

/// Detect and advance the two-match state machine in one pass. The viewport
/// is scanned (lowercased + signature-matched) at most once per feed: the
/// same `MatchedPrompt` observation feeds both the decision and the state
/// update, instead of each re-running `find_prompt` on the same rows.
pub(crate) fn detect_and_update_any_agent(
    rows: &[RowText],
    cursor: Point,
    ctx: DetectContext,
    state: &mut AutoApproveState,
) -> Decision {
    let (decision, matched) = match suppression(ctx, state.disabled_by_runaway) {
        Some(reason) => {
            // Only the input-cooldown suppressions keep tracking the prompt
            // (see `apply_decision_state`), so only they pay for a scan.
            let matched = matches!(
                reason,
                SuppressReason::RecentUserInput | SuppressReason::PasteActive
            )
            .then(|| find_prompt(rows, cursor, None))
            .flatten();
            (Decision::Suppressed(reason), matched)
        }
        None => {
            let matched = find_prompt(rows, cursor, None);
            (decide(matched.as_ref(), ctx, state), matched)
        }
    };
    apply_decision_state(rows, matched.as_ref(), ctx, state, &decision);
    decision
}

pub(crate) fn rescan_signature(
    rows: &[RowText],
    signature_id: AutoApproveSignature,
    cursor: Point,
    ctx: DetectContext,
) -> Option<MatchedPrompt> {
    if suppression(ctx, false).is_some() {
        return None;
    }
    find_signature(rows, cursor, signature(signature_id))
}

pub(crate) fn viewport_rows_from_terminal(terminal: &Terminal) -> Vec<RowText> {
    terminal
        .active()
        .visible_rows()
        .into_iter()
        .map(|row| row_text(&row.cells))
        .collect()
}

#[cfg(test)]
fn detect_inner(
    rows: &[RowText],
    cursor: Point,
    agent: Option<AgentKind>,
    ctx: DetectContext,
    state: &AutoApproveState,
) -> Decision {
    if let Some(reason) = suppression(ctx, state.disabled_by_runaway) {
        return Decision::Suppressed(reason);
    }
    decide(find_prompt(rows, cursor, agent).as_ref(), ctx, state)
}

/// The unsuppressed decision for one observed viewport: `matched` is the
/// prompt `find_prompt` found there (or `None`).
fn decide(
    matched: Option<&MatchedPrompt>,
    ctx: DetectContext,
    state: &AutoApproveState,
) -> Decision {
    let Some(matched) = matched else {
        return Decision::Hold;
    };

    if state.awaiting_change.is_some_and(|consumed| {
        consumed.signature == matched.signature && consumed.region_hash == matched.region_hash
    }) {
        return Decision::Hold;
    }
    if state.pending_fire.is_some_and(|pending| {
        pending.signature == matched.signature && pending.region_hash == matched.region_hash
    }) {
        return Decision::Hold;
    }

    let key = MatchKey {
        signature: matched.signature,
        region_hash: matched.region_hash,
    };
    if state.last_match != Some(key) || state.match_count < 1 {
        return Decision::Hold;
    }

    let approvals_in_window = count_recent_approvals(&state.approvals, ctx.now);
    Decision::Fire {
        signature: matched.signature,
        region_hash: matched.region_hash,
        disable_after: approvals_in_window + 1 >= APPROVAL_LIMIT,
    }
}

/// Advance the state machine after `decision`. `matched` is the prompt the
/// caller observed on `rows` in the same feed (`None` when none was found,
/// or when the suppression reason makes tracking one pointless).
fn apply_decision_state(
    rows: &[RowText],
    matched: Option<&MatchedPrompt>,
    ctx: DetectContext,
    state: &mut AutoApproveState,
    decision: &Decision,
) {
    state
        .approvals
        .retain(|at| ctx.now.saturating_duration_since(*at) <= APPROVAL_WINDOW);
    match decision {
        Decision::Fire {
            signature,
            region_hash,
            disable_after,
            ..
        } => {
            state.pending_fire = Some(PendingPrompt {
                signature: *signature,
                region_hash: *region_hash,
                disable_after: *disable_after,
            });
        }
        Decision::Hold => {
            if let Some(matched) = matched {
                if state.pending_fire.is_some_and(|pending| {
                    pending.signature != matched.signature
                        || pending.region_hash != matched.region_hash
                }) {
                    state.pending_fire = None;
                }
                if state.awaiting_change.is_some_and(|consumed| {
                    consumed.signature != matched.signature
                        || consumed.region_hash != matched.region_hash
                }) {
                    state.awaiting_change = None;
                }
                let key = MatchKey {
                    signature: matched.signature,
                    region_hash: matched.region_hash,
                };
                if state.last_match == Some(key) {
                    state.match_count = state.match_count.saturating_add(1);
                } else {
                    state.last_match = Some(key);
                    state.match_count = 1;
                }
            } else {
                state.last_match = None;
                state.match_count = 0;
                state.pending_fire = None;
                // A partial status redraw can invalidate the live tail while
                // leaving the accepted dialog itself unchanged.
                state.awaiting_change = state.awaiting_change.filter(|consumed| {
                    menu_prompt_region(rows, &lowercase_rows(rows), signature(consumed.signature))
                        .is_some_and(|region| region_hash(rows, region) == consumed.region_hash)
                });
            }
        }
        Decision::Suppressed(reason) => {
            // A fast reply can become static during the input cooldown. Keep
            // rescanning that known prompt so it can arm when the guard expires,
            // while still requiring two unsuppressed matches before sending.
            state.last_match = if matches!(
                reason,
                SuppressReason::RecentUserInput | SuppressReason::PasteActive
            ) {
                matched.map(|matched| MatchKey {
                    signature: matched.signature,
                    region_hash: matched.region_hash,
                })
            } else {
                None
            };
            state.match_count = 0;
            state.pending_fire = None;
        }
    }
}

fn suppression(ctx: DetectContext, disabled_by_runaway: bool) -> Option<SuppressReason> {
    if disabled_by_runaway {
        return Some(SuppressReason::Disabled);
    }
    if !ctx.alt_screen && ctx.scrollback_offset != 0 {
        return Some(SuppressReason::ViewportNotLive);
    }
    if ctx.guards.ime_preedit_active {
        return Some(SuppressReason::ImePreedit);
    }
    if ctx
        .guards
        .paste_suppressed_until
        .is_some_and(|until| ctx.now < until)
    {
        return Some(SuppressReason::PasteActive);
    }
    if ctx.guards.last_user_input_at.is_some_and(|at| {
        ctx.now < at || ctx.now.saturating_duration_since(at) < USER_INPUT_SUPPRESSION
    }) {
        return Some(SuppressReason::RecentUserInput);
    }
    None
}

#[cfg(test)]
thread_local! {
    /// Per-test-thread count of viewport scans, so a test can assert one
    /// feed costs exactly one scan (tests run on their own threads).
    static PROMPT_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn find_prompt(rows: &[RowText], cursor: Point, agent: Option<AgentKind>) -> Option<MatchedPrompt> {
    #[cfg(test)]
    PROMPT_SCANS.with(|scans| scans.set(scans.get() + 1));
    let lowercase_rows = lowercase_rows(rows);
    SIGNATURES
        .iter()
        .filter(|sig| agent.is_none_or(|agent| sig.agent == agent))
        .find_map(|sig| find_signature_with_lowercase(rows, &lowercase_rows, cursor, sig))
}

fn find_signature(rows: &[RowText], cursor: Point, sig: &Signature) -> Option<MatchedPrompt> {
    let lowercase_rows = lowercase_rows(rows);
    find_signature_with_lowercase(rows, &lowercase_rows, cursor, sig)
}

fn find_signature_with_lowercase(
    rows: &[RowText],
    lowercase_rows: &[RowText],
    cursor: Point,
    sig: &Signature,
) -> Option<MatchedPrompt> {
    if rows.is_empty() || cursor.y as usize >= rows.len() {
        return None;
    }

    if matches!(
        sig.id,
        AutoApproveSignature::CodexCommand
            | AutoApproveSignature::AgyAskUserQuestion
            | AutoApproveSignature::AgyCommand
    ) {
        let region = menu_prompt_region(rows, lowercase_rows, sig)?;
        if !menu_has_live_tail(rows, *region.end(), sig) {
            return None;
        }
        return Some(MatchedPrompt {
            signature: sig.id,
            region_hash: region_hash(rows, region.clone()),
            region,
        });
    }

    let anchor_index = lowercase_rows
        .iter()
        .position(|row| sig.anchors.iter().any(|anchor| row.contains(anchor)))?;

    let option_index = match sig.yes_label {
        Some(label) => {
            let (index, _) = rows
                .iter()
                .enumerate()
                .skip(anchor_index)
                .find(|(_, row)| affirmative_selected(row, label, sig.requires_marker))?;
            index
        }
        None => anchor_index,
    };
    if cursor.y as usize != option_index {
        return None;
    }

    let end = rows
        .len()
        .saturating_sub(1)
        .min(option_index.saturating_add(2));
    let region = anchor_index..=end;
    Some(MatchedPrompt {
        signature: sig.id,
        region_hash: region_hash(rows, region.clone()),
        region,
    })
}

/// These TUIs select with a painted marker and can park the terminal cursor
/// below the menu (or hide it). The dialog's identity excludes mutable status
/// rows beneath its footer.
fn menu_prompt_region(
    rows: &[RowText],
    lowercase_rows: &[RowText],
    sig: &Signature,
) -> Option<RangeInclusive<usize>> {
    if sig.id == AutoApproveSignature::AgyCommand {
        let run_command = Signature {
            anchors: &["requesting permission for:"],
            yes_label: Some("1. Yes, run command"),
            ..*sig
        };
        if let Some(region) = match_menu_prompt_region(rows, lowercase_rows, &run_command) {
            return Some(region);
        }
    }
    match_menu_prompt_region(rows, lowercase_rows, sig)
}

fn match_menu_prompt_region(
    rows: &[RowText],
    lowercase_rows: &[RowText],
    sig: &Signature,
) -> Option<RangeInclusive<usize>> {
    let anchor = lowercase_rows
        .iter()
        .rposition(|row| sig.anchors.contains(&row.trim()))?;
    let footer_text = match sig.id {
        AutoApproveSignature::CodexCommand => "Press enter to confirm or esc to cancel",
        AutoApproveSignature::AgyAskUserQuestion => "↑/↓ Navigate · enter Select · esc Skip",
        AutoApproveSignature::AgyCommand => "↑/↓ Navigate · tab Amend · ctrl+g edit/expand command",
        _ => return None,
    };
    let footer = (anchor + 1..rows.len())
        .find(|&i| rows[i].split_whitespace().collect::<Vec<_>>().join(" ") == footer_text)?;
    let mut selected =
        (anchor + 1..footer).filter_map(|i| selected_option(&rows[i]).map(|label| (i, label)));
    let (option, label) = selected.next()?;
    if selected.next().is_some() {
        return None;
    }
    let expected = sig.yes_label?;
    let valid = match sig.id {
        AutoApproveSignature::CodexCommand => {
            label == expected && codex_command_menu(rows, anchor, option, footer)
        }
        AutoApproveSignature::AgyAskUserQuestion => {
            label
                .strip_prefix(expected)
                .is_some_and(|text| !text.trim().is_empty())
                && agy_question_menu(rows, anchor, option, footer)
        }
        AutoApproveSignature::AgyCommand => {
            label == expected && agy_command_menu(rows, anchor, option, footer)
        }
        _ => false,
    };
    valid.then_some(anchor..=footer)
}

fn menu_has_live_tail(rows: &[RowText], footer: usize, sig: &Signature) -> bool {
    let mut tail = rows[footer + 1..]
        .iter()
        .map(|row| row.trim())
        .filter(|row| !row.is_empty());
    let Some(status) = tail.next() else {
        return true;
    };
    sig.agent == AgentKind::Agy && agy_status_row(status) && tail.next().is_none()
}

/// agy's one-row status bar under a dialog: `[Model] Cost: $0.0000` while
/// idle, or `TOOL USE | Model | branch | Context: 6.94%` while a tool runs.
fn agy_status_row(status: &str) -> bool {
    let cost_status = status.starts_with('[')
        && status.split_once("] Cost: $").is_some_and(|(_, cost)| {
            !cost.is_empty() && cost.bytes().all(|ch| ch.is_ascii_digit() || ch == b'.')
        });
    let tool_status = status.strip_prefix("TOOL USE").is_some_and(|rest| {
        rest.rsplit_once("| Context: ").is_some_and(|(_, context)| {
            context.strip_suffix('%').is_some_and(|percent| {
                !percent.is_empty() && percent.bytes().all(|ch| ch.is_ascii_digit() || ch == b'.')
            })
        })
    });
    cost_status || tool_status
}

fn codex_command_menu(rows: &[RowText], anchor: usize, option: usize, footer: usize) -> bool {
    let context = &rows[anchor + 1..option];
    if !context.iter().any(|row| row.trim() == "Environment: local")
        || !context.iter().any(|row| {
            row.trim()
                .strip_prefix("$ ")
                .is_some_and(|command| !command.trim().is_empty())
        })
    {
        return false;
    }
    let options: Vec<_> = rows[option + 1..footer]
        .iter()
        .map(|row| row.trim())
        .filter(|row| !row.is_empty())
        .collect();
    let Some(last_option) = options
        .iter()
        .rposition(|row| row.starts_with("2. ") || row.starts_with("3. "))
    else {
        return false;
    };
    let (remember, rejection) = options.split_at(last_option);
    let expected = if remember.is_empty() {
        "2. No, and tell Codex what to do differently (esc)"
    } else {
        "3. No, and tell Codex what to do differently (esc)"
    };
    // A physical row can end inside a word or shortcut, so match each
    // fragment against the remaining label without inserting spaces.
    let remaining = rejection.iter().try_fold(expected, |remaining, row| {
        remaining.trim_start().strip_prefix(*row)
    });
    if remaining != Some("") {
        return false;
    }
    let remember = remember.join(" ");
    remember.is_empty()
        || (remember.starts_with("2. Yes, and don't ask again for commands that start with ")
            && remember.ends_with("(p)"))
}

fn agy_question_menu(rows: &[RowText], anchor: usize, option: usize, footer: usize) -> bool {
    let has_question = rows[anchor + 1..option].iter().any(|row| {
        let Some((counter, question)) = row
            .trim()
            .strip_prefix("Question ")
            .and_then(|text| text.split_once(':'))
        else {
            return false;
        };
        let Some((number, total)) = counter.split_once('/') else {
            return false;
        };
        matches!((number.parse::<u32>(), total.parse::<u32>()), (Ok(n), Ok(t)) if n > 0 && n <= t)
            && !question.trim().is_empty()
    });
    if !has_question {
        return false;
    }
    let mut next = 2;
    let mut write_in = false;
    for row in &rows[option + 1..footer] {
        let row = row.trim();
        if row.is_empty() {
            continue;
        }
        if write_in {
            return false;
        }
        if let Some((number, text)) = row.split_once(". ")
            && let Ok(number) = number.parse::<u32>()
        {
            if number != next || text.is_empty() {
                return false;
            }
            next += 1;
            write_in = text == "Write-in...";
        }
    }
    write_in
}

/// agy's permission dialog: `Requesting permission for:` followed by the
/// tool input and either `Do you want to proceed?` / `Yes` / `No` or
/// `Run this command?` / `Yes, run command` / `No, cancel`. Broader
/// `Yes, and always allow …` choices may wrap over several rows.
fn agy_command_menu(rows: &[RowText], anchor: usize, option: usize, footer: usize) -> bool {
    let (question, rejection) = match selected_option(&rows[option]) {
        Some("1. Yes") => ("Do you want to proceed?", "No"),
        Some("1. Yes, run command") => ("Run this command?", "No, cancel"),
        _ => return false,
    };
    let context: Vec<_> = rows[anchor..option].iter().map(|row| row.trim()).collect();
    let Some(request) = context
        .iter()
        .position(|row| *row == "Requesting permission for:")
    else {
        return false;
    };
    let Some(proceed) = context.iter().rposition(|row| *row == question) else {
        return false;
    };
    if proceed <= request + 1
        || !context[request + 1..proceed]
            .iter()
            .any(|row| !row.is_empty())
    {
        return false;
    }

    let mut next = 2;
    let mut saw_no = false;
    for row in &rows[option + 1..footer] {
        let row = row.trim();
        if row.is_empty() {
            continue;
        }
        if saw_no {
            return false;
        }
        match row
            .split_once(". ")
            .and_then(|(number, text)| number.parse::<u32>().ok().map(|number| (number, text)))
        {
            Some((number, text)) => {
                if number != next {
                    return false;
                }
                next += 1;
                if text == rejection {
                    saw_no = true;
                } else if !text.starts_with("Yes, and always allow ") {
                    return false;
                }
            }
            // A wrapped tail of the previous `Yes, and always allow …` choice.
            None => {
                if next == 2 {
                    return false;
                }
            }
        }
    }
    saw_no
}

fn lowercase_rows(rows: &[RowText]) -> Vec<RowText> {
    rows.iter().map(|row| row.to_ascii_lowercase()).collect()
}

fn selected_option(row: &str) -> Option<&str> {
    let label = row
        .trim_start()
        .strip_prefix(['❯', '›', '>'])
        .map(str::trim_start)?;
    // Command text and wrapped choice details can begin with shell redirections.
    let (number, _) = label.split_once(". ")?;
    (!number.is_empty() && number.bytes().all(|ch| ch.is_ascii_digit())).then_some(label)
}

fn affirmative_selected(row: &str, yes_label: &str, requires_marker: bool) -> bool {
    let trimmed = row.trim_start();
    let selected = trimmed
        .strip_prefix('❯')
        .or_else(|| trimmed.strip_prefix('>'));
    let candidate = if requires_marker {
        let Some(rest) = selected else {
            return false;
        };
        rest.trim_start()
    } else {
        selected.unwrap_or(trimmed).trim_start()
    };
    candidate.starts_with(yes_label)
}

fn count_recent_approvals(approvals: &VecDeque<Instant>, now: Instant) -> usize {
    approvals
        .iter()
        .filter(|at| now.saturating_duration_since(**at) <= APPROVAL_WINDOW)
        .count()
}

fn region_hash(rows: &[RowText], region: RangeInclusive<usize>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for idx in region {
        rows.get(idx).hash(&mut hasher);
    }
    hasher.finish()
}

fn row_text(cells: &[Cell]) -> String {
    let mut text = String::new();
    for cell in cells {
        cell.push_text_to(&mut text);
    }
    text.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> Instant {
        Instant::now()
    }

    fn base_ctx(now: Instant) -> DetectContext {
        DetectContext {
            now,
            alt_screen: true,
            scrollback_offset: 0,
            guards: AutoApproveInputGuards::default(),
        }
    }

    fn cursor(row: u16) -> Point {
        Point { x: 0, y: row }
    }

    fn claude_edit_prompt() -> Vec<RowText> {
        rows(&[
            "Claude wants to edit crates/noa-app/src/lib.rs",
            "❯ 1. Yes",
            "  2. No, tell Claude what to do differently",
        ])
    }

    fn prompt_scans() -> usize {
        PROMPT_SCANS.with(|scans| scans.get())
    }

    /// One feed scans the viewport once, whether it holds a prompt (first
    /// match: Hold), a second identical match (Fire), or nothing at all.
    #[test]
    fn detect_and_update_scans_viewport_once_per_feed() {
        let now = Instant::now();
        let mut state = AutoApproveState::default();
        let prompt = rows(&[
            "Claude wants to edit crates/noa-app/src/lib.rs",
            "❯ 1. Yes",
            "  2. No",
        ]);
        let plain = rows(&["plain output", "no prompt here"]);

        let before = prompt_scans();
        assert_eq!(
            detect_and_update_any_agent(&plain, cursor(1), base_ctx(now), &mut state),
            Decision::Hold
        );
        assert_eq!(prompt_scans() - before, 1, "no-prompt feed must scan once");

        let before = prompt_scans();
        assert_eq!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Hold
        );
        assert_eq!(prompt_scans() - before, 1, "first match must scan once");

        let before = prompt_scans();
        assert!(matches!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Fire { .. }
        ));
        assert_eq!(prompt_scans() - before, 1, "firing feed must scan once");

        let mut ctx = base_ctx(now);
        ctx.guards.last_user_input_at = Some(now);
        let before = prompt_scans();
        assert!(matches!(
            detect_and_update_any_agent(&prompt, cursor(1), ctx, &mut state),
            Decision::Suppressed(SuppressReason::RecentUserInput)
        ));
        assert_eq!(
            prompt_scans() - before,
            1,
            "input-cooldown feed must scan once"
        );
    }

    fn rows(input: &[&str]) -> Vec<RowText> {
        input.iter().map(|line| (*line).to_string()).collect()
    }

    // Synthetic content with the layouts supplied in the September 2026
    // screenshots; no local terminal transcript or command output is stored.
    fn codex_command_prompt() -> Vec<RowText> {
        rows(&[
            "Would you like to run the following command?",
            "",
            "Environment: local",
            "",
            "Reason: 変更をステージしてよいですか？",
            "",
            "$ git add sample.rs",
            "",
            "› 1. Yes, proceed (y)",
            "  2. Yes, and don't ask again for commands that start with `git add` (p)",
            "  3. No, and tell Codex what to do differently (esc)",
            "",
            "Press enter to confirm or esc to cancel",
        ])
    }

    fn agy_question_prompt() -> Vec<RowText> {
        rows(&[
            "Question",
            "────────────────────",
            "",
            "Question 1/1: どの作業を進めますか？",
            "",
            "> 1. (Recommended) テストを実行する",
            "  2. 静的解析を実行する",
            "  3. 変更内容を確認する",
            "  4. ドキュメントを読む",
            "  5. Write-in...",
            "",
            "  ↑/↓ Navigate · enter Select · esc Skip",
            "[Gemini 3.8 Flash (High)] Cost: $0.0000",
        ])
    }

    fn agy_command_prompt() -> Vec<RowText> {
        rows(&[
            "Command",
            "────────────────────",
            "",
            "Requesting permission for:",
            "    python3 -c '",
            "    import time, gzip, re",
            "",
            "    t0 = time.time()",
            "    ⋯ (10 lines hidden)",
            "",
            "Do you want to proceed?",
            "> 1. Yes",
            "  2. Yes, and always allow in this conversation for commands that start with",
            "'python3 -c '",
            "import time, gzip, re",
            "",
            "t0 = time.time()",
            "msg_id = \"35285538\"",
            "pat = r...'",
            "  3. Yes, and always allow for commands that start with 'python3 -c '",
            "import time, gzip, re",
            "",
            "t0 = time.time()",
            "msg_id = \"35285538\"",
            "pat = r...' (Persist to settings.json)",
            "  4. No",
            "",
            "  ↑/↓ Navigate · tab Amend · ctrl+g edit/expand command",
            " TOOL USE  | Gemini 3.8 Flash (High) |  main | Context: 6.94%",
        ])
    }

    fn agy_run_command_prompt() -> Vec<RowText> {
        rows(&[
            "────────────────────",
            "",
            "Requesting permission for:",
            "    git -C sample-project submodule status || true",
            "",
            "Run this command?",
            "> 1. Yes, run command",
            "  2. Yes, and always allow in this conversation for commands that start with 'git -C sample-project submodule status'",
            "  3. Yes, and always allow for commands that start with 'git -C sample-project submodule status' (Persist to settings.json)",
            "  4. No, cancel",
            "",
            "  ↑/↓ Navigate · tab Amend · ctrl+g edit/expand command",
            " TOOL USE  | Gemini 3.8 Flash (High) | Context: 2.12%",
        ])
    }

    fn agy_multiline_run_command_prompt() -> Vec<RowText> {
        rows(&[
            "Command",
            "────────────────────",
            "",
            "Requesting permission for:",
            "   for repo in sample-admin-python sample-admin-node; do",
            "     echo \"=== $repo ===\"",
            "     git -C \"$repo\" status --short",
            "     git -C \"$repo\" log -n 1 --oneline",
            "   ⋯ (1 lines hidden)",
            "",
            "Run this command?",
            "> 1. Yes, run command",
            "  2. Yes, and always allow in this conversation for commands that start with 'for repo in sample-admin-python sample-admin-node; do",
            "  echo \"=== $repo ===\"...'",
            "  3. Yes, and always allow for commands that start with 'for repo in sample-admin-python sample-admin-node; do",
            "  echo \"=== $repo ===\"...' (Persist to settings.json)",
            "  4. No, cancel",
            "",
            "  ↑/↓ Navigate · tab Amend · ctrl+g edit/expand command",
            " TOOL USE  | Gemini 3.8 Flash (High) | Context: 3.68%",
        ])
    }

    fn assert_no_auto_approval(prompt: &[RowText]) {
        let mut state = AutoApproveState::default();
        let ctx = base_ctx(fixed_now());
        for _ in 0..3 {
            assert_eq!(
                detect_and_update_any_agent(prompt, cursor(0), ctx, &mut state),
                Decision::Hold,
                "must not approve {prompt:?}"
            );
        }
    }

    #[test]
    fn detect_codex_and_agy_screenshot_layouts_confirm_only_once_with_enter() {
        let now = fixed_now();
        for (prompt, expected, agent, label) in [
            (
                codex_command_prompt(),
                AutoApproveSignature::CodexCommand,
                AgentKind::Codex,
                "Command",
            ),
            (
                agy_question_prompt(),
                AutoApproveSignature::AgyAskUserQuestion,
                AgentKind::Agy,
                "Question",
            ),
            (
                agy_command_prompt(),
                AutoApproveSignature::AgyCommand,
                AgentKind::Agy,
                "Command",
            ),
            (
                agy_run_command_prompt(),
                AutoApproveSignature::AgyCommand,
                AgentKind::Agy,
                "Command",
            ),
            (
                agy_multiline_run_command_prompt(),
                AutoApproveSignature::AgyCommand,
                AgentKind::Agy,
                "Command",
            ),
        ] {
            let mut state = AutoApproveState::default();
            let mut ctx = base_ctx(now);
            ctx.alt_screen = false;
            let cursor = cursor((prompt.len() - 1) as u16);
            assert_eq!(
                detect_and_update_any_agent(&prompt, cursor, ctx, &mut state),
                Decision::Hold
            );
            assert!(state.needs_static_rescan());
            let Decision::Fire {
                signature,
                region_hash,
                disable_after,
            } = detect_and_update_any_agent(&prompt, cursor, ctx, &mut state)
            else {
                panic!("stable menu should fire");
            };
            assert_eq!(signature, expected);
            assert_eq!(signature.agent(), agent);
            assert_eq!(signature.bytes(), b"\r");
            assert_eq!(signature.label(), label);
            assert!(!disable_after);
            assert_eq!(
                rescan_signature(&prompt, signature, cursor, ctx)
                    .unwrap()
                    .region_hash,
                region_hash
            );
            assert_eq!(
                detect_and_update_any_agent(&prompt, cursor, ctx, &mut state),
                Decision::Hold
            );
            state.apply_feedback(signature, region_hash, true, now);
            assert_eq!(
                detect_and_update_any_agent(&prompt, cursor, ctx, &mut state),
                Decision::Hold
            );
            assert!(!state.needs_static_rescan());
            assert_eq!(
                detect_and_update_any_agent(
                    &rows(&["Working..."]),
                    Point { x: 0, y: 0 },
                    ctx,
                    &mut state
                ),
                Decision::Hold
            );
            assert_eq!(
                detect_and_update_any_agent(&prompt, cursor, ctx, &mut state),
                Decision::Hold
            );
            assert!(matches!(
                detect_and_update_any_agent(&prompt, cursor, ctx, &mut state),
                Decision::Fire { .. }
            ));
        }
    }

    #[test]
    fn agy_command_redirections_do_not_count_as_selected_options() {
        for prompt in [
            agy_run_command_prompt(),
            agy_multiline_run_command_prompt(),
            agy_command_prompt(),
        ] {
            for redirection in ["> output.log", ">> output.log", ">| output.log"] {
                let mut prompt = prompt.clone();
                let command_row = prompt
                    .iter()
                    .position(|row| row == "Requesting permission for:")
                    .unwrap()
                    + 1;
                prompt.insert(command_row, format!("    {redirection}"));
                let mut terminal = Terminal::new(noa_core::GridSize::new(140, 40));
                let mut stream = noa_vt::Stream::new();
                stream.feed(prompt.join("\r\n").as_bytes(), &mut terminal);
                let screen = viewport_rows_from_terminal(&terminal);
                let cursor = Point {
                    x: terminal.active().cursor.x,
                    y: terminal.active().cursor.y,
                };
                let now = fixed_now();
                let ctx = base_ctx(now);
                let mut state = AutoApproveState::default();
                assert_eq!(
                    detect_and_update_any_agent(&screen, cursor, ctx, &mut state),
                    Decision::Hold
                );
                let Decision::Fire {
                    signature,
                    region_hash,
                    ..
                } = detect_and_update_any_agent(&screen, cursor, ctx, &mut state)
                else {
                    panic!("command redirection must not block approval: {screen:?}");
                };
                assert_eq!(signature, AutoApproveSignature::AgyCommand);
                assert_eq!(signature.bytes(), b"\r");
                assert_eq!(
                    rescan_signature(&screen, signature, cursor, ctx)
                        .unwrap()
                        .region_hash,
                    region_hash
                );
                state.apply_feedback(signature, region_hash, true, now);
                assert_eq!(
                    detect_and_update_any_agent(&screen, cursor, ctx, &mut state),
                    Decision::Hold
                );
            }
        }
    }

    #[test]
    fn detect_menu_requires_complete_context_and_selected_safe_choice() {
        for (prompt, mutations) in [
            (
                codex_command_prompt(),
                vec![
                    (0, "Would you like to do something else?"),
                    (2, "Environment: remote"),
                    (6, "$ "),
                    (8, "  1. Yes, proceed (y)"),
                    (8, "› 1. Yes, proceed (y), and remember this choice"),
                    (8, "› 2. Yes, and don't ask again (p)"),
                    (
                        9,
                        "› 2. Yes, and don't ask again for commands that start with `git add` (p)",
                    ),
                    (10, "  3. Yes, approve everything"),
                    (12, "Press enter to confirm"),
                ],
            ),
            (
                agy_question_prompt(),
                vec![
                    (0, "Not a question dialog"),
                    (3, "Question 0/1: Choose a task"),
                    (3, "Question 2/1: Choose a task"),
                    (3, "Question 1/1:"),
                    (5, "  1. (Recommended) テストを実行する"),
                    (5, "> 1. テストを実行する"),
                    (5, "> 2. (Recommended) テストを実行する"),
                    (6, "> 2. 静的解析を実行する"),
                    (9, "  5. Delete everything"),
                    (11, "space Toggle · enter Submit"),
                    (12, "$ another command"),
                ],
            ),
            (
                agy_command_prompt(),
                vec![
                    (0, "Question"),
                    (3, "Requesting something else:"),
                    (10, "Do you want to continue?"),
                    (11, "  1. Yes"),
                    (11, "> 1. Yes, and always allow in this conversation"),
                    (
                        12,
                        "> 2. Yes, and always allow in this conversation for commands that start with",
                    ),
                    (12, "  2. Yes, approve everything"),
                    (
                        12,
                        "  3. Yes, and always allow in this conversation for commands that start with",
                    ),
                    (12, "stray text before any broader choice"),
                    (25, "  4. No, and tell agy what to do differently"),
                    (25, "  5. No"),
                    (25, ""),
                    (27, "  ↑/↓ Navigate · enter Select · esc Skip"),
                    (28, "$ another command"),
                    (
                        28,
                        "TOOL USE  | Gemini 3.8 Flash (High) |  main | Context: n/a",
                    ),
                ],
            ),
            (
                agy_run_command_prompt(),
                vec![
                    (2, "Requesting something else:"),
                    (3, ""),
                    (5, "Do you want to proceed?"),
                    (5, "Run another command?"),
                    (6, "  1. Yes, run command"),
                    (6, "> 1. Yes"),
                    (6, "> 1. Yes, run command, and always allow"),
                    (
                        7,
                        "> 2. Yes, and always allow in this conversation for commands that start with 'git'",
                    ),
                    (7, "  2. Yes, approve everything"),
                    (7, "stray text before any broader choice"),
                    (
                        8,
                        "> 3. Yes, and always allow for commands that start with 'git' (Persist to settings.json)",
                    ),
                    (9, "  4. No"),
                    (9, "  4. No, cancel and do something else"),
                    (9, "  5. No, cancel"),
                    (9, ""),
                    (11, "  ↑/↓ Navigate · enter Select · esc Skip"),
                    (12, "$ another command"),
                    (12, "TOOL USE | Gemini 3.8 Flash (High) | Context: n/a"),
                ],
            ),
        ] {
            for (index, replacement) in mutations {
                let mut changed = prompt.clone();
                changed[index] = replacement.to_string();
                assert_no_auto_approval(&changed);
            }
            // Earlier output cannot be mistaken for an active dialog.
            let mut stale = prompt.clone();
            stale.push("The task is complete. Type another request.".to_string());
            assert_no_auto_approval(&stale);
            for prefix_len in 0..prompt.len() - 1 {
                assert_no_auto_approval(&prompt[..prefix_len]);
            }
        }
    }

    #[test]
    fn detect_menu_supports_wrapped_choices_and_no_remember_option() {
        let mut codex = codex_command_prompt();
        codex.remove(9);
        codex[9] = "  2. No, and tell Codex what to do differently (esc)".to_string();
        let mut agy = agy_question_prompt();
        agy.insert(
            6,
            "     with additional details on the next row".to_string(),
        );
        agy[3] = "Question 2/3: 次の作業は？".to_string();
        for prompt in [codex, agy] {
            let mut state = AutoApproveState::default();
            let ctx = base_ctx(fixed_now());
            assert_eq!(
                detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state),
                Decision::Hold
            );
            assert!(matches!(
                detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state),
                Decision::Fire { .. }
            ));
        }
    }

    #[test]
    fn menu_signatures_remain_bound_to_their_agent_and_input_guards() {
        let now = fixed_now();
        for (prompt, agent) in [
            (codex_command_prompt(), AgentKind::Codex),
            (agy_question_prompt(), AgentKind::Agy),
            (agy_command_prompt(), AgentKind::Agy),
            (agy_run_command_prompt(), AgentKind::Agy),
        ] {
            let mut state = AutoApproveState::default();
            let ctx = base_ctx(now);
            let _ = detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state);
            assert!(matches!(
                detect(&prompt, cursor(0), agent, ctx, &state),
                Decision::Fire { .. }
            ));
            for other in [
                AgentKind::ClaudeCode,
                AgentKind::Codex,
                AgentKind::Agy,
                AgentKind::Generic,
            ] {
                if other != agent {
                    assert!(!matches!(
                        detect(&prompt, cursor(0), other, ctx, &state),
                        Decision::Fire { .. }
                    ));
                }
            }
            let mut blocked = ctx;
            blocked.alt_screen = false;
            blocked.scrollback_offset = 1;
            assert_eq!(
                detect(&prompt, cursor(0), agent, blocked, &state),
                Decision::Suppressed(SuppressReason::ViewportNotLive)
            );
            blocked = ctx;
            blocked.guards.ime_preedit_active = true;
            assert_eq!(
                detect(&prompt, cursor(0), agent, blocked, &state),
                Decision::Suppressed(SuppressReason::ImePreedit)
            );
        }
    }

    #[test]
    fn menu_hash_covers_command_question_and_all_choices_but_not_cost() {
        let now = fixed_now();
        let ctx = base_ctx(now);
        for (mut prompt, change_row) in [
            (codex_command_prompt(), 6),
            (agy_question_prompt(), 8),
            (agy_command_prompt(), 5),
            (agy_run_command_prompt(), 3),
        ] {
            let mut state = AutoApproveState::default();
            let _ = detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state);
            let Decision::Fire {
                signature,
                region_hash,
                ..
            } = detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state)
            else {
                panic!("stable menu should fire");
            };
            state.apply_feedback(signature, region_hash, true, now);
            if signature == AutoApproveSignature::AgyAskUserQuestion {
                prompt[12] = "[Gemini 3.8 Flash (High)] Cost: $0.0100".to_string();
            }
            if signature == AutoApproveSignature::AgyCommand {
                *prompt.last_mut().unwrap() =
                    " TOOL USE  | Gemini 3.8 Flash (High) |  main | Context: 7.10%".to_string();
            }
            if signature.agent() == AgentKind::Agy {
                assert_eq!(
                    detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state),
                    Decision::Hold
                );
            }
            prompt[change_row].push_str(" --changed");
            assert_ne!(
                rescan_signature(&prompt, signature, cursor(0), ctx)
                    .unwrap()
                    .region_hash,
                region_hash
            );
            assert_eq!(
                detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state),
                Decision::Hold
            );
            assert!(matches!(
                detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state),
                Decision::Fire { .. }
            ));
            let selected = prompt
                .iter_mut()
                .find(|row| selected_option(row).is_some())
                .unwrap();
            *selected = selected_option(selected).unwrap().to_string();
            assert!(rescan_signature(&prompt, signature, cursor(0), ctx).is_none());
        }
    }

    #[test]
    fn static_menu_rearms_after_input_cooldown_without_new_output() {
        let now = fixed_now();
        for prompt in [
            codex_command_prompt(),
            agy_question_prompt(),
            agy_command_prompt(),
            agy_run_command_prompt(),
        ] {
            for paste in [false, true] {
                let mut state = AutoApproveState::default();
                let mut ctx = base_ctx(now);
                if paste {
                    ctx.guards.mark_paste(now);
                } else {
                    ctx.guards.mark_user_input(now);
                }
                assert!(matches!(
                    detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state),
                    Decision::Suppressed(_)
                ));
                assert!(state.needs_static_rescan());
                ctx.now += USER_INPUT_SUPPRESSION;
                assert_eq!(
                    detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state),
                    Decision::Hold
                );
                assert!(matches!(
                    detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state),
                    Decision::Fire { .. }
                ));
            }
        }
        let mut state = AutoApproveState::default();
        let mut ctx = base_ctx(now);
        ctx.guards.mark_user_input(now);
        let _ = detect_and_update_any_agent(&codex_command_prompt(), cursor(0), ctx, &mut state);
        let _ =
            detect_and_update_any_agent(&rows(&["unrelated output"]), cursor(0), ctx, &mut state);
        assert!(!state.needs_static_rescan());
    }

    #[test]
    fn static_rescan_tracks_changed_prompts_after_an_accepted_approval() {
        let now = fixed_now();
        let mut state = AutoApproveState::default();
        let mut ctx = base_ctx(now);
        let prompt = codex_command_prompt();
        let _ = detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state);
        let Decision::Fire {
            signature,
            region_hash,
            ..
        } = detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state)
        else {
            panic!("stable menu should fire");
        };
        state.apply_feedback(signature, region_hash, true, now);
        ctx.guards.mark_user_input(now);
        let _ = detect_and_update_any_agent(&prompt, cursor(0), ctx, &mut state);
        assert!(
            !state.needs_static_rescan(),
            "an unchanged consumed dialog must stay idle"
        );
        let mut changed = prompt;
        changed[6] = "$ git diff --cached".to_string();
        let _ = detect_and_update_any_agent(&changed, cursor(0), ctx, &mut state);
        assert!(
            state.needs_static_rescan(),
            "a new dialog must survive the cooldown"
        );
        ctx.now += USER_INPUT_SUPPRESSION;
        let _ = detect_and_update_any_agent(&changed, cursor(0), ctx, &mut state);
        let Decision::Fire {
            signature,
            region_hash,
            ..
        } = detect_and_update_any_agent(&changed, cursor(0), ctx, &mut state)
        else {
            panic!("new command should fire");
        };
        state.apply_feedback(signature, region_hash, true, ctx.now);
        let _ = detect_and_update_any_agent(&agy_question_prompt(), cursor(0), ctx, &mut state);
        assert!(
            state.needs_static_rescan(),
            "changing signature also advances the screen"
        );
    }

    #[test]
    fn detect_menu_from_vt_grid_with_hidden_cursor_and_split_utf8() {
        for (prompt, expected, cols) in [
            (
                codex_command_prompt(),
                AutoApproveSignature::CodexCommand,
                140,
            ),
            (
                agy_question_prompt(),
                AutoApproveSignature::AgyAskUserQuestion,
                140,
            ),
            (agy_command_prompt(), AutoApproveSignature::AgyCommand, 140),
            (
                agy_run_command_prompt(),
                AutoApproveSignature::AgyCommand,
                140,
            ),
            (
                agy_multiline_run_command_prompt(),
                AutoApproveSignature::AgyCommand,
                90,
            ),
            (
                agy_multiline_run_command_prompt(),
                AutoApproveSignature::AgyCommand,
                140,
            ),
        ] {
            // Tall enough for every fixture: a dialog taller than the viewport
            // scrolls its title off and is (correctly) never recognized.
            let grid_rows = prompt.len() as u16 + 4;
            let mut terminal = Terminal::new(noa_core::GridSize::new(cols, grid_rows));
            let mut stream = noa_vt::Stream::new();
            let frame = format!("\x1b[?25l\x1b[36m{}\x1b[0m\r\n", prompt.join("\r\n"));
            for chunk in frame.as_bytes().chunks(7) {
                stream.feed(chunk, &mut terminal);
            }
            let screen = viewport_rows_from_terminal(&terminal);
            let cursor = terminal.active().cursor;
            assert!(!cursor.visible);
            let cursor = Point {
                x: cursor.x,
                y: cursor.y,
            };
            let mut state = AutoApproveState::default();
            let ctx = base_ctx(fixed_now());
            assert_eq!(
                detect_and_update_any_agent(&screen, cursor, ctx, &mut state),
                Decision::Hold
            );
            assert!(
                matches!(detect_and_update_any_agent(&screen, cursor, ctx, &mut state), Decision::Fire { signature, .. } if signature == expected)
            );
        }
    }

    #[test]
    fn agy_partial_status_redraw_does_not_repeat_accepted_approval() {
        // (fixture, status row, partial redraw, completion, dialog edit, edited row)
        for (prompt, status_row, partial, rest, edit, edit_row) in [
            (
                agy_question_prompt(),
                12u16,
                "[Gemini 3.8 Flash (High)] Cost: $",
                "0.0100",
                "Question 1/1: 次はどの作業を進めますか？",
                3u16,
            ),
            (
                agy_command_prompt(),
                28,
                " TOOL USE  | Gemini 3.8 Flash (High) |  main | Context: ",
                "7.10%",
                "    python3 -c 'print(1)'",
                4,
            ),
            (
                agy_run_command_prompt(),
                12,
                " TOOL USE  | Gemini 3.8 Flash (High) | Context: ",
                "2.50%",
                "    git -C sample-project diff",
                3,
            ),
        ] {
            let grid_rows = prompt.len() as u16 + 4;
            let mut terminal = Terminal::new(noa_core::GridSize::new(140, grid_rows));
            let mut stream = noa_vt::Stream::new();
            stream.feed(prompt.join("\r\n").as_bytes(), &mut terminal);
            let now = fixed_now();
            let ctx = base_ctx(now);
            let mut state = AutoApproveState::default();
            let screen = viewport_rows_from_terminal(&terminal);
            assert_eq!(
                detect_and_update_any_agent(&screen, cursor(status_row), ctx, &mut state),
                Decision::Hold
            );
            let Decision::Fire {
                signature,
                region_hash,
                ..
            } = detect_and_update_any_agent(&screen, cursor(status_row), ctx, &mut state)
            else {
                panic!("stable dialog should fire: {screen:?}");
            };
            state.apply_feedback(signature, region_hash, true, now);

            stream.feed(
                format!("\x1b[{};1H\x1b[2K{partial}", status_row + 1).as_bytes(),
                &mut terminal,
            );
            let partial_screen = viewport_rows_from_terminal(&terminal);
            assert_no_auto_approval(&partial_screen);
            assert_eq!(
                detect_and_update_any_agent(&partial_screen, cursor(status_row), ctx, &mut state),
                Decision::Hold
            );
            stream.feed(rest.as_bytes(), &mut terminal);
            let restored = viewport_rows_from_terminal(&terminal);
            assert_eq!(
                rescan_signature(&restored, signature, cursor(status_row), ctx)
                    .unwrap()
                    .region_hash,
                region_hash
            );
            for _ in 0..3 {
                assert_eq!(
                    detect_and_update_any_agent(&restored, cursor(status_row), ctx, &mut state),
                    Decision::Hold,
                    "redrawing only the status row must not send another Enter"
                );
            }
            assert!(!state.needs_static_rescan());
            assert_eq!(state.approvals.len(), 1);

            stream.feed(
                format!("\x1b[{};1H\x1b[2K{edit}", edit_row + 1).as_bytes(),
                &mut terminal,
            );
            let changed = viewport_rows_from_terminal(&terminal);
            assert_eq!(
                detect_and_update_any_agent(&changed, cursor(edit_row), ctx, &mut state),
                Decision::Hold
            );
            assert!(matches!(
                detect_and_update_any_agent(&changed, cursor(edit_row), ctx, &mut state),
                Decision::Fire { region_hash: new_hash, .. } if new_hash != region_hash
            ));
        }
    }

    #[test]
    fn codex_wrapped_rejection_is_detected_from_vt_grid() {
        for remember in [false, true] {
            let mut prompt = codex_command_prompt();
            if !remember {
                prompt.remove(9);
                prompt[9] = "  2. No, and tell Codex what to do differently (esc)".to_string();
            }
            let mut terminal = Terminal::new(noa_core::GridSize::new(48, 30));
            let mut stream = noa_vt::Stream::new();
            stream.feed(prompt.join("\r\n").as_bytes(), &mut terminal);
            let screen = viewport_rows_from_terminal(&terminal);
            let rejection_end = screen
                .iter()
                .position(|row| row.trim() == "esc)")
                .expect("the rejection shortcut should wrap onto another physical row");
            let mut state = AutoApproveState::default();
            let ctx = base_ctx(fixed_now());
            let position = terminal.active().cursor;
            let cursor = Point {
                x: position.x,
                y: position.y,
            };
            assert_eq!(
                detect_and_update_any_agent(&screen, cursor, ctx, &mut state),
                Decision::Hold
            );
            let Decision::Fire {
                signature,
                region_hash,
                ..
            } = detect_and_update_any_agent(&screen, cursor, ctx, &mut state)
            else {
                panic!("complete wrapped menu should fire (remember={remember}): {screen:?}");
            };
            assert_eq!(signature, AutoApproveSignature::CodexCommand);
            assert_eq!(signature.bytes(), b"\r");
            assert_eq!(
                rescan_signature(&screen, signature, cursor, ctx)
                    .unwrap()
                    .region_hash,
                region_hash
            );
            state.apply_feedback(signature, region_hash, true, ctx.now);
            for _ in 0..3 {
                assert_eq!(
                    detect_and_update_any_agent(&screen, cursor, ctx, &mut state),
                    Decision::Hold
                );
            }
            for invalid_suffix in ["", "esc", "enter)", "esc) extra output"] {
                let mut incomplete = screen.clone();
                incomplete[rejection_end] = invalid_suffix.to_string();
                assert_no_auto_approval(&incomplete);
            }
        }
    }

    #[test]
    fn detect_holds_for_generic_agent_even_with_known_signature() {
        let now = fixed_now();
        let state = AutoApproveState::default();
        assert_eq!(
            detect(
                &claude_edit_prompt(),
                cursor(1),
                AgentKind::Generic,
                base_ctx(now),
                &state
            ),
            Decision::Suppressed(SuppressReason::UnknownAgent)
        );
    }

    #[test]
    fn detect_holds_for_bash_approval_and_unknown_text() {
        let now = fixed_now();
        let state = AutoApproveState::default();
        for fixture in [
            rows(&["Claude wants to use Bash", "❯ 1. Yes"]),
            rows(&["Proceed with unsafe operation?", "❯ 1. Yes"]),
        ] {
            assert_eq!(
                detect(
                    &fixture,
                    cursor(1),
                    AgentKind::ClaudeCode,
                    base_ctx(now),
                    &state
                ),
                Decision::Hold
            );
        }
    }

    #[test]
    fn detect_requires_marker_on_first_affirmative_choice() {
        let now = fixed_now();
        let state = AutoApproveState {
            last_match: Some(MatchKey {
                signature: AutoApproveSignature::ClaudeEdit,
                region_hash: region_hash(&claude_edit_prompt(), 0..=2),
            }),
            match_count: 1,
            ..Default::default()
        };
        for fixture in [
            rows(&[
                "Claude wants to edit crates/noa-app/src/lib.rs",
                "  1. Yes",
                "❯ 2. No",
            ]),
            rows(&[
                "Claude wants to edit crates/noa-app/src/lib.rs",
                "  1. Yes",
                "  2. No",
            ]),
        ] {
            assert_eq!(
                detect(
                    &fixture,
                    cursor(1),
                    AgentKind::ClaudeCode,
                    base_ctx(now),
                    &state
                ),
                Decision::Hold
            );
        }
    }

    #[test]
    fn detect_requires_two_consecutive_matching_scans() {
        let now = fixed_now();
        let mut state = AutoApproveState::default();
        let prompt = claude_edit_prompt();
        assert!(!state.needs_static_rescan());
        assert_eq!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Hold
        );
        assert!(state.needs_static_rescan());
        let second = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        assert!(matches!(
            second,
            Decision::Fire {
                signature: AutoApproveSignature::ClaudeEdit,
                ..
            }
        ));
        assert!(!state.needs_static_rescan());

        let mut state = AutoApproveState::default();
        let _ = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        assert_eq!(
            detect_and_update_any_agent(&rows(&[""]), cursor(0), base_ctx(now), &mut state),
            Decision::Hold
        );
        assert_eq!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Hold
        );
    }

    #[test]
    fn detect_suppresses_when_not_alt_screen_and_scrolled_back() {
        let now = fixed_now();
        let state = AutoApproveState::default();
        let mut ctx = base_ctx(now);
        ctx.alt_screen = false;
        ctx.scrollback_offset = 1;
        assert_eq!(
            detect(
                &claude_edit_prompt(),
                cursor(1),
                AgentKind::ClaudeCode,
                ctx,
                &state
            ),
            Decision::Suppressed(SuppressReason::ViewportNotLive)
        );
    }

    #[test]
    fn detect_does_not_refire_until_matched_region_hash_changes() {
        let now = fixed_now();
        let prompt = claude_edit_prompt();
        let mut state = AutoApproveState::default();
        let _ = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        let _ = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        assert_eq!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Hold
        );
        let changed = rows(&[
            "Claude wants to edit crates/noa-app/src/main.rs",
            "❯ 1. Yes",
            "  2. No",
        ]);
        assert_eq!(
            detect_and_update_any_agent(&changed, cursor(1), base_ctx(now), &mut state),
            Decision::Hold
        );
        assert!(matches!(
            detect_and_update_any_agent(&changed, cursor(1), base_ctx(now), &mut state),
            Decision::Fire { .. }
        ));
    }

    #[test]
    fn fire_waits_for_feedback_before_consuming_prompt() {
        let now = fixed_now();
        let prompt = claude_edit_prompt();
        let mut state = AutoApproveState::default();
        let _ = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        let decision = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        let Decision::Fire {
            signature,
            region_hash,
            ..
        } = decision
        else {
            panic!("second stable scan should fire: {decision:?}");
        };

        assert_eq!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Hold,
            "pending candidates must not spam the main thread"
        );

        state.apply_feedback(signature, region_hash, false, now);
        assert!(state.needs_static_rescan());
        assert!(matches!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Fire { .. }
        ));

        state.apply_feedback(signature, region_hash, true, now);
        assert_eq!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Hold,
            "accepted prompts stay consumed until their region changes"
        );
    }

    #[test]
    fn detect_suppresses_during_ime_paste_or_recent_user_input() {
        let now = fixed_now();
        let state = AutoApproveState::default();
        let mut ctx = base_ctx(now);
        ctx.guards.ime_preedit_active = true;
        assert_eq!(
            detect(
                &claude_edit_prompt(),
                cursor(1),
                AgentKind::ClaudeCode,
                ctx,
                &state
            ),
            Decision::Suppressed(SuppressReason::ImePreedit)
        );

        let mut ctx = base_ctx(now);
        ctx.guards.paste_suppressed_until = Some(now + Duration::from_secs(1));
        assert_eq!(
            detect(
                &claude_edit_prompt(),
                cursor(1),
                AgentKind::ClaudeCode,
                ctx,
                &state
            ),
            Decision::Suppressed(SuppressReason::PasteActive)
        );

        let mut ctx = base_ctx(now);
        ctx.guards.last_user_input_at = Some(now - Duration::from_secs(2));
        assert_eq!(
            detect(
                &claude_edit_prompt(),
                cursor(1),
                AgentKind::ClaudeCode,
                ctx,
                &state
            ),
            Decision::Suppressed(SuppressReason::RecentUserInput)
        );
    }

    #[test]
    fn detect_disables_after_six_approvals_in_sixty_seconds() {
        let now = fixed_now();
        let prompt = claude_edit_prompt();
        let mut state = AutoApproveState {
            approvals: VecDeque::from(vec![
                now - Duration::from_secs(10),
                now - Duration::from_secs(9),
                now - Duration::from_secs(8),
                now - Duration::from_secs(7),
                now - Duration::from_secs(6),
            ]),
            ..Default::default()
        };
        let _ = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        let decision = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        let Decision::Fire {
            signature,
            region_hash,
            disable_after,
            ..
        } = decision
        else {
            panic!("sixth approval should fire: {decision:?}");
        };
        assert!(disable_after);
        state.apply_feedback(signature, region_hash, true, now);
        assert!(matches!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Suppressed(SuppressReason::Disabled)
        ));
    }
}
