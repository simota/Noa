//! `dev/scaffold/registry.json` (I1): the human decisions. Read with the
//! assets validated; written only by 決定を記録 (Decision 4), in the
//! canonical format, and refused when the file changed since it was read.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Asset;
use crate::scan::source::{abs_path, read_capped};
use crate::scan::{PLATFORMS, REGISTRY};
use crate::time;

pub const CONFLICT: &str = "registry.json が外部で変更されました。再読込してください";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WidgetIntent {
    pub ships: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The file's content, in I1's key order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema: u32,
    pub widgets: BTreeMap<String, WidgetIntent>,
    pub assets: Vec<Asset>,
}

/// A registry as read, with the bytes it was read from (for the conflict
/// check on save).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub registry: Registry,
    pub bytes: Vec<u8>,
}

impl Loaded {
    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }

    /// The line of asset `id`'s `"id"` key.
    pub fn id_line(&self, id: &str) -> Option<u32> {
        line_of(self.text(), id)
    }
}

/// The line holding `"id": "<id>"`.
pub fn line_of(text: &str, id: &str) -> Option<u32> {
    let needle = format!("\"id\": \"{id}\"");
    text.lines()
        .position(|l| l.trim_start().starts_with(&needle))
        .map(|i| i as u32 + 1)
}

/// Why the registry was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// No `dev/scaffold/registry.json`: 未登録リポジトリ.
    Absent,
    Invalid(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Absent => write!(f, "registry.json がありません"),
            LoadError::Invalid(e) => write!(f, "registry.json を読めません: {e}"),
        }
    }
}

pub fn path(repo: &Path) -> PathBuf {
    abs_path(repo, REGISTRY)
}

fn is_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_relative_path(p: &str) -> bool {
    !p.is_empty() && !p.starts_with('/') && !p.split('/').any(|part| part == "..")
}

/// The checks of I1 that serde's types do not make.
pub fn validate(r: &Registry) -> Result<(), String> {
    if r.schema != 1 {
        return Err("schema is not 1".into());
    }
    for (name, w) in &r.widgets {
        if w.ships.is_empty() || w.ships.iter().any(|p| !PLATFORMS.contains(&p.as_str())) {
            return Err(format!(
                "ships of {name} is not a non-empty list of linux/macos"
            ));
        }
    }
    let mut seen = Vec::new();
    for a in &r.assets {
        if !is_id(&a.id) {
            return Err(format!("asset id {:?} is not [a-z0-9][a-z0-9-]*", a.id));
        }
        if seen.contains(&&a.id) {
            return Err(format!("asset id {} appears twice", a.id));
        }
        seen.push(&a.id);
        if !is_relative_path(&a.path) {
            return Err(format!("path of {} is not relative to the repo", a.id));
        }
        for d in &a.decisions {
            if !time::is_date(&d.at) {
                return Err(format!("decision of {} has date {:?}", a.id, d.at));
            }
            if d.reason.trim().is_empty() {
                return Err(format!("decision of {} has no reason", a.id));
            }
        }
    }
    Ok(())
}

/// Parses and validates registry bytes.
pub fn parse(bytes: &[u8]) -> Result<Registry, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_owned())?;
    let r: Registry = serde_json::from_str(text).map_err(|e| e.to_string())?;
    validate(&r)?;
    Ok(r)
}

pub fn load(repo: &Path) -> Result<Loaded, LoadError> {
    let bytes = match read_capped(&path(repo)) {
        Ok(b) => b,
        Err("missing") => return Err(LoadError::Absent),
        Err(e) => return Err(LoadError::Invalid(e.to_owned())),
    };
    let registry = parse(&bytes).map_err(LoadError::Invalid)?;
    Ok(Loaded { registry, bytes })
}

/// The canonical text of I1.
pub fn canonical(r: &Registry) -> String {
    let mut s = serde_json::to_string_pretty(r).unwrap_or_default();
    s.push('\n');
    s
}

