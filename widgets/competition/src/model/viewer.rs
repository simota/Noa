//! Generates a self-contained HTML comparison viewer (`compare.html`) inside
//! the competition's artifact root so visual deliverables (HTML/CSS/JS designs,
//! SVG, PNG/JPG/WebP images) and code/text reports can be compared side by side
//! in the system browser across rounds.
//!
//! A judge panel's page also carries each judge's score, the mean and the
//! spread, rendered by a script block of its own ([`PANEL_SCRIPT`]); a
//! single judge's page has neither, byte for byte as before the panel.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;

use serde::Serialize;

use super::aggregate::{PanelScores, SPREAD_WARN};
use super::types::{ArtifactFile, ArtifactKind, PartId, Phase, Round, RoundKind, RoundNo, Session};
use crate::{preview, store};

#[derive(Serialize)]
struct ViewerHeader<'a> {
    id: &'a str,
    request: &'a str,
    reveal_interval: u32,
    winner: Option<&'a str>,
    contestants: Vec<&'a str>,
    /// The judge panel, lead first; absent for a single judge. Before
    /// `rounds`: the page splices the rounds into the header's last field.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    judges: Vec<ViewerJudge<'a>>,
    rounds: [(); 0],
}

#[derive(Serialize)]
struct ViewerJudge<'a> {
    id: &'a str,
    /// `claude · opus`: the agent and what it runs on.
    agent: String,
    lead: bool,
}

/// An entry's score by each judge of a panel, once every judge scored.
#[derive(Serialize)]
struct ViewerPanel<'a> {
    /// The mean to one decimal: `81.3`.
    mean: String,
    scores: Vec<ViewerJudgeScore<'a>>,
    spread: u8,
    /// The spread is [`SPREAD_WARN`] or more.
    warn: bool,
}

#[derive(Serialize)]
struct ViewerJudgeScore<'a> {
    judge: &'a str,
    score: u8,
}

impl<'a> ViewerPanel<'a> {
    fn of(panel: &'a PanelScores) -> Self {
        Self {
            mean: panel.mean_label(),
            scores: panel
                .scores
                .iter()
                .map(|(judge, score)| ViewerJudgeScore {
                    judge: judge.as_str(),
                    score: *score,
                })
                .collect(),
            spread: panel.spread(),
            warn: panel.spread() >= SPREAD_WARN,
        }
    }
}

#[derive(Serialize)]
struct ViewerRound<'a> {
    no: u32,
    kind: &'static str,
    overview: Option<&'a str>,
    ranking: Vec<&'a str>,
    final_verdict: Option<&'a str>,
    entries: Vec<ViewerEntry<'a>>,
}

#[derive(Serialize)]
struct ViewerEntry<'a> {
    part: &'a str,
    score: Option<u8>,
    strengths: &'a str,
    feedback: &'a str,
    /// "basis: claim" lines the judge scored on.
    evidence: Vec<String>,
    /// How a later round corrected this score, if it did.
    amended: Option<String>,
    /// "axis score/max" lines, when the judge split the score.
    breakdown: Vec<String>,
    summary: &'a str,
    report: &'a str,
    files: Vec<ViewerFile<'a>>,
    /// Each judge's score, the mean and the spread (panel rounds only).
    #[serde(skip_serializing_if = "Option::is_none")]
    panel: Option<ViewerPanel<'a>>,
}

#[derive(Serialize)]
struct ViewerFile<'a> {
    rel_path: &'a str,
    /// Relative URL from `compare.html` to the round snapshot file.
    href: Cow<'a, str>,
    /// What the preview frame loads instead of `href`: the generated
    /// self-contained copy of a root `index.html`, when one exists.
    frame_href: Option<Cow<'a, str>>,
    kind: &'static str,
    bytes: u64,
    /// Inline preview text for code/markdown files (truncated for large files).
    preview: Option<Cow<'a, str>>,
}

/// Cached per-snapshot file previews and serialized closed-round JSON across
/// `RefreshViewer` calls so immutable prior rounds are neither re-read from
/// disk nor re-serialized on every new submission or judgment.
///
/// [`render_html_cached`] returns exactly what [`render_html`] would only
/// while these invariants hold:
///
/// - A snapshot directory `r<N>/artifacts/<part>/` changes on disk only by
///   being rewritten whole (`store::snapshot_artifacts`), and every rewrite
///   is followed by [`ViewerCache::invalidate`] for that `(round, part)`.
///   Any other writer to a snapshot makes the cache serve stale previews.
/// - `.preview/index.html` inside a snapshot is only ever added (by
///   `preview::write`), never removed. The cache re-checks it for files
///   that lacked it, but never notices one disappearing.
/// - A closed round's JSON depends only on what `RoundStamp` records plus
///   the disk state above. After a round closes its outcome changes only
///   through an amendment, which always changes a score and the amending
///   round, both in the stamp.
///
/// Memory: `entries` holds previews only for rounds whose closed JSON is not
/// stored; storing a round's closed JSON drops that round's entries (a later
/// stamp mismatch rebuilds them from disk, as [`render_html`] always does).
/// So the cache holds at most the previews one uncached render builds, plus
/// one JSON string per closed round.
#[derive(Debug, Default)]
pub struct ViewerCache {
    entries: HashMap<(RoundNo, PartId), CachedEntry>,
    rounds: HashMap<RoundNo, CachedRound>,
}

