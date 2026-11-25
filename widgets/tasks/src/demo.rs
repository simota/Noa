//! The canned data of `--demo` (and `--snapshot`): a repository with
//! cards in every column, local tasks `t-1`… and issues `#12`…, a local
//! file that does not parse, and runs with reports. Nothing is read,
//! started or written.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tasks::model::types::{
    AgentKind, AgentState, Comment, GitHubView, IssueDetail, Liveness, Millis, Report, ResultFact,
    Run, RunId, RunView, State, Task, TaskRef,
};

use crate::ui::{Feed, Intent, View};

/// The demo's clock: 2026-11-25 10:00 JST.
pub const NOW: Millis = 1_795_568_400_000;
pub const REPO: &str = "noa-dev/noa";
const MIN: Millis = 60_000;
const DAY: Millis = 86_400_000;

fn local(id: &str, title: &str, labels: &[&str], state: State, updated: Millis) -> Task {
    Task {
        r#ref: TaskRef::Local(id.to_owned()),
        title: title.to_owned(),
        body: Some(format!("{title}。\n\n手順と完了条件はこの本文に書きます。")),
        state,
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        updated,
        url: None,
        closed: None,
        problem: None,
    }
}

fn issue(number: u64, title: &str, labels: &[&str], state: State, updated: Millis) -> Task {
    Task {
        r#ref: TaskRef::GitHub {
            repo: REPO.to_owned(),
            number,
        },
        title: title.to_owned(),
        body: None,
        state,
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        updated,
        url: Some(format!("https://github.com/{REPO}/issues/{number}")),
        closed: None,
        problem: None,
    }
}

fn gh(number: u64) -> TaskRef {
    TaskRef::GitHub {
        repo: REPO.to_owned(),
        number,
    }
}

fn run(
    task: TaskRef,
    agent: AgentKind,
    started: Millis,
    liveness: Liveness,
    report: Option<&str>,
) -> RunView {
    let id = format!("run-{started}");
    let gone = liveness == Liveness::Gone;
    RunView {
        run: Run {
            run: RunId::new(id.clone()),
            task,
            agent,
            workdir: PathBuf::from(format!("/src/noa-wt/task-{id}")),
            branch: Some(format!("task-{id}")),
            pane: (liveness != Liveness::Starting).then_some(40),
            board: Some(7),
            started,
            closed: gone.then_some(started + 20 * MIN),
        },
        liveness,
        result: match report {
            Some(text) => ResultFact::Written {
                bytes: text.len() as u64,
                modified: started + 15 * MIN,
            },
            None => ResultFact::Missing,
        },
        report: report.map(|text| Report {
            text: text.to_owned(),
            cut: false,
        }),
    }
}

