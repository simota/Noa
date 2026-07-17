#!/usr/bin/env python3
"""Aggregate raw.tsv -> results.json + table.md, plus environment capture."""
import json
import os
import subprocess
import sys
from collections import defaultdict

RAW, OUT_DIR, TS = sys.argv[1], sys.argv[2], sys.argv[3]


def sh(*args):
    try:
        return subprocess.check_output(args, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return ""


def plist(app, key):
    p = f"/Applications/{app}/Contents/Info.plist"
    return plist_at(p, key)


def plist_at(path, key):
    return sh("/usr/libexec/PlistBuddy", "-c", f"Print :{key}", path)


def terminal_versions(repo):
    v = {}
    noa_bin = os.path.join(repo, "target/release/noa")
    if os.path.exists(noa_bin):
        out = sh(noa_bin, "+version") or sh(noa_bin, "--version")
        v["noa"] = out.splitlines()[0] if out else "(release build)"
    v["ghostty"] = plist("Ghostty.app", "CFBundleShortVersionString")
    gn_app = os.environ.get(
        "GHOSTTY_NIGHTLY_APP",
        os.path.expanduser(
            "~/repos/github.com/ghostty/macos/build/ReleaseLocal/Ghostty.app"))
    gn_bin = os.path.join(gn_app, "Contents/MacOS/ghostty")
    if os.path.exists(gn_bin):
        gn_out = sh(gn_bin, "--version")
        v["ghostty-nightly"] = (gn_out.splitlines()[0] if gn_out
                                else "(frozen nightly build)")
    v["termy"] = plist("Termy.app", "CFBundleShortVersionString")
    v["kitty"] = plist("kitty.app", "CFBundleShortVersionString")
    v["alacritty"] = plist("Alacritty.app", "CFBundleShortVersionString")
    v["iterm2"] = plist("iTerm.app", "CFBundleShortVersionString")
    v["warp"] = plist("Warp.app", "CFBundleShortVersionString")
    v["terminal"] = plist_at(
        "/System/Applications/Utilities/Terminal.app/Contents/Info.plist",
        "CFBundleShortVersionString")
    v["rio"] = plist("Rio.app", "CFBundleShortVersionString")
    return {k: val for k, val in v.items() if val}


# ── parse raw ──────────────────────────────────────────────────────
# rows keyed by (terminal, axis, variant, rep) -> {metric: (value, unit)}
rows = []
with open(RAW) as f:
    header = f.readline()
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) != 7:
            continue
        term, axis, variant, rep, metric, value, unit = parts
        rows.append(dict(terminal=term, axis=axis, variant=variant, rep=rep,
                         metric=metric, value=value, unit=unit))

# medians / status per (terminal, axis, variant)
agg = defaultdict(dict)
for r in rows:
    if r["rep"] in ("median", "-", "pooled") or r["metric"] == "status":
        key = (r["terminal"], r["axis"], r["variant"])
        agg[key][r["metric"]] = r["value"]

terminals = sorted({r["terminal"] for r in rows if r["axis"] != "meta"})
axes_with_data = {r["axis"] for r in rows if r["axis"] not in ("meta",)}
equalized_notes = {r["terminal"]: r["value"] for r in rows
                   if r["axis"] == "meta" and r["metric"] == "equalized"}
# per-terminal config-isolation notes + harness-level contention bookends
# (loadavg/uptime at start+end, builder-quiescence gate result) — recorded by
# harness >= 2026-07-16; absent in older raw files.
isolation_notes = {r["terminal"]: r["value"] for r in rows
                   if r["axis"] == "meta" and r["metric"] == "isolation"}
contention = {r["metric"]: r["value"] for r in rows
              if r["terminal"] == "harness" and r["axis"] == "meta"}
# noa build provenance (path/sha256/mtime) — version strings alone can't
# distinguish two builds of the same version
noa_bin_meta = {r["metric"]: r["value"] for r in rows
                if r["axis"] == "meta" and r["metric"].startswith("noa_bin")}

repo = os.path.abspath(os.path.join(os.path.dirname(RAW), "..", "..", ".."))
# repo is bench/results/<ts> -> ../../.. = repo root
repo = os.path.abspath(os.path.join(OUT_DIR, "..", "..", ".."))

