# Noa Positioning: The Fastest Terminal

> Status: draft (ratified upon merge). Source of truth for how "fastest" is
> claimed, defended, and measured. Benchmarks live in `bench/`
> (see `bench/METHODOLOGY.md`).

## One-line definition

**Noa is the terminal that swallows output floods fastest** — `cat huge.log`,
build logs, CI streams — with class-leading render throughput and no
wide-character (CJK) penalty.

## The claim, precisely

"Fastest" means **fastest at sustained output throughput and bulk scroll**.
Every number below is reproducible via `bench/run_all.sh`.

| Axis | Measurement (2026-07-17, 5-run median) | Role |
|---|---|---|
| render.throughput (plain/ansi/cjk) | 220–233 MB/s, stable across all runs | **Headline** |
| scroll proxy | 54 ms | **Headline** |
| cmd.overhead (zsh) | 1336 µs | Supporting |
| idle RSS | 52 MB settled | Supporting |
| input.latency (idle) | 1.2–1.3 ms | Supporting |

CJK being the fastest throughput variant (233 MB/s) is a distinctive
strength: wide-character handling carries no cost.

## Design character

Noa buys speed with parallelism — parse and render scale across cores under
load. This is a deliberate desktop-first choice: when output floods in, Noa
spends hardware to stay ahead of it.

## Guardrails (what keeps "fastest" honest)

A headline metric may never be bought by regressing elsewhere. Tracked on
every perf PR via `bench/run_all.sh`:

- **input.latency under load** — keep tightening toward the idle-level
  figure. PR #21 (isolate keyboard input path from pty output processing)
  is the first step; re-measure post-#21 before citing numbers.
- **CPU under load** — stay within the current envelope.
- **idle RSS** — hold the 52 MB settled figure.

A regression in any guardrail metric blocks the merge regardless of
headline gains.

## Elevator pitch

> The terminal that swallows output floods fastest — `cat huge.log`, build
> logs, CI streams — and proves it with a benchmark harness on every change.

## What this positioning demands next

1. Re-run `bench/` on post-#21 main; record the loaded input-latency figure.
2. Add an efficiency variant to `bench/` (throughput per watt) so battery
   behavior is measured, not assumed.
3. Surface the headline numbers in `README.md` with a link here and to
   `bench/METHODOLOGY.md` — claims without reproducible numbers are marketing.
