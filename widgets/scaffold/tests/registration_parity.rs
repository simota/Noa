//! The port of the checker's rules against the shared oracle (Decision 1):
//! each fixture repo of `scripts/tests/fixtures/widget-registration/` (I4)
//! scanned by `scaffold::scan::scan` must give exactly its `expected.json`,
//! the checker's `--json` output, compared as JSON values.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The cases that have a `parity_*` test below.
const CASES: [&str; 9] = [
    "clean",
    "missing-launch",
    "missing-verify",
    "missing-packaging",
    "os-difference-explained",
    "os-difference-unexplained",
    "unparseable-script",
    "unparseable-registry",
    "mismatches",
];

fn fixtures() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/tests/fixtures/widget-registration");
    assert!(
        dir.is_dir(),
        "the fixture directory {} is missing: the parity tests need the checker's fixtures (unit U1)",
        dir.display()
    );
    dir
}

fn parity(case: &str) {
    let dir = fixtures().join(case);
    let expected_path = dir.join("expected.json");
    let text = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("{} cannot be read: {e}", expected_path.display()));
    let expected: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not JSON: {e}", expected_path.display()));
    let repo = dir.join("repo");
    assert!(repo.is_dir(), "{} is missing", repo.display());
    let actual = serde_json::to_value(scaffold::scan::scan(&repo)).unwrap();
    if actual != expected {
        panic!(
            "case {case}: the port disagrees with expected.json\n--- expected\n{}\n--- port\n{}",
            serde_json::to_string_pretty(&expected).unwrap(),
            serde_json::to_string_pretty(&actual).unwrap()
        );
    }
}

#[test]
fn parity_clean() {
    parity("clean");
}

#[test]
fn parity_missing_launch() {
    parity("missing-launch");
}

#[test]
fn parity_missing_verify() {
    parity("missing-verify");
}

#[test]
fn parity_missing_packaging() {
    parity("missing-packaging");
}

#[test]
fn parity_os_difference_explained() {
    parity("os-difference-explained");
}

#[test]
fn parity_os_difference_unexplained() {
    parity("os-difference-unexplained");
}

#[test]
fn parity_unparseable_script() {
    parity("unparseable-script");
}

#[test]
fn parity_unparseable_registry() {
    parity("unparseable-registry");
}

#[test]
fn parity_mismatches() {
    parity("mismatches");
}

#[test]
fn parity_every_case_is_tested() {
    let mut found: Vec<String> = std::fs::read_dir(fixtures())
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    found.sort();
    let mut tested: Vec<String> = CASES.iter().map(|c| c.to_string()).collect();
    tested.sort();
    assert_eq!(
        found, tested,
        "every fixture case needs a parity_* test, and every parity_* test a case"
    );
}
