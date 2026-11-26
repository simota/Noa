//! The port on mini repos built in a temp dir. The shared oracle with the
//! Python checker is `tests/registration_parity.rs`; these tests pin each
//! rule here too, so the port is checked even before the fixtures exist.

use std::path::Path;

use super::*;

/// One widget of a mini repo: where it is registered.
#[derive(Clone)]
pub struct W {
    pub name: &'static str,
    pub konst: &'static str,
    pub variant: &'static str,
    pub bundled: &'static str,
    pub accepts_repo: bool,
    pub repo_arg: bool,
    pub check: &'static str,
    pub launch: bool,
    pub verify: bool,
    pub widget_uses: bool,
    pub macos: bool,
    pub sign: bool,
    pub linux: bool,
    pub linux_name: Option<&'static str>,
}

pub fn w(name: &'static str, konst: &'static str, variant: &'static str) -> W {
    W {
        name,
        konst,
        variant,
        bundled: "",
        accepts_repo: false,
        repo_arg: false,
        check: "widget_check",
        launch: true,
        verify: true,
        widget_uses: true,
        macos: true,
        sign: true,
        linux: true,
        linux_name: None,
    }
}

fn bundled(w: &W) -> String {
    if w.bundled.is_empty() {
        let mut c = w.name.chars();
        let first = c.next().unwrap().to_ascii_uppercase();
        format!("Noa {first}{}", c.as_str())
    } else {
        w.bundled.to_owned()
    }
}