machine = {
    "model": sh("sysctl", "-n", "hw.model"),
    "chip": sh("sysctl", "-n", "machdep.cpu.brand_string"),
    "cores": sh("sysctl", "-n", "hw.ncpu"),
    "mem_bytes": sh("sysctl", "-n", "hw.memsize"),
    "os": sh("sw_vers", "-productVersion"),
    "build": sh("sw_vers", "-buildVersion"),
    "arch": sh("uname", "-m"),
}

results = {
    "timestamp": TS,
    "machine": machine,
    "terminal_versions": terminal_versions(repo),
    "noa_bin": noa_bin_meta,
    "equalized": equalized_notes,
    "isolation": isolation_notes,
    "contention": contention,
    "axes": {},
    "raw_rows": rows,
}


def get(term, axis, variant, metric):
    return agg.get((term, axis, variant), {}).get(metric)


# throughput
tp = {}
for term in terminals:
    tp[term] = {}
    for variant in ("ascii", "unicode"):
        cell = agg.get((term, "throughput", variant), {})
        if cell.get("status") == "UNMEASURED":
            tp[term][variant] = {"status": "UNMEASURED"}
        elif "mib_per_s" in cell:
            tp[term][variant] = {
                "mib_per_s": float(cell["mib_per_s"]),
                "median_ns": int(cell.get("inner_ns", 0)),
            }
        else:
            tp[term][variant] = {"status": "UNMEASURED"}
results["axes"]["throughput"] = tp

# scroll
sc = {}
for term in terminals:
    cell = agg.get((term, "scroll", "-"), {})
    if cell.get("status") == "UNMEASURED":
        sc[term] = {"status": "UNMEASURED"}
    elif "mib_per_s" in cell:
        sc[term] = {"mib_per_s": float(cell["mib_per_s"]),
                    "median_ms": round(int(cell.get("inner_ns", 0)) / 1e6)}
    else:
        sc[term] = {"status": "UNMEASURED"}
results["axes"]["scroll"] = sc

# latency
# Harness >= 2026-07-16 pools raw samples across all launches and reports
# median/p95/p99/max over the pooled distribution (pooled_* metrics), plus
# how many samples/launches back it. Older raw files carry only per-run
# medianed median_ns/p99_ns — kept as a fallback so old dirs still aggregate.
lat = {}
for term in terminals:
    cell = agg.get((term, "latency", "-"), {})
    if cell.get("status") == "UNMEASURED":
        lat[term] = {"status": "UNMEASURED"}
    elif "pooled_median_ns" in cell:
        lat[term] = {
            "median_us": round(int(cell["pooled_median_ns"]) / 1000, 1),
            "p95_us": round(int(cell.get("pooled_p95_ns", 0)) / 1000, 1),
            "p99_us": round(int(cell.get("pooled_p99_ns", 0)) / 1000, 1),
            "max_us": round(int(cell.get("pooled_max_ns", 0)) / 1000, 1),
            "pooled_samples": int(cell.get("pooled_count", 0)),
            "launches": int(cell.get("pooled_launches", 0)),
        }
    elif "median_ns" in cell:
        lat[term] = {"median_us": round(int(cell["median_ns"]) / 1000, 1),
                     "p99_us": round(int(cell.get("p99_ns", 0)) / 1000, 1)}
    else:
        lat[term] = {"status": "UNMEASURED"}
results["axes"]["latency"] = lat

# fire (DOOM-fire IO stress — fixed 80x24 truecolor full-region repaint fps
# under pty flow control; see docs/specs/bench-doom-fire.md). Gated on
# axes_with_data: the axis only exists in raw files from harness >= 2026-07-17.
fire = {}
if "fire" in axes_with_data:
    for term in terminals:
        cell = agg.get((term, "fire", "-"), {})
        if cell.get("status") == "UNMEASURED":
            fire[term] = {"status": "UNMEASURED"}
        elif "fps" in cell:
            reps = [float(r["value"]) for r in rows
                    if r["terminal"] == term and r["axis"] == "fire"
                    and r["metric"] == "fps" and r["rep"] not in ("median", "-")]
            winsz = next((r["value"] for r in rows if r["terminal"] == term
                          and r["axis"] == "fire" and r["metric"] == "winsize"), None)
            # region rows exist from harness >= 2026-07-17 (full-window fire);
            # older raw files were always the fixed 80x24 region
            region = next((r["value"] for r in rows if r["terminal"] == term
                           and r["axis"] == "fire" and r["metric"] == "region"),
                          "80x24")
            fire[term] = {"fps_median": float(cell["fps"]), "fps_reps": reps,
                          "region": region,
                          **({"winsize": winsz} if winsz else {})}
        else:
            fire[term] = {"status": "UNMEASURED"}
    results["axes"]["fire"] = fire

