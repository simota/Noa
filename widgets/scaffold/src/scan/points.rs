//! The eight registration points of one widget (I3 "Points of widget w").

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::source::{Source, line_of, pat};
use super::{Evidence, Point, PointState, Sources, evidence_sorted};

/// The fact keys each point always carries (I2).
pub const FACT_KEYS: [(&str, &[&str]); 8] = [
    ("crate", &["binary", "accepts_repo"]),
    ("launch", &["const", "bundled", "repo_arg"]),
    ("command", &["variant", "action"]),
    ("menu", &[]),
    ("palette", &[]),
    ("verify", &["check"]),
    ("macos_bundle", &["name"]),
    ("linux_package", &["name"]),
];

fn keys(point: &str) -> &'static [&'static str] {
    FACT_KEYS
        .iter()
        .find(|(name, _)| *name == point)
        .map(|(_, k)| *k)
        .unwrap_or(&[])
}

fn make(
    state: PointState,
    facts: impl IntoIterator<Item = (&'static str, Value)>,
    missing: Vec<&str>,
    evidence: Vec<Evidence>,
) -> Point {
    Point {
        state,
        missing: missing.into_iter().map(str::to_owned).collect(),
        evidence: evidence_sorted(evidence),
        facts: facts
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// A point in `state` with every fact `null` and nothing else.
fn empty(name: &str, state: PointState) -> Point {
    make(
        state,
        keys(name).iter().map(|k| (*k, Value::Null)),
        Vec::new(),
        Vec::new(),
    )
}

fn opt(s: Option<&str>) -> Value {
    s.map_or(Value::Null, |s| Value::String(s.to_owned()))
}

/// `parts` without those in `found`, in order.
fn missing<'a>(parts: &[&'a str], found: &[&str]) -> Vec<&'a str> {
    parts
        .iter()
        .copied()
        .filter(|p| !found.contains(p))
        .collect()
}

/// A widget's `Cargo.toml`, with its package line, package name and binary.
pub struct Cargo {
    pub source: Source,
    pub package_line: Option<u32>,
    pub binary: Option<String>,
}

/// The first `^name = "…"$` in the table that starts after line index `at`.
fn table_name(src: &Source, at: usize) -> Option<(u32, &str)> {
    for (i, line) in src.lines.iter().enumerate().skip(at + 1) {
        if line.starts_with('[') {
            return None;
        }
        if let Some(name) = pat::toml_name(line) {
            return Some((i as u32 + 1, name));
        }
    }
    None
}

pub fn read_cargo(repo: &Path, widget: &str) -> Cargo {
    let mut source = Source::load(repo, &format!("widgets/{widget}/Cargo.toml"));
    if !source.ok() {
        return Cargo {
            source,
            package_line: None,
            binary: None,
        };
    }
    let package = source
        .lines
        .iter()
        .position(|l| l == "[package]")
        .and_then(|at| table_name(&source, at))
        .map(|(n, name)| (n, name.to_owned()));
    let Some((line, name)) = package else {
        source.fail_anchor("[package] name");
        return Cargo {
            source,
            package_line: None,
            binary: None,
        };
    };
    source.anchor = line;
    let binary = source
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| *l == "[[bin]]")
        .find_map(|(at, _)| table_name(&source, at).map(|(_, n)| n.to_owned()))
        .unwrap_or(name);
    Cargo {
        source,
        package_line: Some(line),
        binary: Some(binary),
    }
}

/// `(line, names)` of the `for widget in …; do` line of `bundle-macos.sh`.
pub fn for_widget_line(src: &Source) -> Option<(u32, Vec<&str>)> {
    src.numbered()
        .find_map(|(n, l)| pat::for_widget(l).map(|list| (n, list.split(' ').collect())))
}

/// `(line of WIDGETS=(, the numbered lines before the closing ")")`.
pub fn package_block(src: &Source) -> Option<(u32, Vec<(u32, &str)>)> {
    let start = src.lines.iter().position(|l| l == "WIDGETS=(")?;
    let end = (start + 1..src.lines.len()).find(|i| src.lines[*i] == ")")?;
    Some((
        start as u32 + 1,
        (start + 1..end)
            .map(|i| (i as u32 + 1, src.lines[i].as_str()))
            .collect(),
    ))
}

fn crate_point(cargo: &Cargo, main: &Source) -> Point {
    if !cargo.source.ok() || !main.ok() {
        return empty("crate", PointState::Unknown);
    }
    let mut evidence = vec![Evidence::new(
        &cargo.source.path,
        cargo.package_line.unwrap_or(1),
    )];
    let accepts = main.first_line("\"--repo=\"");
    if let Some(n) = accepts {
        evidence.push(Evidence::new(&main.path, n));
    }
    make(
        PointState::Present,
        [
            ("binary", opt(cargo.binary.as_deref())),
            ("accepts_repo", Value::Bool(accepts.is_some())),
        ],
        Vec::new(),
        evidence,
    )
}