fn put(repo: &Path, rel: &str, text: &str) {
    let path = repo.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Writes a mini repo holding the I3 sources for `widgets` and `registry`.
pub fn mini_repo(repo: &Path, widgets: &[W], registry: Option<&str>) {
    let mut helper = String::from("use std::path::Path;\n\n");
    let mut launch_arms = String::new();
    let mut dispatch = String::new();
    let mut menu_ids = String::new();
    let mut menu_consts = String::new();
    let mut from_menu = String::new();
    let mut actions = String::new();
    let mut from_actions = String::new();
    let mut menu = String::new();
    let mut titles = String::new();
    let mut entries = String::new();
    let mut verify_vars = String::new();
    let mut verify_full = String::new();
    let mut verify_case = String::new();
    let mut verify_uses = String::new();
    let mut verify_changed = String::new();
    let mut bundle_list = Vec::new();
    let mut bundle_vars = String::new();
    let mut bundle_checks = String::new();
    let mut bundle_copies = String::new();
    let mut bundle_signs = String::new();
    let mut linux = String::new();
    for w in widgets {
        let name = w.name;
        let k = w.konst;
        let v = w.variant;
        let b = bundled(w);
        let menu_const = format!("{k}_MENU_ID");
        if w.launch {
            helper.push_str(&format!(
                "pub(crate) const {k}: HelperProgram = HelperProgram {{\n    title: \"{name}\",\n    bundled: \"{b}\",\n    crate_name: \"{name}\",\n}};\n\n"
            ));
        }
        if w.repo_arg {
            launch_arms.push_str(&format!(
                "                || helper.crate_name == {k}.crate_name\n"
            ));
        }
        dispatch.push_str(&format!(
            "            AppCommand::{v} => self.open_helper_program(crate::helper_program::{k}),\n"
        ));
        menu_consts.push_str(&format!(
            "    pub(crate) const {menu_const}: &'static str = \"noa.view.{name}\";\n"
        ));
        menu_ids.push_str(&format!(
            "            AppCommand::{v} => Self::{menu_const},\n"
        ));
        from_menu.push_str(&format!(
            "            Self::{menu_const} => Some(Self::{v}),\n"
        ));
        actions.push_str(&format!("            Self::{v} => \"{name}.open\",\n"));
        from_actions.push_str(&format!(
            "            \"{name}.open\" => Some(Self::{v}),\n"
        ));
        menu.push_str(&format!(
            "    let item = MenuItem::with_id(AppCommand::{v}.menu_id(), \"{name}\", true, None);\n"
        ));
        titles.push_str(&format!("        AppCommand::{v} => \"Open {name}\",\n"));
        entries.push_str(&format!("        AppCommand::{v},\n"));
        let var = name.replace('-', "_");
        if w.verify {
            verify_vars.push_str(&format!("{var}=widgets/{name}/Cargo.toml\n"));
            verify_full.push_str(&format!("  run {var} {} \"${var}\"\n", w.check));
            verify_case.push_str(&format!(
                "    case $path in widgets/{name}/*) c_{var}=1 ;; esac\n"
            ));
            if w.widget_uses {
                verify_uses.push_str(&format!(
                    "    if uses=$(widget_uses \"${var}\"); then\n      c_{var}=1\n    fi\n"
                ));
            }
            verify_changed.push_str(&format!(
                "  [ \"$c_{var}\" = 1 ] && run {var} {} \"${var}\"\n",
                w.check
            ));
        }
        if w.macos {
            let bv = format!("{}_BIN", var.to_ascii_uppercase());
            bundle_list.push(name);
            bundle_vars.push_str(&format!("{bv}=\"$TARGET_DIR/{name}\"\n"));
            bundle_checks.push_str(&format!(
                "[ -x \"${bv}\" ] || {{ echo \"error: no ${bv}\" >&2; exit 1; }}\n"
            ));
            bundle_copies.push_str(&format!("cp \"${bv}\" \"$CONTENTS/Helpers/{b}\"\n"));
            if w.sign {
                bundle_signs.push_str(&format!(
                    "  codesign --force \\\n    --sign \"$NOA_SIGN_IDENTITY\" \"$CONTENTS/Helpers/{b}\"\n"
                ));
            }
        }
        if w.linux {
            let ln = w.linux_name.map(str::to_owned).unwrap_or_else(|| b.clone());
            linux.push_str(&format!("  \"{name}:{ln}\"\n"));
        }
        let main = if w.accepts_repo {
            "fn main() {\n    let _ = \"--repo=\";\n}\n"
        } else {
            "fn main() {}\n"
        };
        put(repo, &format!("widgets/{name}/src/main.rs"), main);
        put(
            repo,
            &format!("widgets/{name}/Cargo.toml"),
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\n\n[workspace]\n\n[[bin]]\nname = \"{name}\"\npath = \"src/main.rs\"\n"
            ),
        );
    }
    helper.push_str(&format!(
        "pub(crate) fn launch_args(helper: HelperProgram, focused_cwd: Option<&str>) -> Vec<String> {{\n    match focused_cwd {{\n        Some(cwd)\n            if (false\n{launch_arms}                ) && Path::new(cwd).is_absolute() =>\n        {{\n            vec![format!(\"--repo={{cwd}}\")]\n        }}\n        _ => Vec::new(),\n    }}\n}}\n"
    ));
    put(repo, HELPER, &helper);
    put(
        repo,
        APP_COMMANDS,
        &format!(
            "impl App {{\n    fn run(&mut self, command: AppCommand) {{\n        match command {{\n{dispatch}            _ => {{}}\n        }}\n    }}\n\n    fn open_helper_program(&mut self, helper: HelperProgram) {{\n        let _ = helper;\n    }}\n}}\n"
        ),
    );
    put(
        repo,
        COMMAND,
        &format!(
            "impl AppCommand {{\n{menu_consts}\n    pub(crate) fn menu_id(self) -> &'static str {{\n        match self {{\n{menu_ids}            _ => \"\",\n        }}\n    }}\n\n    pub(crate) fn from_menu_id(id: &str) -> Option<Self> {{\n        match id {{\n{from_menu}            _ => None,\n        }}\n    }}\n\n    fn fixed_action_name(self) -> &'static str {{\n        match self {{\n{actions}            _ => \"\",\n        }}\n    }}\n\n    pub fn from_action_name(name: &str) -> Option<Self> {{\n        match name {{\n{from_actions}            _ => None,\n        }}\n    }}\n}}\n"
        ),
    );
    put(repo, MENU, &format!("fn view_menu() {{\n{menu}}}\n"));
    put(
        repo,
        PALETTE,
        &format!(
            "pub(crate) fn command_palette_title(command: AppCommand) -> &'static str {{\n    match command {{\n{titles}        _ => \"\",\n    }}\n}}\n\npub(crate) fn command_palette_entries() -> &'static [AppCommand] {{\n    &[\n{entries}    ]\n}}\n"
        ),
    );
    put(
        repo,
        VERIFY,
        &format!(
            "#!/bin/sh\n{verify_vars}widget_check() {{\n  cargo test --manifest-path \"$1\"\n}}\nusage_check() {{\n  cargo test --manifest-path \"$1\"\n}}\nif [ \"$full\" = 1 ]; then\n{verify_full}else\n  for path in $changed; do\n{verify_case}  done\n{verify_uses}{verify_changed}fi\n"
        ),
    );
    put(
        repo,
        BUNDLE,
        &format!(
            "#!/bin/sh\nfor widget in {}; do\n  cargo build --manifest-path \"widgets/$widget/Cargo.toml\"\ndone\n{bundle_vars}{bundle_checks}{bundle_copies}if [ -n \"$NOA_SIGN_IDENTITY\" ]; then\n{bundle_signs}fi\n",
            bundle_list.join(" ")
        ),
    );
    put(
        repo,
        PACKAGE,
        &format!("#!/bin/bash\nWIDGETS=(\n{linux})\n"),
    );
    if let Some(registry) = registry {
        put(repo, REGISTRY, registry);
    }
}