# startup
st = {}
for term in terminals:
    cell = agg.get((term, "startup", "-"), {})
    if cell.get("status") == "UNMEASURED":
        st[term] = {"status": "UNMEASURED"}
    elif "total_ns" in cell:
        st[term] = {"median_ms": round(int(cell["total_ns"]) / 1e6)}
    else:
        st[term] = {"status": "UNMEASURED"}
results["axes"]["startup"] = st

# memory (idle / scrollback / multitab / longevity physical footprint)
# Dual metric per scenario (see METHODOLOGY.md "Memory: two numbers per
# scenario"): active_mib = in-use footprint (t=15s), settled_mib = long-lived
# idle footprint (median of the last 3 trajectory samples, past macOS
# GPU-driver-pool reclaim). Rankings use settled. Legacy raw.tsv (single
# rss_bytes sample, pre dual protocol) maps to active_mib only — its single
# blind-settle sample raced the reclaim window, which is what the dual
# protocol exists to fix.
mem = {}
for term in terminals:
    mem[term] = {}
    for scenario in ("idle", "scrollback", "multitab", "longevity"):
        cell = agg.get((term, "memory", scenario), {})
        if scenario == "longevity":
            cyc_rows = sorted(
                (r for r in rows if r["terminal"] == term and r["axis"] == "memory"
                 and r["variant"] == "longevity" and r["rep"].startswith("cycle")
                 and r["metric"] == "rss_bytes"),
                key=lambda r: int(r["rep"][len("cycle"):]))
            cycles_mib = [round(int(r["value"]) / 1048576, 1) for r in cyc_rows]
            if cycles_mib:
                # Triple metric, all first-class: growth-per-cycle (does it
                # leak?), final-active (footprint under churn, last cycle),
                # final-settled (footprint after >=75s quiescence). See
                # METHODOLOGY.md "Longevity".
                entry = {"cycles_mib": cycles_mib, "final_active_mib": cycles_mib[-1]}
                if "growth_per_cycle_bytes" in cell:
                    entry["growth_per_cycle_kib_per_cycle"] = round(int(cell["growth_per_cycle_bytes"]) / 1024, 1)
                if "final_settled_bytes" in cell:
                    entry["final_settled_mib"] = round(int(cell["final_settled_bytes"]) / 1048576, 1)
                mem[term][scenario] = entry
            else:
                mem[term][scenario] = {"status": cell.get("status", "UNMEASURED")}
        elif "active_bytes" in cell or "rss_bytes" in cell:
            entry = {}
            if "active_bytes" in cell:
                entry["active_mib"] = round(int(cell["active_bytes"]) / 1048576, 1)
            elif "rss_bytes" in cell:  # legacy single-sample harness
                entry["active_mib"] = round(int(cell["rss_bytes"]) / 1048576, 1)
            if "settled_bytes" in cell:
                entry["settled_mib"] = round(int(cell["settled_bytes"]) / 1048576, 1)
            if scenario == "multitab":
                entry["windows_requested"] = int(cell.get("windows_requested", 0))
                entry["processes_observed"] = int(cell.get("processes_observed", 0))
                # only present in raw.tsv from harness >= 2026-07-16 —
                # absence means "not instrumented", never "zero windows"
                if "windows_observed" in cell:
                    entry["windows_observed"] = int(cell["windows_observed"])
                if "proc_breakdown" in cell:
                    entry["proc_breakdown"] = cell["proc_breakdown"]
            mem[term][scenario] = entry
        else:
            mem[term][scenario] = {"status": cell.get("status", "UNMEASURED")}
results["axes"]["memory"] = mem