#[derive(Debug)]
struct CachedEntry {
    artifacts: Vec<ArtifactFile>,
    files: Vec<CachedFile>,
}

#[derive(Debug)]
struct CachedFile {
    href: String,
    frame_href: Option<String>,
    preview: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct RoundStamp {
    kind: RoundKind,
    contestants: Vec<PartId>,
    submissions: Vec<(PartId, u64, usize)>,
    ranking: Vec<PartId>,
    scores: Vec<(PartId, u8, Option<RoundNo>)>,
    /// Every judge's score per entry (panel rounds): an amendment can move
    /// one without moving the rounded mean in `scores`.
    panel: Vec<(PartId, Vec<u8>)>,
    has_outcome: bool,
}

#[derive(Debug)]
struct CachedRound {
    stamp: RoundStamp,
    /// Contestants whose `index.html` lacked `.preview/index.html` when serialized.
    unframed_html_parts: Vec<PartId>,
    /// Already `<`-escaped JSON for this `ViewerRound`.
    json: String,
}

impl ViewerCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops any cached entry and serialized round for `(round, part)` (call
    /// after writing a fresh artifact snapshot for that round and contestant).
    pub fn invalidate(&mut self, round: RoundNo, part: &PartId) {
        self.entries.remove(&(round, part.clone()));
        self.rounds.remove(&round);
    }
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
    let file = std::fs::File::open(path).ok()?;
    let cap = file.metadata().ok().map_or(0, |m| {
        m.len().min(MAX_INLINE_TEXT_BYTES as u64 + 1) as usize
    });
    // +1 avoids a reallocation when `read_to_end` probes for EOF.
    let mut bytes = Vec::with_capacity(cap + 1);
    file.take(MAX_INLINE_TEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() > MAX_INLINE_TEXT_BYTES;
    let slice = if truncated {
        &bytes[..MAX_INLINE_TEXT_BYTES]
    } else {
        &bytes
    };
    let text = String::from_utf8_lossy(slice);
    if truncated {
        Some(format!("{text}\n\n… (truncated)"))
    } else {
        Some(text.into_owned())
    }
}

/// The URL of a round snapshot file, relative to `compare.html`.
fn file_href(round: RoundNo, part: &PartId, rel_path: &str) -> String {
    let mut href = format!("r{round}/artifacts/{part}/");
    push_url_path(&mut href, rel_path);
    href
}

/// Appends `path` to `url`, percent-encoding every byte but unreserved ones
/// and `/`.
fn push_url_path(url: &mut String, path: &str) {
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            url.push(byte as char);
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
}

/// [`render_html`] for a page written outside `artifact_root`: a
/// `<base href>` makes its links, frames and images, all relative to
/// `compare.html`, resolve under `artifact_root`.
pub fn render_html_elsewhere(session: &Session, artifact_root: &Path) -> String {
    let html = render_html(session, artifact_root);
    let mut base = String::from("<base href=\"file://");
    push_url_path(&mut base, &artifact_root.to_string_lossy());
    if !base.ends_with('/') {
        base.push('/');
    }
    base.push_str("\">\n");
    match html.split_once("<head>\n") {
        Some((head, rest)) => format!("{head}<head>\n{base}{rest}"),
        None => html,
    }
}

fn frame_href_for(
    round: RoundNo,
    part: &PartId,
    snap_dir: &Path,
    rel_path: &str,
) -> Option<String> {
    (rel_path == preview::ENTRY_PAGE && preview::preview_path(snap_dir).is_file()).then(|| {
        file_href(
            round,
            part,
            &format!("{}/{}", preview::PREVIEW_DIR, preview::ENTRY_PAGE),
        )
    })
}

fn build_cached_files(
    artifact_root: &Path,
    round: RoundNo,
    part: &PartId,
    artifacts: &[ArtifactFile],
) -> Vec<CachedFile> {
    if artifacts.is_empty() {
        return Vec::new();
    }
    let snap_dir = store::artifact_snapshot_dir(artifact_root, round, part);
    artifacts
        .iter()
        .map(|f| {
            let href = file_href(round, part, &f.rel_path);
            let preview = match f.kind {
                ArtifactKind::Code | ArtifactKind::Markdown => {
                    read_text_preview(&snap_dir.join(&f.rel_path))
                }
                _ => None,
            };
            let frame_href = frame_href_for(round, part, &snap_dir, &f.rel_path);
            CachedFile {
                href,
                frame_href,
                preview,
            }
        })
        .collect()
}

/// Appends `json` into `out`, escaping every `<` as `\u003c` so embedded JSON
/// can neither close the `<script>` tag (`</script>`) nor open a comment that
/// keeps it from closing (`<!--`).
fn push_escaped_lt(out: &mut String, mut json: &str) {
    while let Some(at) = json.find('<') {
        out.push_str(&json[..at]);
        out.push_str("\\u003c");
        json = &json[at + 1..];
    }
    out.push_str(json);
}

fn escape_lt_owned(json: String) -> String {
    if !json.contains('<') {
        return json;
    }
    let mut out = String::with_capacity(json.len() + 64);
    push_escaped_lt(&mut out, &json);
    out
}

fn round_stamp(session: &Session, r: &Round) -> RoundStamp {
    let (ranking, scores, has_outcome) = match &r.outcome {
        Some(o) => (
            o.ranking.clone(),
            o.evaluations
                .iter()
                .map(|(p, e)| (p.clone(), e.score, e.amended.as_ref().map(|a| a.by_round)))
                .collect(),
            true,
        ),
        None => (Vec::new(), Vec::new(), false),
    };
    RoundStamp {
        kind: r.kind,
        contestants: session.contestant_ids().cloned().collect(),
        submissions: r
            .submissions
            .iter()
            .map(|(p, s)| (p.clone(), s.at, s.artifacts.len()))
            .collect(),
        ranking,
        scores,
        panel: session
            .contestant_ids()
            .filter_map(|part| {
                let panel = session.panel_scores(r.no, part)?;
                Some((part.clone(), panel.scores.iter().map(|(_, s)| *s).collect()))
            })
            .collect(),
        has_outcome,
    }
}

fn build_viewer_round<'a>(
    session: &'a Session,
    r: &'a Round,
    artifact_root: &Path,
    cache: Option<&'a ViewerCache>,
    panels: &'a [Option<PanelScores>],
) -> ViewerRound<'a> {
    let entries = session
        .contestant_ids()
        .zip(panels)
        .map(|(part, panel)| {
            let ev = r.outcome.as_ref().and_then(|o| o.evaluations.get(part));
            let sub = r.submissions.get(part);
            let files: Vec<ViewerFile<'a>> = match sub {
                Some(s) if !s.artifacts.is_empty() => {
                    if let Some(cached) = cache
                        .and_then(|c| c.entries.get(&(r.no, part.clone())))
                        .filter(|c| c.files.len() == s.artifacts.len())
                    {
                        s.artifacts
                            .iter()
                            .zip(&cached.files)
                            .map(|(f, cf)| ViewerFile {
                                rel_path: f.rel_path.as_str(),
                                href: Cow::Borrowed(cf.href.as_str()),
                                frame_href: cf.frame_href.as_deref().map(Cow::Borrowed),
                                kind: artifact_kind_tag(f.kind),
                                bytes: f.bytes,
                                preview: cf.preview.as_deref().map(Cow::Borrowed),
                            })
                            .collect()
                    } else {
                        let snap_dir = store::artifact_snapshot_dir(artifact_root, r.no, part);
                        s.artifacts
                            .iter()
                            .map(|f| {
                                let href = file_href(r.no, part, &f.rel_path);
                                let preview = match f.kind {
                                    ArtifactKind::Code | ArtifactKind::Markdown => {
                                        read_text_preview(&snap_dir.join(&f.rel_path))
                                    }
                                    _ => None,
                                };
                                let frame_href = frame_href_for(r.no, part, &snap_dir, &f.rel_path);
                                ViewerFile {
                                    rel_path: f.rel_path.as_str(),
                                    href: Cow::Owned(href),
                                    frame_href: frame_href.map(Cow::Owned),
                                    kind: artifact_kind_tag(f.kind),
                                    bytes: f.bytes,
                                    preview: preview.map(Cow::Owned),
                                }
                            })
                            .collect()
                    }
                }
                _ => Vec::new(),
            };
            ViewerEntry {
                part: part.as_str(),
                score: ev.map(|e| e.score),
                strengths: ev.map_or("", |e| e.strengths.as_str()),
                feedback: ev.map_or("", |e| e.feedback.as_str()),
                evidence: ev
                    .map(|e| {
                        e.evidence
                            .iter()
                            .map(|item| format!("{}: {}", item.basis.as_str(), item.claim))
                            .collect()
                    })
                    .unwrap_or_default(),
                breakdown: ev
                    .map(|e| {
                        e.breakdown
                            .iter()
                            .map(|a| format!("{} {}/{}", a.axis, a.score, a.max))
                            .collect()
                    })
                    .unwrap_or_default(),
                amended: ev.and_then(|e| e.amended.as_ref()).map(|note| {
                    format!(
                        "R{} で {} → 訂正: {}",
                        note.by_round, note.from, note.reason
                    )
                }),
                summary: sub.map_or("", |s| s.summary.as_str()),
                report: sub.map_or("", |s| s.body.as_str()),
                files,
                panel: panel.as_ref().map(ViewerPanel::of),
            }
        })
        .collect();
    ViewerRound {
        no: r.no,
        kind: match r.kind {
            RoundKind::Blind => "blind",
            RoundKind::Reveal => "reveal",
        },
        overview: r.outcome.as_ref().map(|o| o.overview.as_str()),
        ranking: r
            .outcome
            .as_ref()
            .map(|o| o.ranking.iter().map(PartId::as_str).collect())
            .unwrap_or_default(),
        final_verdict: r.outcome.as_ref().and_then(|o| o.final_verdict.as_deref()),
        entries,
    }
}