/// Writes `registry` over the file `loaded` was read from, unless the file
/// changed since; through a temp file and a rename. `dev` and
/// `dev/scaffold` must be real folders.
pub fn save(repo: &Path, loaded: &Loaded, registry: &Registry) -> Result<Loaded, String> {
    validate(registry)?;
    for dir in ["dev", "dev/scaffold"] {
        let meta = std::fs::symlink_metadata(abs_path(repo, dir)).map_err(|e| e.to_string())?;
        if !meta.file_type().is_dir() {
            return Err(format!("{dir} がフォルダではありません"));
        }
    }
    let target = path(repo);
    match read_capped(&target) {
        Ok(now) if now == loaded.bytes => {}
        _ => return Err(CONFLICT.to_owned()),
    }
    let text = canonical(registry);
    let tmp = crate::store::temp_sibling(&target);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &target)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(Loaded {
        registry: registry.clone(),
        bytes: text.into_bytes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Decision, EvidenceKind, EvidenceRef, Lifecycle};

    const SAMPLE: &str = r#"{
  "schema": 1,
  "widgets": {
    "alpha": {
      "ships": [
        "linux",
        "macos"
      ]
    },
    "scaffold": {
      "ships": [
        "macos"
      ],
      "reason": "Linux では未確認。"
    }
  },
  "assets": [
    {
      "id": "check-widget-registration",
      "kind": "checker",
      "title": "ウィジェット登録の整合チェック",
      "path": "scripts/check-widget-registration.py",
      "decisions": [
        {
          "to": "trial",
          "at": "2026-11-26",
          "reason": "試す",
          "evidence": []
        }
      ]
    }
  ]
}
"#;

    fn repo_with(text: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("dev/scaffold")).unwrap();
        std::fs::write(path(dir.path()), text).unwrap();
        dir
    }

    #[test]
    fn the_canonical_format_round_trips() {
        let r = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(canonical(&r), SAMPLE);
        assert_eq!(r.assets[0].lifecycle(), Lifecycle::Trial);
        let dir = repo_with(SAMPLE);
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.id_line("check-widget-registration"), Some(19));
    }

    /// Loading and saving the committed registry gives identical bytes.
    #[test]
    fn registry_round_trip() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let file = path(&repo);
        let bytes = std::fs::read(&file).unwrap_or_else(|e| {
            panic!(
                "{} cannot be read ({e}): the committed registry (unit U1) must exist",
                file.display()
            )
        });
        let r = parse(&bytes).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert!(!r.widgets.is_empty(), "the registry lists no widget");
        assert_eq!(
            canonical(&r),
            String::from_utf8(bytes).unwrap(),
            "the committed registry is not in the canonical format"
        );
    }

    #[test]
    fn saving_writes_canonically_and_refuses_an_outside_change() {
        let dir = repo_with(SAMPLE);
        let loaded = load(dir.path()).unwrap();
        let mut r = loaded.registry.clone();
        r.assets[0]
            .decide(Decision {
                to: Lifecycle::Evaluating,
                at: "2026-11-27".into(),
                reason: "使えた".into(),
                evidence: vec![EvidenceRef {
                    kind: EvidenceKind::Path,
                    r#ref: "docs/x.md".into(),
                }],
            })
            .unwrap();
        let saved = save(dir.path(), &loaded, &r).unwrap();
        let on_disk = std::fs::read_to_string(path(dir.path())).unwrap();
        assert_eq!(on_disk, canonical(&r));
        assert!(on_disk.contains("\"ref\": \"docs/x.md\""));
        // The stale copy is refused; the fresh one is accepted.
        assert_eq!(save(dir.path(), &loaded, &r), Err(CONFLICT.to_owned()));
        assert!(save(dir.path(), &saved, &r).is_ok());
        // No temp file is left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path().join("dev/scaffold"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["registry.json"]);
    }

    #[test]
    fn invalid_registries_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Err(LoadError::Absent));
        let bad = [
            SAMPLE.replace("\"schema\": 1", "\"schema\": 2"),
            SAMPLE.replace("\"checker\"", "\"script\""),
            SAMPLE.replace("check-widget-registration\"", "Check\""),
            SAMPLE.replace("\"scripts/check", "\"../check"),
            SAMPLE.replace("2026-11-26", "26/11/2026"),
            SAMPLE.replace("\"試す\"", "\" \""),
            SAMPLE.replace("\"to\": \"trial\"", "\"to\": \"done\""),
            SAMPLE.replace(
                "\"evidence\": []",
                "\"evidence\": [{\"kind\": \"url\", \"ref\": \"x\"}]",
            ),
            SAMPLE.replace("\"schema\": 1,", "\"schema\": 1, \"extra\": 0,"),
            SAMPLE.replace("[\n        \"macos\"\n      ]", "[]"),
        ];
        for text in bad {
            let dir = repo_with(&text);
            assert!(
                matches!(load(dir.path()), Err(LoadError::Invalid(_))),
                "{text}"
            );
        }
    }

    #[test]
    fn a_linked_folder_is_not_written_through() {
        let dir = repo_with(SAMPLE);
        let loaded = load(dir.path()).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::remove_dir_all(dir.path().join("dev")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("dev")).unwrap();
        assert!(save(dir.path(), &loaded, &loaded.registry).is_err());
    }
}
