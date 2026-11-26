//! Canned data for `--demo`: a scan with two open candidates and one
//! resolved, a registry with two assets (one verified on the current
//! commit, one skipped), at a fixed clock in UTC. Built from the types
//! only: the demo reads and writes no file, so `--demo --snapshot` gives
//! the same bytes on every run.

use std::collections::BTreeMap;
use std::path::PathBuf;

use scaffold::model::{
    Asset, AssetKind, Decision, EvidenceKind, EvidenceRef, Improvement, Lifecycle, Observation,
    SessionFact, Subject, candidate_id,
};
use scaffold::registry::{self, Loaded, Registry, WidgetIntent};
use scaffold::scan::{
    Evidence, Explained, Finding, REGISTRY, Report, SCOPE, SourceReport, SourceState, WidgetReport,
};
use scaffold::time::Millis;

use crate::ui::backend::{Action, Backend, State};

/// 2026-11-26T09:30:00Z.
pub const NOW: Millis = 1_795_685_400_000;
pub const REPO: &str = "/Users/demo/src/noa";
pub const COMMIT: &str = "4cb17627e3f0a1b2c3d4e5f60718293a4b5c6d7e";
const IDENTITY: &str = "/Users/demo/src/noa/.git";
const VERIFY_ID: &str = "ver-1795500000000";
const TASK_ID: &str = "t-1795680000000";
const DATA: &str = "/Users/demo/Library/Application Support/noa";

const WIDGETS: [&str; 4] = ["agent-usage", "scaffold", "tasks", "verify"];

fn ev(path: &str, line: u32) -> Evidence {
    Evidence::new(path, line)
}

fn finding(
    rule: &str,
    widget: &str,
    target: &str,
    parts: &[&str],
    evidence: Vec<Evidence>,
) -> Finding {
    Finding {
        rule: rule.into(),
        widget: widget.into(),
        target: target.into(),
        parts: parts.iter().map(|p| p.to_string()).collect(),
        evidence,
    }
}

fn explained(widget: &str, line: u32) -> Explained {
    Explained {
        rule: "os-difference".into(),
        widget: widget.into(),
        target: "linux".into(),
        reason: "Linux 配布は未判断。".into(),
        evidence: vec![ev(REGISTRY, line)],
    }
}

