//! Synthetic fixtures. No environment, repository, key, service or network reads.

use ember::model::{
    Artifact, ContextSnapshot, MemoryEntry, Message, Role, Run, RunKind, RunStatus, Scope, State,
    View,
};

pub(crate) const KINDS: &[&str] = &["conversation", "requests", "memory", "settings", "error"];
const TIME: u64 = 1_795_843_200_000;
pub(crate) const DOCUMENT: &str = "# エンバーの設計案\n\n## 最初に届ける体験\n\n一人のエンバーと会話を続け、調査や文書作成を任せる。ウィジェットを閉じても、Noaと端末が稼働している間は依頼が進む。\n\n## 画面\n\n- 会話: 選択したプロジェクトや資料を添えて相談する。\n- 依頼: 進行中の作業、成果物、停止状態を確認する。\n- 記憶: 自分で保存した内容と適用範囲を編集する。\n- 設定: モデル、通知、接続設定を確認する。\n\n## 開発への引継ぎ\n\n開発の仕事はWorkflowの開始画面で内容を確認してから進める。エンバーの文書作成と開発工程はそれぞれの依頼として記録する。\n\nこれは画面確認用のサンプルです。実際の調査結果ではありません。";

pub(crate) fn scope() -> Scope {
    Scope::Project {
        root: "/workspace/Noa".into(),
    }
}

pub(crate) fn view(kind: &str) -> View {
    let project = scope();
    let mut state = State {
        revision: 7,
        ..Default::default()
    };
    state.settings.notifications = true;
    state.messages = vec![
        Message {
            id: "message-demo-user".into(), scope: project.clone(), role: Role::User,
            content: "Noaのウィジェットにエンバーを追加したい。まず設計をまとめて。".into(),
            run_id: "run-demo-conversation".into(), created_at: TIME, ..Default::default()
        },
        Message {
            id: "message-demo-assistant".into(), scope: project.clone(), role: Role::Assistant,
            content: "会話を続けながら、任せた仕事とその結果を追える形にします。画面と記憶、Noaの既存機能とのつながりを設計案にまとめました。".into(),
            run_id: "run-demo-conversation".into(), created_at: TIME + 12_000, ..Default::default()
        },
    ];
    state.memories = vec![
        MemoryEntry {
            id: "memory-demo-language".into(),
            scope: Scope::Global,
            text: "説明は日本語で、要点から。".into(),
            revision: 1,
            created_at: TIME,
        },
        MemoryEntry {
            id: "memory-demo-project".into(),
            scope: project.clone(),
            text: "このプロジェクトでは、設計を確認してから実装を進める。".into(),
            revision: 1,
            created_at: TIME + 1,
        },
    ];
    state.runs = vec![
        Run {
            id:"run-demo-conversation".into(), kind:RunKind::Conversation, scope:project.clone(),
            objective:state.messages[0].content.clone(), status:RunStatus::Completed,
            answer:Some(state.messages[1].content.clone()), created_at:TIME, updated_at:TIME+12_000,
            model:state.settings.model.clone(), context:ContextSnapshot{scope:project.clone(), ..Default::default()}, ..Default::default()
        },
        Run {
            id:"run-demo-document".into(), kind:RunKind::Document, scope:project.clone(),
            objective:"エンバー機能の設計案を文書にまとめる".into(), status:RunStatus::Completed,
            answer:Some(DOCUMENT.into()), artifact:Some(Artifact{
                path:"artifacts/run-demo-document.md".into(), title:"エンバーの設計案".into(), digest:ember::store::digest(DOCUMENT.as_bytes()),
            }), created_at:TIME+14_000, updated_at:TIME+45_000, model:state.settings.model.clone(),
            context:ContextSnapshot{scope:project.clone(), ..Default::default()}, ..Default::default()
        },
        Run {
            id:"run-demo-research".into(), kind:RunKind::Research, scope:project.clone(),
            objective:"選択したウィジェット仕様から、既存機能との役割分担を整理する".into(),
            status:if kind=="error" {RunStatus::Failed} else {RunStatus::Running},
            error:(kind=="error").then(||"接続先から応答を受け取れませんでした。内容を確認し、新しい依頼としてやり直せます。".into()),
            created_at:TIME+50_000, updated_at:TIME+53_000, model:state.settings.model.clone(),
            context:ContextSnapshot{scope:project, ..Default::default()}, ..Default::default()
        },
    ];
    View {
        state,
        backend_available: kind != "error",
        notice: Some("デモ表示です。外部への送信や実行は行いません。".into()),
    }
}