/// The demo's view.
pub fn view() -> View {
    let broken = Task {
        r#ref: TaskRef::Local("t-5".into()),
        title: "t-5".into(),
        body: None,
        state: State::Open,
        labels: Vec::new(),
        updated: NOW - 50 * MIN,
        url: None,
        closed: None,
        problem: Some("読めません: 1 行目が --- ではありません".into()),
    };
    let tasks = vec![
        local(
            "t-1",
            "設定ファイルの再読み込みでフォントサイズを保つ",
            &["config"],
            State::Open,
            NOW - 2 * DAY,
        ),
        local(
            "t-2",
            "リサイズ時のちらつきを直す",
            &["bug", "render"],
            State::Open,
            NOW - DAY,
        ),
        local(
            "t-3",
            "noa-tasks スキルの例を最新の形式にする",
            &["docs"],
            State::Open,
            NOW - 3 * DAY,
        ),
        local(
            "t-4",
            "スクロールバックの検索結果をハイライトする",
            &["ui"],
            State::Closed,
            NOW - DAY,
        ),
        broken,
        local(
            "t-6",
            "古い完了タスク（7 日より前）",
            &[],
            State::Closed,
            NOW - 10 * DAY,
        ),
    ];
    // Two markers in the code, as `git grep` lists them.
    let mut tasks = tasks;
    tasks.extend(tasks::code::parse(
        b"crates/noa-grid/src/screen/reflow.rs\x00212\x00        // TODO: keep the cursor row on a narrow resize\n\
          crates/noa-app/src/app/ipc.rs\x0088\x00    // FIXME(simota): drop the retry once 0.9.16 is the floor\n",
        |_| NOW - 4 * DAY,
    ));
    let issues = vec![
        issue(
            12,
            "タブの並べ替えをキーボードでできるようにする",
            &["enhancement"],
            State::Open,
            NOW - 5 * 60 * MIN,
        ),
        issue(
            15,
            "Linux で IME の候補窓がずれる",
            &["bug", "linux"],
            State::Open,
            NOW - 2 * DAY,
        ),
        issue(
            18,
            "README に Linux パッケージの手順を足す",
            &["docs"],
            State::Open,
            NOW - 4 * DAY,
        ),
        issue(
            21,
            "テーマ一覧を名前で絞り込む",
            &["enhancement", "ui"],
            State::Open,
            NOW - 6 * DAY,
        ),
        issue(
            9,
            "起動直後のウィンドウサイズが設定と違う",
            &["bug"],
            State::Closed,
            NOW - 2 * DAY,
        ),
        issue(
            3,
            "古い完了 issue（30 日前）",
            &[],
            State::Closed,
            NOW - 30 * DAY,
        ),
    ];
    let runs = vec![
        run(
            TaskRef::Local("t-2".into()),
            AgentKind::Claude,
            NOW - 140 * MIN,
            Liveness::Alive {
                agent: Some(AgentState::Running),
            },
            None,
        ),
        run(
            gh(15),
            AgentKind::Codex,
            NOW - 40 * MIN,
            Liveness::Alive {
                agent: Some(AgentState::Permission),
            },
            None,
        ),
        run(
            TaskRef::Local("t-3".into()),
            AgentKind::Claude,
            NOW - 19 * 60 * MIN,
            Liveness::Gone,
            Some(
                "# 報告\n\nSKILL.md の例を新しい見出しの形式に書き換えました。\n\n- 例のファイルが parse されることを確かめました",
            ),
        ),
        run(
            gh(18),
            AgentKind::Agy,
            NOW - 8 * 60 * MIN,
            Liveness::Alive {
                agent: Some(AgentState::Finished),
            },
            Some("# 報告\n\nREADME に Linux の tar.gz の手順を追加しました（Fixes #18）。"),
        ),
        run(
            gh(9),
            AgentKind::Claude,
            NOW - 3 * DAY,
            Liveness::Gone,
            Some("# 報告\n\n起動時に保存済みのサイズを先に読むようにしました。"),
        ),
    ];
    let mut details = BTreeMap::new();
    details.insert(
        12,
        Ok(IssueDetail {
            body: "タブを Cmd+Shift+[ / ] で左右に動かしたい。\n\nマウスのドラッグでは並べ替えられる。"
                .into(),
            comments: vec![Comment {
                author: "reviewer".into(),
                body: "split の移動と同じキー割り当ての考え方でよさそう。".into(),
                at: NOW - 3 * 60 * MIN,
            }],
        }),
    );
    View {
        repo: PathBuf::from("/src/noa"),
        tasks,
        runs,
        github: GitHubView {
            repo: Some(REPO.into()),
            tasks: issues,
            fetched: Some(NOW - 3 * MIN),
            refreshing: false,
            failure: None,
            details,
        },
        agents: Some(vec![AgentKind::Claude, AgentKind::Codex, AgentKind::Agy]),
        notice: Some("デモ表示（何も起動・保存しません）".into()),
        now: NOW,
    }
}

/// The demo's [`Feed`]: [`view`], with 完了にする / 再開 applied in memory
/// and every other intent answered with a notice.
pub struct DemoFeed {
    view: View,
    changed: bool,
}

impl Default for DemoFeed {
    fn default() -> Self {
        DemoFeed {
            view: view(),
            changed: true,
        }
    }
}

impl Feed for DemoFeed {
    fn poll(&mut self) -> Option<View> {
        std::mem::replace(&mut self.changed, false).then(|| self.view.clone())
    }

    fn act(&mut self, intent: Intent) {
        let notice = match intent {
            Intent::SetState { id, state } => {
                for task in &mut self.view.tasks {
                    if task.r#ref == TaskRef::Local(id.clone()) {
                        task.state = state;
                        task.updated = NOW;
                    }
                }
                match state {
                    State::Closed => format!("{id} を完了にしました（デモ）"),
                    State::Open => format!("{id} を再開しました（デモ）"),
                }
            }
            Intent::LoadDetail(_) => return,
            Intent::Refresh => "デモ表示では取得しません".into(),
            other => format!("デモ表示では何もしません: {other:?}"),
        };
        self.view.notice = Some(notice);
        self.changed = true;
    }
}
