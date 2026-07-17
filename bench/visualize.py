#!/usr/bin/env python3
"""Render a results directory (results.json) into a self-contained HTML report.

stdlib only; inline CSS + inline SVG bar charts; zero external requests (works
offline from file://). One section per axis THAT HAS DATA — subset runs (a
single axis, a single terminal) are common, so absent/all-UNMEASURED axes are
skipped entirely rather than drawn as empty. noa's bar is accented; other
terminals are neutral gray. Renders only what is in the given results files —
no published/competitor figures are baked in.
"""
import json
import math
import os
import sys

NOA_ACCENT = "#34d399"   # noa's bar / highlight
BAR_NEUTRAL = "#9ca3af"  # every other terminal
UNMEAS_FG = "#6b7280"    # subtle "unmeasured" footnotes

# fixed terminal display order (matches the harness roster); unknown names append
TERM_ORDER = ["noa", "ghostty", "termy", "kitty", "alacritty",
              "iterm2", "warp", "terminal", "rio"]


def order_terms(terms):
    known = [t for t in TERM_ORDER if t in terms]
    extra = sorted(t for t in terms if t not in TERM_ORDER)
    return known + extra


def esc(s):
    return (str(s).replace("&", "&amp;").replace("<", "&lt;")
            .replace(">", "&gt;").replace('"', "&quot;"))


# ── SVG horizontal-bar chart ───────────────────────────────────────
# rows: list of dicts {name, value (float>0), label (str), noa (bool)}.
# `unmeasured`: list of terminal names to list subtly beneath the chart.
def svg_hbars(rows, higher_better, log=False, unmeasured=None):
    unmeasured = unmeasured or []
    if not rows:
        return ""
    label_w, bar_w, row_h, pad = 96, 300, 26, 6
    val_w = 150
    width = label_w + bar_w + val_w
    height = len(rows) * row_h + 2 * pad
    vals = [r["value"] for r in rows]
    maxv = max(vals) if vals else 1.0

    def frac(v):
        if v <= 0:
            return 0.0
        if log:
            return math.log10(max(v, 1.0)) / math.log10(max(maxv, 10.0))
        return v / maxv if maxv > 0 else 0.0

    parts = [f'<svg viewBox="0 0 {width} {height}" width="{width}" '
             f'height="{height}" role="img" xmlns="http://www.w3.org/2000/svg">']
    for i, r in enumerate(rows):
        y = pad + i * row_h
        cy = y + row_h / 2
        w = max(2.0, frac(r["value"]) * bar_w)
        color = NOA_ACCENT if r["noa"] else BAR_NEUTRAL
        weight = "700" if r["noa"] else "400"
        parts.append(
            f'<text x="{label_w - 8}" y="{cy + 4}" text-anchor="end" '
            f'font-size="12" font-weight="{weight}" '
            f'fill="{"#e5e7eb" if not r["noa"] else NOA_ACCENT}">{esc(r["name"])}</text>')
        parts.append(
            f'<rect x="{label_w}" y="{y + 4}" width="{w:.1f}" '
            f'height="{row_h - 8}" rx="2" fill="{color}"/>')
        parts.append(
            f'<text x="{label_w + w + 6:.1f}" y="{cy + 4}" font-size="11" '
            f'fill="#cbd5e1">{esc(r["label"])}</text>')
    parts.append("</svg>")
    note = "higher is better" if higher_better else "lower is better"
    if log:
        note += " · log-scaled bar length"
    html = [f'<div class="chart">{"".join(parts)}</div>',
            f'<div class="dir">{esc(note)}</div>']
    if unmeasured:
        html.append('<div class="unmeas">unmeasured: '
                    + ", ".join(esc(t) for t in unmeasured) + "</div>")
    return "\n".join(html)


def cell_measured(c):
    return isinstance(c, dict) and c.get("status") != "UNMEASURED"


# ── per-axis section builders. Each returns HTML or "" (skip if no data). ──
def sec_throughput(axis, terms):
    blocks = []
    for var in ("ascii", "unicode"):
        rows, unmeas = [], []
        for t in terms:
            c = axis.get(t, {}).get(var, {})
            if cell_measured(c) and "mib_per_s" in c:
                rows.append({"name": t, "value": c["mib_per_s"],
                             "label": f'{c["mib_per_s"]:.1f} MiB/s', "noa": t == "noa"})
            elif t in axis:
                unmeas.append(t)
        if rows:
            blocks.append(f'<h3>{var}</h3>' + svg_hbars(rows, True, unmeasured=unmeas))
    if not blocks:
        return ""
    return section("Throughput", "PTY write → screen, grouped by charset.", "".join(blocks))