/// The point, and the const's name when present.
fn launch_point(widget: &str, helper: &Source) -> (Point, Option<String>) {
    if !helper.ok() {
        return (empty("launch", PointState::Unknown), None);
    }
    let text = helper.text();
    for (konst, body, body_at) in pat::helper_consts(&text) {
        let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
        let mut crate_at = None;
        for (key, value, at) in pat::fields(body) {
            fields.insert(key, value);
            if key == "crate_name" {
                crate_at = Some(body_at + at);
            }
        }
        if fields.get("crate_name") != Some(&widget) {
            continue;
        }
        let needle = format!("helper.crate_name == {konst}.crate_name");
        let repo_arg = helper
            .body("launch_args")
            .is_some_and(|b| b.iter().any(|(_, l)| l.contains(&needle)));
        let evidence = vec![Evidence::new(
            &helper.path,
            line_of(&text, crate_at.unwrap_or(body_at)),
        )];
        let point = make(
            PointState::Present,
            [
                ("const", Value::String(konst.to_owned())),
                ("bundled", opt(fields.get("bundled").copied())),
                ("repo_arg", Value::Bool(repo_arg)),
            ],
            Vec::new(),
            evidence,
        );
        return (point, Some(konst.to_owned()));
    }
    (empty("launch", PointState::Absent), None)
}

fn command_point(konst: Option<&str>, commands: &Source, command: &Source) -> Point {
    let Some(konst) = konst.filter(|_| commands.ok() && command.ok()) else {
        return empty("command", PointState::Unknown);
    };
    let text = commands.text();
    let Some((variant, at)) = pat::dispatch(&text, konst) else {
        return empty("command", PointState::Absent);
    };
    let mut evidence = vec![Evidence::new(&commands.path, line_of(&text, at))];
    let mut found = vec!["dispatch"];
    let body = |name: &str| command.body(name).unwrap_or_default();

    let action = body("fixed_action_name")
        .into_iter()
        .find_map(|(n, l)| pat::fixed_action(l, variant).map(|a| (n, a)));
    if let Some((n, action)) = action {
        found.push("action");
        evidence.push(Evidence::new(&command.path, n));
        let quoted = format!("\"{action}\"");
        let arm = format!("=> Some(Self::{variant})");
        if body("from_action_name")
            .iter()
            .any(|(_, l)| l.contains(&quoted) && l.contains(&arm))
        {
            found.push("action-lookup");
        }
    }
    let prefix = format!("AppCommand::{variant} => Self::");
    let menu_const = body("menu_id")
        .into_iter()
        .find_map(|(_, l)| pat::search_after(l, &prefix, pat::upper_snake).map(|m| m.0));
    if let Some(m) = menu_const {
        found.push("menu-id");
        let arm = format!("Self::{m} => Some(Self::{variant})");
        if body("from_menu_id").iter().any(|(_, l)| l.contains(&arm)) {
            found.push("menu-id-lookup");
        }
    }
    let missing = missing(
        &["action", "action-lookup", "menu-id", "menu-id-lookup"],
        &found,
    );
    let state = if missing.is_empty() {
        PointState::Present
    } else {
        PointState::Partial
    };
    make(
        state,
        [
            ("variant", Value::String(variant.to_owned())),
            ("action", opt(action.map(|a| a.1))),
        ],
        missing,
        evidence,
    )
}

fn menu_point(variant: Option<&str>, menu: &Source) -> Point {
    let Some(variant) = variant.filter(|_| menu.ok()) else {
        return empty("menu", PointState::Unknown);
    };
    match menu.first_line(&format!("AppCommand::{variant}.menu_id()")) {
        Some(n) => make(
            PointState::Present,
            [],
            Vec::new(),
            vec![Evidence::new(&menu.path, n)],
        ),
        None => empty("menu", PointState::Absent),
    }
}

fn palette_point(variant: Option<&str>, palette: &Source) -> Point {
    let Some(variant) = variant.filter(|_| palette.ok()) else {
        return empty("palette", PointState::Unknown);
    };
    let mut evidence = Vec::new();
    let entry = palette
        .body("command_palette_entries")
        .unwrap_or_default()
        .into_iter()
        .find(|(_, l)| pat::palette_entry(l, variant));
    if let Some((n, _)) = entry {
        evidence.push(Evidence::new(&palette.path, n));
    }
    let arm = format!("AppCommand::{variant} => \"");
    let title = palette
        .body("command_palette_title")
        .unwrap_or_default()
        .into_iter()
        .find(|(_, l)| l.contains(&arm));
    if let Some((n, _)) = title {
        evidence.push(Evidence::new(&palette.path, n));
    }
    match (entry, title) {
        (None, _) => make(PointState::Absent, [], Vec::new(), evidence),
        (Some(_), None) => make(PointState::Partial, [], vec!["title"], evidence),
        (Some(_), Some(_)) => make(PointState::Present, [], Vec::new(), evidence),
    }
}