/// A registry with each `(widget, ships, reason)`.
pub fn registry(entries: &[(&str, &[&str], Option<&str>)]) -> String {
    let widgets: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .map(|(name, ships, reason)| {
            let mut e = serde_json::Map::new();
            e.insert("ships".into(), serde_json::json!(ships));
            if let Some(r) = reason {
                e.insert("reason".into(), serde_json::json!(r));
            }
            (name.to_string(), serde_json::Value::Object(e))
        })
        .collect();
    let v = serde_json::json!({"schema": 1, "widgets": widgets, "assets": []});
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

fn both() -> &'static [&'static str] {
    &["linux", "macos"]
}

fn clean_widgets() -> Vec<W> {
    let mut alpha = w("alpha", "ALPHA", "OpenAlpha");
    alpha.accepts_repo = true;
    alpha.repo_arg = true;
    let mut beta = w("beta", "BETA", "OpenBeta");
    beta.check = "usage_check";
    vec![alpha, beta]
}

fn clean_registry() -> String {
    registry(&[("alpha", both(), None), ("beta", both(), None)])
}

fn rules_of(report: &Report) -> Vec<(String, String, String)> {
    report
        .findings
        .iter()
        .map(|f| (f.widget.clone(), f.target.clone(), f.rule.clone()))
        .collect()
}

fn t(w: &str, target: &str, rule: &str) -> (String, String, String) {
    (w.into(), target.into(), rule.into())
}