# load (idle CPU% + active CPU-time-per-workload)
load = {}
for term in terminals:
    load[term] = {}
    idle_cell = agg.get((term, "load", "idle"), {})
    if "cpu_pct_mean" in idle_cell:

        def reason(metric):
            r = next((r for r in rows if r["terminal"] == term and r["axis"] == "load"
                      and r["variant"] == "idle" and r["metric"] == metric), None)
            return f"{r['value']} ({r['unit']})" if r else "N/A"

        load[term]["idle"] = {
            "cpu_pct_mean": float(idle_cell["cpu_pct_mean"]),
            "cpu_pct_max": float(idle_cell["cpu_pct_max"]),
            # settle/csw only exist in raw.tsv from harness >= 2026-07-16;
            # absence means "not instrumented", never a measured zero
            **({"settle_discarded_s": int(idle_cell["settle_discarded_s"])}
               if "settle_discarded_s" in idle_cell else {}),
            # context switches/s over the settled window — wakeups proxy
            # (top has no wakeups key on this build; see METHODOLOGY.md)
            **({"csw_per_s": float(idle_cell["csw_per_s"])}
               if "csw_per_s" in idle_cell else {}),
            "wakeups": reason("wakeups"),
            "power": reason("power"),
        }
    else:
        load[term]["idle"] = {"status": idle_cell.get("status", "UNMEASURED")}
    for scenario in ("throughput", "scroll"):
        cell = agg.get((term, "load", scenario), {})
        if "cpu_ms" in cell:
            load[term][scenario] = {
                "cpu_ms": int(cell["cpu_ms"]),
                "cpu_ms_per_mib": float(cell.get("cpu_ms_per_mib", 0)),
            }
        else:
            load[term][scenario] = {"status": cell.get("status", "UNMEASURED")}
results["axes"]["load"] = load

with open(os.path.join(OUT_DIR, "results.json"), "w") as f:
    json.dump(results, f, indent=2)


# ── markdown table ─────────────────────────────────────────────────
def fmt_tp(c):
    return "UNMEASURED" if c.get("status") == "UNMEASURED" else f"{c['mib_per_s']:.1f}"


def fmt_sc(c):
    return "UNMEASURED" if c.get("status") == "UNMEASURED" else f"{c['median_ms']} ms / {c['mib_per_s']:.1f}"


def fmt_lat(c):
    if c.get("status") == "UNMEASURED":
        return "UNMEASURED"
    if "p95_us" in c:  # pooled harness (>= 2026-07-16)
        return f"{c['median_us']} / {c['p95_us']} / {c['p99_us']} / {c['max_us']}"
    return f"{c['median_us']} / — / {c['p99_us']} / — (pre-pooling harness)"


def fmt_st(c):
    return "UNMEASURED" if c.get("status") == "UNMEASURED" else f"{c['median_ms']}"


lines = []
lines.append(f"# Terminal Benchmark — {TS}\n")
mv = results["terminal_versions"]
lines.append("Terminals: " + ", ".join(f"{k} {v}" for k, v in mv.items()) + "\n")
lines.append(f"Machine: {machine['chip']} ({machine['cores']} cores), macOS {machine['os']} ({machine['arch']})\n")
if contention:
    q = contention.get("quiescence_check", "n/a")
    lines.append(f"Quiescence gate: {q}")
    if "loadavg_start" in contention or "loadavg_end" in contention:
        lines.append(f"loadavg start {contention.get('loadavg_start', 'n/a')} / "
                     f"end {contention.get('loadavg_end', 'n/a')} "
                     f"(builders at end: {contention.get('builders_at_end', 'n/a')})")
    lines.append("")
if isolation_notes:
    lines.append("**Config isolation** (fresh-install defaults for every terminal):")
    for t in terminals:
        if t in isolation_notes:
            lines.append(f"- {t}: {isolation_notes[t]}")
    lines.append("")
if equalized_notes:
    lines.append("\n**Equalized conditions** (per terminal):")
    for t in terminals:
        if t in equalized_notes:
            lines.append(f"- {t}: {equalized_notes[t]}")
    lines.append("")

hdr = "| Terminal | " + " | ".join(terminals) + " |"
sep = "|---|" + "|".join(["---"] * len(terminals)) + "|"

