//! The registration scan: a port of `scripts/check-widget-registration.py`
//! (I3) that reads files only. [`scan`] gives the report the checker prints
//! with `--json` (I2); `tests/registration_parity.rs` holds the two to the
//! same fixtures.

pub mod points;
pub mod rules;
pub mod source;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use source::Source;

pub const SCOPE: &str =
    "Checked: registration consistency only. Not checked: the contents of built artifacts.";

pub const REGISTRY: &str = "dev/scaffold/registry.json";
pub const HELPER: &str = "crates/noa-app/src/helper_program.rs";
pub const APP_COMMANDS: &str = "crates/noa-app/src/app/commands.rs";
pub const COMMAND: &str = "crates/noa-app/src/commands/command.rs";
pub const MENU: &str = "crates/noa-app/src/macos_menu.rs";
pub const PALETTE: &str = "crates/noa-app/src/command_palette.rs";
pub const VERIFY: &str = "scripts/verify.sh";
pub const BUNDLE: &str = "scripts/bundle-macos.sh";
pub const PACKAGE: &str = "scripts/package-linux.sh";

pub const PLATFORMS: [&str; 2] = ["linux", "macos"];

/// Every rule id of I3.
pub const RULES: [&str; 10] = [
    "missing-registration",
    "incomplete-registration",
    "name-mismatch",
    "repo-arg-mismatch",
    "no-intent",
    "stale-intent",
    "ship-missing",
    "ship-unintended",
    "os-difference-unexplained",
    "os-difference",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Evidence {
    pub path: String,
    pub line: u32,
}

impl Evidence {
    pub fn new(path: &str, line: u32) -> Evidence {
        Evidence {
            path: path.to_owned(),
            line,
        }
    }
}

/// Sorted by `(path, line)`, duplicates removed.
pub fn evidence_sorted(mut items: Vec<Evidence>) -> Vec<Evidence> {
    items.sort();
    items.dedup();
    items
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceState {
    Ok,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReport {
    pub path: String,
    pub state: SourceState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointState {
    Present,
    Partial,
    Absent,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub state: PointState,
    pub missing: Vec<String>,
    pub evidence: Vec<Evidence>,
    /// The point's fact keys (I2), `null` when not found.
    #[serde(flatten)]
    pub facts: BTreeMap<String, serde_json::Value>,
}

impl Point {
    /// A string fact.
    pub fn fact(&self, key: &str) -> Option<&str> {
        self.facts.get(key).and_then(|v| v.as_str())
    }

    /// A boolean fact.
    pub fn flag(&self, key: &str) -> Option<bool> {
        self.facts.get(key).and_then(|v| v.as_bool())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub ships: Vec<String>,
    pub reason: Option<String>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetReport {
    pub widget: String,
    pub intent: Option<Intent>,
    pub points: BTreeMap<String, Point>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: String,
    pub widget: String,
    pub target: String,
    pub parts: Vec<String>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explained {
    pub rule: String,
    pub widget: String,
    pub target: String,
    pub reason: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    pub scope: String,
    pub status: String,
    pub exit: u8,
    pub sources: Vec<SourceReport>,
    pub widgets: Vec<WidgetReport>,
    pub findings: Vec<Finding>,
    pub explained: Vec<Explained>,
}

impl Report {
    /// Whether `dev/scaffold/registry.json` was read and is valid.
    pub fn has_registry(&self) -> bool {
        self.sources
            .iter()
            .any(|s| s.path == REGISTRY && s.state == SourceState::Ok)
    }

    /// Whether any source is unknown.
    pub fn has_unknown(&self) -> bool {
        self.sources.iter().any(|s| s.state == SourceState::Unknown)
    }

    /// 「整合 n/m」: `(n, m)`, m widgets and n with no finding naming them;
    /// only with a registry and no unknown source (AC5, Q5).
    pub fn score(&self) -> Option<(usize, usize)> {
        if !self.has_registry() || self.has_unknown() {
            return None;
        }
        let total = self.widgets.len();
        let clean = self
            .widgets
            .iter()
            .filter(|w| !self.findings.iter().any(|f| f.widget == w.widget))
            .count();
        Some((clean, total))
    }
}

/// The registry's widget entries, as far as the checker reads them.
pub struct RegistryWidgets {
    pub source: Source,
    /// `None` when the source is unknown.
    pub widgets: Option<BTreeMap<String, Entry>>,
}

/// One `widgets` entry: its platforms (sorted, distinct) and raw reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub ships: Vec<String>,
    pub reason: Option<String>,
}

/// Reads the registry with the checker's checks (I3 table, first row).
pub fn read_registry(repo: &Path) -> RegistryWidgets {
    let mut src = Source::load(repo, REGISTRY);
    if !src.ok() {
        return RegistryWidgets {
            source: src,
            widgets: None,
        };
    }
    let Ok(data) = serde_json::from_str::<serde_json::Value>(&src.text()) else {
        src.error = Some("invalid JSON".into());
        return RegistryWidgets {
            source: src,
            widgets: None,
        };
    };
    src.anchor = src.first_line("\"widgets\"").unwrap_or(1);
    match registry_widgets(&data) {
        Ok(widgets) => RegistryWidgets {
            source: src,
            widgets: Some(widgets),
        },
        Err(e) => {
            src.error = Some(format!("invalid registry: {e}"));
            RegistryWidgets {
                source: src,
                widgets: None,
            }
        }
    }
}

fn registry_widgets(data: &serde_json::Value) -> Result<BTreeMap<String, Entry>, String> {
    let schema = data.get("schema");
    if !data.is_object() || schema.and_then(|s| s.as_i64()) != Some(1) {
        return Err("schema is not 1".into());
    }
    let Some(widgets) = data.get("widgets").and_then(|w| w.as_object()) else {
        return Err("widgets is not an object".into());
    };
    let mut names: Vec<&String> = widgets.keys().collect();
    names.sort();
    let mut out = BTreeMap::new();
    for name in names {
        let entry = &widgets[name];
        let ships = entry
            .get("ships")
            .and_then(|s| s.as_array())
            .filter(|s| !s.is_empty())
            .and_then(|s| {
                s.iter()
                    .map(|p| p.as_str().filter(|p| PLATFORMS.contains(p)))
                    .collect::<Option<Vec<_>>>()
            });
        let Some(mut ships) = ships.filter(|_| entry.is_object()) else {
            return Err(format!(
                "ships of {name} is not a non-empty list of linux/macos"
            ));
        };
        let reason = match entry.get("reason") {
            None => None,
            Some(serde_json::Value::String(r)) => Some(r.clone()),
            Some(_) => return Err(format!("reason of {name} is not a string")),
        };
        ships.sort();
        ships.dedup();
        out.insert(
            name.clone(),
            Entry {
                ships: ships.into_iter().map(str::to_owned).collect(),
                reason,
            },
        );
    }
    if !data.get("assets").is_some_and(|a| a.is_array()) {
        return Err("assets is not a list".into());
    }
    Ok(out)
}

/// The widgets: direct child folders of `widgets/` (not links, not hidden)
/// holding a `Cargo.toml`, sorted; and the `widgets` source.
pub fn discover_widgets(repo: &Path) -> (Source, Vec<String>) {
    let root = repo.join("widgets");
    let meta = match std::fs::symlink_metadata(&root) {
        Ok(meta) => meta,
        Err(_) => return (Source::bare("widgets", Some("missing")), Vec::new()),
    };
    if !meta.file_type().is_dir() {
        return (Source::bare("widgets", Some("not a directory")), Vec::new());
    }
    let Ok(children) = std::fs::read_dir(&root) else {
        return (Source::bare("widgets", Some("missing")), Vec::new());
    };
    let mut names: Vec<String> = children
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            let path = root.join(&name);
            let dir = std::fs::symlink_metadata(&path).ok()?.file_type().is_dir();
            let cargo = std::fs::symlink_metadata(path.join("Cargo.toml")).is_ok();
            (dir && cargo).then_some(name)
        })
        .collect();
    names.sort();
    (Source::bare("widgets", None), names)
}

/// The fixed sources, each read and anchored.
pub struct Sources {
    pub helper: Source,
    pub commands: Source,
    pub command: Source,
    pub menu: Source,
    pub palette: Source,
    pub verify: Source,
    pub bundle: Source,
    pub package: Source,
}

impl Sources {
    pub fn read(repo: &Path) -> Sources {
        let anchored = |rel: &str, names: &[&str]| {
            let mut s = Source::load(repo, rel);
            if s.ok() {
                s.anchor_functions(names);
            }
            s
        };
        let mut helper = anchored(HELPER, &["launch_args"]);
        if helper.ok() && source::pat::helper_consts(&helper.text()).is_empty() {
            helper.fail_anchor("HelperProgram const");
        }
        let line_anchor = |rel: &str, needle: &str| {
            let mut s = Source::load(repo, rel);
            if s.ok() {
                match s.first_line(needle) {
                    Some(n) => s.anchor = n,
                    None => s.fail_anchor(needle),
                }
            }
            s
        };
        let mut bundle = Source::load(repo, BUNDLE);
        if bundle.ok() {
            match points::for_widget_line(&bundle) {
                Some((n, _)) => bundle.anchor = n,
                None => bundle.fail_anchor("for widget in"),
            }
        }
        let mut package = Source::load(repo, PACKAGE);
        if package.ok() {
            match points::package_block(&package) {
                Some((n, _)) => package.anchor = n,
                None => package.fail_anchor("WIDGETS=("),
            }
        }
        Sources {
            helper,
            commands: anchored(APP_COMMANDS, &["open_helper_program"]),
            command: anchored(
                COMMAND,
                &[
                    "menu_id",
                    "from_menu_id",
                    "fixed_action_name",
                    "from_action_name",
                ],
            ),
            menu: line_anchor(MENU, "MenuItem::with_id"),
            palette: anchored(
                PALETTE,
                &["command_palette_title", "command_palette_entries"],
            ),
            verify: line_anchor(VERIFY, "widget_check()"),
            bundle,
            package,
        }
    }

    fn into_vec(self) -> Vec<Source> {
        vec![
            self.helper,
            self.commands,
            self.command,
            self.menu,
            self.palette,
            self.verify,
            self.bundle,
            self.package,
        ]
    }
}

/// Scans `repo` (I3) and gives the I2 report. Reads files only.
pub fn scan(repo: &Path) -> Report {
    let registry = read_registry(repo);
    let (widgets_src, names) = discover_widgets(repo);
    let fixed = Sources::read(repo);

    let mut reports = Vec::new();
    let mut found = rules::Found::default();
    let mut extra_sources = Vec::new();
    for widget in &names {
        let cargo = points::read_cargo(repo, widget);
        let main = Source::load(repo, &format!("widgets/{widget}/src/main.rs"));
        let pts = points::widget_points(widget, &cargo, &main, &fixed);
        let entry = registry
            .widgets
            .as_ref()
            .and_then(|w| w.get(widget.as_str()));
        let intent = entry.map(|e| Intent {
            ships: e.ships.clone(),
            reason: e.reason.as_deref().map(|r| r.trim().to_owned()),
            evidence: vec![Evidence::new(
                REGISTRY,
                rules::entry_line(&registry.source, widget),
            )],
        });
        rules::widget_rules(
            &mut found,
            &rules::Input {
                widget,
                cargo: &cargo,
                points: &pts,
                intent: intent.as_ref(),
                registry: registry.widgets.as_ref().map(|_| &registry.source),
                fixed: &fixed,
            },
        );
        reports.push(WidgetReport {
            widget: widget.clone(),
            intent,
            points: pts,
        });
        extra_sources.push(cargo.source);
        extra_sources.push(main);
    }
    if let Some(entries) = &registry.widgets
        && widgets_src.ok()
    {
        for key in entries.keys() {
            if !names.contains(key) {
                found.add(
                    "stale-intent",
                    key,
                    "registry",
                    vec![Evidence::new(
                        REGISTRY,
                        rules::entry_line(&registry.source, key),
                    )],
                    Vec::new(),
                );
            }
        }
    }

    let mut sources = vec![registry.source, widgets_src];
    sources.extend(fixed.into_vec());
    sources.extend(extra_sources);
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    let unknown = sources.iter().any(|s| !s.ok());
    let rules::Found {
        mut findings,
        mut explained,
    } = found;
    findings.sort_by(|a, b| (&a.widget, &a.rule, &a.target).cmp(&(&b.widget, &b.rule, &b.target)));
    explained.sort_by(|a, b| (&a.widget, &a.rule, &a.target).cmp(&(&b.widget, &b.rule, &b.target)));
    let exit = if unknown {
        2
    } else if findings.is_empty() {
        0
    } else {
        1
    };
    Report {
        schema: 1,
        scope: SCOPE.to_owned(),
        status: ["consistent", "inconsistent", "unknown"][exit as usize].to_owned(),
        exit,
        sources: sources
            .into_iter()
            .map(|s| SourceReport {
                path: s.path,
                state: if s.error.is_none() {
                    SourceState::Ok
                } else {
                    SourceState::Unknown
                },
                error: s.error,
            })
            .collect(),
        widgets: reports,
        findings,
        explained,
    }
}

#[cfg(test)]
mod tests;
