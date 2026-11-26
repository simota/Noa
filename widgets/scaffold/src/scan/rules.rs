//! Findings and explained differences from a widget's points and intent
//! (I3 "Rules").

use std::collections::BTreeMap;

use super::points::Cargo;
use super::source::{Source, pat};
use super::{
    Evidence, Explained, Finding, Intent, PLATFORMS, Point, PointState, REGISTRY, Sources,
    evidence_sorted,
};

#[derive(Debug, Default)]
pub struct Found {
    pub findings: Vec<Finding>,
    pub explained: Vec<Explained>,
}

impl Found {
    pub fn add(
        &mut self,
        rule: &str,
        widget: &str,
        target: &str,
        evidence: Vec<Evidence>,
        parts: Vec<String>,
    ) {
        self.findings.push(Finding {
            rule: rule.to_owned(),
            widget: widget.to_owned(),
            target: target.to_owned(),
            parts,
            evidence: evidence_sorted(evidence),
        });
    }
}

/// The line of a widget's registry entry: the first `^\s*"key"\s*:` after
/// the `"widgets"` line, else that line.
pub fn entry_line(registry: &Source, key: &str) -> u32 {
    registry
        .numbered()
        .skip(registry.anchor as usize)
        .find(|(_, l)| pat::json_key_line(l, key))
        .map_or(registry.anchor, |(n, _)| n)
}

pub struct Input<'a> {
    pub widget: &'a str,
    pub cargo: &'a Cargo,
    pub points: &'a BTreeMap<String, Point>,
    pub intent: Option<&'a Intent>,
    /// The registry source when it was read and is valid.
    pub registry: Option<&'a Source>,
    pub fixed: &'a Sources,
}

fn anchor(s: &Source) -> Evidence {
    Evidence::new(&s.path, s.anchor)
}

pub fn widget_rules(found: &mut Found, input: &Input) {
    let Input {
        widget,
        cargo,
        points,
        intent,
        registry,
        fixed,
    } = *input;
    let point = |name: &str| &points[name];
    let crate_p = point("crate");
    let crate_evidence = crate_p.evidence.clone();
    let package_evidence: Vec<Evidence> = match (cargo.source.ok(), cargo.package_line) {
        (true, Some(n)) => vec![Evidence::new(&cargo.source.path, n)],
        _ => Vec::new(),
    };

    for (target, source) in [
        ("launch", &fixed.helper),
        ("command", &fixed.commands),
        ("menu", &fixed.menu),
        ("palette", &fixed.palette),
        ("verify", &fixed.verify),
    ] {
        if point(target).state == PointState::Absent {
            let mut evidence = package_evidence.clone();
            evidence.push(anchor(source));
            found.add("missing-registration", widget, target, evidence, Vec::new());
        }
    }
    for (target, source) in [
        ("command", &fixed.command),
        ("palette", &fixed.palette),
        ("verify", &fixed.verify),
        ("macos_bundle", &fixed.bundle),
    ] {
        let p = point(target);
        if p.state == PointState::Partial {
            let mut evidence = p.evidence.clone();
            evidence.push(anchor(source));
            found.add(
                "incomplete-registration",
                widget,
                target,
                evidence,
                p.missing.clone(),
            );
        }
    }
    if crate_p.state != PointState::Unknown && crate_p.fact("binary") != Some(widget) {
        found.add(
            "name-mismatch",
            widget,
            "crate",
            crate_evidence.clone(),
            Vec::new(),
        );
    }
    let launch = point("launch");
    if launch.state == PointState::Present {
        for target in ["macos_bundle", "linux_package"] {
            let p = point(target);
            if p.state == PointState::Unknown {
                continue;
            }
            if let (Some(name), Some(bundled)) = (p.fact("name"), launch.fact("bundled"))
                && name != bundled
            {
                let mut evidence = launch.evidence.clone();
                evidence.extend(p.evidence.iter().cloned());
                found.add("name-mismatch", widget, target, evidence, Vec::new());
            }
        }
        if crate_p.state != PointState::Unknown
            && crate_p.flag("accepts_repo") != launch.flag("repo_arg")
        {
            let mut evidence = crate_evidence.clone();
            evidence.push(anchor(&fixed.helper));
            found.add("repo-arg-mismatch", widget, "launch", evidence, Vec::new());
        }
    }

    let Some(registry) = registry else {
        return;
    };
    let Some(intent) = intent else {
        let mut evidence = crate_evidence;
        evidence.push(anchor(registry));
        found.add("no-intent", widget, "registry", evidence, Vec::new());
        return;
    };
    let entry = Evidence::new(REGISTRY, entry_line(registry, widget));
    let ships = &intent.ships;
    for (platform, point_name, script) in [
        ("linux", "linux_package", &fixed.package),
        ("macos", "macos_bundle", &fixed.bundle),
    ] {
        let p = point(point_name);
        if p.state == PointState::Unknown {
            continue;
        }
        let shipped = p.state == PointState::Present
            || (platform == "macos" && p.state == PointState::Partial);
        let intended = ships.iter().any(|s| s == platform);
        if intended && !shipped {
            found.add(
                "ship-missing",
                widget,
                platform,
                vec![entry.clone(), anchor(script)],
                Vec::new(),
            );
        }
        if !intended && shipped {
            let mut evidence = vec![entry.clone()];
            evidence.extend(p.evidence.iter().cloned());
            found.add("ship-unintended", widget, platform, evidence, Vec::new());
        }
    }
    if ships.iter().map(String::as_str).ne(PLATFORMS) {
        for platform in PLATFORMS {
            if ships.iter().any(|s| s == platform) {
                continue;
            }
            match intent.reason.as_deref().filter(|r| !r.is_empty()) {
                None => found.add(
                    "os-difference-unexplained",
                    widget,
                    platform,
                    vec![entry.clone()],
                    Vec::new(),
                ),
                Some(reason) => found.explained.push(Explained {
                    rule: "os-difference".to_owned(),
                    widget: widget.to_owned(),
                    target: platform.to_owned(),
                    reason: reason.to_owned(),
                    evidence: vec![entry.clone()],
                }),
            }
        }
    }
}