#[test]
fn a_clean_repo_is_consistent() {
    let dir = tempfile::tempdir().unwrap();
    mini_repo(dir.path(), &clean_widgets(), Some(&clean_registry()));
    let r = scan(dir.path());
    assert_eq!(r.findings, [], "{:#?}", r.sources);
    assert_eq!((r.exit, r.status.as_str()), (0, "consistent"));
    assert_eq!(r.scope, SCOPE);
    assert_eq!(r.widgets.len(), 2);
    let alpha = &r.widgets[0].points;
    assert_eq!(alpha.len(), 8);
    for (name, p) in alpha {
        assert_eq!(p.state, PointState::Present, "{name}");
    }
    assert_eq!(alpha["launch"].fact("const"), Some("ALPHA"));
    assert_eq!(alpha["launch"].fact("bundled"), Some("Noa Alpha"));
    assert_eq!(alpha["launch"].flag("repo_arg"), Some(true));
    assert_eq!(alpha["crate"].flag("accepts_repo"), Some(true));
    assert_eq!(alpha["command"].fact("variant"), Some("OpenAlpha"));
    assert_eq!(alpha["command"].fact("action"), Some("alpha.open"));
    assert_eq!(
        r.widgets[1].points["verify"].fact("check"),
        Some("usage_check")
    );
    assert_eq!(r.score(), Some((2, 2)));
    // Sources by path, the widgets folder among them.
    let paths: Vec<&str> = r.sources.iter().map(|s| s.path.as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);
    assert!(paths.contains(&"widgets"));
    assert!(paths.contains(&"widgets/alpha/src/main.rs"));
    assert_eq!(paths.len(), 10 + 4);
}

#[test]
fn the_json_has_exactly_the_i2_keys() {
    let dir = tempfile::tempdir().unwrap();
    mini_repo(dir.path(), &clean_widgets(), Some(&clean_registry()));
    let v = serde_json::to_value(scan(dir.path())).unwrap();
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys(&v),
        [
            "exit",
            "explained",
            "findings",
            "schema",
            "scope",
            "sources",
            "status",
            "widgets"
        ]
    );
    let points = &v["widgets"][0]["points"];
    assert_eq!(
        keys(&points["crate"]),
        ["accepts_repo", "binary", "evidence", "missing", "state"]
    );
    assert_eq!(
        keys(&points["launch"]),
        [
            "bundled", "const", "evidence", "missing", "repo_arg", "state"
        ]
    );
    assert_eq!(keys(&points["menu"]), ["evidence", "missing", "state"]);
    assert_eq!(
        keys(&v["widgets"][0]["intent"]),
        ["evidence", "reason", "ships"]
    );
    assert_eq!(v["sources"][0]["error"], serde_json::Value::Null);
    // And it reads back as the same report (scan.json keeps it).
    let back: Report = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(serde_json::to_value(back).unwrap(), v);
}

#[test]
fn a_widget_with_no_launch_const_is_missing_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut ws = clean_widgets();
    let mut gamma = w("gamma", "GAMMA", "OpenGamma");
    gamma.launch = false;
    ws.push(gamma);
    let reg = registry(&[
        ("alpha", both(), None),
        ("beta", both(), None),
        ("gamma", both(), None),
    ]);
    mini_repo(dir.path(), &ws, Some(&reg));
    let r = scan(dir.path());
    assert_eq!(rules_of(&r), [t("gamma", "launch", "missing-registration")]);
    // The crate's package line and the anchor of helper_program.rs.
    let helper = std::fs::read_to_string(dir.path().join(HELPER)).unwrap();
    let anchor = helper
        .lines()
        .position(|l| l.contains("fn launch_args"))
        .unwrap() as u32
        + 1;
    assert_eq!(
        r.findings[0].evidence,
        [
            Evidence::new(HELPER, anchor),
            Evidence::new("widgets/gamma/Cargo.toml", 2)
        ]
    );
    let g = &r.widgets[2].points;
    assert_eq!(g["launch"].state, PointState::Absent);
    assert_eq!(g["command"].state, PointState::Unknown);
    assert_eq!(g["menu"].state, PointState::Unknown);
    assert_eq!(g["palette"].state, PointState::Unknown);
    assert_eq!(r.exit, 1);
    assert_eq!(r.score(), Some((2, 3)));
}

