//! Generates a self-contained HTML comparison viewer (`compare.html`) inside
//! the competition's artifact root so visual deliverables (HTML/CSS/JS designs,
//! SVG, PNG/JPG/WebP images) and code/text reports can be compared side by side
//! in the system browser across rounds.

use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use super::types::{ArtifactKind, PartId, RoundKind, RoundNo, Session};
use crate::store;

#[derive(Serialize)]
struct ViewerData {
    id: String,
    request: String,
    reveal_interval: u32,
    winner: Option<String>,
    contestants: Vec<String>,
    rounds: Vec<ViewerRound>,
}

#[derive(Serialize)]
struct ViewerRound {
    no: u32,
    kind: &'static str,
    overview: Option<String>,
    ranking: Vec<String>,
    final_verdict: Option<String>,
    entries: Vec<ViewerEntry>,
}

#[derive(Serialize)]
struct ViewerEntry {
    part: String,
    score: Option<u8>,
    strengths: String,
    feedback: String,
    summary: String,
    report: String,
    files: Vec<ViewerFile>,
}

#[derive(Serialize)]
struct ViewerFile {
    rel_path: String,
    /// Relative URL from `compare.html` to the round snapshot file.
    href: String,
    kind: &'static str,
    bytes: u64,
    /// Inline preview text for code/markdown files (truncated for large files).
    preview: Option<String>,
}

const MAX_INLINE_TEXT_BYTES: usize = 32_768;

fn artifact_kind_tag(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Html => "html",
        ArtifactKind::Image => "image",
        ArtifactKind::Svg => "svg",
        ArtifactKind::Code => "code",
        ArtifactKind::Markdown => "markdown",
        ArtifactKind::Other => "other",
    }
}

fn read_text_preview(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let slice = if bytes.len() > MAX_INLINE_TEXT_BYTES {
        &bytes[..MAX_INLINE_TEXT_BYTES]
    } else {
        &bytes
    };
    let text = String::from_utf8_lossy(slice);
    if bytes.len() > MAX_INLINE_TEXT_BYTES {
        Some(format!("{text}\n\n… (truncated)"))
    } else {
        Some(text.into_owned())
    }
}

/// The URL of a round snapshot file, relative to `compare.html`.
fn file_href(round: RoundNo, part: &PartId, rel_path: &str) -> String {
    let mut href = format!("r{round}/artifacts/{part}/");
    for byte in rel_path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            href.push(byte as char);
        } else {
            let _ = write!(href, "%{byte:02X}");
        }
    }
    href
}