if "throughput" in axes_with_data:
    lines.append("\n## Throughput — ASCII (MiB/s, higher better)")
    lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
    lines.append("| ascii MiB/s | " + " | ".join(fmt_tp(tp[t]["ascii"]) for t in terminals) + " |")
    lines.append("| unicode MiB/s | " + " | ".join(fmt_tp(tp[t]["unicode"]) for t in terminals) + " |")

if "scroll" in axes_with_data:
    lines.append("\n## Frame / Scroll (time ms / MiB·s⁻¹, lower ms better)")
    lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
    lines.append("| scroll_stress | " + " | ".join(fmt_sc(sc[t]) for t in terminals) + " |")

if "fire" in axes_with_data:
    def fmt_fire(c):
        if c.get("status") == "UNMEASURED":
            return "UNMEASURED"
        return f"{c['fps_median']:.1f} ({c.get('region', '?')})"
    fire_cond = contention.get("fire_condition", "fixed 80x24 region (pre-2026-07-17 harness)")
    lines.append("\n## Fire — DOOM-fire IO stress (fps, higher better)")
    lines.append(f"Condition: {fire_cond}. Producer-side fps under pty flow control "
                 "(frames written ≈ frames consumed); per-terminal render region in "
                 "parentheses. Not comparable to published DOOM-fire-zig figures "
                 "(other machines/displays).")
    lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
    lines.append("| fire fps (region) | " + " | ".join(fmt_fire(fire[t]) for t in terminals) + " |")

if "latency" in axes_with_data:
    lines.append("\n## Input Latency — DSR round-trip proxy (median / p95 / p99 / max µs, lower better)")
    budget = {f"{lat[t].get('pooled_samples', 0)} samples / {lat[t].get('launches', 0)} launches"
              for t in terminals if lat[t].get("pooled_samples")}
    if budget:
        lines.append("Percentiles over the POOLED per-iteration samples of all "
                     "independent launches: " + "; ".join(sorted(budget)) + ".")
    lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
    lines.append("| DSR µs | " + " | ".join(fmt_lat(lat[t]) for t in terminals) + " |")

if "startup" in axes_with_data:
    lines.append("\n## Warm Startup — spawn→pty-ready (ms, lower better)")
    lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
    lines.append("| startup ms | " + " | ".join(fmt_st(st[t]) for t in terminals) + " |")


def fmt_mem_settled(c):
    if c.get("status") == "UNMEASURED":
        return "UNMEASURED"
    if "settled_mib" not in c:
        return "n/a (pre-dual-protocol harness)"
    s = f"{c['settled_mib']} MiB"
    if "processes_observed" in c:
        s += (f" ({c.get('windows_observed', '?')} win obs / {c['windows_requested']} req, "
              f"{c['processes_observed']} procs)")
    return s


def fmt_mem_active(c):
    if c.get("status") == "UNMEASURED":
        return "UNMEASURED"
    return f"{c['active_mib']} MiB" if "active_mib" in c else "n/a"


def fmt_longevity_traj(c):
    if c.get("status") == "UNMEASURED":
        return "UNMEASURED"
    if "cycles_mib" not in c:
        return "n/a"
    return "→".join(str(v) for v in c["cycles_mib"]) + " MiB"


def fmt_longevity_growth(c):
    if c.get("status") == "UNMEASURED":
        return "UNMEASURED"
    g = c.get("growth_per_cycle_kib_per_cycle")
    return f"{g:+.0f} KiB/cycle" if g is not None else "n/a"


def fmt_longevity_final_active(c):
    if c.get("status") == "UNMEASURED":
        return "UNMEASURED"
    return f"{c['final_active_mib']} MiB" if "final_active_mib" in c else "n/a"


def fmt_longevity_final_settled(c):
    if c.get("status") == "UNMEASURED":
        return "UNMEASURED"
    return (f"{c['final_settled_mib']} MiB" if "final_settled_mib" in c
            else "n/a (pre-dual-protocol harness)")


def fmt_load_active(c):
    return "UNMEASURED" if c.get("status") == "UNMEASURED" else f"{c['cpu_ms']} ms ({c['cpu_ms_per_mib']:.2f} ms/MiB)"


