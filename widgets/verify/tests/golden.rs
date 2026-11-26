//! Golden tests pinning every brief, correction note and the final report
//! of a fixed two-round verification byte for byte.
//!
//! The fixtures under `golden/` were generated from the first
//! implementation (`VERIFY_BLESS=1 cargo test --test golden` writes them)
//! and reviewed by hand; a difference here is a behaviour change, to be
//! blessed only on purpose.

#[path = "golden/scenario.rs"]
mod scenario;

use std::collections::BTreeMap;
use std::path::Path;

use verify::model::export::{self, SECTIONS};
use verify::model::types::{EvidenceKey, RulingKind};

fn assert_same(what: &str, got: &str, file: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(file);
    if std::env::var_os("VERIFY_BLESS").is_some() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if got == want {
        return;
    }
    let at = got
        .bytes()
        .zip(want.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or(got.len().min(want.len()));
    let window = |s: &str| {
        let from = s.floor_char_boundary(at.saturating_sub(200));
        let to = s.ceil_char_boundary((at + 200).min(s.len()));
        s[from..to].to_owned()
    };
    panic!(
        "{what} differs from {file} at byte {at} (got {} bytes, want {})\n--- got ---\n{}\n--- want ---\n{}",
        got.len(),
        want.len(),
        window(got),
        window(&want),
    );
}

fn report() -> String {
    let (_, session) = scenario::run();
    export::report_markdown(&session, Some(&verify::store::reply_dir(&session)))
}

/// The body of `heading` (up to the next `## `) in `report`.
fn section<'a>(report: &'a str, heading: &str) -> &'a str {
    let start = report.find(heading).unwrap() + heading.len();
    let rest = &report[start..];
    rest[..rest.find("\n## ").unwrap_or(rest.len())].trim()
}

#[test]
fn ac21_briefs_and_corrections_match_the_golden_file() {
    let (briefs, _) = scenario::run();
    assert_same("briefs", &briefs, "briefs.txt");
}

#[test]
fn ac20_the_report_matches_the_golden_file() {
    assert_same("report", &report(), "report.md");
}

#[test]
fn ac7_round_one_verifier_briefs_name_no_peer_worktree_or_reply_path() {
    let (briefs, _) = scenario::run();
    let mut checked = Vec::new();
    for brief in briefs
        .split("===== ")
        .filter(|s| s.starts_with("R1 brief to ") || s.starts_with("R1 correction to "))
    {
        let to = brief
            .split(" to ")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap();
        if to == "judge" {
            continue;
        }
        checked.push(to.to_owned());
        for peer in scenario::VERIFIERS.iter().filter(|p| **p != to) {
            for named in [
                format!("verifier {peer}"),
                format!("verify-ver-golden-{peer}"),
                format!("/r1/{peer}/"),
                format!("{peer}/"),
            ] {
                assert!(!brief.contains(&named), "R1 brief to {to} names {named:?}");
            }
        }
        assert!(
            to == "codex" || !brief.contains("symlink too"),
            "R1 brief to {to} shows codex's intervention"
        );
    }
    assert_eq!(
        checked,
        ["claude", "codex", "agy", "claude"],
        "every blind brief and the correction were checked"
    );
}

#[test]
fn ac13_the_round_two_ruling_brief_lists_the_earlier_pass_a_verified_ruling_rests_on() {
    let (briefs, _) = scenario::run();
    let brief = briefs
        .split("===== ")
        .find(|s| s.starts_with("R2 brief to judge"))
        .unwrap();
    let ac1 = &brief[brief.find("### AC-1 ").unwrap()..];
    let ac1 = &ac1[..ac1.find("### AC-2 ").unwrap()];
    for line in [
        "Earlier rounds on this criterion (the evidence standard reads these too):",
        "- claude R1: pass — pwd shows /tmp.",
        "  - claude/r1/e1: command `cargo test -p app cwd_` (cwd .) exit 0; re-run exit code matched (0)",
        "    stored: /golden/repo/.noa/verify/ver-golden/evidence/r1/claude/logs/1.log",
        "- codex R1: fail — With a symlinked cwd the tab opens in $HOME.",
    ] {
        assert!(ac1.contains(line), "{line:?} in {ac1}");
    }
}