/// Build the viewer's HTML document for `session`, referencing artifact
/// snapshots under `artifact_root` (where `compare.html` will be written).
pub fn render_html(session: &Session, artifact_root: &Path) -> String {
    let rounds = session
        .rounds
        .iter()
        .map(|r| {
            let entries = session
                .contestant_ids()
                .map(|part| {
                    let ev = r.outcome.as_ref().and_then(|o| o.evaluations.get(part));
                    let sub = r.submissions.get(part);
                    let files = sub
                        .map(|s| {
                            s.artifacts
                                .iter()
                                .map(|f| {
                                    let href = file_href(r.no, part, &f.rel_path);
                                    let preview = match f.kind {
                                        ArtifactKind::Code | ArtifactKind::Markdown => {
                                            read_text_preview(
                                                &store::artifact_snapshot_dir(
                                                    artifact_root,
                                                    r.no,
                                                    part,
                                                )
                                                .join(&f.rel_path),
                                            )
                                        }
                                        _ => None,
                                    };
                                    ViewerFile {
                                        rel_path: f.rel_path.clone(),
                                        href,
                                        kind: artifact_kind_tag(f.kind),
                                        bytes: f.bytes,
                                        preview,
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    ViewerEntry {
                        part: part.to_string(),
                        score: ev.map(|e| e.score),
                        strengths: ev.map(|e| e.strengths.clone()).unwrap_or_default(),
                        feedback: ev.map(|e| e.feedback.clone()).unwrap_or_default(),
                        summary: sub.map(|s| s.summary.clone()).unwrap_or_default(),
                        report: sub.map(|s| s.body.clone()).unwrap_or_default(),
                        files,
                    }
                })
                .collect();
            ViewerRound {
                no: r.no,
                kind: match r.kind {
                    RoundKind::Blind => "blind",
                    RoundKind::Reveal => "reveal",
                },
                overview: r.outcome.as_ref().map(|o| o.overview.clone()),
                ranking: r
                    .outcome
                    .as_ref()
                    .map(|o| o.ranking.iter().map(|p| p.to_string()).collect())
                    .unwrap_or_default(),
                final_verdict: r.outcome.as_ref().and_then(|o| o.final_verdict.clone()),
                entries,
            }
        })
        .collect();

    let data = ViewerData {
        id: session.id.to_string(),
        request: session.request.clone(),
        reveal_interval: session.reveal_interval,
        winner: session.winner.as_ref().map(|w| w.to_string()),
        contestants: session.contestant_ids().map(|p| p.to_string()).collect(),
        rounds,
    };

    // Escape every `<` so embedded JSON can neither close the `<script>` tag
    // (`</script>`) nor open a comment that keeps it from closing (`<!--`).
    let json = serde_json::to_string(&data)
        .unwrap_or_else(|_| "{}".to_owned())
        .replace('<', "\\u003c");

    let mut html = String::with_capacity(16_384);
    let _ = write!(
        html,
        r##"<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Noa Competition Viewer</title>
<style>
  :root {{
    color-scheme: dark;
    --bg: #0f1117;
    --surface: #171a23;
    --surface-alt: #1f2430;
    --border: #2c3344;
    --text: #e6eaf2;
    --muted: #9aa4b8;
    --accent: #6aa6ff;
    --gold: #f5c451;
    --reveal: #a78bfa;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    background: var(--bg);
    color: var(--text);
  }}
  header {{
    position: sticky;
    top: 0;
    z-index: 10;
    background: rgba(15, 17, 23, 0.94);
    backdrop-filter: blur(8px);
    border-bottom: 1px solid var(--border);
    padding: 12px 18px;
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    align-items: center;
    justify-content: space-between;
  }}
  .title-block h1 {{
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }}
  .title-block p {{
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--muted);
    max-width: 720px;
  }}
  .controls {{
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }}
  select, button {{
    background: var(--surface-alt);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 10px;
    font-size: 12px;
    cursor: pointer;
  }}
  button.active {{
    border-color: var(--accent);
    color: var(--accent);
  }}
  .banner {{
    margin: 12px 18px 0;
    padding: 10px 14px;
    border-radius: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    font-size: 13px;
    line-height: 1.5;
  }}
  .badge {{
    display: inline-block;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    margin-right: 6px;
  }}
  .badge.blind {{ background: #262d3d; color: var(--muted); }}
  .badge.reveal {{ background: rgba(167, 139, 250, 0.18); color: var(--reveal); }}
  .badge.winner {{ background: rgba(245, 196, 81, 0.18); color: var(--gold); }}
  .grid {{
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(420px, 1fr));
    gap: 14px;
    padding: 14px 18px 32px;
  }}
  .card {{
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }}
  .card.winner-card {{
    border-color: var(--gold);
  }}
  .card-head {{
    padding: 12px 14px;
    border-bottom: 1px solid var(--border);
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }}
  .score {{
    font-size: 18px;
    font-weight: 700;
    color: var(--accent);
  }}
  .file-tabs {{
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 8px 14px;
    background: var(--surface-alt);
    border-bottom: 1px solid var(--border);
  }}
  .preview-box {{
    background: #0b0d12;
    min-height: 360px;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: auto;
    position: relative;
  }}
  .preview-box iframe {{
    border: 0;
    background: #fff;
    height: 460px;
    transition: width 0.15s ease;
  }}
  .preview-box img {{
    max-width: 100%;
    max-height: 460px;
    object-fit: contain;
    cursor: zoom-in;
  }}
  .preview-box pre {{
    margin: 0;
    padding: 12px;
    width: 100%;
    max-height: 460px;
    overflow: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
  }}
  .meta {{
    padding: 12px 14px;
    font-size: 12px;
    line-height: 1.55;
    border-top: 1px solid var(--border);
  }}
  .meta h4 {{
    margin: 8px 0 4px;
    font-size: 11px;
    text-transform: uppercase;
    color: var(--muted);
    letter-spacing: 0.04em;
  }}
  .meta h4:first-child {{ margin-top: 0; }}
  .report-body {{
    white-space: pre-wrap;
    max-height: 220px;
    overflow: auto;
    background: var(--surface-alt);
    padding: 8px 10px;
    border-radius: 6px;
  }}
  .modal {{
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.85);
    display: none;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 24px;
  }}
  .modal.open {{ display: flex; }}
  .modal img {{ max-width: 95vw; max-height: 92vh; object-fit: contain; }}
</style>
</head>
<body>
<header>
  <div class="title-block">
    <h1 id="title">Noa Competition Viewer</h1>
    <p id="request"></p>
  </div>
  <div class="controls">
    <label for="roundSelect" style="font-size:12px;color:var(--muted)">ラウンド:</label>
    <select id="roundSelect"></select>
    <button data-vp="100%" class="active">PC (100%)</button>
    <button data-vp="768px">Tablet (768px)</button>
    <button data-vp="390px">Mobile (390px)</button>
  </div>
</header>
<div id="banner" class="banner"></div>
<main id="grid" class="grid"></main>
<div id="modal" class="modal"><img id="modalImg" alt="zoom"></div>
<script>
const DATA = {json};
let viewportWidth = "100%";
const selectedFile = {{}};

const roundSelect = document.getElementById("roundSelect");
const banner = document.getElementById("banner");
const grid = document.getElementById("grid");
const modal = document.getElementById("modal");
const modalImg = document.getElementById("modalImg");
modal.addEventListener("click", () => modal.classList.remove("open"));

document.getElementById("request").textContent = DATA.request || "";
document.querySelectorAll("button[data-vp]").forEach(btn => {{
  btn.addEventListener("click", () => {{
    viewportWidth = btn.dataset.vp;
    document.querySelectorAll("button[data-vp]").forEach(b => b.classList.toggle("active", b === btn));
    document.querySelectorAll(".preview-box iframe").forEach(f => f.style.width = viewportWidth);
  }});
}});

function init() {{
  roundSelect.innerHTML = "";
  if (!DATA.rounds.length) {{
    banner.textContent = "まだ提出されたラウンドがありません。";
    return;
  }}
  DATA.rounds.forEach((r, idx) => {{
    const opt = document.createElement("option");
    opt.value = idx;
    const tag = r.kind === "reveal" ? " [相互閲覧]" : " [ブラインド]";
    opt.textContent = "Round " + r.no + tag;
    roundSelect.appendChild(opt);
  }});
  roundSelect.value = String(DATA.rounds.length - 1);
  roundSelect.addEventListener("change", () => renderRound(Number(roundSelect.value)));
  renderRound(DATA.rounds.length - 1);
}}

function pickDefaultFile(entry) {{
  const key = entry.part;
  if (selectedFile[key] && entry.files.some(f => f.rel_path === selectedFile[key])) {{
    return selectedFile[key];
  }}
  const visual = entry.files.find(f => f.kind === "html")
    || entry.files.find(f => f.kind === "image" || f.kind === "svg")
    || entry.files[0];
  return visual ? visual.rel_path : null;
}}

function renderPreview(box, file) {{
  box.innerHTML = "";
  if (!file) {{
    const p = document.createElement("p");
    p.style.color = "var(--muted)";
    p.style.fontSize = "12px";
    p.textContent = "成果物ファイルなし";
    box.appendChild(p);
    return;
  }}
  if (file.kind === "html") {{
    const iframe = document.createElement("iframe");
    iframe.src = file.href;
    iframe.style.width = viewportWidth;
    // No allow-same-origin: with it, an entry's scripts could reach this
    // page (`parent.document`) and rewrite scores or other entries.
    iframe.setAttribute("sandbox", "allow-scripts");
    box.appendChild(iframe);
  }} else if (file.kind === "image" || file.kind === "svg") {{
    const img = document.createElement("img");
    img.src = file.href;
    img.alt = file.rel_path;
    img.addEventListener("click", () => {{
      modalImg.src = file.href;
      modal.classList.add("open");
    }});
    box.appendChild(img);
  }} else if (file.preview !== null && file.preview !== undefined) {{
    const pre = document.createElement("pre");
    pre.textContent = file.preview;
    box.appendChild(pre);
  }} else {{
    const a = document.createElement("a");
    a.href = file.href;
    a.target = "_blank";
    a.style.color = "var(--accent)";
    a.textContent = file.rel_path + " を開く";
    box.appendChild(a);
  }}
}}

function renderRound(idx) {{
  const r = DATA.rounds[idx];
  if (!r) return;
  banner.innerHTML = "";
  const badge = document.createElement("span");
  badge.className = "badge " + r.kind;
  badge.textContent = r.kind === "reveal" ? "👁 相互閲覧ラウンド" : "🔒 ブラインドラウンド";
  banner.appendChild(badge);
  if (DATA.winner) {{
    const w = document.createElement("span");
    w.className = "badge winner";
    w.textContent = "🏆 優勝: " + DATA.winner;
    banner.appendChild(w);
  }}
  const txt = document.createElement("span");
  txt.textContent = r.final_verdict || r.overview || "審判の採点待ちです。";
  banner.appendChild(txt);

  grid.innerHTML = "";
  const order = r.ranking.length ? r.ranking : DATA.contestants;
  order.forEach((part, rankIdx) => {{
    const entry = r.entries.find(e => e.part === part);
    if (!entry) return;
    const card = document.createElement("section");
    card.className = "card" + (DATA.winner === part ? " winner-card" : "");

    const head = document.createElement("div");
    head.className = "card-head";
    const left = document.createElement("div");
    const rankStr = r.ranking.length ? "#" + (rankIdx + 1) + " " : "";
    left.innerHTML = "<strong>" + rankStr + part + "</strong>";
    const score = document.createElement("div");
    score.className = "score";
    score.textContent = entry.score !== null ? entry.score + " / 100" : "—";
    head.appendChild(left);
    head.appendChild(score);
    card.appendChild(head);

    const tabs = document.createElement("div");
    tabs.className = "file-tabs";
    const previewBox = document.createElement("div");
    previewBox.className = "preview-box";
    const activeRel = pickDefaultFile(entry);
    entry.files.forEach(file => {{
      const b = document.createElement("button");
      b.textContent = file.rel_path;
      if (file.rel_path === activeRel) b.classList.add("active");
      b.addEventListener("click", () => {{
        selectedFile[part] = file.rel_path;
        tabs.querySelectorAll("button").forEach(x => x.classList.toggle("active", x === b));
        renderPreview(previewBox, file);
      }});
      tabs.appendChild(b);
    }});
    if (entry.files.length) {{
      const activeFile = entry.files.find(f => f.rel_path === activeRel) || entry.files[0];
      renderPreview(previewBox, activeFile);
    }} else {{
      renderPreview(previewBox, null);
    }}
    card.appendChild(tabs);
    card.appendChild(previewBox);

    const meta = document.createElement("div");
    meta.className = "meta";
    const addBlock = (label, content, isPre) => {{
      if (!content) return;
      const h = document.createElement("h4");
      h.textContent = label;
      const d = document.createElement("div");
      if (isPre) d.className = "report-body";
      d.textContent = content;
      meta.appendChild(h);
      meta.appendChild(d);
    }};
    addBlock("サマリー", entry.summary, false);
    addBlock("評価された点", entry.strengths, false);
    addBlock("審判フィードバック", entry.feedback, false);
    addBlock("提出レポート", entry.report, true);
    card.appendChild(meta);

    grid.appendChild(card);
  }});
}}

init();
</script>
</body>
</html>
"##
    );
    html
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{
        AgentKind, ArtifactFile, JUDGE_ID, PartId, Participant, Phase, Role, Round, SessionId,
        Submission,
    };
    use std::collections::BTreeMap;

    #[test]
    fn viewer_html_embeds_rounds_and_escapes_script_closing_tags() {
        let p = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
        };
        let mut subs = BTreeMap::new();
        subs.insert(
            PartId::new("claude"),
            Submission {
                part: PartId::new("claude"),
                summary: "Design </script><script>alert(1)</script>".into(),
                body: "Body <!-- <script> unclosed".into(),
                artifacts: vec![ArtifactFile {
                    rel_path: "index.html".into(),
                    kind: ArtifactKind::Html,
                    bytes: 42,
                }],
                at: 2,
            },
        );
        let session = Session {
            id: SessionId::new("comp-1"),
            request: "Design a dashboard".into(),
            workdir: None,
            max_rounds: Some(10),
            reveal_interval: 5,
            rev: 1,
            created_at: 1,
            judge: p(JUDGE_ID, AgentKind::Claude, Role::Judge),
            contestants: vec![p("claude", AgentKind::Claude, Role::Contestant)],
            messages: vec![],
            rounds: vec![Round {
                no: 1,
                kind: RoundKind::Blind,
                phase: Phase::Closed,
                started_at: 1,
                briefs: vec![],
                submissions: subs,
                judge_brief: None,
                outcome: None,
            }],
            winner: None,
            closed: false,
        };
        let html = render_html(&session, Path::new("/nonexistent"));
        assert!(html.contains("Noa Competition Viewer"));
        assert!(html.contains("r1/artifacts/claude/index.html"));
        assert!(!html.contains("</script><script>alert(1)"));
        // An unclosed `<!--` before `<script` would keep the page's own
        // `</script>` from ending the data script.
        assert!(!html.contains("<!-- <script"));
        assert!(html.contains(r#"setAttribute("sandbox", "allow-scripts")"#));
        assert!(!html.contains("allow-same-origin\")"));
    }

    #[test]
    fn hrefs_escape_url_reserved_characters_in_file_names() {
        let file = ArtifactFile {
            rel_path: "pages/design#1 v?2%.html".into(),
            kind: ArtifactKind::Html,
            bytes: 1,
        };
        let part = PartId::new("claude");
        assert_eq!(
            file_href(3, &part, &file.rel_path),
            "r3/artifacts/claude/pages/design%231%20v%3F2%25.html"
        );
    }
}