if "memory" in axes_with_data:
    lines.append("\n## Memory — physical footprint (MiB, lower better; ranked on SETTLED)")
    lines.append("")
    lines.append("Two readings per scenario (see METHODOLOGY.md): **active** = in-use "
                 "footprint shortly after the scenario's work (GPU-driver pools still "
                 "resident); **settled** = long-lived idle footprint after macOS "
                 "GPU-pool reclaim (median of the last 3 samples of the full "
                 "trajectory, kept in raw.tsv). Rankings use **settled** — a "
                 "terminal's steady state is what long-lived sessions pay for; "
                 "active is shown alongside because reclaim timing is "
                 "driver-scheduled and nondeterministic (5-40s), making any single "
                 "blind-settle sample a coin flip.")
    lines.append("")
    lines.append(hdr.replace("Terminal", "Scenario")); lines.append(sep)
    for scenario, label in (("idle", "idle"), ("scrollback", "scrollback (post-flood)"), ("multitab", "multitab")):
        lines.append(f"| {label} — settled | " + " | ".join(fmt_mem_settled(mem[t][scenario]) for t in terminals) + " |")
        lines.append(f"| {label} — active | " + " | ".join(fmt_mem_active(mem[t][scenario]) for t in terminals) + " |")
    lines.append("| longevity trajectory (per cycle, under churn) | " + " | ".join(fmt_longevity_traj(mem[t]["longevity"]) for t in terminals) + " |")
    # Longevity is deliberately THREE labeled metrics (see METHODOLOGY.md):
    lines.append("| longevity growth rate (KiB/cycle, 0=flat is best) | " + " | ".join(fmt_longevity_growth(mem[t]["longevity"]) for t in terminals) + " |")
    lines.append("| longevity final — active (last cycle, under churn) | " + " | ".join(fmt_longevity_final_active(mem[t]["longevity"]) for t in terminals) + " |")
    lines.append("| longevity final — settled (after quiescence) | " + " | ".join(fmt_longevity_final_settled(mem[t]["longevity"]) for t in terminals) + " |")

if "load" in axes_with_data:
    lines.append("\n## Load — CPU (idle % / active CPU-time per workload, lower better)")
    lines.append(hdr.replace("Terminal", "Scenario")); lines.append(sep)
    settles = {load[t]["idle"].get("settle_discarded_s") for t in terminals
               if load[t]["idle"].get("settle_discarded_s")}
    settle_note = f" (settled: first {'/'.join(str(s) for s in sorted(settles))}s after launch discarded)" if settles else ""
    lines.append(f"| idle mean% / max%{settle_note} | " + " | ".join(
        ("UNMEASURED" if load[t]["idle"].get("status") == "UNMEASURED"
         else f"{load[t]['idle']['cpu_pct_mean']:.2f}% / {load[t]['idle']['cpu_pct_max']:.2f}%")
        for t in terminals) + " |")
    lines.append("| idle csw/s (wakeups proxy) | " + " | ".join(
        ("UNMEASURED" if load[t]["idle"].get("status") == "UNMEASURED"
         else (f"{load[t]['idle']['csw_per_s']:.1f}"
               if "csw_per_s" in load[t]["idle"] else "n/a (pre-2026-07-16 harness)"))
        for t in terminals) + " |")
    lines.append("| wakeups (idle) | " + " | ".join(load[t]["idle"].get("wakeups", "N/A") for t in terminals) + " |")
    lines.append("| power (idle) | " + " | ".join(load[t]["idle"].get("power", "N/A") for t in terminals) + " |")
    lines.append("| active: throughput workload | " + " | ".join(fmt_load_active(load[t]["throughput"]) for t in terminals) + " |")
    lines.append("| active: scroll workload | " + " | ".join(fmt_load_active(load[t]["scroll"]) for t in terminals) + " |")

# noa rank per axis — only when noa was actually measured in this run
# (an all-n/a section on --only runs without noa reads like a failure)
def rank(better_high, getval):
    # standard competition ranking: 1 + number of STRICTLY better entries,
    # so ties share a rank instead of being ordered arbitrarily
    scored = {t: getval(t) for t in terminals if getval(t) is not None}
    if "noa" not in scored:
        return (None, len(scored))
    noa_v = scored["noa"]
    if better_high:
        better = sum(1 for v in scored.values() if v > noa_v)
    else:
        better = sum(1 for v in scored.values() if v < noa_v)
    return (better + 1, len(scored))

