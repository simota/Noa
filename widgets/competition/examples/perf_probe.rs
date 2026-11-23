use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use competition::check;
use competition::engine::{AgentState, Config, Engine, Input, Setup};
use competition::model::brief::{contestant_brief, judge_brief};
use competition::model::export::result_markdown;
use competition::model::map;
use competition::model::reduce::{self, Event};
use competition::model::types::{
    AgentKind, ArtifactFile, ArtifactKind, PartId, RoundNo, Session, SessionId,
};
use competition::model::validate::{parse_judge, parse_submission};
use competition::model::viewer;
use competition::preview;
use competition::store::{self, Store};

pub const ROUNDS: RoundNo = 10;
const REPORT_BYTES: usize = 20 * 1024;

fn make_body(tag: &str, size: usize) -> String {
    let line = format!(
        "Detailed design report for {tag}: typography scale, layout grid, contrast ratios, \
         responsive breakpoints, keyboard navigation, and verification notes.\n"
    );
    let mut s = String::with_capacity(size + line.len());
    while s.len() < size {
        s.push_str(&line);
    }
    s.truncate(size);
    s
}

fn make_submission(round: RoundNo, part: &str) -> String {
    format!(
        "# Summary: Round {round} iteration by {part}\n\n## Beyond feedback\nImproved polish.\n\n{}",
        make_body(&format!("r{round}-{part}"), REPORT_BYTES)
    )
}

fn make_judge_json(round: RoundNo) -> String {
    let base = 70 + (round.min(20)) as u8;
    format!(
        r#"{{
  "overview": "Round {round} overview: all three contestants improved layout, accessibility, and responsiveness across viewports.",
  "evaluations": {{
    "claude": {{
      "score": {s1},
      "strengths": "Strong visual hierarchy and accessible contrast",
      "feedback": "Tighten mobile card spacing and verify 320px overflow",
      "breakdown": [
        {{ "axis": "impact", "score": {b1a}, "max": 60 }},
        {{ "axis": "craft", "score": {b1b}, "max": 40 }}
      ],
      "evidence": [
        {{ "claim": "index.html renders cleanly at 1440x900", "basis": "measured" }},
        {{ "claim": "0 axe violations", "basis": "self_reported" }}
      ]
    }},
    "codex": {{
      "score": {s2},
      "strengths": "Crisp typography and minimal CSS footprint",
      "feedback": "Improve focus states and header alignment at 768px",
      "breakdown": [
        {{ "axis": "impact", "score": {b2a}, "max": 60 }},
        {{ "axis": "craft", "score": {b2b}, "max": 40 }}
      ],
      "evidence": [
        {{ "claim": "No horizontal scroll at 320x568", "basis": "measured" }}
      ]
    }},
    "agy": {{
      "score": {s3},
      "strengths": "Rich interactive preview and clean component structure",
      "feedback": "Reduce vertical height of hero section on mobile",
      "breakdown": [
        {{ "axis": "impact", "score": {b3a}, "max": 60 }},
        {{ "axis": "craft", "score": {b3b}, "max": 40 }}
      ],
      "evidence": [
        {{ "claim": "Keyboard shortcuts work in sandboxed iframe", "basis": "measured" }}
      ]
    }}
  }},
  "ranking": ["claude", "codex", "agy"],
  "checklist": [
    "320x568: no horizontal scroll",
    "1440x900: primary CTA visible above the fold",
    "Contrast >= 4.5:1 on body text"
  ],
  "amendments": [],
  "winner": null,
  "final_verdict": null,
  "close": false
}}"#,
        s1 = base + 4,
        b1a = 44 + (round.min(10)) as u8,
        b1b = base + 4 - (44 + (round.min(10)) as u8),
        s2 = base + 2,
        b2a = 43 + (round.min(10)) as u8,
        b2b = base + 2 - (43 + (round.min(10)) as u8),
        s3 = base,
        b3a = 42 + (round.min(10)) as u8,
        b3b = base - (42 + (round.min(10)) as u8),
    )
}

