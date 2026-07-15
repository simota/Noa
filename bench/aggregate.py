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
    return sh("/usr/libexec/PlistBuddy", "-c", f"Print :{key}", p)


def terminal_versions(repo):
    v = {}
    noa_bin = os.path.join(repo, "target/release/noa")
    if os.path.exists(noa_bin):
        out = sh(noa_bin, "+version") or sh(noa_bin, "--version")
        v["noa"] = out.splitlines()[0] if out else "(release build)"
    v["ghostty"] = plist("Ghostty.app", "CFBundleShortVersionString")
    v["termy"] = plist("Termy.app", "CFBundleShortVersionString")
    v["kitty"] = plist("kitty.app", "CFBundleShortVersionString")
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
    if r["rep"] in ("median", "-") or r["metric"] == "status":
        key = (r["terminal"], r["axis"], r["variant"])
        agg[key][r["metric"]] = r["value"]

terminals = sorted({r["terminal"] for r in rows if r["axis"] != "meta"})
axes_with_data = {r["axis"] for r in rows if r["axis"] not in ("meta",)}
equalized_notes = {r["terminal"]: r["value"] for r in rows
                   if r["axis"] == "meta" and r["metric"] == "equalized"}

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
    "equalized": equalized_notes,
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
lat = {}
for term in terminals:
    cell = agg.get((term, "latency", "-"), {})
    if cell.get("status") == "UNMEASURED":
        lat[term] = {"status": "UNMEASURED"}
    elif "median_ns" in cell:
        lat[term] = {"median_us": round(int(cell["median_ns"]) / 1000, 1),
                     "p99_us": round(int(cell.get("p99_ns", 0)) / 1000, 1)}
    else:
        lat[term] = {"status": "UNMEASURED"}
results["axes"]["latency"] = lat

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

with open(os.path.join(OUT_DIR, "results.json"), "w") as f:
    json.dump(results, f, indent=2)


# ── markdown table ─────────────────────────────────────────────────
def fmt_tp(c):
    return "UNMEASURED" if c.get("status") == "UNMEASURED" else f"{c['mib_per_s']:.1f}"


def fmt_sc(c):
    return "UNMEASURED" if c.get("status") == "UNMEASURED" else f"{c['median_ms']} ms / {c['mib_per_s']:.1f}"


def fmt_lat(c):
    return "UNMEASURED" if c.get("status") == "UNMEASURED" else f"{c['median_us']} / {c['p99_us']}"


def fmt_st(c):
    return "UNMEASURED" if c.get("status") == "UNMEASURED" else f"{c['median_ms']}"


lines = []
lines.append(f"# Terminal Benchmark — {TS}\n")
mv = results["terminal_versions"]
lines.append("Terminals: " + ", ".join(f"{k} {v}" for k, v in mv.items()) + "\n")
lines.append(f"Machine: {machine['chip']} ({machine['cores']} cores), macOS {machine['os']} ({machine['arch']})\n")
if equalized_notes:
    lines.append("\n**Equalized conditions** (per terminal):")
    for t in terminals:
        if t in equalized_notes:
            lines.append(f"- {t}: {equalized_notes[t]}")
    lines.append("")

hdr = "| Terminal | " + " | ".join(terminals) + " |"
sep = "|---|" + "|".join(["---"] * len(terminals)) + "|"

lines.append("\n## Throughput — ASCII (MiB/s, higher better)")
lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
lines.append("| ascii MiB/s | " + " | ".join(fmt_tp(tp[t]["ascii"]) for t in terminals) + " |")
lines.append("| unicode MiB/s | " + " | ".join(fmt_tp(tp[t]["unicode"]) for t in terminals) + " |")

lines.append("\n## Frame / Scroll (time ms / MiB·s⁻¹, lower ms better)")
lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
lines.append("| scroll_stress | " + " | ".join(fmt_sc(sc[t]) for t in terminals) + " |")

if "latency" in axes_with_data:
    lines.append("\n## Input Latency — DSR round-trip proxy (median / p99 µs, lower better)")
    lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
    lines.append("| DSR µs | " + " | ".join(fmt_lat(lat[t]) for t in terminals) + " |")

if "startup" in axes_with_data:
    lines.append("\n## Warm Startup — spawn→pty-ready (ms, lower better)")
    lines.append(hdr.replace("Terminal", "Metric")); lines.append(sep)
    lines.append("| startup ms | " + " | ".join(fmt_st(st[t]) for t in terminals) + " |")

# noa rank per axis
lines.append("\n## noa rank per axis")
def rank(better_high, getval):
    scored = []
    for t in terminals:
        v = getval(t)
        if v is None:
            continue
        scored.append((t, v))
    scored.sort(key=lambda x: x[1], reverse=better_high)
    order = [t for t, _ in scored]
    return (order.index("noa") + 1, len(order)) if "noa" in order else (None, len(order))

def tp_val(t, var):
    c = tp[t][var]
    return None if c.get("status") == "UNMEASURED" else c["mib_per_s"]
def sc_val(t):
    c = sc[t]; return None if c.get("status") == "UNMEASURED" else c["mib_per_s"]
def lat_val(t):
    c = lat[t]; return None if c.get("status") == "UNMEASURED" else c["median_us"]
def st_val(t):
    c = st[t]; return None if c.get("status") == "UNMEASURED" else c["median_ms"]

rank_items = [
    ("throughput ascii", rank(True, lambda t: tp_val(t, "ascii"))),
    ("throughput unicode", rank(True, lambda t: tp_val(t, "unicode"))),
    ("scroll (MiB/s)", rank(True, sc_val)),
]
if "latency" in axes_with_data:
    rank_items.append(("latency (median)", rank(False, lat_val)))
if "startup" in axes_with_data:
    rank_items.append(("startup", rank(False, st_val)))
for label, r in rank_items:
    pos, n = r
    lines.append(f"- {label}: {'#'+str(pos)+' of '+str(n) if pos else 'n/a'}")

with open(os.path.join(OUT_DIR, "table.md"), "w") as f:
    f.write("\n".join(lines) + "\n")

print("wrote results.json + table.md")
