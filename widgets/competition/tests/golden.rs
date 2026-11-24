//! Golden tests pinning brief texts and `compare.html` byte for byte to the
//! output of `main` before the perf5 optimizations (commit 9b5a8de).
//!
//! The files under `golden/` were produced by running `golden/scenario.rs`,
//! unchanged, against that commit's crate (`compare-*.html` with its
//! `viewer::render_html`); they are not regenerated from current code. A
//! difference here is a behaviour change.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;

use competition::model::export;
use competition::model::viewer::{self, ViewerCache};

#[path = "golden/scenario.rs"]
mod scenario;

fn golden_html(step: &str) -> &'static str {
    match step {
        "r1-open" => include_str!("golden/compare-r1-open.html"),
        "r1-closed" => include_str!("golden/compare-r1-closed.html"),
        "r1-preview-late" => include_str!("golden/compare-r1-preview-late.html"),
        "r2-amended" => include_str!("golden/compare-r2-amended.html"),
        "r3-final" => include_str!("golden/compare-r3-final.html"),
        other => panic!("unknown step {other}"),
    }
}

fn assert_same(what: &str, got: &str, want: &str) {
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
        "{what} differs from main at byte {at} (got {} bytes, want {})\n--- got ---\n{}\n--- want ---\n{}",
        got.len(),
        want.len(),
        window(got),
        window(want),
    );
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "noa-competition-golden-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

#[test]
fn briefs_match_main_byte_for_byte() {
    let root = temp_root("briefs");
    let got = scenario::run(&root, &mut |_, _| {}, &mut |_, _, _| {});
    assert_same("briefs", &got, include_str!("golden/briefs.txt"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn compare_html_matches_main_byte_for_byte() {
    let root = temp_root("viewer");
    let mut seen = Vec::new();
    scenario::run(&root, &mut |_, _| {}, &mut |step, session, root| {
        let html = viewer::render_html(session, root);
        assert_same(&format!("compare.html at {step}"), &html, golden_html(step));
        seen.push(step.to_owned());
    });
    assert_eq!(seen, scenario::STEPS);
    let _ = fs::remove_dir_all(&root);
}

/// The runner's path: one cache kept across every refresh, invalidated after
/// each snapshot, with `.preview` appearing after its round closed.
#[test]
fn cached_compare_html_matches_main_byte_for_byte() {
    let root = temp_root("viewer-cached");
    let cache = RefCell::new(ViewerCache::new());
    let mut seen = Vec::new();
    scenario::run(
        &root,
        &mut |round, part| cache.borrow_mut().invalidate(round, part),
        &mut |step, session, root| {
            // Twice: a fresh fill, then a pure cache hit.
            for pass in ["fill", "hit"] {
                let html = viewer::render_html_cached(session, root, &mut cache.borrow_mut());
                assert_same(
                    &format!("cached compare.html at {step} ({pass})"),
                    &html,
                    golden_html(step),
                );
            }
            seen.push(step.to_owned());
        },
    );
    assert_eq!(seen, scenario::STEPS);
    let _ = fs::remove_dir_all(&root);
}

/// The Markdown report of the finished scenario. `golden/report-r3-final.md`
/// was generated from the code before the judge panel changed the export,
/// so a single-judge report must stay byte-identical to it.
#[test]
fn report_markdown_matches_main_byte_for_byte() {
    let root = temp_root("report");
    let mut report = None;
    scenario::run(&root, &mut |_, _| {}, &mut |step, session, _| {
        if step == "r3-final" {
            report = Some(export::result_markdown(session));
        }
    });
    let got = report.expect("the scenario reaches r3-final");
    assert_same("report", &got, include_str!("golden/report-r3-final.md"));
    let _ = fs::remove_dir_all(&root);
}