pub fn report() -> Report {
    let mut sources: Vec<String> = [
        REGISTRY,
        "widgets",
        scaffold::scan::HELPER,
        scaffold::scan::APP_COMMANDS,
        scaffold::scan::COMMAND,
        scaffold::scan::MENU,
        scaffold::scan::PALETTE,
        scaffold::scan::VERIFY,
        scaffold::scan::BUNDLE,
        scaffold::scan::PACKAGE,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for w in WIDGETS {
        sources.push(format!("widgets/{w}/Cargo.toml"));
        sources.push(format!("widgets/{w}/src/main.rs"));
    }
    sources.sort();
    Report {
        schema: 1,
        scope: SCOPE.into(),
        status: "inconsistent".into(),
        exit: 1,
        sources: sources
            .into_iter()
            .map(|path| SourceReport {
                path,
                state: SourceState::Ok,
                error: None,
            })
            .collect(),
        widgets: WIDGETS
            .iter()
            .map(|w| WidgetReport {
                widget: w.to_string(),
                intent: None,
                points: BTreeMap::new(),
            })
            .collect(),
        findings: vec![
            finding(
                "missing-registration",
                "scaffold",
                "verify",
                &[],
                vec![
                    ev("scripts/verify.sh", 79),
                    ev("widgets/scaffold/Cargo.toml", 2),
                ],
            ),
            finding(
                "incomplete-registration",
                "tasks",
                "macos_bundle",
                &["sign"],
                vec![
                    ev("scripts/bundle-macos.sh", 73),
                    ev("scripts/bundle-macos.sh", 124),
                ],
            ),
        ],
        explained: vec![
            explained("scaffold", 22),
            explained("tasks", 28),
            explained("verify", 34),
        ],
    }
}

fn decision(to: Lifecycle, at: &str, reason: &str, evidence: Vec<EvidenceRef>) -> Decision {
    Decision {
        to,
        at: at.into(),
        reason: reason.into(),
        evidence,
    }
}

pub fn registry() -> Loaded {
    let mut widgets = BTreeMap::new();
    for w in WIDGETS {
        let macos_only = w != "agent-usage";
        widgets.insert(
            w.to_string(),
            WidgetIntent {
                ships: if macos_only {
                    vec!["macos".into()]
                } else {
                    vec!["linux".into(), "macos".into()]
                },
                reason: macos_only.then(|| "Linux 配布は未判断。".into()),
            },
        );
    }
    let r = Registry {
        schema: 1,
        widgets,
        assets: vec![
            Asset {
                id: "check-widget-registration".into(),
                kind: AssetKind::Checker,
                title: "ウィジェット登録の整合チェック".into(),
                path: "scripts/check-widget-registration.py".into(),
                decisions: vec![
                    decision(
                        Lifecycle::Trial,
                        "2026-11-20",
                        "verify.sh で毎回走らせて試す",
                        vec![],
                    ),
                    decision(
                        Lifecycle::Evaluating,
                        "2026-11-24",
                        "登録漏れを 2 件見つけた",
                        vec![EvidenceRef {
                            kind: EvidenceKind::Verify,
                            r#ref: VERIFY_ID.into(),
                        }],
                    ),
                ],
            },
            Asset {
                id: "add-widget-runbook".into(),
                kind: AssetKind::Runbook,
                title: "ウィジェット追加の手順書".into(),
                path: "docs/runbooks/add-widget.md".into(),
                decisions: vec![decision(
                    Lifecycle::Trial,
                    "2026-11-20",
                    "次のウィジェット追加で使う",
                    vec![],
                )],
            },
        ],
    };
    Loaded {
        bytes: registry::canonical(&r).into_bytes(),
        registry: r,
    }
}

fn improvement(
    rule: &str,
    widget: &str,
    target: &str,
    evidence: Vec<Evidence>,
    resolved: Option<Millis>,
) -> Improvement {
    Improvement {
        id: candidate_id(IDENTITY, rule, widget, target),
        rule: rule.into(),
        widget: widget.into(),
        target: target.into(),
        evidence,
        first_seen: NOW - 3 * 86_400_000,
        last_seen: if resolved.is_some() {
            NOW - 86_400_000
        } else {
            NOW
        },
        resolved,
    }
}

pub fn state() -> State {
    let report = report();
    let mut improvements: Vec<Improvement> = report
        .findings
        .iter()
        .map(|f| improvement(&f.rule, &f.widget, &f.target, f.evidence.clone(), None))
        .collect();
    improvements.push(improvement(
        "name-mismatch",
        "agent-usage",
        "linux_package",
        vec![ev("scripts/package-linux.sh", 54)],
        Some(NOW - 86_400_000 + 60_000),
    ));
    improvements.sort_by(|a, b| a.id.cmp(&b.id));
    let tasks_bundle = candidate_id(IDENTITY, "incomplete-registration", "tasks", "macos_bundle");
    let verify_path = PathBuf::from(format!("{DATA}/verifications/{VERIFY_ID}"));
    State {
        repo: PathBuf::from(REPO),
        commit: Some(COMMIT.into()),
        scanned_at: NOW,
        report,
        registry: Ok(registry()),
        improvements,
        observations: vec![
            Observation::Verify {
                subject: Subject::Asset("check-widget-registration".into()),
                session: VERIFY_ID.into(),
                path: verify_path,
                at: NOW - 2 * 3_600_000,
            },
            Observation::Skipped {
                subject: Subject::Asset("add-widget-runbook".into()),
                reason: "次のウィジェット追加まで確認しない".into(),
                commit: Some(COMMIT.into()),
                at: NOW - 3_600_000,
            },
            Observation::Task {
                subject: Subject::Improvement(tasks_bundle),
                id: TASK_ID.into(),
                path: PathBuf::from(format!("{REPO}/.noa/tasks/{TASK_ID}.md")),
                at: NOW - 30 * 60_000,
            },
        ],
        sessions: BTreeMap::from([(
            VERIFY_ID.to_owned(),
            SessionFact::Finished {
                decision: Some("pass".into()),
                commit: Some(COMMIT.into()),
            },
        )]),
        problem: None,
    }
}

/// The demo's backend: the canned state; every action is refused.
pub struct Demo;

impl Backend for Demo {
    fn scan(&mut self) -> State {
        state()
    }

    fn act(&mut self, _action: Action, _state: &State) -> Result<String, String> {
        Err("デモ表示のため記録しません".into())
    }

    fn now(&self) -> Millis {
        NOW
    }

    fn offset(&self, _at: Millis) -> Millis {
        0
    }

    fn is_demo(&self) -> bool {
        true
    }
}
