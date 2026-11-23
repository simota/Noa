//! Golden tests pinning brief texts byte for byte to the output of `main`
//! before the perf5 optimizations (commit 9b5a8de).
//!
//! `golden/briefs.txt` was produced by running `golden/scenario.rs`, unchanged,
//! against that commit's crate; it is not regenerated from current code. A
//! difference here is a behaviour change in the briefs.

#[path = "golden/scenario.rs"]
mod scenario;

#[test]
fn briefs_match_main_byte_for_byte() {
    let got = scenario::briefs();
    let want = include_str!("golden/briefs.txt");
    if got != want {
        let at = got
            .bytes()
            .zip(want.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(got.len().min(want.len()));
        let from = at.saturating_sub(200);
        panic!(
            "briefs differ from main at byte {at} (got {} bytes, want {})\n--- got ---\n{}\n--- want ---\n{}",
            got.len(),
            want.len(),
            got.get(from..(at + 200).min(got.len())).unwrap_or(""),
            want.get(from..(at + 200).min(want.len())).unwrap_or(""),
        );
    }
}
