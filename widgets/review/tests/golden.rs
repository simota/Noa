//! Golden tests pinning every brief, correction note and the final report
//! of a fixed three-round review byte for byte.
//!
//! The fixtures under `golden/` were generated once from the first
//! implementation (`REVIEW_BLESS=1 cargo test --test golden` writes them) and
//! reviewed by hand; a difference here is a behaviour change, to be blessed
//! only on purpose.

#[path = "golden/scenario.rs"]
mod scenario;

use std::path::Path;

fn assert_same(what: &str, got: &str, file: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(file);
    if std::env::var_os("REVIEW_BLESS").is_some() {
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

#[test]
fn ac21_briefs_and_corrections_match_the_golden_file() {
    let (briefs, _) = scenario::run();
    assert_same("briefs", &briefs, "briefs.txt");
}

#[test]
fn ac21_the_report_matches_the_golden_file() {
    let (_, report) = scenario::run();
    assert_same("report", &report, "report.md");
}

#[test]
fn ac8_blind_briefs_name_no_peer() {
    let (briefs, _) = scenario::run();
    let mut checked = Vec::new();
    for section in briefs
        .split("===== ")
        .filter(|s| s.starts_with("R1 brief to "))
    {
        let to = section["R1 brief to ".len()..].split(' ').next().unwrap();
        if to == "lead" {
            continue;
        }
        checked.push(to);
        for peer in ["claude", "codex", "agy"].iter().filter(|p| **p != to) {
            assert!(
                !section.contains(&format!("{peer}/"))
                    && !section.contains(&format!("{peer}.json")),
                "R1 brief to {to} names {peer}"
            );
            assert!(
                !section.contains(&format!("reviewer {peer}")),
                "R1 brief to {to} names reviewer {peer}"
            );
        }
        assert!(
            !section.contains("### F"),
            "R1 brief to {to} shows findings"
        );
    }
    assert_eq!(
        checked,
        ["claude", "codex", "agy"],
        "every blind brief was checked"
    );
}

#[test]
fn ac21_a_reply_breaking_several_rules_gets_one_line_for_each() {
    let (briefs, _) = scenario::run();
    let correction = briefs
        .split("===== ")
        .find(|s| s.starts_with("R2 correction to codex"))
        .expect("codex's round-2 reply was turned down");
    assert!(
        correction.contains("stances[0].note is required for refine"),
        "{correction}"
    );
    assert!(
        correction.contains("stances[1].finding \"F9\" is not a finding"),
        "{correction}"
    );
}

#[test]
fn ac13_a_finding_merged_into_another_shows_in_the_report() {
    let (_, report) = scenario::run();
    assert!(report.contains("F4"), "{report}");
    assert!(
        report.contains("→ F3"),
        "the merge names where F4 went: {report}"
    );
}

#[test]
fn ac22_the_report_has_its_sections_in_order_and_the_gaps() {
    let (_, report) = scenario::run();
    let mut at = 0;
    assert!(report.starts_with("# レビュー結果\n"), "{report}");
    for heading in review::model::export::SECTIONS {
        let found = report[at..]
            .find(heading)
            .unwrap_or_else(|| panic!("{heading} missing or out of order"));
        at += found + heading.len();
    }
    assert!(
        report.contains("- 担当のいない観点: 性能、設計・アーキテクチャ、テスト、UX・ドキュメント"),
        "{report}"
    );
    assert!(
        report.contains("README.md"),
        "the unreviewed file is listed"
    );
    assert!(report.contains("スナップショット外: logo.png"));
    assert!(report.contains("claude/2（R1 退け）"));
    assert!(
        report.contains("### F2"),
        "the disputed finding has its own section"
    );
    assert!(report.contains("- claude: 反論（R2） flock was never exported."));
}