fn make_artifacts_list() -> Vec<ArtifactFile> {
    vec![
        ArtifactFile {
            rel_path: "index.html".into(),
            kind: ArtifactKind::Html,
            bytes: 32_000,
        },
        ArtifactFile {
            rel_path: "styles/main.css".into(),
            kind: ArtifactKind::Code,
            bytes: 24_000,
        },
        ArtifactFile {
            rel_path: "styles/theme.css".into(),
            kind: ArtifactKind::Code,
            bytes: 12_000,
        },
        ArtifactFile {
            rel_path: "app.js".into(),
            kind: ArtifactKind::Code,
            bytes: 80_000,
        },
        ArtifactFile {
            rel_path: "README.md".into(),
            kind: ArtifactKind::Markdown,
            bytes: 16_000,
        },
        ArtifactFile {
            rel_path: "assets/hero.png".into(),
            kind: ArtifactKind::Image,
            bytes: 48_000,
        },
        ArtifactFile {
            rel_path: "assets/icon.svg".into(),
            kind: ArtifactKind::Svg,
            bytes: 4_000,
        },
    ]
}

pub fn populate_snapshot(dir: &Path) {
    fs::create_dir_all(dir.join("styles")).unwrap();
    fs::create_dir_all(dir.join("assets")).unwrap();

    // Realistic HTML with 250 elements, stylesheet, deferred + plain scripts, repeated assets
    let mut html = String::with_capacity(32_000);
    html.push_str(
        "<!doctype html><html lang=\"en\"><head>\n\
         <meta charset=\"utf-8\">\n\
         <title>Competition Entry</title>\n\
         <link rel=\"stylesheet\" href=\"styles/main.css\">\n\
         <link rel=\"icon\" href=\"assets/icon.svg\">\n\
         <script src=\"app.js\" defer></script>\n\
         <script src=\"inline.js\"></script>\n\
         <style>.banner { background: url('assets/hero.png'); }</style>\n\
         </head><body>\n",
    );
    for i in 0..80 {
        html.push_str(&format!(
            "<div class=\"card\" data-idx=\"{i}\" style=\"background-image:url('assets/icon.svg')\">\
             <img src=\"assets/hero.png\" alt=\"Hero {i}\">\
             <img src=\"assets/icon.svg\" alt=\"Icon {i}\">\
             <p class=\"desc\">Item {i}: Craft and usability details &amp; notes.</p>\
             <a href=\"other.html\">Details</a>\
             </div>\n"
        ));
    }
    html.push_str("</body></html>");
    fs::write(dir.join("index.html"), html).unwrap();

    let mut theme_css = String::with_capacity(12_000);
    for i in 0..100 {
        theme_css.push_str(&format!(
            ".theme-icon-{i} {{ background: url('../assets/icon.svg'); color: #e6eaf2; }}\n"
        ));
    }
    fs::write(dir.join("styles/theme.css"), theme_css).unwrap();

    let mut main_css = String::from("@import url(\"theme.css\");\n");
    for i in 0..150 {
        main_css.push_str(&format!(
            ".card-{i} {{ padding: 12px; background-image: url('../assets/hero.png'); }}\n"
        ));
    }
    fs::write(dir.join("styles/main.css"), main_css).unwrap();

    let mut js = String::with_capacity(80_000);
    for i in 0..1200 {
        js.push_str(&format!(
            "function handler_{i}(x) {{ return x * 2 + {i}; }}\n"
        ));
    }
    fs::write(dir.join("app.js"), &js).unwrap();
    fs::write(dir.join("inline.js"), &js[..20_000]).unwrap();
    fs::write(dir.join("README.md"), make_body("readme", 16_000)).unwrap();

    // Deterministic synthetic PNG & SVG payloads
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    for i in 0..48_000usize {
        png.push(((i * 31 + 17) & 0xff) as u8);
    }
    fs::write(dir.join("assets/hero.png"), png).unwrap();

    let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">");
    for i in 0..60 {
        svg.push_str(&format!(
            "<circle cx=\"{}\" cy=\"{}\" r=\"2\"/>",
            i % 64,
            (i * 3) % 64
        ));
    }
    svg.push_str("</svg>");
    fs::write(dir.join("assets/icon.svg"), svg).unwrap();
    fs::write(dir.join("other.html"), "<p>Other page</p>").unwrap();
}

