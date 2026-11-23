use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use discussion::engine::{AgentState, Config, Engine, Input, Setup};
use discussion::model::brief::{moderator_brief, panelist_brief};
use discussion::model::export::{minutes_markdown, result_markdown};
use discussion::model::map;
use discussion::model::reduce::{self, Event};
use discussion::model::types::{AgentKind, PartId, RoundNo, Session, SessionId};
use discussion::model::validate::{parse_moderator, parse_reply};
use discussion::store::Store;

const ROUNDS: RoundNo = 10;
const MSG_BYTES: usize = 20 * 1024;

fn make_body(tag: &str, size: usize) -> String {
    let line = format!(
        "Analysis paragraph for {tag}: evaluating latency, memory overhead, \
         consistency guarantees, operational complexity, and migration risk.\n"
    );
    let mut s = String::with_capacity(size + line.len());
    while s.len() < size {
        s.push_str(&line);
    }
    s.truncate(size);
    s
}

fn make_reply(round: RoundNo, part: &str, issues: &[String], reply_to: &[String]) -> String {
    let mut out = String::with_capacity(MSG_BYTES + 256);
    out.push_str("---\n");
    if !issues.is_empty() {
        out.push_str("issues: [");
        out.push_str(&issues.join(", "));
        out.push_str("]\n");
    }
    if !reply_to.is_empty() {
        out.push_str("reply_to: [");
        out.push_str(&reply_to.join(", "));
        out.push_str("]\n");
    }
    if let Some(first) = issues.first() {
        let pos = match part {
            "claude" => "for",
            "codex" => "against",
            _ => "neutral",
        };
        out.push_str(&format!("stance: {{{first}: {pos}}}\n"));
    }
    out.push_str("---\n");
    out.push_str(&make_body(&format!("r{round}-{part}"), MSG_BYTES));
    out
}

fn make_moderator_json(round: RoundNo, basis_msg: &str) -> String {
    let root_id = format!("R{round}");
    let c1 = format!("R{round}a");
    let c2 = format!("R{round}b");
    let c3 = format!("R{round}a1");
    let summary = make_body(&format!("summary-r{round}"), 2048);
    let summary_escaped = serde_json::to_string(&summary).unwrap();
    format!(
        r#"{{
  "summary": {summary_escaped},
  "issue_ops": [
    {{"op":"add","id":"{root_id}","parent":null,"title":"Root issue for round {round}"}},
    {{"op":"add","id":"{c1}","parent":"{root_id}","title":"Sub-issue A for round {round}"}},
    {{"op":"add","id":"{c2}","parent":"{root_id}","title":"Sub-issue B for round {round}"}},
    {{"op":"add","id":"{c3}","parent":"{c1}","title":"Leaf issue for round {round}"}},
    {{"op":"set_state","id":"{c1}","state":"contested"}},
    {{"op":"set_stance","id":"{root_id}","part":"claude","pos":"for","basis":"{basis_msg}"}},
    {{"op":"set_stance","id":"{root_id}","part":"codex","pos":"against","basis":"{basis_msg}"}},
    {{"op":"set_stance","id":"{c1}","part":"agy","pos":"neutral","basis":"{basis_msg}"}}
  ],
  "minutes_ops": [
    {{"op":"add","section":"decisions","text":"Decision from round {round}","issues":["{root_id}"]}},
    {{"op":"add","section":"contested","text":"Contested point from round {round}","issues":["{c1}"]}},
    {{"op":"add","section":"open_questions","text":"Open question from round {round}","issues":["{c2}"]}},
    {{"op":"add","section":"actions","text":"Action item from round {round}","issues":["{c3}"]}}
  ],
  "questions": {{
    "claude": "Elaborate on {c1} for the next round.",
    "codex": "Provide counter-evidence on {root_id}.",
    "agy": "Synthesize tradeoffs between {c1} and {c2}."
  }},
  "close": false
}}"#
    )
}

