//! Runs every fixture under `fixtures/` through the dump harness and compares
//! it against its `## expect:` section.
//!
//! `NOA_PARITY_BLESS=1 cargo test -p noa-parity` rewrites diverging expect
//! sections in place instead of failing (review the diff before committing).

use std::fs;
use std::path::PathBuf;

use noa_parity::{Fixture, fixture, render_diff, run_fixture_with_mode};

#[test]
fn fixtures_match_expectations() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", dir.display()))
        .map(|entry| entry.expect("fixture dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures found in {}", dir.display());

    let should_bless = std::env::var("NOA_PARITY_BLESS").as_deref() == Ok("1");
    let mut failures = Vec::new();
    let mut blessed = 0usize;
    for path in &paths {
        let name = path.file_name().expect("fixture file name").to_string_lossy();
        let source = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("{name}: cannot read fixture: {err}"));
        let fx = Fixture::parse(&source).unwrap_or_else(|err| panic!("{name}: {err}"));
        let actual = run_fixture_with_mode(&fx.input, fx.cols, fx.rows, fx.mode);
        if actual == fx.expect {
            continue;
        }
        if should_bless {
            let updated =
                fixture::bless(&source, &actual).unwrap_or_else(|err| panic!("{name}: {err}"));
            fs::write(path, updated)
                .unwrap_or_else(|err| panic!("{name}: cannot write blessed fixture: {err}"));
            blessed += 1;
        } else {
            failures.push(format!("=== {name}\n{}", render_diff(&fx.expect, &actual)));
        }
    }

    if blessed > 0 {
        eprintln!("blessed {blessed} fixture(s) — review the diff before committing");
    }
    assert!(
        failures.is_empty(),
        "{} fixture(s) diverged (NOA_PARITY_BLESS=1 rewrites expectations):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