pub fn simulate_competition(rounds: RoundNo) -> (Session, Vec<Vec<Event>>, String, String) {
    let setup = Setup {
        request: "Design a responsive, accessible audio workstation web UI",
        workdir: None,
        max_rounds: Some(rounds + 1),
        reveal_interval: 5,
        judge: AgentKind::Claude.into(),
        panel: &[
            AgentKind::Claude.into(),
            AgentKind::Codex.into(),
            AgentKind::Agy.into(),
        ],
    };
    let (mut engine, init_out) = Engine::create_in(
        SessionId::new("comp-bench"),
        setup,
        Path::new("/tmp/comp-bench"),
        Config::default(),
        1_000,
    )
    .unwrap();
    let mut batches = init_out.batches;
    let o = engine.handle(Input::Start { now: 1_100 }).unwrap();
    batches.extend(o.batches);

    let panel = [
        PartId::new("claude"),
        PartId::new("codex"),
        PartId::new("agy"),
    ];
    let judge = PartId::new("judge");
    let artifacts = make_artifacts_list();
    let mut now = 2_000u64;
    let mut sample_sub = String::new();
    let mut sample_judge = String::new();

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
        for part in &panel {
            now += 500;
            let content = make_submission(r, part.as_str());
            if r == rounds && part.as_str() == "claude" {
                sample_sub = content.clone();
            }
            let o = engine
                .handle(Input::ReplyFileWritten {
                    round: r,
                    part: part.clone(),
                    content,
                    artifacts: artifacts.clone(),
                    at: now,
                })
                .unwrap();
            batches.extend(o.batches);
        }
        now += 100;
        let o = engine
            .handle(Input::Sent {
                round: r,
                part: judge.clone(),
                at: now,
            })
            .unwrap();
        batches.extend(o.batches);
        now += 100;
        let o = engine
            .handle(Input::AgentStateChanged {
                part: judge.clone(),
                state: AgentState::Running,
                at: now,
            })
            .unwrap();
        batches.extend(o.batches);

        let j = make_judge_json(r);
        if r == rounds {
            sample_judge = j.clone();
        }
        now += 500;
        let o = engine
            .handle(Input::ReplyFileWritten {
                round: r,
                part: judge.clone(),
                content: j,
                artifacts: Vec::new(),
                at: now,
            })
            .unwrap();
        batches.extend(o.batches);
    }

    (engine.session().clone(), batches, sample_sub, sample_judge)
}

fn bench<F: FnMut()>(name: &str, iters: usize, mut f: F) {
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
    println!("{name:<36} median={med:9.1} us  min={min:9.1} us  max={max:9.1} us  (n={iters})");
}