fn assemble_html<'a>(
    session: &Session,
    round_jsons: impl ExactSizeIterator<Item = &'a str> + Clone,
) -> String {
    let header = ViewerHeader {
        id: session.id.as_str(),
        request: session.request.as_str(),
        reveal_interval: session.reveal_interval,
        winner: session.winner.as_ref().map(PartId::as_str),
        contestants: session.contestant_ids().map(PartId::as_str).collect(),
        judges: if session.is_panel() {
            session
                .judges()
                .enumerate()
                .map(|(seat, judge)| ViewerJudge {
                    id: judge.id.as_str(),
                    agent: match judge.setup.label() {
                        Some(setup) => format!("{} · {setup}", judge.kind.command()),
                        None => judge.kind.command().to_owned(),
                    },
                    lead: seat == 0,
                })
                .collect()
        } else {
            Vec::new()
        },
        rounds: [],
    };
    let header_json = serde_json::to_string(&header).unwrap_or_else(|_| "{}".to_owned());
    let rounds_bytes: usize = round_jsons.clone().map(|s| s.len() + 1).sum();
    let mut html = String::with_capacity(
        HTML_PREFIX.len() + HTML_SUFFIX.len() + header_json.len() + rounds_bytes + 64,
    );
    html.push_str(HTML_PREFIX);
    if let Some(prefix) = header_json.strip_suffix("]}") {
        push_escaped_lt(&mut html, prefix);
        for (i, rj) in round_jsons.enumerate() {
            if i > 0 {
                html.push(',');
            }
            html.push_str(rj);
        }
        html.push_str("]}");
    } else {
        push_escaped_lt(&mut html, &header_json);
    }
    if session.is_panel() {
        let body_end = HTML_SUFFIX
            .strip_suffix(HTML_END)
            .expect("the page ends with </body></html>");
        html.push_str(body_end);
        html.push_str(PANEL_SCRIPT);
        html.push_str(HTML_END);
    } else {
        html.push_str(HTML_SUFFIX);
    }
    html
}