#[test]
fn ac20_the_report_ties_a_defects_reproduction_to_its_log() {
    let report = report();
    let defects = section(&report, SECTIONS[2]);
    assert!(
        defects.contains("- 追試の証跡（R2 claude・再現）: claude/r2/e2"),
        "{defects}"
    );
    let checks = section(&report, SECTIONS[3]);
    assert!(
        checks.contains(
            "- R2 claude → D1: 再現 — A link inside a link resolves wrong. · 証跡: claude/r2/e2"
        ),
        "{checks}"
    );
}

#[test]
fn ac21_a_reply_breaking_several_rules_gets_one_line_for_each() {
    let (briefs, _) = scenario::run();
    let correction = briefs
        .split("===== ")
        .find(|s| s.starts_with("R1 correction to claude"))
        .expect("claude's first reply was turned down");
    assert!(
        correction.contains(
            "- evidence[0] (e1).log \"../e1.log\" must be a plain relative path, without \"..\""
        ),
        "{correction}"
    );
    assert!(
        correction
            .contains("- results: missing AC-2, AC-3, AC-5 (one result per criterion in round 1)"),
        "{correction}"
    );
    let criteria = briefs
        .split("===== ")
        .find(|s| s.starts_with("criteria correction to judge"))
        .expect("the first criteria were turned down");
    assert!(
        criteria.contains("Your criteria JSON could not be used"),
        "{criteria}"
    );
    assert!(
        criteria.contains("the person's criteria missing"),
        "{criteria}"
    );
}

#[test]
fn ac21_rerun_briefs_start_the_widgets_script_and_say_reply_nothing() {
    let (briefs, _) = scenario::run();
    let rerun = briefs
        .split("===== ")
        .find(|s| s.starts_with("R1 re-run brief to codex"))
        .unwrap();
    assert!(
        rerun.contains("\n    sh '/golden/repo-wt/verify-ver-golden-codex/.noa/verify/ver-golden/r1/codex/rerun/run.sh'\n"),
        "{rerun}"
    );
    assert!(
        rerun.contains("Start exactly this once with your shell tool in the foreground")
            && !rerun.contains("nohup"),
        "{rerun}"
    );
    assert!(
        rerun.contains("runs 2 commands, each allowed up to 10 minutes, so the whole run can take up to 25 minutes")
            && rerun.contains("keep waiting on that same running process")
            && rerun.contains("Never start it a second time and never stop it."),
        "{rerun}"
    );
    assert!(
        !rerun.contains("timeout"),
        "codex and agy have no run-deadline argument to set: {rerun}"
    );
    assert!(rerun.contains("Reply nothing."), "{rerun}");
    assert!(
        !rerun.contains("cargo test"),
        "the brief holds no reported command"
    );
}