fn main() {
    let (session, batches, sample_sub, sample_judge) = simulate_competition(ROUNDS);
    let total_msg_bytes: usize = session.messages.iter().map(|m| m.body.len()).sum();
    let total_brief_bytes: usize = session
        .rounds
        .iter()
        .flat_map(|r| r.briefs.iter().chain(&r.judge_brief))
        .map(|b| b.body.len())
        .sum();
    println!(
        "=== competition perf_probe ({ROUNDS} rounds x 3 contestants x {} KB) ===",
        REPORT_BYTES / 1024
    );
    println!(
        "batches={} messages={} ({} KB) briefs_total={} KB",
        batches.len(),
        session.messages.len(),
        total_msg_bytes / 1024,
        total_brief_bytes / 1024,
    );

    bench("engine_full_10_rounds", 15, || {
        black_box(simulate_competition(ROUNDS));
    });

    bench("reduce_replay_10_rounds", 30, || {
        black_box(reduce::replay(&batches).unwrap());
    });

    let panel = [
        PartId::new("claude"),
        PartId::new("codex"),
        PartId::new("agy"),
    ];
    let reply_dir = PathBuf::from("/tmp/comp-bench");
    let entry_dir = reply_dir.join("entries/claude");
    let reply_path = reply_dir.join("r11/report-claude.md");
    // Round 11 is Reveal when reveal_interval=5
    bench("brief_3_contestants_reveal_r11", 50, || {
        for p in &panel {
            black_box(contestant_brief(
                &session,
                p,
                ROUNDS + 1,
                &entry_dir,
                &reply_dir,
                &reply_path,
            ));
        }
    });

    bench("brief_judge_r10", 50, || {
        black_box(judge_brief(&session, ROUNDS, &reply_dir, &reply_path));
    });

    bench("validate_submission_20kb", 100, || {
        black_box(parse_submission(&sample_sub).unwrap());
    });

    bench("validate_judge_r10", 100, || {
        black_box(parse_judge(&sample_judge, &session, ROUNDS).unwrap());
    });

    let order = session.current_ranking();
    bench("map_layout_10_rounds", 200, || {
        black_box(map::layout(&session, &order));
    });

    bench("export_result_md", 100, || {
        black_box(result_markdown(&session));
    });

    bench("session_clone_10_rounds", 50, || {
        black_box(session.clone());
    });

    // Disk-backed probes: preview::write, viewer::render_html, check::inspect_with, Store::open
    let root = std::env::temp_dir().join(format!("noa-comp-probe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    // Populate snapshots for all 10 rounds x 3 contestants
    for r in 1..=ROUNDS {
        for p in &panel {
            let snap = store::artifact_snapshot_dir(&root, r, p);
            populate_snapshot(&snap);
            if r == 1 && p.as_str() == "claude" {
                let _ = preview::write(&snap).unwrap();
            }
        }
    }

    let sample_snap = store::artifact_snapshot_dir(&root, 1, &panel[0]);
    bench("preview_write_realistic_page", 15, || {
        black_box(preview::write(&sample_snap).unwrap());
    });

    let html_r10 = viewer::render_html(&session, &root);
    println!("compare.html size at R10 = {} KB", html_r10.len() / 1024);

    bench("viewer_render_html_r10", 15, || {
        black_box(viewer::render_html(&session, &root));
    });

    // Cumulative 40 viewer refreshes across 10 rounds (3 submissions + 1 judge per round)
    bench("viewer_40_refreshes_over_10r", 5, || {
        let mut partial = session.clone();
        partial.rounds.clear();
        for r in &session.rounds {
            partial.rounds.push(r.clone());
            for _ in 0..4 {
                black_box(viewer::render_html(&partial, &root));
            }
        }
    });

    bench("viewer_40_refreshes_cached", 5, || {
        let mut cache = viewer::ViewerCache::new();
        let mut partial = session.clone();
        partial.rounds.clear();
        for r in &session.rounds {
            partial.rounds.push(r.clone());
            for _ in 0..4 {
                black_box(viewer::render_html_cached(&partial, &root, &mut cache));
            }
        }
    });

    let checks_out = root.join("checks-bench");
    let prev = preview::write(&sample_snap).unwrap();
    bench("check_manifest_no_chrome", 30, || {
        black_box(check::inspect_with(&sample_snap, &checks_out, prev.clone(), None).unwrap());
    });

    let log_dir = root.join("store");
    let mut store = Store::create(&log_dir, &batches[0]).unwrap();
    store.append_all(&batches[1..]).unwrap();
    drop(store);
    let log_bytes = fs::metadata(log_dir.join("events.jsonl")).unwrap().len();
    println!("events.jsonl size = {} KB", log_bytes / 1024);

    bench("store_open_10_rounds", 15, || {
        black_box(Store::open(&log_dir).unwrap());
    });

    let _ = fs::remove_dir_all(&root);
}
