//! Which column a task is in, and whether it passes the filter line. Pure.
//!
//! The column is derived, never stored (Decision 2): GitHub stores only
//! open/closed, so a stored column would need either labels written to
//! GitHub or a local overlay that drifts from what Noa reports.

use super::types::{AgentState, Column, Liveness, RunView, Source, State, Task, TaskRef};

/// L1-1, first match: Closed → Done; Starting or Alive{agent ∉ {Finished, Error}} → Running;
/// any other last run → Review; no run → Todo.
///
/// A result file written while the agent still runs keeps the task in
/// 実行中: agents write their report before they are done.
pub fn column(task: &Task, last_run: Option<&RunView>) -> Column {
    if task.state == State::Closed {
        return Column::Done;
    }
    let Some(run) = last_run else {
        return Column::Todo;
    };
    match &run.liveness {
        Liveness::Starting => Column::Running,
        Liveness::Alive { agent } => match agent {
            Some(AgentState::Finished | AgentState::Error) => Column::Review,
            _ => Column::Running,
        },
        Liveness::Gone => Column::Review,
    }
}

/// The filter line: a source, a label, and text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// `None`: すべて.
    pub source: Option<Source>,
    pub label: Option<String>,
    /// Matched against the title and the id (`t-…`, `#N`); empty matches all.
    pub text: String,
}

/// Whether `task` passes `filter`; label and text case-insensitive.
pub fn matches(task: &Task, filter: &Filter) -> bool {
    if filter
        .source
        .is_some_and(|source| task.r#ref.source() != source)
    {
        return false;
    }
    if let Some(label) = &filter.label
        && !task
            .labels
            .iter()
            .any(|l| l.to_lowercase() == label.to_lowercase())
    {
        return false;
    }
    let needle = filter.text.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    task.title.to_lowercase().contains(&needle) || id_text(&task.r#ref).contains(&needle)
}

/// The id the text filter matches: a local task's `t-…`, an issue's `#N`,
/// a marker's file path.
fn id_text(r: &TaskRef) -> String {
    match r {
        TaskRef::Local(id) => id.to_lowercase(),
        TaskRef::GitHub { number, .. } => format!("#{number}"),
        TaskRef::Code { path, .. } => path.to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::ResultFact;
    use crate::testkit::{closed, issue, labelled, local, run_view};

    const WRITTEN: ResultFact = ResultFact::Written {
        bytes: 10,
        modified: 2_000,
    };

    fn alive(agent: Option<AgentState>) -> Liveness {
        Liveness::Alive { agent }
    }

    #[test]
    fn ac01_column_is_the_first_match_of_state_liveness_and_result() {
        let open = local("t-1", "a");
        let done = closed(local("t-1", "a"));
        let r = &open.r#ref;
        let livenesses = [
            (Liveness::Starting, Column::Running),
            (alive(None), Column::Running),
            (alive(Some(AgentState::Running)), Column::Running),
            (alive(Some(AgentState::Permission)), Column::Running),
            (alive(Some(AgentState::Input)), Column::Running),
            (alive(Some(AgentState::Finished)), Column::Review),
            (alive(Some(AgentState::Error)), Column::Review),
            (Liveness::Gone, Column::Review),
        ];
        for (liveness, open_column) in livenesses {
            for result in [ResultFact::Missing, WRITTEN] {
                let view = run_view(r, liveness.clone(), result.clone());
                assert_eq!(
                    column(&open, Some(&view)),
                    open_column,
                    "{liveness:?} {result:?}"
                );
                // Closed wins over any run.
                assert_eq!(
                    column(&done, Some(&view)),
                    Column::Done,
                    "{liveness:?} {result:?}"
                );
            }
        }
        // No run.
        assert_eq!(column(&open, None), Column::Todo);
        assert_eq!(column(&done, None), Column::Done);
        assert_eq!(column(&issue(3, "b"), None), Column::Todo);
        assert_eq!(column(&closed(issue(3, "b")), None), Column::Done);
    }

    #[test]
    fn ac01_a_result_written_while_the_agent_runs_stays_running() {
        let task = issue(7, "x");
        let view = run_view(&task.r#ref, alive(Some(AgentState::Running)), WRITTEN);
        assert_eq!(column(&task, Some(&view)), Column::Running);
        let view = run_view(&task.r#ref, Liveness::Starting, WRITTEN);
        assert_eq!(column(&task, Some(&view)), Column::Running);
    }

    #[test]
    fn ac01_columns_are_labelled_in_order() {
        let labels: Vec<_> = Column::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels, ["未着手", "実行中", "確認待ち", "完了"]);
    }

    fn passes(tasks: &[Task], filter: &Filter) -> Vec<String> {
        tasks
            .iter()
            .filter(|t| matches(t, filter))
            .map(|t| t.r#ref.badge() + " " + &t.title)
            .collect()
    }

    #[test]
    fn ac02_filter_by_source_label_and_text() {
        let tasks = [
            labelled(local("t-1700000000000", "Fix Resize"), &["bug"]),
            labelled(issue(12, "Add search"), &["Enhancement", "ui"]),
            issue(120, "Crash on start"),
        ];
        let all = Filter::default();
        assert_eq!(passes(&tasks, &all).len(), 3);

        let local_only = Filter {
            source: Some(Source::Local),
            ..Filter::default()
        };
        assert_eq!(passes(&tasks, &local_only), ["ローカル Fix Resize"]);
        let github = Filter {
            source: Some(Source::GitHub),
            ..Filter::default()
        };
        assert_eq!(
            passes(&tasks, &github),
            ["#12 Add search", "#120 Crash on start"]
        );

        // Labels, case-insensitive, whole label.
        let label = |l: &str| Filter {
            label: Some(l.into()),
            ..Filter::default()
        };
        assert_eq!(passes(&tasks, &label("enhancement")), ["#12 Add search"]);
        assert_eq!(passes(&tasks, &label("BUG")), ["ローカル Fix Resize"]);
        assert!(passes(&tasks, &label("enh")).is_empty());

        let text = |t: &str| Filter {
            text: t.into(),
            ..Filter::default()
        };
        // Title, case-insensitive.
        assert_eq!(passes(&tasks, &text("resize")), ["ローカル Fix Resize"]);
        assert_eq!(passes(&tasks, &text("  CRASH ")), ["#120 Crash on start"]);
        // A local id.
        assert_eq!(passes(&tasks, &text("T-1700")), ["ローカル Fix Resize"]);
        // An issue's #N (and the start of a longer number).
        assert_eq!(
            passes(&tasks, &text("#12")),
            ["#12 Add search", "#120 Crash on start"]
        );
        assert_eq!(passes(&tasks, &text("#120")), ["#120 Crash on start"]);
        assert!(passes(&tasks, &text("nothing")).is_empty());

        // All three at once.
        let both = Filter {
            source: Some(Source::GitHub),
            label: Some("ui".into()),
            text: "search".into(),
        };
        assert_eq!(passes(&tasks, &both), ["#12 Add search"]);
        let none = Filter {
            source: Some(Source::Local),
            ..both
        };
        assert!(passes(&tasks, &none).is_empty());
    }
}