/// Every contestant's panel scores in round `r`, in contestant order
/// (`None` each for a single judge or a round not yet combined).
fn round_panels(session: &Session, r: &Round) -> Vec<Option<PanelScores>> {
    session
        .contestant_ids()
        .map(|part| session.panel_scores(r.no, part))
        .collect()
}

/// Build the viewer's HTML document for `session`, referencing artifact
/// snapshots under `artifact_root` (where `compare.html` will be written).
pub fn render_html(session: &Session, artifact_root: &Path) -> String {
    let round_jsons: Vec<String> = session
        .rounds
        .iter()
        .map(|r| {
            let panels = round_panels(session, r);
            let vr = build_viewer_round(session, r, artifact_root, None, &panels);
            escape_lt_owned(serde_json::to_string(&vr).unwrap_or_else(|_| "{}".to_owned()))
        })
        .collect();
    assemble_html(session, round_jsons.iter().map(String::as_str))
}

/// Like [`render_html`], reusing cached artifact previews and serialized
/// closed-round JSON across refreshes.
pub fn render_html_cached(
    session: &Session,
    artifact_root: &Path,
    cache: &mut ViewerCache,
) -> String {
    let mut ephemeral: Vec<Option<String>> = Vec::with_capacity(session.rounds.len());
    for r in &session.rounds {
        let stamp = (r.phase == Phase::Closed).then(|| round_stamp(session, r));
        let round_hit = stamp.as_ref().is_some_and(|st| {
            cache.rounds.get(&r.no).is_some_and(|cr| {
                cr.stamp == *st
                    && !cr.unframed_html_parts.iter().any(|part| {
                        preview::preview_path(&store::artifact_snapshot_dir(
                            artifact_root,
                            r.no,
                            part,
                        ))
                        .is_file()
                    })
            })
        });
        if round_hit {
            ephemeral.push(None);
            continue;
        }
        let mut unframed_html_parts = Vec::new();
        for part in session.contestant_ids() {
            let Some(sub) = r.submissions.get(part) else {
                continue;
            };
            let key = (r.no, part.clone());
            let needs_build = cache
                .entries
                .get(&key)
                .is_none_or(|c| c.artifacts != sub.artifacts);
            if needs_build {
                let files = build_cached_files(artifact_root, r.no, part, &sub.artifacts);
                cache.entries.insert(
                    key.clone(),
                    CachedEntry {
                        artifacts: sub.artifacts.clone(),
                        files,
                    },
                );
            } else if let Some(entry) = cache.entries.get_mut(&key) {
                let mut snap_dir = None;
                for (art, cached) in sub.artifacts.iter().zip(&mut entry.files) {
                    if art.rel_path == preview::ENTRY_PAGE && cached.frame_href.is_none() {
                        let dir = snap_dir.get_or_insert_with(|| {
                            store::artifact_snapshot_dir(artifact_root, r.no, part)
                        });
                        cached.frame_href = frame_href_for(r.no, part, dir, &art.rel_path);
                    }
                }
            }
            if let Some(entry) = cache.entries.get(&key)
                && sub
                    .artifacts
                    .iter()
                    .zip(&entry.files)
                    .any(|(art, cf)| art.rel_path == preview::ENTRY_PAGE && cf.frame_href.is_none())
            {
                unframed_html_parts.push(part.clone());
            }
        }
        let panels = round_panels(session, r);
        let vr = build_viewer_round(session, r, artifact_root, Some(cache), &panels);
        let json = escape_lt_owned(serde_json::to_string(&vr).unwrap_or_else(|_| "{}".to_owned()));
        if let Some(stamp) = stamp {
            // The round's previews now live in `json`; a stamp mismatch later
            // rebuilds them from disk.
            cache.entries.retain(|(no, _), _| *no != r.no);
            let mut json = json;
            json.shrink_to_fit();
            cache.rounds.insert(
                r.no,
                CachedRound {
                    stamp,
                    unframed_html_parts,
                    json,
                },
            );
            ephemeral.push(None);
        } else {
            ephemeral.push(Some(json));
        }
    }

    let slices: Vec<&str> = session
        .rounds
        .iter()
        .zip(&ephemeral)
        .map(|(r, eph)| match eph {
            Some(s) => s.as_str(),
            None => cache.rounds[&r.no].json.as_str(),
        })
        .collect();
    assemble_html(session, slices.into_iter())
}

