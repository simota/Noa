//! Golden tests pinning what a judge panel's competition shows outside the
//! window: `compare.html` (cached and uncached) and the Markdown report.
//!
//! The files under `golden/multi/` were generated once from the first
//! implementation of the judge panel's views and reviewed by hand; they are
//! not regenerated from current code. A difference here is a behaviour
//! change.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;

use competition::model::export;
use competition::model::viewer::{self, ViewerCache};

#[path = "golden/multi/session.rs"]
mod session;

fn golden_html(step: &str) -> &'static str {
    match step {
        "r1-closed" => include_str!("golden/multi/compare-r1-closed.html"),
        "r2-verdict-pending" => include_str!("golden/multi/compare-r2-verdict-pending.html"),
        "r2-final" => include_str!("golden/multi/compare-r2-final.html"),
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
        "{what} differs from the fixture at byte {at} (got {} bytes, want {})\n--- got ---\n{}\n--- want ---\n{}",
        got.len(),
        want.len(),
        window(got),
        window(want),
    );
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "noa-competition-golden-multi-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

#[test]
fn multi_judge_compare_html_matches_the_fixture() {
    let root = temp_root("viewer");
    let mut seen = Vec::new();
    session::run(&root, &mut |_, _| {}, &mut |step, session, root| {
        let html = viewer::render_html(session, root);
        assert_same(&format!("compare.html at {step}"), &html, golden_html(step));
        seen.push(step.to_owned());
    });
    assert_eq!(seen, session::STEPS);
    let _ = fs::remove_dir_all(&root);
}

/// The runner's path: one cache kept across every refresh, invalidated
/// after each snapshot. R1 is cached once closed, and then `judge-2`'s
/// amendment moves its own R1 score without moving the rounded mean.
#[test]
fn cached_multi_judge_compare_html_matches_uncached() {
    let root = temp_root("viewer-cached");
    let cache = RefCell::new(ViewerCache::new());
    let mut seen = Vec::new();
    session::run(
        &root,
        &mut |round, part| cache.borrow_mut().invalidate(round, part),
        &mut |step, session, root| {
            let uncached = viewer::render_html(session, root);
            // Twice: a fresh fill, then a pure cache hit.
            for pass in ["fill", "hit"] {
                let html = viewer::render_html_cached(session, root, &mut cache.borrow_mut());
                assert_same(
                    &format!("cached compare.html at {step} ({pass})"),
                    &html,
                    &uncached,
                );
            }
            assert_same(
                &format!("compare.html at {step}"),
                &uncached,
                golden_html(step),
            );
            seen.push(step.to_owned());
        },
    );
    assert_eq!(seen, session::STEPS);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn multi_judge_report_matches_the_fixture() {
    let root = temp_root("report");
    let session = session::run(&root, &mut |_, _| {}, &mut |_, _, _| {});
    let got = export::result_markdown(&session);
    assert_same("report", &got, include_str!("golden/multi/report.md"));
    let _ = fs::remove_dir_all(&root);
}