def sec_scroll(axis, terms):
    rows, unmeas = [], []
    for t in terms:
        c = axis.get(t, {})
        if cell_measured(c) and "mib_per_s" in c:
            ms = c.get("median_ms")
            lbl = f'{c["mib_per_s"]:.1f} MiB/s' + (f' · {ms} ms' if ms is not None else "")
            rows.append({"name": t, "value": c["mib_per_s"], "label": lbl, "noa": t == "noa"})
        elif t in axis:
            unmeas.append(t)
    if not rows:
        return ""
    return section("Scroll", "Scrollback flood throughput (frame time as label).",
                   svg_hbars(rows, True, unmeasured=unmeas))


def sec_fire(axis, terms, contention):
    rows, unmeas = [], []
    for t in terms:
        c = axis.get(t, {})
        if cell_measured(c) and "fps_median" in c:
            region = c.get("region", "?")
            rows.append({"name": t, "value": c["fps_median"],
                         "label": f'{c["fps_median"]:.1f} fps ({region})', "noa": t == "noa"})
        elif t in axis:
            unmeas.append(t)
    if not rows:
        return ""
    cond = contention.get("fire_condition")
    cap = f"DOOM-fire IO stress. Condition: {cond}." if cond else "DOOM-fire IO stress."
    return section("Fire", cap, svg_hbars(rows, True, unmeasured=unmeas))


def sec_latency(axis, terms):
    rows, unmeas = [], []
    for t in terms:
        c = axis.get(t, {})
        if cell_measured(c) and "median_us" in c:
            extra = []
            if "p95_us" in c:
                extra.append(f'p95 {c["p95_us"]}')
            if "p99_us" in c:
                extra.append(f'p99 {c["p99_us"]}')
            tail = (" · " + " / ".join(extra)) if extra else ""
            rows.append({"name": t, "value": c["median_us"],
                         "label": f'{c["median_us"]} µs{tail}', "noa": t == "noa"})
        elif t in axis:
            unmeas.append(t)
    if not rows:
        return ""
    return section("Input Latency", "DSR round-trip proxy, median µs (p95/p99 in labels).",
                   svg_hbars(rows, False, log=True, unmeasured=unmeas))


def sec_startup(axis, terms):
    rows, unmeas = [], []
    for t in terms:
        c = axis.get(t, {})
        if cell_measured(c) and "median_ms" in c:
            rows.append({"name": t, "value": c["median_ms"],
                         "label": f'{c["median_ms"]} ms', "noa": t == "noa"})
        elif t in axis:
            unmeas.append(t)
    if not rows:
        return ""
    return section("Warm Startup", "spawn → pty-ready.", svg_hbars(rows, False, unmeasured=unmeas))


def sec_memory(axis, terms):
    # settled MiB per scenario (longevity uses final-settled), lower better
    scenarios = [("idle", "idle"), ("scrollback", "scrollback (post-flood)"),
                 ("multitab", "multitab"), ("longevity", "longevity (final-settled)")]
    blocks = []
    for key, label in scenarios:
        rows, unmeas = [], []
        for t in terms:
            c = axis.get(t, {}).get(key, {})
            if not cell_measured(c):
                if key in axis.get(t, {}):
                    unmeas.append(t)
                continue
            if key == "longevity":
                v = c.get("final_settled_mib", c.get("final_active_mib"))
            else:
                v = c.get("settled_mib", c.get("active_mib"))
            if v is None:
                unmeas.append(t)
                continue
            rows.append({"name": t, "value": v, "label": f"{v} MiB", "noa": t == "noa"})
        if rows:
            blocks.append(f'<h3>{esc(label)}</h3>' + svg_hbars(rows, False, unmeasured=unmeas))
    if not blocks:
        return ""
    return section("Memory", "Settled physical footprint per scenario.", "".join(blocks))