const HTML_PREFIX: &str = r##"<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Noa Competition Viewer</title>
<style>
  :root {
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
  }
  * { box-sizing: border-box; }
  body {
    margin: 0;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    background: var(--bg);
    color: var(--text);
  }
  header {
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
  }
  .title-block h1 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }
  .title-block p {
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--muted);
    max-width: 720px;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  select, button {
    background: var(--surface-alt);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 10px;
    font-size: 12px;
    cursor: pointer;
  }
  button.active {
    border-color: var(--accent);
    color: var(--accent);
  }
  .banner {
    margin: 12px 18px 0;
    padding: 10px 14px;
    border-radius: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    font-size: 13px;
    line-height: 1.5;
  }
  .badge {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    margin-right: 6px;
  }
  .badge.blind { background: #262d3d; color: var(--muted); }
  .badge.reveal { background: rgba(167, 139, 250, 0.18); color: var(--reveal); }
  .badge.winner { background: rgba(245, 196, 81, 0.18); color: var(--gold); }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(420px, 1fr));
    gap: 14px;
    padding: 14px 18px 32px;
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .card.winner-card {
    border-color: var(--gold);
  }
  .card-head {
    padding: 12px 14px;
    border-bottom: 1px solid var(--border);
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }
  .score {
    font-size: 18px;
    font-weight: 700;
    color: var(--accent);
  }
  .file-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 8px 14px;
    background: var(--surface-alt);
    border-bottom: 1px solid var(--border);
  }
  .preview-box {
    background: #0b0d12;
    min-height: 360px;
    display: flex;
    /* `safe`: a frame wider than the box scrolls instead of clipping left. */
    align-items: safe center;
    justify-content: safe center;
    overflow: auto;
    position: relative;
  }
  .frame-note {
    position: absolute;
    right: 8px;
    bottom: 6px;
    font-size: 11px;
    color: var(--muted);
  }
  .preview-box iframe {
    /* Keep a fixed viewport's size: a flex item would shrink to the card. */
    flex: none;
    border: 0;
    background: #fff;
    height: 460px;
    transition: width 0.15s ease;
  }
  .preview-box img {
    max-width: 100%;
    max-height: 460px;
    object-fit: contain;
    cursor: zoom-in;
  }
  .preview-box pre {
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
  }
  .meta {
    padding: 12px 14px;
    font-size: 12px;
    line-height: 1.55;
    border-top: 1px solid var(--border);
  }
  .meta h4 {
    margin: 8px 0 4px;
    font-size: 11px;
    text-transform: uppercase;
    color: var(--muted);
    letter-spacing: 0.04em;
  }
  .meta h4:first-child { margin-top: 0; }
  .report-body {
    white-space: pre-wrap;
    max-height: 220px;
    overflow: auto;
    background: var(--surface-alt);
    padding: 8px 10px;
    border-radius: 6px;
  }
  .modal {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.85);
    display: none;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 24px;
  }
  .modal.open { display: flex; }
  .modal img { max-width: 95vw; max-height: 92vh; object-fit: contain; }
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
    <button data-vp="100%" class="active">幅いっぱい</button>
    <button data-vp="1440x900">1440×900</button>
    <button data-vp="768x1024">768×1024</button>
    <button data-vp="320x568">320×568</button>
  </div>
</header>
<div id="banner" class="banner"></div>
<main id="grid" class="grid"></main>
<div id="modal" class="modal"><img id="modalImg" alt="zoom"></div>
<script>
const DATA = "##;

const HTML_SUFFIX: &str = r##";
let viewportWidth = "100%";
let viewportHeight = "460px";
// The sizes the judge's screenshots use: width × height, height = first view.
function applyViewport(frame) {
  frame.style.width = viewportWidth;
  frame.style.height = viewportHeight;
}
const selectedFile = {};

const roundSelect = document.getElementById("roundSelect");
const banner = document.getElementById("banner");
const grid = document.getElementById("grid");
const modal = document.getElementById("modal");
const modalImg = document.getElementById("modalImg");
modal.addEventListener("click", () => modal.classList.remove("open"));

document.getElementById("request").textContent = DATA.request || "";
document.querySelectorAll("button[data-vp]").forEach(btn => {
  btn.addEventListener("click", () => {
    const [w, h] = btn.dataset.vp.split("x");
    viewportWidth = h ? w + "px" : w;
    viewportHeight = h ? h + "px" : "460px";
    document.querySelectorAll("button[data-vp]").forEach(b => b.classList.toggle("active", b === btn));
    document.querySelectorAll(".preview-box iframe").forEach(applyViewport);
  });
});

function init() {
  roundSelect.innerHTML = "";
  if (!DATA.rounds.length) {
    banner.textContent = "まだ提出されたラウンドがありません。";
    return;
  }
  DATA.rounds.forEach((r, idx) => {
    const opt = document.createElement("option");
    opt.value = idx;
    const tag = r.kind === "reveal" ? " [相互閲覧]" : " [ブラインド]";
    opt.textContent = "Round " + r.no + tag;
    roundSelect.appendChild(opt);
  });
  roundSelect.value = String(DATA.rounds.length - 1);
  roundSelect.addEventListener("change", () => renderRound(Number(roundSelect.value)));
  renderRound(DATA.rounds.length - 1);
}

