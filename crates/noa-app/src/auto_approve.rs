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
pub(crate) const AUDIT_CAPACITY: usize = 16;

pub(crate) type RowText = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PromptKind {
    Edit,
    Write,
    Read,
    AskUserQuestion,
    EnterConfirm,
}

impl PromptKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::Write => "Write",
            Self::Read => "Read",
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
        anchors: &["Claude wants to edit"],
        yes_label: Some("1. Yes"),
        requires_marker: true,
        bytes: b"1\r",
    },
    Signature {
        id: AutoApproveSignature::ClaudeWrite,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::Write,
        anchors: &["Claude wants to write", "Claude wants to create"],
        yes_label: Some("1. Yes"),
        requires_marker: true,
        bytes: b"1\r",
    },
    Signature {
        id: AutoApproveSignature::ClaudeRead,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::Read,
        anchors: &["Claude wants to read"],
        yes_label: Some("1. Yes"),
        requires_marker: true,
        bytes: b"1\r",
    },
    Signature {
        id: AutoApproveSignature::ClaudeAskUserQuestion,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::AskUserQuestion,
        anchors: &["Claude has a question", "Claude asks"],
        yes_label: Some("1."),
        requires_marker: true,
        bytes: b"1\r",
    },
    Signature {
        id: AutoApproveSignature::ClaudeEnterConfirm,
        agent: AgentKind::ClaudeCode,
        kind: PromptKind::EnterConfirm,
        anchors: &["Press Enter to continue"],
        yes_label: None,
        requires_marker: false,
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
        bytes: &'static [u8],
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
    awaiting_change: Option<ConsumedPrompt>,
    approvals: VecDeque<Instant>,
    disabled_by_runaway: bool,
}

impl AutoApproveState {
    pub(crate) fn reset_for_mode_off(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MatchKey {
    signature: AutoApproveSignature,
    region_hash: u64,
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

pub(crate) fn detect_any_agent(
    rows: &[RowText],
    cursor: Point,
    ctx: DetectContext,
    state: &AutoApproveState,
) -> Decision {
    detect_inner(rows, cursor, None, ctx, state)
}

pub(crate) fn detect_and_update_any_agent(
    rows: &[RowText],
    cursor: Point,
    ctx: DetectContext,
    state: &mut AutoApproveState,
) -> Decision {
    let decision = detect_any_agent(rows, cursor, ctx, state);
    apply_decision_state(rows, cursor, ctx, state, &decision);
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

    let Some(matched) = find_prompt(rows, cursor, agent) else {
        return Decision::Hold;
    };

    if state.awaiting_change.is_some_and(|consumed| {
        consumed.signature == matched.signature && consumed.region_hash == matched.region_hash
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
        bytes: matched.signature.bytes(),
        region_hash: matched.region_hash,
        disable_after: approvals_in_window + 1 >= APPROVAL_LIMIT,
    }
}

fn apply_decision_state(
    rows: &[RowText],
    cursor: Point,
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
            state.approvals.push_back(ctx.now);
            state.awaiting_change = Some(ConsumedPrompt {
                signature: *signature,
                region_hash: *region_hash,
            });
            state.last_match = None;
            state.match_count = 0;
            state.disabled_by_runaway = *disable_after;
        }
        Decision::Hold => {
            if let Some(matched) = find_prompt(rows, cursor, None) {
                if state.awaiting_change.is_some_and(|consumed| {
                    consumed.signature == matched.signature
                        && consumed.region_hash != matched.region_hash
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
                state.awaiting_change = None;
            }
        }
        Decision::Suppressed(_) => {
            state.last_match = None;
            state.match_count = 0;
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

fn find_prompt(rows: &[RowText], cursor: Point, agent: Option<AgentKind>) -> Option<MatchedPrompt> {
    SIGNATURES
        .iter()
        .filter(|sig| agent.is_none_or(|agent| sig.agent == agent))
        .find_map(|sig| find_signature(rows, cursor, sig))
}

fn find_signature(rows: &[RowText], cursor: Point, sig: &Signature) -> Option<MatchedPrompt> {
    if rows.is_empty() || cursor.y as usize >= rows.len() {
        return None;
    }

    let anchor_index = rows.iter().position(|row| {
        let lowered = row.to_ascii_lowercase();
        sig.anchors
            .iter()
            .any(|anchor| lowered.contains(&anchor.to_ascii_lowercase()))
    })?;

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

    fn rows(input: &[&str]) -> Vec<RowText> {
        input.iter().map(|line| (*line).to_string()).collect()
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
        assert_eq!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Hold
        );
        let second = detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state);
        assert!(matches!(
            second,
            Decision::Fire {
                signature: AutoApproveSignature::ClaudeEdit,
                ..
            }
        ));

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
        assert!(matches!(
            decision,
            Decision::Fire {
                disable_after: true,
                ..
            }
        ));
        assert_eq!(
            detect_and_update_any_agent(&prompt, cursor(1), base_ctx(now), &mut state),
            Decision::Suppressed(SuppressReason::Disabled)
        );
    }
}