fn verify_point(widget: &str, verify: &Source) -> Point {
    if !verify.ok() {
        return empty("verify", PointState::Unknown);
    }
    let mut evidence = Vec::new();
    let var = verify
        .numbered()
        .find_map(|(n, l)| pat::manifest_var(l, widget).map(|v| (n, v)));
    let check = var.and_then(|(n, var)| {
        evidence.push(Evidence::new(&verify.path, n));
        verify
            .numbered()
            .find_map(|(n, l)| pat::full_run(l, var).map(|c| (n, c)))
    });
    let (Some((_, var)), Some((n, check))) = (var, check) else {
        return make(
            PointState::Absent,
            [("check", Value::Null)],
            Vec::new(),
            evidence,
        );
    };
    evidence.push(Evidence::new(&verify.path, n));
    let mut found = vec!["manifest", "full-run"];
    let alternative = format!("widgets/{widget}/*");
    let uses = format!("widget_uses \"${var}\"");
    for line in &verify.lines {
        if pat::case_path(line).is_some_and(|alts| alts.split('|').any(|a| a.trim() == alternative))
        {
            found.push("changed-path");
        }
        if pat::changed_run(line, var) {
            found.push("changed-run");
        }
        if line.contains(&uses) {
            found.push("widget-uses");
        }
    }
    let missing = missing(&["changed-path", "changed-run", "widget-uses"], &found);
    let state = if missing.is_empty() {
        PointState::Present
    } else {
        PointState::Partial
    };
    make(
        state,
        [("check", Value::String(check.to_owned()))],
        missing,
        evidence,
    )
}

fn bundle_point(widget: &str, bundle: &Source) -> Point {
    if !bundle.ok() {
        return empty("macos_bundle", PointState::Unknown);
    }
    let var = bundle.lines.iter().find_map(|l| pat::binary_var(l, widget));
    let copy = var.and_then(|var| {
        bundle
            .numbered()
            .find_map(|(n, l)| pat::copy_name(l, var).map(|name| (n, name)))
    });
    let (Some(var), Some((copy_line, name))) = (var, copy) else {
        return empty("macos_bundle", PointState::Absent);
    };
    let mut found = vec!["binary", "copy"];
    if for_widget_line(bundle).is_some_and(|(_, list)| list.contains(&widget)) {
        found.push("build");
    }
    let check = format!("[ -x \"${var}\" ]");
    let sign = format!("--sign \"$NOA_SIGN_IDENTITY\" \"$CONTENTS/Helpers/{name}\"");
    if bundle.lines.iter().any(|l| l.contains(&check)) {
        found.push("check");
    }
    if bundle.lines.iter().any(|l| l.contains(&sign)) {
        found.push("sign");
    }
    let missing = missing(&["build", "check", "sign"], &found);
    let state = if missing.is_empty() {
        PointState::Present
    } else {
        PointState::Partial
    };
    make(
        state,
        [("name", Value::String(name.to_owned()))],
        missing,
        vec![Evidence::new(&bundle.path, copy_line)],
    )
}

fn package_point(widget: &str, package: &Source) -> Point {
    if !package.ok() {
        return empty("linux_package", PointState::Unknown);
    }
    let item = package_block(package).and_then(|(_, lines)| {
        lines.into_iter().find_map(|(n, l)| {
            pat::package_item(l)
                .filter(|(w, _)| *w == widget)
                .map(|(_, name)| (n, name.to_owned()))
        })
    });
    match item {
        Some((n, name)) => make(
            PointState::Present,
            [("name", Value::String(name))],
            Vec::new(),
            vec![Evidence::new(&package.path, n)],
        ),
        None => empty("linux_package", PointState::Absent),
    }
}

/// All eight points of `widget`, keyed by point name.
pub fn widget_points(
    widget: &str,
    cargo: &Cargo,
    main: &Source,
    s: &Sources,
) -> BTreeMap<String, Point> {
    let (launch, konst) = launch_point(widget, &s.helper);
    let command = command_point(konst.as_deref(), &s.commands, &s.command);
    let variant = command.fact("variant").map(str::to_owned);
    let variant = variant.as_deref();
    [
        ("crate", crate_point(cargo, main)),
        ("launch", launch),
        ("command", command.clone()),
        ("menu", menu_point(variant, &s.menu)),
        ("palette", palette_point(variant, &s.palette)),
        ("verify", verify_point(widget, &s.verify)),
        ("macos_bundle", bundle_point(widget, &s.bundle)),
        ("linux_package", package_point(widget, &s.package)),
    ]
    .into_iter()
    .map(|(k, p)| (k.to_owned(), p))
    .collect()
}