def sec_load(axis, terms):
    blocks = []
    # idle mean CPU%
    rows, unmeas = [], []
    for t in terms:
        c = axis.get(t, {}).get("idle", {})
        if cell_measured(c) and "cpu_pct_mean" in c:
            mx = c.get("cpu_pct_max")
            lbl = f'{c["cpu_pct_mean"]:.2f}%' + (f' · max {mx:.2f}%' if mx is not None else "")
            rows.append({"name": t, "value": c["cpu_pct_mean"], "label": lbl, "noa": t == "noa"})
        elif "idle" in axis.get(t, {}):
            unmeas.append(t)
    if rows:
        blocks.append('<h3>idle mean CPU%</h3>' + svg_hbars(rows, False, unmeasured=unmeas))
    # active cpu_ms per workload
    for wk in ("throughput", "scroll"):
        rows, unmeas = [], []
        for t in terms:
            c = axis.get(t, {}).get(wk, {})
            if cell_measured(c) and "cpu_ms" in c:
                per = c.get("cpu_ms_per_mib")
                lbl = f'{c["cpu_ms"]} ms' + (f' · {per:.2f} ms/MiB' if per else "")
                rows.append({"name": t, "value": c["cpu_ms"], "label": lbl, "noa": t == "noa"})
            elif wk in axis.get(t, {}):
                unmeas.append(t)
        if rows:
            blocks.append(f'<h3>active: {wk} workload (CPU-ms)</h3>'
                          + svg_hbars(rows, False, unmeasured=unmeas))
    if not blocks:
        return ""
    return section("Load", "Idle CPU% and active CPU-time per workload.", "".join(blocks))


def section(title, caption, body):
    return (f'<section><h2>{esc(title)}</h2>'
            f'<p class="cap">{esc(caption)}</p>{body}</section>')


# ── noa trend across multiple runs (simple bars, timestamps on x) ──────
def noa_trend(runs):
    # runs: list of (timestamp, results). Extract one comparable noa scalar per
    # axis and draw a simple bar per run.
    def getter(res):
        ax = res.get("axes", {})
        out = {}
        tp = ax.get("throughput", {}).get("noa", {})
        for var in ("ascii", "unicode"):
            c = tp.get(var, {})
            if cell_measured(c) and "mib_per_s" in c:
                out[f"throughput {var} (MiB/s)"] = (c["mib_per_s"], True)
        sc = ax.get("scroll", {}).get("noa", {})
        if cell_measured(sc) and "mib_per_s" in sc:
            out["scroll (MiB/s)"] = (sc["mib_per_s"], True)
        fi = ax.get("fire", {}).get("noa", {})
        if cell_measured(fi) and "fps_median" in fi:
            out["fire (fps)"] = (fi["fps_median"], True)
        la = ax.get("latency", {}).get("noa", {})
        if cell_measured(la) and "median_us" in la:
            out["latency median (µs)"] = (la["median_us"], False)
        st = ax.get("startup", {}).get("noa", {})
        if cell_measured(st) and "median_ms" in st:
            out["startup (ms)"] = (st["median_ms"], False)
        mi = ax.get("memory", {}).get("noa", {}).get("idle", {})
        if cell_measured(mi) and (mi.get("settled_mib") or mi.get("active_mib")):
            out["memory idle (MiB)"] = (mi.get("settled_mib", mi.get("active_mib")), False)
        li = ax.get("load", {}).get("noa", {}).get("idle", {})
        if cell_measured(li) and "cpu_pct_mean" in li:
            out["load idle (CPU%)"] = (li["cpu_pct_mean"], False)
        return out

    per_run = [(ts, getter(res)) for ts, res in runs]
    metrics = []
    for _, d in per_run:
        for k in d:
            if k not in metrics:
                metrics.append(k)
    blocks = []
    for m in metrics:
        rows = []
        higher = True
        for ts, d in per_run:
            if m in d:
                v, higher = d[m]
                rows.append({"name": ts, "value": v, "label": f"{v}", "noa": True})
        if len(rows) >= 2:
            blocks.append(f'<h3>{esc(m)}</h3>' + svg_hbars(rows, higher))
    if not blocks:
        return ""
    return section("noa trend", "noa's per-axis values across the given runs.", "".join(blocks))


# ── header ─────────────────────────────────────────────────────────
def header(res):
    ts = res.get("timestamp", "?")
    m = res.get("machine", {})
    mline = " · ".join(x for x in [
        m.get("chip") or m.get("model"),
        f'{m.get("cores")} cores' if m.get("cores") else "",
        f'macOS {m.get("os")}' if m.get("os") else "",
        f'{m.get("build")}' if m.get("build") else "",
        m.get("arch"),
    ] if x)
    vers = res.get("terminal_versions", {})
    vline = ", ".join(f"{k} {v}" for k, v in vers.items())
    cont = res.get("contention", {})
    meta = []
    for key in ("quiescence_check", "fullscreen", "fire_condition", "target_display"):
        if cont.get(key):
            meta.append(f'<div class="metarow"><span>{esc(key)}</span> {esc(cont[key])}</div>')
    h = [f'<header><h1>Benchmark Report</h1>',
         f'<div class="ts">{esc(ts)}</div>']
    if mline:
        h.append(f'<div class="machine">{esc(mline)}</div>')
    if vline:
        h.append(f'<div class="vers">{esc(vline)}</div>')
    if meta:
        h.append('<div class="meta">' + "".join(meta) + "</div>")
    h.append("</header>")
    return "\n".join(h)