#[test]
fn missing_verify_and_packaging() {
    let dir = tempfile::tempdir().unwrap();
    let mut ws = clean_widgets();
    let mut gamma = w("gamma", "GAMMA", "OpenGamma");
    gamma.verify = false;
    gamma.macos = false;
    gamma.linux = false;
    ws.push(gamma);
    let reg = registry(&[
        ("alpha", both(), None),
        ("beta", both(), None),
        ("gamma", both(), None),
    ]);
    mini_repo(dir.path(), &ws, Some(&reg));
    let r = scan(dir.path());
    assert_eq!(
        rules_of(&r),
        [
            t("gamma", "verify", "missing-registration"),
            t("gamma", "linux", "ship-missing"),
            t("gamma", "macos", "ship-missing"),
        ]
    );
    let linux = &r.findings[1];
    assert_eq!(linux.evidence[0].path, REGISTRY);
    assert_eq!(linux.evidence[1], Evidence::new(PACKAGE, 2));
}

#[test]
fn os_differences_are_explained_by_a_reason_or_reported() {
    for (reason, explained) in [(Some("理由"), true), (None, false), (Some("  "), false)] {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = clean_widgets();
        ws[1].linux = false;
        let reg = registry(&[("alpha", both(), None), ("beta", &["macos"], reason)]);
        mini_repo(dir.path(), &ws, Some(&reg));
        let r = scan(dir.path());
        if explained {
            assert_eq!(r.findings, []);
            assert_eq!(r.explained.len(), 1);
            let e = &r.explained[0];
            assert_eq!(
                (e.widget.as_str(), e.target.as_str(), e.rule.as_str()),
                ("beta", "linux", "os-difference")
            );
            assert_eq!(e.reason, "理由");
            assert_eq!(r.exit, 0);
        } else {
            assert_eq!(
                rules_of(&r),
                [t("beta", "linux", "os-difference-unexplained")]
            );
            assert_eq!(r.exit, 1);
        }
    }
}

#[test]
fn every_remaining_rule_fires() {
    let dir = tempfile::tempdir().unwrap();
    let mut ws = clean_widgets();
    // alpha: bundled under another name, takes --repo without launch_args.
    ws[0].repo_arg = false;
    let mut alpha_bundle = ws[0].clone();
    alpha_bundle.linux_name = Some("Noa Alfa");
    ws[0] = alpha_bundle;
    // beta: not signed, no widget_uses; shipped on Linux unintended.
    ws[1].sign = false;
    ws[1].widget_uses = false;
    let delta = w("delta", "DELTA", "OpenDelta");
    ws.push(delta);
    let reg = registry(&[
        ("alpha", both(), None),
        ("beta", &["macos"], Some("macOS のみ")),
        ("zeta", both(), None),
    ]);
    mini_repo(dir.path(), &ws, Some(&reg));
    let r = scan(dir.path());
    assert_eq!(
        rules_of(&r),
        [
            t("alpha", "linux_package", "name-mismatch"),
            t("alpha", "launch", "repo-arg-mismatch"),
            t("beta", "macos_bundle", "incomplete-registration"),
            t("beta", "verify", "incomplete-registration"),
            t("beta", "linux", "ship-unintended"),
            t("delta", "registry", "no-intent"),
            t("zeta", "registry", "stale-intent"),
        ]
    );
    assert_eq!(r.findings[2].parts, ["sign"]);
    assert_eq!(r.findings[3].parts, ["widget-uses"]);
    assert_eq!(r.explained.len(), 1);
    let mut seen: Vec<&str> = r.findings.iter().map(|f| f.rule.as_str()).collect();
    seen.extend(r.explained.iter().map(|e| e.rule.as_str()));
    for rule in RULES {
        if ![
            "missing-registration",
            "ship-missing",
            "os-difference-unexplained",
        ]
        .contains(&rule)
        {
            assert!(seen.contains(&rule), "{rule}");
        }
    }
}

