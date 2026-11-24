//! Golden test pinning every brief of a competition judged by a panel of
//! three, byte for byte. `golden/multi/briefs.txt` was produced once by
//! `golden/multi/scenario.rs` with the code that introduced the judge
//! panel, and reviewed by hand: no judge brief holds anything another
//! judge wrote. A difference here is a behaviour change.

use competition::model::types::{PartId, Session};
use competition::store;

#[path = "golden/multi/scenario.rs"]
mod scenario;

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

/// AC-4 over every round: each judge's scorecard brief names its own reply
/// path and no other judge's, and quotes nothing any judge wrote (every
/// judge text in the scenario carries a `[by …]` mark). The lead's
/// verdict brief is the one place the judges' scores and overviews meet.
fn assert_blind_between_judges(session: &Session) {
    let dir = std::path::Path::new(scenario::DIR);
    let judges: Vec<PartId> = session.judges().map(|j| j.id.clone()).collect();
    assert_eq!(judges.len(), 3);
    for round in &session.rounds {
        let briefs: Vec<_> = round
            .judge_brief
            .iter()
            .chain(&round.co_judge_briefs)
            .collect();
        assert_eq!(briefs.len(), 3, "R{}: a brief per judge", round.no);
        for brief in briefs {
            assert!(
                !brief.body.contains("[by "),
                "R{} brief to {} quotes a judge",
                round.no,
                brief.to
            );
            for judge in &judges {
                let path = store::judge_reply_path(dir, round.no, judge);
                assert_eq!(
                    brief.body.contains(&path.display().to_string()),
                    *judge == brief.to,
                    "R{} brief to {} and {}",
                    round.no,
                    brief.to,
                    path.display()
                );
            }
            let verdict = store::verdict_reply_path(dir, round.no);
            assert!(!brief.body.contains(&verdict.display().to_string()));
        }
    }
    let last = session.rounds.last().unwrap();
    let verdict = &last.verdict_brief.as_ref().unwrap().body;
    for judge in &judges {
        assert!(verdict.contains(&format!("[by {judge}] R{} overview", last.no)));
    }
}

#[test]
fn multi_judge_briefs_match_the_fixture() {
    let (got, session) = scenario::run();
    assert_blind_between_judges(&session);
    assert_same("briefs", &got, include_str!("golden/multi/briefs.txt"));
}
