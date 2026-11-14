## Purpose

<!-- The issue or the need, in one or two sentences. -->

## What changed

<!-- The behavior that is different, not the list of files. -->

## Verification

<!-- `scripts/verify.sh --full`: its last line, and the test count if it changed.
     Any extra command run (a feature-map test, a smoke script) with its result. -->

## Not verified

<!-- What could not be run and why: GUI behaviour and visual parity, a GPU test
     that skipped, a platform not tried. Screenshots or recordings for visible changes. -->

## Risks

<!-- What could go wrong, and how it would show. -->

## Risk class

<!-- low: docs, tests, internal structure with the gate green
     medium: behavior inside one feature
     high: dependencies or Cargo.lock, vendor/, release signing, the VT parser or
           grid model, or the gate's own files (scripts/verify.sh, .github/, AGENTS.md) -->