function pickDefaultFile(entry) {
  const key = entry.part;
  if (selectedFile[key] && entry.files.some(f => f.rel_path === selectedFile[key])) {
    return selectedFile[key];
  }
  const visual = entry.files.find(f => f.rel_path === "index.html")
    || entry.files.find(f => f.kind === "html")
    || entry.files.find(f => f.kind === "image" || f.kind === "svg")
    || entry.files[0];
  return visual ? visual.rel_path : null;
}

function renderPreview(box, file) {
  box.innerHTML = "";
  if (!file) {
    const p = document.createElement("p");
    p.style.color = "var(--muted)";
    p.style.fontSize = "12px";
    p.textContent = "成果物ファイルなし";
    box.appendChild(p);
    return;
  }
  if (file.kind === "html") {
    const iframe = document.createElement("iframe");
    iframe.src = file.frame_href || file.href;
    applyViewport(iframe);
    // No allow-same-origin: with it, an entry's scripts could reach this
    // page (`parent.document`) and rewrite scores or other entries.
    iframe.setAttribute("sandbox", "allow-scripts");
    box.appendChild(iframe);
    if (file.frame_href) {
      const note = document.createElement("a");
      note.className = "frame-note";
      note.href = file.href;
      note.target = "_blank";
      note.textContent = "自動インライン版を表示中 · 元の index.html";
      box.appendChild(note);
    }
  } else if (file.kind === "image" || file.kind === "svg") {
    const img = document.createElement("img");
    img.src = file.href;
    img.alt = file.rel_path;
    img.addEventListener("click", () => {
      modalImg.src = file.href;
      modal.classList.add("open");
    });
    box.appendChild(img);
  } else if (file.preview !== null && file.preview !== undefined) {
    const pre = document.createElement("pre");
    pre.textContent = file.preview;
    box.appendChild(pre);
  } else {
    const a = document.createElement("a");
    a.href = file.href;
    a.target = "_blank";
    a.style.color = "var(--accent)";
    a.textContent = file.rel_path + " を開く";
    box.appendChild(a);
  }
}

function renderRound(idx) {
  const r = DATA.rounds[idx];
  if (!r) return;
  banner.innerHTML = "";
  const badge = document.createElement("span");
  badge.className = "badge " + r.kind;
  badge.textContent = r.kind === "reveal" ? "👁 相互閲覧ラウンド" : "🔒 ブラインドラウンド";
  banner.appendChild(badge);
  if (DATA.winner) {
    const w = document.createElement("span");
    w.className = "badge winner";
    w.textContent = "🏆 優勝: " + DATA.winner;
    banner.appendChild(w);
  }
  const txt = document.createElement("span");
  txt.textContent = r.final_verdict || r.overview || "審判の採点待ちです。";
  banner.appendChild(txt);

  grid.innerHTML = "";
  const order = r.ranking.length ? r.ranking : DATA.contestants;
  order.forEach((part, rankIdx) => {
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
    entry.files.forEach(file => {
      const b = document.createElement("button");
      b.textContent = file.rel_path;
      if (file.rel_path === activeRel) b.classList.add("active");
      b.addEventListener("click", () => {
        selectedFile[part] = file.rel_path;
        tabs.querySelectorAll("button").forEach(x => x.classList.toggle("active", x === b));
        renderPreview(previewBox, file);
      });
      tabs.appendChild(b);
    });
    if (entry.files.length) {
      const activeFile = entry.files.find(f => f.rel_path === activeRel) || entry.files[0];
      renderPreview(previewBox, activeFile);
    } else {
      renderPreview(previewBox, null);
    }
    card.appendChild(tabs);
    card.appendChild(previewBox);

    const meta = document.createElement("div");
    meta.className = "meta";
    const addBlock = (label, content, isPre) => {
      if (!content) return;
      const h = document.createElement("h4");
      h.textContent = label;
      const d = document.createElement("div");
      if (isPre) d.className = "report-body";
      d.textContent = content;
      meta.appendChild(h);
      meta.appendChild(d);
    };
    addBlock("サマリー", entry.summary, false);
    addBlock("評価された点", entry.strengths, false);
    addBlock("審判フィードバック", entry.feedback, false);
    addBlock("観点別の内訳", entry.breakdown.join(" · "), false);
    addBlock("スコア訂正", entry.amended, false);
    addBlock("採点の根拠", entry.evidence.join("\n"), true);
    addBlock("提出レポート", entry.report, true);
    card.appendChild(meta);

    grid.appendChild(card);
  });
}

init();
</script>
</body>
</html>
"##;

/// How the page ends; [`PANEL_SCRIPT`] goes right before it.
const HTML_END: &str = "</body>\n</html>\n";

/// A panel page's own script, after the shared one: it lists the judges
/// under the request and wraps `renderRound` so each card shows the mean,
/// every judge's score and the spread (highlighted from `SPREAD_WARN`).
const PANEL_SCRIPT: &str = r##"<style>
  .panel-scores {
    padding: 8px 14px;
    font-size: 12px;
    border-bottom: 1px solid var(--border);
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
  }
  .panel-scores .label { color: var(--muted); }
  .panel-scores .spread.warn { color: var(--gold); font-weight: 700; }
  .judges { margin: 4px 0 0; font-size: 12px; color: var(--muted); }