#[test]
fn ac20_the_report_has_every_section_differed_reruns_first_and_the_criteria_changes() {
    let report = report();
    let mut at = 0;
    for heading in SECTIONS {
        let found = report[at..]
            .find(heading)
            .unwrap_or_else(|| panic!("{heading} missing or out of order"));
        at += found + heading.len();
    }
    assert!(report.contains("\n# 検証結果\n"), "final, not provisional");
    assert!(section(&report, SECTIONS[0]).starts_with("**条件付き合格**"));
    let evidence = section(&report, SECTIONS[4]);
    assert!(
        evidence.starts_with("再実行は各検証者自身のペインで、その検証者のエージェントが"),
        "where the re-runs ran is said first: {evidence}"
    );
    let first = evidence.lines().find(|l| l.starts_with("- ")).unwrap();
    assert!(
        first.starts_with("- `codex/r1/e3` コマンド `cargo test -p app cwd_gone`"),
        "{evidence}"
    );
    let changes = section(&report, SECTIONS[5]);
    for line in [
        "- 編集: AC-3 Opening a tab stays under 50 ms",
        "- 追加: AC-5 Quitting keeps every tab's cwd for the next launch",
        "- 削除: AC-4 The change is documented（request）",
        "- 対象外と判定: AC-5",
    ] {
        assert!(changes.contains(line), "{line:?} in {changes}");
    }
    let gaps = section(&report, SECTIONS[6]);
    for line in [
        "- AC-3 を codex が実行できませんでした（R1）: No display in the sandbox.",
        "- 取り込み前に消えた証跡: `agy/r1/e2`（logs/2.log）",
        "- 観察だけを根拠にした結果: AC-5（agy R1・実行不能）",
    ] {
        assert!(gaps.contains(line), "{line:?} in {gaps}");
    }
}

#[test]
fn ac33_the_report_embeds_images_and_links_videos_with_captions_and_marks() {
    let report = report();
    let base = "/golden/repo/.noa/verify/ver-golden";
    assert!(
        report.starts_with(&format!("> 証跡のリンク先: `{base}`")),
        "{report}"
    );
    let ac1 = section(&report, "### AC-1 ");
    assert!(
        ac1.contains(&format!(
            "![pwd in the new tab shows /tmp]({base}/evidence/r1/claude/shots/new%20tab.png)"
        )),
        "{ac1}"
    );
    assert!(
        ac1.contains(&format!(
            "▶ [cd into a symlink, Cmd-T, pwd]({base}/evidence/r1/codex/video/symlink.mov)"
        )),
        "{ac1}"
    );
    assert!(ac1.contains("  - 00:06.5 the new tab shows $HOME"), "{ac1}");
    assert!(
        ac1.contains(&format!(
            "![pwd shows /tmp in the new tab]({base}/evidence/user/1.png)（あなたが添付）"
        )),
        "{ac1}"
    );
    // 証跡ごと書き出す: the same report links into the bundle relatively.
    let (_, session) = scenario::run();
    let bundle = export::report_markdown(&session, None);
    assert!(bundle.starts_with("# 検証結果\n"), "{bundle}");
    assert!(bundle.contains("](evidence/r1/claude/shots/new%20tab.png)"));
    assert!(bundle.contains("](evidence/user/1.png)"));
    assert!(!bundle.contains(base), "no absolute link in the bundle");
}

#[test]
fn ac24_the_fix_task_lists_failed_criteria_with_their_log_tails_and_the_defects_repro() {
    let (_, session) = scenario::run();
    // What 修正を依頼 offers: the criteria whose final ruling is failed.
    let failed: Vec<String> = session
        .criteria
        .iter()
        .filter(|c| session.ruling(&c.id).map(|r| r.kind) == Some(RulingKind::Failed))
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(failed, ["AC-3"], "the scenario ends with AC-3 failed");
    let tail: String = (1..=80).map(|n| format!("line {n}\n")).collect();
    let tails = BTreeMap::from([(EvidenceKey::new("agy/r2/e1"), tail)]);
    let task = export::fix_task(&session, &failed, &["D1".into()], &tails);
    assert!(
        task.starts_with("# Fix request from verification ver-golden\n"),
        "{task}"
    );
    assert!(
        task.contains("### AC-3 [should] Opening a tab stays under 50 ms"),
        "{task}"
    );
    assert!(
        task.contains(
            "- Judge (R2): failed — agy's cold-start benchmark takes 72 ms, over the limit."
        ),
        "{task}"
    );
    assert!(
        task.contains("- agy (R2) reported fail: A cold start takes 72 ms."),
        "{task}"
    );
    assert!(task.contains("./bench open-tab --cold"), "{task}");
    assert!(
        task.contains("Last 80 lines of its log:\n\n```\nline 1\n"),
        "{task}"
    );
    assert!(task.contains("line 80\n```"), "{task}");
    assert!(
        task.contains(
            "Stored copy: /golden/repo/.noa/verify/ver-golden/evidence/r2/agy/logs/1.log"
        ),
        "{task}"
    );
    assert!(
        task.contains("### D1 [high] A symlinked cwd is resolved to its target's parent"),
        "{task}"
    );
    assert!(
        task.contains("    1. ln -s /tmp /tmp/l 2. cd /tmp/l 3. Cmd-T 4. pwd"),
        "{task}"
    );
    assert!(
        task.contains(
            "Stored copy: /golden/repo/.noa/verify/ver-golden/evidence/r1/codex/logs/1.log"
        ),
        "{task}"
    );
}