def tp_val(t, var):
    c = tp[t][var]
    return None if c.get("status") == "UNMEASURED" else c["mib_per_s"]
def sc_val(t):
    c = sc[t]; return None if c.get("status") == "UNMEASURED" else c["mib_per_s"]
def lat_val(t):
    c = lat[t]; return None if c.get("status") == "UNMEASURED" else c["median_us"]
def st_val(t):
    c = st[t]; return None if c.get("status") == "UNMEASURED" else c["median_ms"]
def mem_val(t, scenario):
    # Ranked on SETTLED (steady state = what long-lived sessions pay for);
    # legacy pre-dual-protocol raw files fall back to the active reading so
    # old result dirs still aggregate.
    c = mem[t][scenario]
    if c.get("status") == "UNMEASURED":
        return None
    return c.get("settled_mib", c.get("active_mib"))
def mem_longevity_val(t):
    # Ranked on final-SETTLED; legacy fallback = final-active (last cycle).
    c = mem[t]["longevity"]
    if c.get("status") == "UNMEASURED":
        return None
    return c.get("final_settled_mib", c.get("final_active_mib"))
def mem_longevity_leak_val(t):
    # Leak RATE: growth clamped at 0 — a negative trajectory (memory returned
    # while settling) is "no leak", not a bonus; ranking raw signed growth
    # would reward a transient shrink over a truly flat profile. Raw signed
    # values stay visible in the table row.
    c = mem[t]["longevity"]
    if c.get("status") == "UNMEASURED":
        return None
    g = c.get("growth_per_cycle_kib_per_cycle")
    return None if g is None else max(g, 0.0)
def load_idle_val(t):
    c = load[t]["idle"]; return None if c.get("status") == "UNMEASURED" else c["cpu_pct_mean"]
def load_active_val(t, scenario):
    c = load[t][scenario]; return None if c.get("status") == "UNMEASURED" else c["cpu_ms"]

def fire_val(t):
    c = fire.get(t, {})
    return c.get("fps_median")

rank_items = []
if "throughput" in axes_with_data:
    rank_items.append(("throughput ascii", rank(True, lambda t: tp_val(t, "ascii"))))
    rank_items.append(("throughput unicode", rank(True, lambda t: tp_val(t, "unicode"))))
if "scroll" in axes_with_data:
    rank_items.append(("scroll (MiB/s)", rank(True, sc_val)))
if "fire" in axes_with_data:
    rank_items.append(("fire (fps)", rank(True, fire_val)))
if "latency" in axes_with_data:
    rank_items.append(("latency (median)", rank(False, lat_val)))
if "startup" in axes_with_data:
    rank_items.append(("startup", rank(False, st_val)))
if "memory" in axes_with_data:
    rank_items.append(("memory idle (settled)", rank(False, lambda t: mem_val(t, "idle"))))
    rank_items.append(("memory scrollback (settled)", rank(False, lambda t: mem_val(t, "scrollback"))))
    rank_items.append(("memory multitab (settled)", rank(False, lambda t: mem_val(t, "multitab"))))
    # longevity is a multi-reading metric — rank leak rate + final settled
    rank_items.append(("memory longevity leak rate (KiB/cycle, <=0 counts as 0)", rank(False, mem_longevity_leak_val)))
    rank_items.append(("memory longevity final footprint (settled MiB)", rank(False, mem_longevity_val)))
if "load" in axes_with_data:
    rank_items.append(("load idle (mean CPU%)", rank(False, load_idle_val)))
    rank_items.append(("load active (throughput, CPU-ms)", rank(False, lambda t: load_active_val(t, "throughput"))))
    rank_items.append(("load active (scroll, CPU-ms)", rank(False, lambda t: load_active_val(t, "scroll"))))
if "noa" in terminals:
    ranked = [(label, r) for label, r in rank_items if r[0]]
    if ranked:
        lines.append("\n## noa rank per axis")
        for label, (pos, n) in ranked:
            lines.append(f"- {label}: #{pos} of {n}")

with open(os.path.join(OUT_DIR, "table.md"), "w") as f:
    f.write("\n".join(lines) + "\n")

print("wrote results.json + table.md")