#[test]
fn unreadable_sources_make_the_report_unknown() {
    let dir = tempfile::tempdir().unwrap();
    mini_repo(dir.path(), &clean_widgets(), Some("{not json"));
    let r = scan(dir.path());
    assert_eq!((r.exit, r.status.as_str()), (2, "unknown"));
    let reg = r.sources.iter().find(|s| s.path == REGISTRY).unwrap();
    assert_eq!(reg.error.as_deref(), Some("invalid JSON"));
    assert!(r.widgets.iter().all(|w| w.intent.is_none()));
    assert_eq!(r.score(), None);
    assert!(!r.has_registry());

    let dir = tempfile::tempdir().unwrap();
    mini_repo(dir.path(), &clean_widgets(), Some(&clean_registry()));
    std::fs::write(dir.path().join(PACKAGE), "#!/bin/bash\necho\n").unwrap();
    let r = scan(dir.path());
    let pkg = r.sources.iter().find(|s| s.path == PACKAGE).unwrap();
    assert_eq!(pkg.error.as_deref(), Some("anchor not found: WIDGETS=("));
    assert_eq!(r.exit, 2);
    assert_eq!(
        r.widgets[0].points["linux_package"].state,
        PointState::Unknown
    );
    // No ship rule is evaluated on an unknown point.
    assert!(r.findings.iter().all(|f| f.target != "linux"));
}

#[test]
fn registry_checks_name_the_first_failure() {
    let cases = [
        (
            r#"{"schema": 2, "widgets": {}, "assets": []}"#,
            "schema is not 1",
        ),
        (
            r#"{"schema": true, "widgets": {}, "assets": []}"#,
            "schema is not 1",
        ),
        (r#"[1]"#, "schema is not 1"),
        (
            r#"{"schema": 1, "widgets": [], "assets": []}"#,
            "widgets is not an object",
        ),
        (
            r#"{"schema": 1, "widgets": {"b": {"ships": []}, "a": {"ships": ["win"]}}, "assets": []}"#,
            "ships of a is not a non-empty list of linux/macos",
        ),
        (
            r#"{"schema": 1, "widgets": {"a": {"ships": ["macos"], "reason": null}}, "assets": []}"#,
            "reason of a is not a string",
        ),
        (r#"{"schema": 1, "widgets": {}}"#, "assets is not a list"),
    ];
    for (text, error) in cases {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("dev/scaffold")).unwrap();
        std::fs::write(dir.path().join(REGISTRY), text).unwrap();
        let r = read_registry(dir.path());
        assert_eq!(
            r.source.error.as_deref(),
            Some(format!("invalid registry: {error}").as_str()),
            "{text}"
        );
        assert!(r.widgets.is_none());
    }
    // Duplicates and order are the checker's to tolerate: sorted, distinct.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("dev/scaffold")).unwrap();
    std::fs::write(
        dir.path().join(REGISTRY),
        r#"{"schema": 1, "widgets": {"a": {"ships": ["macos", "linux", "macos"]}}, "assets": []}"#,
    )
    .unwrap();
    let r = read_registry(dir.path());
    assert_eq!(r.widgets.unwrap()["a"].ships, ["linux", "macos"]);
}

#[test]
fn no_widgets_folder_is_an_unknown_source() {
    let dir = tempfile::tempdir().unwrap();
    let r = scan(dir.path());
    let w = r.sources.iter().find(|s| s.path == "widgets").unwrap();
    assert_eq!(w.error.as_deref(), Some("missing"));
    assert!(r.widgets.is_empty());
    assert_eq!(r.exit, 2);
}

#[test]
fn this_repository_scans_without_an_unknown_source() {
    // widgets/scaffold/ → the repository root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let r = scan(&root);
    let unknown: Vec<_> = r
        .sources
        .iter()
        .filter(|s| s.state == SourceState::Unknown && s.path != REGISTRY)
        .collect();
    assert_eq!(unknown, Vec::<&SourceReport>::new());
    let tasks = r.widgets.iter().find(|w| w.widget == "tasks").unwrap();
    for (name, p) in &tasks.points {
        let expected = if name == "linux_package" {
            PointState::Absent
        } else {
            PointState::Present
        };
        assert_eq!(p.state, expected, "tasks {name}");
    }
}
