//! Harness unit tests: input unescaping, fixture parsing, both dump modes,
//! and the bless splice.

use crate::diff::render_diff;
use crate::dump::{DumpMode, run_fixture, run_fixture_with_mode};
use crate::fixture::{Fixture, bless, unescape};

#[test]
fn unescape_decodes_known_escapes() {
    assert_eq!(
        unescape(r"\e[31m\r\n\t\\A\x07").unwrap(),
        b"\x1b[31m\r\n\t\\A\x07"
    );
    assert_eq!(unescape("漢x").unwrap(), "漢x".as_bytes());
}

#[test]
fn unescape_rejects_unknown_escape() {
    assert!(unescape(r"\q").is_err());
    assert!(unescape(r"\x1").is_err());
    assert!(unescape("dangling\\").is_err());
}

#[test]
fn text_dump_trims_trailing_blanks_and_reports_cursor() {
    assert_eq!(run_fixture(b"hi", 5, 2), "hi\n\n# cursor: 0,2");
}

#[test]
fn text_dump_marks_deferred_wrap() {
    assert_eq!(
        run_fixture(b"abcde", 5, 1),
        "abcde\n# cursor: 0,4 (pending-wrap)"
    );
}

#[test]
fn text_dump_prints_wide_cells_once() {
    assert_eq!(run_fixture("漢x".as_bytes(), 6, 1), "漢x\n# cursor: 0,3");
}

#[test]
fn attrs_dump_groups_style_runs() {
    let dump = run_fixture_with_mode(b"\x1b[1;31mAB\x1b[0mC", 8, 1, DumpMode::Attrs);
    assert_eq!(
        dump,
        "0: [0-1] \"AB\" fg=1 attrs=bold\n0: [2-2] \"C\"\n# cursor: 0,3"
    );
}

#[test]
fn attrs_dump_folds_wide_lead_and_spacer() {
    let dump = run_fixture_with_mode("\x1b[44m漢".as_bytes(), 6, 1, DumpMode::Attrs);
    assert_eq!(dump, "0: [0-1] \"漢\" bg=4\n# cursor: 0,2");
}

const SAMPLE: &str = "\
## cols: 5
## rows: 2
## mode: text
## input:
hi
## expect:
stale
## why:
sample fixture for the parser tests.
";

#[test]
fn parse_reads_all_sections() {
    let fixture = Fixture::parse(SAMPLE).unwrap();
    assert_eq!((fixture.cols, fixture.rows), (5, 2));
    assert_eq!(fixture.mode, DumpMode::Text);
    assert_eq!(fixture.input, b"hi");
    assert_eq!(fixture.expect, "stale");
    assert!(fixture.why.contains("sample fixture"));
}

#[test]
fn parse_rejects_missing_sections() {
    assert!(Fixture::parse("## cols: 5\n## rows: 2\n## mode: text\n").is_err());
    assert!(Fixture::parse(&SAMPLE.replace("## why:\n", "## why:\n\n")).is_ok());
    assert!(
        Fixture::parse(
            "## cols: 5\n## rows: 2\n## mode: bogus\n## input:\n## expect:\n## why:\nx\n"
        )
        .is_err()
    );
}

#[test]
fn bless_replaces_only_the_expect_section() {
    let actual = run_fixture(b"hi", 5, 2);
    let blessed = bless(SAMPLE, &actual).unwrap();
    assert_eq!(blessed, SAMPLE.replace("stale", "hi\n\n# cursor: 0,2"));
    let fixture = Fixture::parse(&blessed).unwrap();
    assert_eq!(fixture.expect, actual);
}

#[test]
fn diff_pairs_lines_and_marks_divergence() {
    assert_eq!(
        render_diff("same\nold", "same\nnew\nextra"),
        "--- expected\n+++ actual\n  same\n- old\n+ new\n+ extra"
    );
}