CSS = """
:root { color-scheme: dark; }
* { box-sizing: border-box; }
body { margin: 0; padding: 24px 28px 60px; background: #0b0f14; color: #e5e7eb;
  font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
h1 { font-size: 22px; margin: 0 0 2px; }
h2 { font-size: 17px; margin: 0 0 4px; color: #f9fafb; }
h3 { font-size: 13px; margin: 14px 0 2px; color: #9ca3af; font-weight: 600; }
header { border-bottom: 1px solid #1f2937; padding-bottom: 16px; margin-bottom: 8px; }
.ts { color: #34d399; font-weight: 600; font-variant-numeric: tabular-nums; }
.machine, .vers { color: #9ca3af; font-size: 12.5px; margin-top: 4px; }
.meta { margin-top: 10px; display: flex; flex-direction: column; gap: 3px; }
.metarow { font-size: 12px; color: #cbd5e1; }
.metarow span { color: #6b7280; display: inline-block; min-width: 130px; }
section { padding: 18px 0; border-bottom: 1px solid #131a22; }
.cap { color: #9ca3af; font-size: 12.5px; margin: 0 0 6px; }
.chart { overflow-x: auto; margin: 4px 0; }
.chart svg { max-width: 100%; }
.dir { font-size: 11px; color: #6b7280; margin: 2px 0 4px; }
.unmeas { font-size: 11px; color: #6b7280; font-style: italic; margin-bottom: 6px; }
footer { color: #4b5563; font-size: 11px; margin-top: 24px; }
"""


def present_axes(res):
    return res.get("axes", {})


def build_report(runs, primary):
    """runs: list of (ts, results); primary: the results whose axes drive sections."""
    res = primary
    axes = present_axes(res)
    terms = order_terms([t for ax in axes.values() for t in ax])
    body = [f'<style>{CSS}</style>', header(res)]

    builders = {
        "throughput": lambda a: sec_throughput(a, terms),
        "scroll": lambda a: sec_scroll(a, terms),
        "fire": lambda a: sec_fire(a, terms, res.get("contention", {})),
        "latency": lambda a: sec_latency(a, terms),
        "startup": lambda a: sec_startup(a, terms),
        "memory": lambda a: sec_memory(a, terms),
        "load": lambda a: sec_load(a, terms),
    }
    any_section = False
    for name, fn in builders.items():
        if name in axes:
            html = fn(axes[name])
            if html:
                body.append(html)
                any_section = True
    if not any_section:
        body.append('<section><p class="cap">No measured axes in this run.</p></section>')

    if len(runs) > 1:
        tr = noa_trend(runs)
        if tr:
            body.append(tr)

    body.append('<footer>Generated by bench/visualize.py — renders only the '
                'values present in the given results files.</footer>')
    return "<!-- report body -->\n" + "\n".join(body)


def newest_results_dir(bench_dir):
    root = os.path.join(bench_dir, "results")
    dirs = [os.path.join(root, d) for d in os.listdir(root)
            if os.path.isdir(os.path.join(root, d))
            and os.path.exists(os.path.join(root, d, "results.json"))]
    if not dirs:
        return None
    return max(dirs, key=lambda d: os.path.basename(d))


def load(d):
    with open(os.path.join(d, "results.json")) as f:
        return json.load(f)


def main(argv):
    out = None
    dirs = []
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--out":
            out = argv[i + 1]
            i += 2
            continue
        dirs.append(a)
        i += 1

    bench_dir = os.path.dirname(os.path.abspath(__file__))
    if not dirs:
        d = newest_results_dir(bench_dir)
        if not d:
            print("no results dirs found", file=sys.stderr)
            return 1
        dirs = [d]

    runs = [(os.path.basename(os.path.normpath(d)), load(d)) for d in dirs]
    # primary = last dir (its report.html, its axes drive the per-axis sections)
    primary = runs[-1][1]
    html = build_report(runs, primary)

    out = out or os.path.join(dirs[-1], "report.html")
    with open(out, "w") as f:
        f.write(html)
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