#[test]
fn ac21_the_ruling_brief_shows_the_log_tail_of_a_command_only_a_cross_check_cites() {
    let (briefs, _) = scenario::run();
    let judge = briefs
        .split("===== ")
        .find(|s| s.starts_with("R2 brief to judge"))
        .unwrap();
    let checks = &judge[judge.find("## 5. Cross-checks").unwrap()..judge.find("## 6.").unwrap()];
    assert!(
        checks.contains(
            "  - claude/r2/e2: command `./scripts/nested-link.sh` (cwd .) exit 1\n    stored: "
        ),
        "{checks}"
    );
    assert!(
        checks.contains(
            "    log tail:\n```\nrunning ./scripts/nested-link.sh\ntest cwd_inherit ... FAILED\nexit 1\n```"
        ),
        "{checks}"
    );
}

#[test]
fn ac31_the_persons_screenshot_is_in_the_next_ruling_brief() {
    let (briefs, _) = scenario::run();
    let judge = briefs
        .split("===== ")
        .find(|s| s.starts_with("R1 brief to judge"))
        .unwrap();
    assert!(
        judge.contains(
            "  - user/1: PNG image Screenshot 2026-11-24.png — \"pwd shows /tmp in the new tab\""
        ),
        "{judge}"
    );
    assert!(judge.contains("    shows: AC-1"), "{judge}");
}

#[test]
fn ac21_a_round_after_an_extension_briefs_every_ruling_and_what_checks_and_rulings_cite() {
    let briefs = scenario::extended();
    for part in scenario::VERIFIERS {
        let brief = briefs
            .split("===== ")
            .find(|s| s.starts_with(&format!("R3 brief to {part} ")))
            .unwrap_or_else(|| panic!("no R3 brief to {part}:\n{briefs}"));
        let standing = &brief
            [brief.find("## 5. Rulings and results so far").unwrap()..brief.find("## 6.").unwrap()];
        // The command only claude's R2 reproduction of D1 cites.
        assert!(
            standing.contains(
                "  - claude/r2/e2: command `./scripts/nested-link.sh` (cwd .) exit 1\n    stored: /golden/repo/.noa/verify/ver-golden/evidence/r2/claude/logs/2.log"
            ),
            "{part}:\n{standing}"
        );
        // The person's screenshot only AC-1's R2 ruling cites.
        assert!(
            standing.contains(
                "  - user/1: PNG image Screenshot 2026-11-24.png — \"pwd shows /tmp in the new tab\"\n    stored: /golden/repo/.noa/verify/ver-golden/evidence/user/1.png"
            ),
            "{part}:\n{standing}"
        );
        // AC-3's R1 ruling beside its R2 one.
        let ac3 = &standing[standing.find("### AC-3").unwrap()..standing.find("### AC-5").unwrap()];
        assert!(
            ac3.starts_with(
                "### AC-3 — failed (R2): agy's cold-start benchmark takes 72 ms, over the limit.\n\
                 - judge R1: inconclusive — Only claude's benchmark is stored; agy's log was gone before ingest and counts for nothing.\n"
            ),
            "{part}:\n{ac3}"
        );
    }
}