fn simulate_discussion(rounds: RoundNo) -> (Session, Vec<Vec<Event>>, String, String) {
    let setup = Setup {
        topic: "Select the primary persistence and indexing architecture",
        workdir: None,
        max_rounds: Some(rounds + 1),
        moderator: AgentKind::Claude.into(),
        panel: &[
            AgentKind::Claude.into(),
            AgentKind::Codex.into(),
            AgentKind::Agy.into(),
        ],
    };
    let (mut engine, init_out) = Engine::create_in(
        SessionId::new("disc-bench"),
        setup,
        Path::new("/tmp/disc-bench"),
        Config::default(),
        1_000,
    )
    .unwrap();
    let mut batches = init_out.batches;
    let out = engine.handle(Input::Start { now: 1_100 }).unwrap();
    batches.extend(out.batches);

    let panel = [
        PartId::new("claude"),
        PartId::new("codex"),
        PartId::new("agy"),
    ];
    let moderator = PartId::new("noa");
    let mut now = 2_000u64;
    let mut last_reply = String::new();
    let mut last_mod = String::new();

    for r in 1..=rounds {
        for part in &panel {
            now += 100;
            let o = engine
                .handle(Input::Sent {
                    round: r,
                    part: part.clone(),
                    at: now,
                })
                .unwrap();
            batches.extend(o.batches);
            now += 100;
            let o = engine
                .handle(Input::AgentStateChanged {
                    part: part.clone(),
                    state: AgentState::Running,
                    at: now,
                })
                .unwrap();
            batches.extend(o.batches);
        }

        let known_issues: Vec<String> = engine
            .session()
            .issues
            .issues
            .iter()
            .rev()
            .take(3)
            .map(|i| i.id.to_string())
            .collect();
        let reply_to: Vec<String> = engine
            .session()
            .messages
            .iter()
            .rev()
            .take(2)
            .map(|m| m.id.to_string())
            .collect();

        for part in &panel {
            now += 500;
            let content = make_reply(r, part.as_str(), &known_issues, &reply_to);
            if r == rounds && part.as_str() == "claude" {
                last_reply = content.clone();
            }
            let o = engine
                .handle(Input::ReplyFileWritten {
                    round: r,
                    part: part.clone(),
                    content,
                    at: now,
                })
                .unwrap();
            batches.extend(o.batches);
        }

        now += 100;
        let o = engine
            .handle(Input::Sent {
                round: r,
                part: moderator.clone(),
                at: now,
            })
            .unwrap();
        batches.extend(o.batches);
        now += 100;
        let o = engine
            .handle(Input::AgentStateChanged {
                part: moderator.clone(),
                state: AgentState::Running,
                at: now,
            })
            .unwrap();
        batches.extend(o.batches);

        let basis_msg = engine
            .session()
            .messages
            .last()
            .map(|m| m.id.to_string())
            .unwrap();
        let mod_json = make_moderator_json(r, &basis_msg);
        if r == rounds {
            last_mod = mod_json.clone();
        }
        now += 500;
        let o = engine
            .handle(Input::ReplyFileWritten {
                round: r,
                part: moderator.clone(),
                content: mod_json,
                at: now,
            })
            .unwrap();
        batches.extend(o.batches);
    }

    (engine.session().clone(), batches, last_reply, last_mod)
}

fn bench<F: FnMut()>(name: &str, iters: usize, mut f: F) {
    // Warmup
    f();
    let mut times_us = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t0 = Instant::now();
        f();
        times_us.push(t0.elapsed().as_secs_f64() * 1_000_000.0);
    }
    times_us.sort_by(|a, b| a.total_cmp(b));
    let med = times_us[iters / 2];
    let min = times_us[0];
    let max = times_us[iters - 1];
    println!("{name:<34} median={med:9.1} us  min={min:9.1} us  max={max:9.1} us  (n={iters})");
}

fn main() {
    let (session, batches, sample_reply, sample_mod) = simulate_discussion(ROUNDS);
    let total_msg_bytes: usize = session.messages.iter().map(|m| m.body.len()).sum();
    let total_brief_bytes: usize = session
        .rounds
        .iter()
        .flat_map(|r| r.briefs.iter().chain(&r.moderator_brief))
        .map(|b| b.body.len())
        .sum();
    println!(
        "=== discussion perf_probe ({ROUNDS} rounds x 3 panelists x {} KB) ===",
        MSG_BYTES / 1024
    );
    println!(
        "batches={} messages={} ({} KB) briefs_total={} KB issues={} minutes={}",
        batches.len(),
        session.messages.len(),
        total_msg_bytes / 1024,
        total_brief_bytes / 1024,
        session.issues.issues.len(),
        session.minutes.decisions.len()
            + session.minutes.contested.len()
            + session.minutes.open_questions.len()
            + session.minutes.actions.len()
    );

    bench("engine_full_10_rounds", 15, || {
        black_box(simulate_discussion(ROUNDS));
    });

    bench("reduce_replay_10_rounds", 30, || {
        black_box(reduce::replay(&batches).unwrap());
    });

    let panel = [
        PartId::new("claude"),
        PartId::new("codex"),
        PartId::new("agy"),
    ];
    let path = PathBuf::from("/tmp/disc-bench/r11/reply.md");
    let next_round = ROUNDS + 1;
    bench("brief_3_panelists_r11", 50, || {
        for p in &panel {
            black_box(panelist_brief(
                &session,
                p,
                next_round,
                "Follow-up question",
                &path,
            ));
        }
    });

    bench("brief_moderator_r10", 50, || {
        black_box(moderator_brief(&session, ROUNDS, &path));
    });

    bench("validate_reply_20kb", 100, || {
        black_box(parse_reply(&sample_reply, &session).unwrap());
    });

    // Validate moderator JSON against round ROUNDS (needs issues not yet added, so test round 1-like fresh ids)
    let fresh_mod = sample_mod.replace(&format!("R{ROUNDS}"), "R99");
    bench("validate_moderator_r10", 100, || {
        black_box(parse_moderator(&fresh_mod, &session, ROUNDS).unwrap());
    });

    bench("map_layout_40_issues", 200, || {
        black_box(map::layout(&session));
    });

    bench("export_result_md", 100, || {
        black_box(result_markdown(&session));
    });

    bench("export_minutes_md", 50, || {
        black_box(minutes_markdown(&session));
    });

    bench("session_clone_10_rounds", 50, || {
        black_box(session.clone());
    });

    let dir = std::env::temp_dir().join(format!("noa-disc-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = Store::create(&dir, &batches[0]).unwrap();
    store.append_all(&batches[1..]).unwrap();
    drop(store);
    let log_bytes = std::fs::metadata(dir.join("events.jsonl")).unwrap().len();
    println!("events.jsonl size = {} KB", log_bytes / 1024);

    bench("store_open_10_rounds", 15, || {
        black_box(Store::open(&dir).unwrap());
    });

    let _ = std::fs::remove_dir_all(&dir);
}
