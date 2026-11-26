//! The `/` text filter (AC2): which workers the panels show.

use noa_gpui_core::Event;

use crate::fleet::{Worker, one_line};

/// The query typed into the header's field, kept across rebuilds (like the
/// feed) so a rebuilt field shows the same text with the caret where it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Filter {
    pub(super) query: String,
    /// The caret as a byte index into `query`.
    pub(super) caret: usize,
}

impl Filter {
    /// The filter is on once anything is typed, blanks too: the header
    /// shows the match count, an empty panel says "該当なし" and the graph
    /// mutes what does not match, all by this one rule. (A query of blanks
    /// matches every worker, so it hides no one.)
    pub(super) fn active(&self) -> bool {
        !self.query.is_empty()
    }

    pub(super) fn matches(&self, worker: &Worker) -> bool {
        matches(worker, &self.query)
    }
}

/// Every whitespace-separated term of `query` (ASCII space and U+3000 both
/// separate), lowercased, must match the worker. A term may be prefixed with
/// `-` to negate it, and may use a `field:value` qualifier (`state:`/`s:`,
/// `need:`/`n:`, `agent:`/`a:`, `model:`/`m:`, `branch:`/`b:`, `cwd:`/`c:`,
/// `parent:`/`p:`, `label:`/`l:`, `task:`/`t:`, `result:`/`r:`; `state:closed`
/// is a worker whose pane is gone, rebuilt from its journal;
/// `state:waiting` is [`Worker::needs_attention`], while `need:any` is any
/// need, notices and quiet workers included). Plain terms match label,
/// agent, model, branch or task, never the cwd: every cwd shares the repo's
/// path. An empty query matches every worker.
pub(super) fn matches(worker: &Worker, query: &str) -> bool {
    let fields = [
        &worker.label,
        &worker.kind,
        &worker.model,
        &worker.branch,
        &worker.task,
    ]
    .map(|field| field.to_lowercase());
    query
        .split(char::is_whitespace)
        .filter(|term| !term.is_empty())
        .all(|raw| match_term(worker, &fields, raw))
}

fn match_term(worker: &Worker, fields: &[String; 5], raw: &str) -> bool {
    let (negated, term) = match raw.strip_prefix('-') {
        Some(rest) if !rest.is_empty() => (true, rest),
        _ => (false, raw),
    };
    let term = term.to_lowercase();
    let matched = match_qualified(worker, &term)
        .unwrap_or_else(|| fields.iter().any(|field| field.contains(&term)));
    if negated { !matched } else { matched }
}

fn match_qualified(worker: &Worker, term: &str) -> Option<bool> {
    let (key, value) = term.split_once(':')?;
    if value.is_empty() {
        return None;
    }
    use crate::fleet::{Need, State};
    let hit = match key {
        "state" | "s" => match value {
            "running" | "作業中" => worker.state == State::Running,
            "permission" | "approval" | "承認" | "承認待ち" => {
                worker.state == State::Permission
            }
            "input" | "question" | "入力" | "入力待ち" | "質問" => {
                worker.state == State::Input
            }
            "finished" | "done" | "完了" => worker.state == State::Finished,
            "error" | "fail" | "failure" | "失敗" => worker.state == State::Error,
            "quiet" | "静止" => worker.quiet,
            "closed" | "閉じた" => worker.is_closed(),
            "waiting" | "要対応" => worker.needs_attention(),
            _ => false,
        },
        "need" | "n" => match value {
            "approval" | "承認" | "承認待ち" => worker.need() == Some(Need::Approval),
            "question" | "質問" => worker.need() == Some(Need::Question),
            "failure" | "失敗" => worker.need() == Some(Need::Failure),
            "notice" | "通知" => worker.need() == Some(Need::Notice),
            "quiet" | "静止" => worker.need() == Some(Need::Quiet),
            "decision" | "判断待ち" => worker
                .need()
                .is_some_and(|n| n.tier() == crate::fleet::Tier::Decision),
            "any" | "あり" => worker.need().is_some(),
            _ => false,
        },
        "agent" | "a" => worker.kind.to_lowercase().contains(value),
        "model" | "m" => {
            worker.model.to_lowercase().contains(value)
                || worker.effort.to_lowercase().contains(value)
        }
        "branch" | "b" => worker.branch.to_lowercase().contains(value),
        "cwd" | "c" => worker.cwd.to_lowercase().contains(value),
        "parent" | "p" => worker.orchestrator.to_lowercase().contains(value),
        "label" | "l" => worker.label.to_lowercase().contains(value),
        "task" | "t" => worker.task.to_lowercase().contains(value),
        "result" | "r" => match value {
            "yes" | "true" | "1" | "あり" => worker.has_result,
            "no" | "false" | "0" | "なし" => !worker.has_result,
            _ => false,
        },
        _ => return None,
    };
    Some(hit)
}

/// Typed text and an IME's composition as the filter field is given them:
/// on one line ([`one_line`]), with the composition's cursor moved to match.
/// The field drops control characters itself but keeps a line or
/// paragraph separator, and the shaper panics on a paragraph separator
/// next to right-to-left text. A paste does not come as an event (the
/// field reads the clipboard): `on_one_line` puts it right after.
pub(super) fn one_line_input(event: Event) -> Event {
    match event {
        Event::Text(text) => Event::Text(one_line(&text)),
        Event::Composition { text, cursor } => {
            // A byte offset into `text` to the same place in its one line.
            let at = |i: usize| text.get(..i).map_or(i, |head| one_line(head).len());
            let cursor = cursor.map(|(start, end)| (at(start), at(end)));
            Event::Composition {
                text: one_line(&text),
                cursor,
            }
        }
        event => event,
    }
}

/// `query` with the caret at byte `caret`, on one line: the text and where
/// the caret is in it. `None` when it already is.
pub(super) fn on_one_line(query: &str, caret: usize) -> Option<(String, usize)> {
    let line = one_line(query);
    if line == query {
        return None;
    }
    let caret = query
        .get(..caret)
        .map_or(line.len(), |head| one_line(head).len());
    Some((line, caret))
}