</style>
<script>
(function () {
  const judges = document.createElement("p");
  judges.className = "judges";
  judges.textContent = "審判: " + DATA.judges
    .map(j => j.id + " (" + j.agent + (j.lead ? ", リード" : "") + ")")
    .join(" · ");
  document.querySelector(".title-block").appendChild(judges);

  const renderShared = renderRound;
  renderRound = function (idx) {
    renderShared(idx);
    const r = DATA.rounds[idx];
    if (!r) return;
    // The cards, in the order the shared script made them.
    const order = (r.ranking.length ? r.ranking : DATA.contestants)
      .filter(part => r.entries.some(e => e.part === part));
    const cards = grid.querySelectorAll("section.card");
    order.forEach((part, i) => {
      const entry = r.entries.find(e => e.part === part);
      const card = cards[i];
      if (!card || !entry.panel) return;
      card.querySelector(".score").textContent = entry.panel.mean + " / 100";
      const block = document.createElement("div");
      block.className = "panel-scores";
      const scores = document.createElement("span");
      const label = document.createElement("span");
      label.className = "label";
      label.textContent = "審判別スコア ";
      scores.appendChild(label);
      scores.appendChild(document.createTextNode(
        entry.panel.scores.map(s => s.judge + " " + s.score).join(" · ")));
      const spread = document.createElement("span");
      spread.className = "spread" + (entry.panel.warn ? " warn" : "");
      spread.textContent = "審判間の差 " + entry.panel.spread;
      block.appendChild(scores);
      block.appendChild(spread);
      card.insertBefore(block, card.querySelector(".file-tabs"));
    });
  };
  renderRound(Number(roundSelect.value));
})();
</script>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{
        AgentKind, ArtifactFile, JUDGE_ID, PartId, Participant, Phase, Role, Round, SessionId,
        Submission,
    };
    use std::collections::BTreeMap;

    #[test]
    fn a_fixed_viewport_frame_does_not_shrink_to_the_card() {
        let html = render_html(
            &Session {
                id: SessionId::new("comp-1"),
                request: String::new(),
                workdir: None,
                max_rounds: None,
                reveal_interval: 5,
                rev: 1,
                created_at: 1,
                judge: Participant {
                    id: PartId::new(JUDGE_ID),
                    kind: AgentKind::Claude,
                    role: Role::Judge,
                    pane_id: None,
                    setup: Default::default(),
                },
                co_judges: Vec::new(),
                contestants: vec![],
                messages: vec![],
                rounds: vec![],
                winner: None,
                closed: false,
                hold: false,
            },
            Path::new("/nonexistent"),
        );
        let start = html.find(".preview-box iframe {").unwrap();
        let rule = &html[start..start + html[start..].find('}').unwrap()];
        assert!(rule.contains("flex: none"), "{rule}");
    }

    #[test]
    fn viewer_html_embeds_rounds_and_escapes_script_closing_tags() {
        let p = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
            setup: Default::default(),
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
            co_judges: Vec::new(),
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
                co_judge_briefs: Vec::new(),
                scorecards: BTreeMap::new(),
                verdict_brief: None,
                outcome: None,
            }],
            winner: None,
            closed: false,
            hold: false,
        };
        let html = render_html(&session, Path::new("/nonexistent"));
        assert!(
            html.contains("\"frame_href\":null"),
            "no preview, no frame copy"
        );

        // With a generated preview, the frame loads it instead of the raw page.
        let root = std::env::temp_dir().join(format!("competition-viewer-{}", std::process::id()));
        let preview = crate::preview::preview_path(&store::artifact_snapshot_dir(
            &root,
            1,
            &PartId::new("claude"),
        ));
        std::fs::create_dir_all(preview.parent().unwrap()).unwrap();
        std::fs::write(&preview, "<p>inlined</p>").unwrap();
        let framed = render_html(&session, &root);
        assert!(framed.contains("\"frame_href\":\"r1/artifacts/claude/.preview/index.html\""));
        let _ = std::fs::remove_dir_all(&root);

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

    #[test]
    fn text_preview_truncation_matches_exact_boundary_including_utf8() {
        let dir =
            std::env::temp_dir().join(format!("competition-viewer-trunc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let exact_path = dir.join("exact.txt");
        let exact_bytes = vec![b'a'; MAX_INLINE_TEXT_BYTES];
        std::fs::write(&exact_path, &exact_bytes).unwrap();
        let exact = read_text_preview(&exact_path).unwrap();
        assert_eq!(exact.len(), MAX_INLINE_TEXT_BYTES);
        assert!(!exact.ends_with("… (truncated)"));

        let over_path = dir.join("over.txt");
        let mut over_bytes = vec![b'a'; MAX_INLINE_TEXT_BYTES - 1];
        // 3-byte UTF-8 char straddling the 32,768-byte cutoff.
        over_bytes.extend_from_slice("あ".as_bytes());
        std::fs::write(&over_path, &over_bytes).unwrap();
        let over = read_text_preview(&over_path).unwrap();
        assert!(over.ends_with("\n\n… (truncated)"));
        assert!(over.contains('\u{FFFD}'));
        let expected_prefix = String::from_utf8_lossy(&over_bytes[..MAX_INLINE_TEXT_BYTES]);
        assert_eq!(over, format!("{expected_prefix}\n\n… (truncated)"));

        assert_eq!(read_text_preview(&dir.join("missing.txt")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cached_viewer_render_matches_uncached_across_updates_and_invalidation() {
        let root =
            std::env::temp_dir().join(format!("competition-viewer-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let claude = PartId::new("claude");
        let snap1 = store::artifact_snapshot_dir(&root, 1, &claude);
        std::fs::create_dir_all(&snap1).unwrap();
        std::fs::write(snap1.join("index.html"), "<h1>v1</h1>").unwrap();
        std::fs::write(snap1.join("app.js"), "const a = 1;").unwrap();

        let p = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
            setup: Default::default(),
        };
        let mut subs = BTreeMap::new();
        subs.insert(
            claude.clone(),
            Submission {
                part: claude.clone(),
                summary: "v1".into(),
                body: "report <script>".into(),
                artifacts: vec![
                    ArtifactFile {
                        rel_path: "index.html".into(),
                        kind: ArtifactKind::Html,
                        bytes: 11,
                    },
                    ArtifactFile {
                        rel_path: "app.js".into(),
                        kind: ArtifactKind::Code,
                        bytes: 12,
                    },
                ],
                at: 2,
            },
        );
        let session = Session {
            id: SessionId::new("comp-cache"),
            request: "req".into(),
            workdir: None,
            max_rounds: Some(3),
            reveal_interval: 5,
            rev: 1,
            created_at: 1,
            judge: p(JUDGE_ID, AgentKind::Claude, Role::Judge),
            co_judges: Vec::new(),
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
                co_judge_briefs: Vec::new(),
                scorecards: BTreeMap::new(),
                verdict_brief: None,
                outcome: None,
            }],
            winner: None,
            closed: false,
            hold: false,
        };

        let mut cache = ViewerCache::new();
        assert_eq!(
            render_html_cached(&session, &root, &mut cache),
            render_html(&session, &root)
        );
        // When `.preview/index.html` appears later, cached render picks it up.
        let prev = preview::preview_path(&snap1);
        std::fs::create_dir_all(prev.parent().unwrap()).unwrap();
        std::fs::write(&prev, "<h1>v1-inlined</h1>").unwrap();
        assert_eq!(
            render_html_cached(&session, &root, &mut cache),
            render_html(&session, &root)
        );
        // After snapshot overwrite + invalidate, cached render picks up new file text.
        std::fs::write(snap1.join("app.js"), "const a = 2;").unwrap();
        cache.invalidate(1, &claude);
        let after = render_html_cached(&session, &root, &mut cache);
        assert_eq!(after, render_html(&session, &root));
        assert!(after.contains("const a = 2;"));

        // An amendment to a closed round invalidates its cached JSON automatically.
        let mut amended = session;
        let mut evals = BTreeMap::new();
        evals.insert(
            claude.clone(),
            super::super::types::EntryEvaluation {
                score: 91,
                strengths: "s".into(),
                feedback: "f".into(),
                evidence: vec![],
                amended: Some(super::super::types::AmendNote {
                    from: 80,
                    by_round: 2,
                    reason: "corrected".into(),
                }),
                breakdown: vec![],
            },
        );
        amended.rounds[0].outcome = Some(super::super::types::JudgeOutput {
            overview: "ov".into(),
            evaluations: evals,
            ranking: vec![claude.clone()],
            winner: None,
            final_verdict: None,
            close: false,
            amendments: vec![],
            checklist: vec![],
        });
        let after_amend = render_html_cached(&amended, &root, &mut cache);
        assert_eq!(after_amend, render_html(&amended, &root));
        assert!(after_amend.contains("91"));
        assert!(after_amend.contains("corrected"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A closed round's cached JSON is reused only while its stamp holds:
    /// a judge's score that moves without moving the rounded mean must
    /// still change the stamp, or the page would show the old panel.
    #[test]
    fn the_round_stamp_follows_each_judges_score() {
        use crate::model::aggregate::tests::{cards, panel_session, settle};
        use crate::model::types::judge_id;
        let mut session = panel_session(Some(3));
        let all = cards(
            &[("codex", 85), ("claude", 80)],
            &[("claude", 83), ("codex", 80)],
            &[("codex", 88), ("claude", 81)],
        );
        settle(&mut session, 1, &all).unwrap();
        session.rounds[0].phase = Phase::Closed;
        let before = round_stamp(&session, &session.rounds[0]);
        assert_eq!(before.panel.len(), 2);
        // judge-2's claude 83 → 82: the mean 81.3 → 81.0 still rounds to 81.
        let claude = PartId::new("claude");
        let card = session.rounds[0].scorecards.get_mut(&judge_id(1)).unwrap();
        card.evaluations.get_mut(&claude).unwrap().score = 82;
        let after = round_stamp(&session, &session.rounds[0]);
        assert_eq!(before.scores, after.scores);
        assert_ne!(before, after);
        let root = std::env::temp_dir();
        let mut cache = ViewerCache::new();
        let _ = render_html_cached(&session, &root, &mut cache);
        assert_eq!(
            render_html_cached(&session, &root, &mut cache),
            render_html(&session, &root)
        );
        assert!(render_html(&session, &root).contains("\"mean\":\"81.0\""));
    }
}
