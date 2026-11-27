//! A read-only inventory of the files that help someone start working in a
//! repository. A found file is evidence of an entry point, not of its quality
//! or of a successful build. The Noa registration checker stays independent.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use crate::hash;
use crate::model::{Asset, AssetKind};
use crate::scan::source::MAX_BYTES;
use crate::scan::{Evidence, HELPER, REGISTRY, SourceReport, SourceState, evidence_sorted};

/// A scan has a bounded amount of work even in an unfamiliar repository.
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 256;
const MAX_WORKFLOWS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckState {
    Found,
    Missing,
    Unknown,
}

impl CheckState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Found => "見つかりました",
            Self::Missing => "これから整える",
            Self::Unknown => "確認できません",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub id: String,
    pub title: String,
    pub description: String,
    pub state: CheckState,
    pub evidence: Vec<Evidence>,
    pub next_step: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    pub stacks: Vec<String>,
    pub checks: Vec<Check>,
    pub assets: Vec<Asset>,
    pub sources: Vec<SourceReport>,
    pub registration_applicable: bool,
}

impl Setup {
    pub fn found_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.state == CheckState::Found)
            .count()
    }

    pub fn attention_count(&self) -> usize {
        self.checks.len() - self.found_count()
    }

    /// Registration diagnostics belong to the separate Noa checker. Only an
    /// unreadable source used by a setup check is part of this work brief.
    fn unknown_evidence(&self) -> impl Iterator<Item = &SourceReport> {
        self.sources.iter().filter(|source| {
            source.state == SourceState::Unknown
                && self
                    .checks
                    .iter()
                    .any(|check| check.evidence.iter().any(|e| e.path == source.path))
        })
    }

    /// No timestamp, absolute path or directory iteration order enters the
    /// key: the same inventory can be handed off only once by the caller.
    pub fn task_key(&self) -> String {
        let checks: Vec<_> = self
            .checks
            .iter()
            .map(|c| {
                (
                    &c.id,
                    &c.title,
                    &c.description,
                    c.state.label(),
                    &c.evidence,
                    &c.next_step,
                )
            })
            .collect();
        let unknown_sources: Vec<_> = self.unknown_evidence().collect();
        let bytes =
            serde_json::to_vec(&(1, &self.stacks, checks, unknown_sources)).unwrap_or_default();
        format!("setup-{}", hash::hex(&bytes))
    }

    /// A Tasks-compatible brief. Repository-controlled file contents are not
    /// copied into instructions; the evidence consists of paths and lines.
    pub fn task_text(&self) -> String {
        let mut text = format!(
            "---\ntitle: [足場] 開発環境の導入を整える\nstate: open\nlabels: scaffold, setup\nscaffold-setup: {}\n---\n\n## 目的\n\n初めて参加する人が、必要な環境を揃えて検証を始められるようにする。\n\nこの一覧は既知のファイル名と入口の静的な確認です。ファイルの存在は、内容の正しさや実行の成功を意味しません。独自の構成は既存の手順を確認して判断してください。\n",
            self.task_key()
        );
        if !self.stacks.is_empty() {
            text.push_str(&format!(
                "\n検出した構成: {}\n",
                self.stacks
                    .iter()
                    .map(|s| one_line(s))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
        }
        text.push_str("\n## 現在の入口と根拠\n\n");
        for check in &self.checks {
            text.push_str(&format!(
                "- {}: {}\n",
                one_line(&check.title),
                check.state.label()
            ));
            for evidence in &check.evidence {
                text.push_str(&format!(
                    "  - {}:{}\n",
                    one_line(&evidence.path),
                    evidence.line
                ));
            }
        }
        text.push_str("\n## 整えること\n\n");
        let attention: Vec<_> = self
            .checks
            .iter()
            .filter(|c| c.state != CheckState::Found)
            .collect();
        if attention.is_empty() {
            text.push_str("- 既存の入口を順に確認し、新しく参加する人が辿れる導入手順になっているか確かめる。\n");
        }
        for check in attention {
            text.push_str(&format!(
                "- {}: {}\n",
                one_line(&check.title),
                one_line(&check.next_step)
            ));
        }
        let unknown: Vec<_> = self.unknown_evidence().collect();
        if !unknown.is_empty() {
            text.push_str("\n## 読み取れなかった根拠\n\n");
            for source in unknown {
                text.push_str(&format!(
                    "- {}: {}\n",
                    one_line(&source.path),
                    one_line(source.error.as_deref().unwrap_or("読み取り結果が不明"))
                ));
            }
        }
        text.push_str(
            "\n## 完了の条件\n\n- 既存の説明・設定を確認し、このプロジェクトに必要な項目を整える。独自の入口や対象外の項目は理由と参照先を記録する。\n- 必要なツールとバージョン、依存の導入、検証の実行方法を、リポジトリ内の手順から辿れるようにする。\n- 既存の設定を尊重し、認証情報や個人の環境に依存する値を追加しない。\n- 実施した検証の結果と、未実施・実行不能の項目を区別して報告する。ファイルを作っただけで動作確認済みにはしない。\n\n## 報告\n\n- Tasks の実行指示のとおり、変更点・検証結果・残る判断を `$NOA_RESULT` に書く。\n",
        );
        text
    }
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

#[derive(Debug, Clone)]
enum Read {
    Missing,
    Found { path: String, text: String },
    Unknown { path: String, error: String },
}

impl Read {
    fn text(&self) -> Option<&str> {
        match self {
            Self::Found { text, .. } => Some(text),
            _ => None,
        }
    }

    fn path(&self) -> Option<&str> {
        match self {
            Self::Found { path, .. } | Self::Unknown { path, .. } => Some(path),
            Self::Missing => None,
        }
    }
}

/// An open root and descriptor-relative reads keep intermediate symlinks from
/// redirecting the inventory outside the chosen repository, including when a
/// directory is replaced while the scan is running.
struct Reader {
    root: File,
}

impl Reader {
    fn new(repo: &Path) -> Result<Self, String> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(repo)
            .map_err(|e| format!("リポジトリを読み取れません: {e}"))?;
        Ok(Self { root })
    }

    fn open(&self, path: &str, directory: bool) -> Result<Option<File>, Read> {
        let unknown = |path: String, error: String| Read::Unknown { path, error };
        let mut parent = self
            .root
            .try_clone()
            .map_err(|e| unknown(".".into(), format!("リポジトリを読み取れません: {e}")))?;
        let parts: Vec<_> = path.split('/').collect();
        let mut prefix = String::new();
        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() || matches!(*part, "." | "..") {
                return Err(unknown(path.into(), "相対パスではありません".into()));
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let name = CString::new(*part)
                .map_err(|_| unknown(prefix.clone(), "パスに NUL があります".into()))?;
            let is_dir = index + 1 < parts.len() || directory;
            let flags = libc::O_RDONLY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | if is_dir { libc::O_DIRECTORY } else { 0 };
            // Each name is one component under a held directory descriptor.
            let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(unknown(
                    prefix,
                    match error.raw_os_error() {
                        Some(libc::ELOOP) => "シンボリックリンクは読み取りません".into(),
                        Some(libc::ENOTDIR) => {
                            "通常のディレクトリではありません（リンクは辿りません）".into()
                        }
                        _ => format!("読み取れません: {error}"),
                    },
                ));
            }
            // openat returned a new, owned descriptor.
            parent = unsafe { File::from_raw_fd(fd) };
        }
        Ok(Some(parent))
    }

    fn read(&self, path: &str, remaining: &mut usize) -> Read {
        let failed = |error: String| Read::Unknown {
            path: path.into(),
            error,
        };
        let file = match self.open(path, false) {
            Ok(Some(file)) => file,
            Ok(None) => return Read::Missing,
            Err(error) => return error,
        };
        let meta = match file.metadata() {
            Ok(meta) => meta,
            Err(e) => return failed(format!("読み取れません: {e}")),
        };
        if !meta.is_file() {
            return failed("通常のファイルではありません".into());
        }
        if meta.len() > MAX_BYTES {
            return failed("1 MiB の読み取り上限を超えています".into());
        }
        if meta.len() > *remaining as u64 {
            return failed("スキャン全体の読み取り上限を超えています".into());
        }
        let mut bytes = Vec::new();
        let cap = (MAX_BYTES as usize).min(*remaining);
        if let Err(e) = file.take(cap as u64 + 1).read_to_end(&mut bytes) {
            return failed(format!("読み取れません: {e}"));
        }
        if bytes.len() > cap {
            return failed("読み取り中にファイルが上限を超えました".into());
        }
        *remaining -= bytes.len();
        let Ok(text) = String::from_utf8(bytes) else {
            return failed("UTF-8 ではありません".into());
        };
        if text.trim().is_empty() {
            return failed("ファイルが空です".into());
        }
        if path.ends_with(".json") {
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) if value.is_object() => {}
                Ok(_) => return failed("JSON の最上位がオブジェクトではありません".into()),
                Err(error) => {
                    return failed(format!(
                        "JSON を解釈できません（{} 行 {} 列）",
                        error.line(),
                        error.column()
                    ));
                }
            }
        }
        Read::Found {
            path: path.into(),
            text,
        }
    }

    fn workflows(&self) -> Result<Vec<String>, Read> {
        const DIR: &str = ".github/workflows";
        let file = match self.open(DIR, true)? {
            Some(file) => file,
            None => return Ok(Vec::new()),
        };
        let fail = |error: &str| Read::Unknown {
            path: DIR.into(),
            error: error.into(),
        };
        // fdopendir takes ownership on success; Drop closes it on every exit.
        let fd = file.into_raw_fd();
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            unsafe { libc::close(fd) };
            return Err(fail("ワークフローの一覧を読み取れません"));
        }
        struct Directory(*mut libc::DIR);
        impl Drop for Directory {
            fn drop(&mut self) {
                unsafe { libc::closedir(self.0) };
            }
        }
        let directory = Directory(raw);
        let mut paths = Vec::new();
        for _ in 0..MAX_DIRECTORY_ENTRIES {
            // readdir distinguishes end-of-directory from an error by errno.
            #[cfg(target_os = "macos")]
            let errno = unsafe { libc::__error() };
            #[cfg(not(target_os = "macos"))]
            let errno = unsafe { libc::__errno_location() };
            unsafe { *errno = 0 };
            let entry = unsafe { libc::readdir(directory.0) };
            if entry.is_null() {
                if unsafe { *errno } != 0 {
                    return Err(fail("ワークフローの一覧を読み取れません"));
                }
                paths.sort();
                return Ok(paths);
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let Ok(name) = name.to_str() else {
                return Err(fail("ワークフローの名前が UTF-8 ではありません"));
            };
            if name.ends_with(".yml") || name.ends_with(".yaml") {
                if paths.len() == MAX_WORKFLOWS {
                    return Err(fail("ワークフローが 64 件の探索上限を超えています"));
                }
                paths.push(format!("{DIR}/{name}"));
            }
        }
        Err(fail("ワークフローのフォルダが探索上限を超えています"))
    }
}

struct Inventory {
    reader: Reader,
    remaining: usize,
    cache: BTreeMap<String, Read>,
    sources: BTreeMap<String, SourceReport>,
    assets: BTreeMap<String, Asset>,
}

impl Inventory {
    fn record(&mut self, result: &Read) {
        let (path, error) = match result {
            Read::Missing => return,
            Read::Found { path, .. } => (path, None),
            Read::Unknown { path, error } => (path, Some(error.clone())),
        };
        self.sources.insert(
            path.clone(),
            SourceReport {
                path: path.clone(),
                state: if error.is_some() {
                    SourceState::Unknown
                } else {
                    SourceState::Ok
                },
                error,
            },
        );
    }

    fn read(&mut self, path: &str) -> Read {
        if let Some(cached) = self.cache.get(path) {
            return cached.clone();
        }
        let result = self.reader.read(path, &mut self.remaining);
        self.record(&result);
        self.cache.insert(path.into(), result.clone());
        result
    }

    fn asset(&mut self, file: &Read, kind: AssetKind, title: &str) {
        let Read::Found { path, .. } = file else {
            return;
        };
        // CONTRIBUTING.md, for example, can serve two checks while remaining
        // one useful entry in the asset library.
        if self.assets.values().any(|asset| asset.path == *path) {
            return;
        }
        let kind_id = match kind {
            AssetKind::Checker => "checker",
            AssetKind::Runbook => "runbook",
            AssetKind::Rule => "rule",
        };
        let id = format!("setup-{kind_id}-{}", hash::hex(path.as_bytes()));
        self.assets.entry(id.clone()).or_insert_with(|| Asset {
            id,
            kind,
            title: title.into(),
            path: path.clone(),
            decisions: Vec::new(),
        });
    }
}

#[derive(Default)]
struct EvidenceSet {
    found: Vec<Evidence>,
    unknown: Vec<Evidence>,
}

impl EvidenceSet {
    fn observe(&mut self, file: &Read) {
        match file {
            Read::Found { path, .. } => self.found.push(Evidence::new(path, 1)),
            Read::Unknown { path, .. } => self.unknown.push(Evidence::new(path, 1)),
            Read::Missing => {}
        }
    }

    fn entry(&mut self, file: &Read, line: Option<u32>) {
        match (file, line) {
            (Read::Found { path, .. }, Some(line)) => {
                self.found.push(Evidence::new(path, line));
            }
            (Read::Unknown { .. }, _) => self.observe(file),
            _ => {}
        }
    }

    fn check(self, id: &str, title: &str, description: &str, next: &str) -> Check {
        let state = if !self.unknown.is_empty() {
            CheckState::Unknown
        } else if self.found.is_empty() {
            CheckState::Missing
        } else {
            CheckState::Found
        };
        let next_step = match state {
            CheckState::Unknown => {
                format!("読めないファイルの根拠を確認し、必要なら内容・権限・配置を直す。{next}")
            }
            _ => next.to_owned(),
        };
        Check {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            state,
            evidence: evidence_sorted(self.found.into_iter().chain(self.unknown).collect()),
            next_step,
        }
    }
}

fn first_line(text: &str, predicate: impl Fn(&str) -> bool) -> Option<u32> {
    text.lines()
        .position(predicate)
        .map(|index| index as u32 + 1)
}

fn verification_name(name: &str) -> bool {
    ["test", "check", "lint", "verify", "typecheck", "type-check"]
        .iter()
        .any(|prefix| {
            name == *prefix
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with([':', '-', '_']))
        })
}

fn make_entry(text: &str) -> Option<u32> {
    first_line(text, |line| {
        if line.starts_with(['\t', '#']) {
            return false;
        }
        let Some((targets, _)) = line.split_once(':') else {
            return false;
        };
        !targets.contains('=') && targets.split_whitespace().any(verification_name)
    })
}

fn assignment_line(text: &str, keys: &[&str]) -> Option<u32> {
    first_line(text, |line| {
        let line = line.trim();
        keys.iter().any(|key| {
            line.strip_prefix(key)
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .is_some_and(|value| !value.trim().is_empty())
        })
    })
}

/// Inventories only known entry points and a bounded workflow directory. It
/// neither walks arbitrary source trees nor treats absent optional filenames
/// as source errors.
pub fn scan(repo: &Path) -> Setup {
    let reader = match Reader::new(repo) {
        Ok(reader) => reader,
        Err(error) => return unreadable_repo(error),
    };
    let mut inventory = Inventory {
        reader,
        remaining: MAX_TOTAL_BYTES,
        cache: BTreeMap::new(),
        sources: BTreeMap::new(),
        assets: BTreeMap::new(),
    };
    let mut guide = EvidenceSet::default();
    for path in [
        "README.md",
        "README.rst",
        "README.txt",
        "README",
        "readme.md",
        "CONTRIBUTING.md",
        ".github/CONTRIBUTING.md",
        "SETUP.md",
        "docs/setup.md",
        "docs/SETUP.md",
        "docs/development.md",
        "docs/getting-started.md",
    ] {
        let file = inventory.read(path);
        guide.observe(&file);
        inventory.asset(&file, AssetKind::Runbook, "プロジェクトの導入・開発手順");
    }
    let mut rules = EvidenceSet::default();
    for (path, title) in [
        ("AGENTS.md", "エージェントの作業ルール"),
        ("CLAUDE.md", "Claude の作業ルール"),
        ("CONTRIBUTING.md", "開発への参加ルール"),
        (".github/CONTRIBUTING.md", "開発への参加ルール"),
        (".editorconfig", "エディタの共通設定"),
    ] {
        let file = inventory.read(path);
        rules.observe(&file);
        inventory.asset(&file, AssetKind::Rule, title);
    }
    let mut dependencies = EvidenceSet::default();
    let mut runtime = EvidenceSet::default();
    let mut verification = EvidenceSet::default();
    let mut stacks = Vec::new();
    for (path, stack) in [
        ("Cargo.toml", "Rust"),
        ("package.json", "Node.js"),
        ("pyproject.toml", "Python"),
        ("requirements.txt", "Python"),
        ("Pipfile", "Python"),
        ("setup.py", "Python"),
        ("setup.cfg", "Python"),
        ("go.mod", "Go"),
    ] {
        let file = inventory.read(path);
        dependencies.observe(&file);
        if file.text().is_some() && !stacks.iter().any(|s| s == stack) {
            stacks.push(stack.to_owned());
        }
    }
    for (path, title) in [
        ("rust-toolchain.toml", "Rust のツールチェーン指定"),
        ("rust-toolchain", "Rust のツールチェーン指定"),
        (".nvmrc", "Node.js のバージョン指定"),
        (".node-version", "Node.js のバージョン指定"),
        (".python-version", "Python のバージョン指定"),
        (".tool-versions", "開発ツールのバージョン指定"),
        ("mise.toml", "開発ツールのバージョン指定"),
        (".mise.toml", "開発ツールのバージョン指定"),
    ] {
        let file = inventory.read(path);
        runtime.observe(&file);
        inventory.asset(&file, AssetKind::Rule, title);
    }
    for (path, keys) in [
        ("Cargo.toml", &["rust-version"][..]),
        ("pyproject.toml", &["requires-python"][..]),
    ] {
        let file = inventory.read(path);
        let line = file.text().and_then(|text| assignment_line(text, keys));
        runtime.entry(&file, line);
    }
    let go = inventory.read("go.mod");
    let go_line = go.text().and_then(|text| {
        first_line(text, |line| {
            let words: Vec<_> = line.split_whitespace().collect();
            words.len() >= 2 && matches!(words[0], "go" | "toolchain")
        })
    });
    runtime.entry(&go, go_line);

    let package = inventory.read("package.json");
    match &package {
        Read::Found { path, text } => {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
                let has_runtime = value
                    .get("engines")
                    .and_then(|v| v.as_object())
                    .is_some_and(|engines| {
                        engines
                            .values()
                            .any(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
                    })
                    || value
                        .get("packageManager")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.trim().is_empty());
                if has_runtime {
                    runtime.found.push(Evidence::new(
                        path,
                        first_line(text, |line| {
                            line.contains("\"engines\"") || line.contains("\"packageManager\"")
                        })
                        .unwrap_or(1),
                    ));
                }
                if let Some(scripts) = value.get("scripts").and_then(|v| v.as_object()) {
                    for (name, command) in scripts {
                        if verification_name(name)
                            && command.as_str().is_some_and(|s| {
                                !s.trim().is_empty() && !s.contains("no test specified")
                            })
                        {
                            verification.found.push(Evidence::new(
                                path,
                                first_line(text, |line| line.contains(&format!("\"{name}\"")))
                                    .unwrap_or(1),
                            ));
                            inventory.asset(&package, AssetKind::Checker, "Node.js の検証コマンド");
                        }
                    }
                }
            }
        }
        Read::Unknown { .. } => {
            runtime.observe(&package);
            verification.observe(&package);
        }
        Read::Missing => {}
    }
    for path in [
        "scripts/verify.sh",
        "scripts/check.sh",
        "scripts/test.sh",
        "scripts/verify.py",
        "scripts/check.py",
        "scripts/test.py",
        "verify.sh",
        "test.sh",
        "pytest.ini",
        "tox.ini",
        "noxfile.py",
        ".pre-commit-config.yaml",
    ] {
        let file = inventory.read(path);
        verification.observe(&file);
        inventory.asset(&file, AssetKind::Checker, "検証の入口");
    }
    for path in [
        "Makefile",
        "makefile",
        "justfile",
        "Justfile",
        "Taskfile.yml",
        "Taskfile.yaml",
    ] {
        let file = inventory.read(path);
        let line = file.text().and_then(make_entry);
        verification.entry(&file, line);
        if line.is_some() {
            inventory.asset(&file, AssetKind::Checker, "検証タスクの定義");
        }
    }
    let pyproject = inventory.read("pyproject.toml");
    let py_line = pyproject.text().and_then(|text| {
        first_line(text, |line| {
            matches!(
                line.trim(),
                "[tool.pytest.ini_options]" | "[tool.tox]" | "[tool.hatch.envs.test]"
            )
        })
    });
    verification.entry(&pyproject, py_line);
    if py_line.is_some() {
        inventory.asset(&pyproject, AssetKind::Checker, "Python の検証設定");
    }

    let mut ci = EvidenceSet::default();
    for path in [
        ".gitlab-ci.yml",
        ".circleci/config.yml",
        "azure-pipelines.yml",
        "Jenkinsfile",
    ] {
        let file = inventory.read(path);
        ci.observe(&file);
        inventory.asset(&file, AssetKind::Rule, "CI のワークフロー定義");
    }
    match inventory.reader.workflows() {
        Ok(paths) => {
            for path in paths {
                let file = inventory.read(&path);
                ci.observe(&file);
                inventory.asset(&file, AssetKind::Rule, "CI のワークフロー定義");
            }
        }
        Err(error) => {
            inventory.record(&error);
            ci.observe(&error);
        }
    }

    let registry = inventory.read(REGISTRY);
    let declared_widgets = registry
        .text()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| {
            value
                .get("widgets")
                .and_then(|w| w.as_object())
                .map(|widgets| !widgets.is_empty())
        })
        .unwrap_or(false);
    let noa_helper = inventory.read(HELPER);
    // A blocked `crates/` directory is not evidence of a Noa repository.
    // Only a result naming the helper itself establishes that entry point.
    let registration_applicable = declared_widgets || noa_helper.path() == Some(HELPER);
    stacks.sort();
    Setup {
        stacks,
        checks: checks([guide, rules, dependencies, runtime, verification, ci]),
        assets: inventory.assets.into_values().collect(),
        sources: inventory.sources.into_values().collect(),
        registration_applicable,
    }
}

fn checks(sets: [EvidenceSet; 6]) -> Vec<Check> {
    let [guide, rules, dependencies, runtime, verification, ci] = sets;
    vec![
        guide.check(
            "getting-started",
            "導入手順",
            "初めて参加する人が辿れる README・セットアップ手順の入口。",
            "README.md または既存の導入文書に、必要なツール・依存の導入・最初の検証までの手順をまとめる。",
        ),
        rules.check(
            "working-agreements",
            "作業ルール",
            "人とエージェントが共有する作業方針や編集設定。",
            "AGENTS.md や CONTRIBUTING.md に、作業範囲・編集規約・確認方法を既存方針に合わせて記す。",
        ),
        dependencies.check(
            "dependencies",
            "依存の定義",
            "言語と依存を共有するマニフェスト。導入の実行は行いません。",
            "プロジェクトに合う Cargo.toml・package.json・pyproject.toml・go.mod などの依存定義と、その導入方法を確認する。",
        ),
        runtime.check(
            "runtime",
            "実行環境の指定",
            "必要な言語・ツールのバージョンを共有する設定。",
            "必要な言語・ツールのバージョンを、rust-toolchain.toml・.node-version・.python-version・mise.toml など、既存の管理方法で指定する。",
        ),
        verification.check(
            "verification",
            "検証の入口",
            "テストや静的検査を始めるためのスクリプト・設定。実行結果は未確認です。",
            "テスト・静的検査を呼び出す検証コマンドを用意する。標準コマンドを使う構成では、そのコマンドと成功条件を導入手順に明記する。",
        ),
        ci.check(
            "continuous-integration",
            "CI の設定",
            "変更時に自動で処理するワークフローの定義。実行履歴は読みません。",
            "既存の CI に検証の入口を繋ぐ。CI が不要・外部管理の場合は、その理由と確認方法を導入手順に記す。",
        ),
    ]
}

fn unreadable_repo(error: String) -> Setup {
    let sets = std::array::from_fn(|_| EvidenceSet {
        found: Vec::new(),
        unknown: vec![Evidence::new(".", 1)],
    });
    Setup {
        stacks: Vec::new(),
        checks: checks(sets),
        assets: Vec::new(),
        sources: vec![SourceReport {
            path: ".".into(),
            state: SourceState::Unknown,
            error: Some(error),
        }],
        registration_applicable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Lifecycle;
    use std::os::unix::fs::symlink;

    fn put(repo: &Path, path: &str, contents: impl AsRef<[u8]>) {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn check<'a>(setup: &'a Setup, id: &str) -> &'a Check {
        setup.checks.iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn an_empty_directory_has_a_useful_plan_without_phantom_sources() {
        let repo = tempfile::tempdir().unwrap();
        let setup = scan(repo.path());
        assert_eq!(setup.checks.len(), 6);
        assert_eq!(setup.found_count(), 0);
        assert_eq!(setup.attention_count(), 6);
        assert!(setup.sources.is_empty());
        assert!(setup.assets.is_empty());
        assert!(!setup.registration_applicable);
        assert!(setup.checks.iter().all(|c| {
            c.state == CheckState::Missing && !c.next_step.is_empty() && c.evidence.is_empty()
        }));
        assert_eq!(std::fs::read_dir(repo.path()).unwrap().count(), 0);
        assert!(setup.task_text().contains("$NOA_RESULT"));
    }

    #[test]
    fn a_node_project_exposes_real_entry_points_without_running_them() {
        let repo = tempfile::tempdir().unwrap();
        put(repo.path(), "README.md", "# Start\nSee CONTRIBUTING.md.\n");
        put(repo.path(), "AGENTS.md", "Use the test script.\n");
        put(
            repo.path(),
            "package.json",
            "{\n  \"engines\": {\"node\": \">=22\"},\n  \"scripts\": {\"test\": \"touch should-not-exist\"}\n}\n",
        );
        put(
            repo.path(),
            ".github/workflows/checks.yml",
            "name: checks\njobs: {}\n",
        );
        let setup = scan(repo.path());
        assert_eq!(setup.stacks, ["Node.js"]);
        assert_eq!(setup.found_count(), 6);
        assert_eq!(setup.attention_count(), 0);
        assert!(!setup.registration_applicable);
        assert_eq!(
            check(&setup, "verification").evidence,
            [Evidence::new("package.json", 3)]
        );
        assert!(!repo.path().join("should-not-exist").exists());
        assert!(setup.assets.iter().all(|a| {
            a.lifecycle() == Lifecycle::Candidate
                && a.decisions.is_empty()
                && repo.path().join(&a.path).is_file()
        }));
        let registry = crate::registry::Registry {
            schema: 1,
            widgets: BTreeMap::new(),
            assets: setup.assets.clone(),
        };
        assert!(crate::registry::validate(&registry).is_ok());
        assert_eq!(scan(repo.path()), setup);
    }

    #[test]
    fn language_signals_and_standard_commands_are_kept_distinct() {
        for (manifest, contents, stack, runtime_state) in [
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\nrust-version = \"1.92\"\n",
                "Rust",
                CheckState::Found,
            ),
            (
                "pyproject.toml",
                "[project]\nname = \"app\"\nrequires-python = \">=3.12\"\n",
                "Python",
                CheckState::Found,
            ),
            (
                "go.mod",
                "module example.test/app\n\ngo 1.24\n",
                "Go",
                CheckState::Found,
            ),
            (
                "requirements.txt",
                "pytest==8.4.0\n",
                "Python",
                CheckState::Missing,
            ),
        ] {
            let repo = tempfile::tempdir().unwrap();
            put(repo.path(), manifest, contents);
            let setup = scan(repo.path());
            assert_eq!(setup.stacks, [stack], "{manifest}");
            assert_eq!(check(&setup, "dependencies").state, CheckState::Found);
            assert_eq!(check(&setup, "runtime").state, runtime_state);
            assert_eq!(check(&setup, "verification").state, CheckState::Missing);
        }
    }

    #[test]
    fn validation_targets_and_python_configuration_are_real_evidence() {
        let repo = tempfile::tempdir().unwrap();
        put(
            repo.path(),
            "Makefile",
            ".PHONY: test\n\ntest: unit\n\tcargo test\n",
        );
        put(
            repo.path(),
            "pyproject.toml",
            "[project]\nname = \"app\"\n\n[tool.pytest.ini_options]\ntestpaths = [\"tests\"]\n",
        );
        let setup = scan(repo.path());
        assert_eq!(
            check(&setup, "verification").evidence,
            [
                Evidence::new("Makefile", 3),
                Evidence::new("pyproject.toml", 4)
            ]
        );
        put(repo.path(), "Makefile", "build:\n\techo done\n");
        put(repo.path(), "pyproject.toml", "[project]\nname = \"app\"\n");
        assert_eq!(
            check(&scan(repo.path()), "verification").state,
            CheckState::Missing
        );
    }

    #[test]
    fn one_document_can_satisfy_two_checks_without_duplicate_assets() {
        let repo = tempfile::tempdir().unwrap();
        put(
            repo.path(),
            "CONTRIBUTING.md",
            "# Development\nRun the tests.\n",
        );
        let setup = scan(repo.path());
        assert_eq!(check(&setup, "getting-started").state, CheckState::Found);
        assert_eq!(check(&setup, "working-agreements").state, CheckState::Found);
        assert_eq!(setup.assets.len(), 1);
        assert_eq!(setup.assets[0].path, "CONTRIBUTING.md");
    }

    #[test]
    fn the_default_npm_placeholder_is_not_a_verification_entry() {
        let repo = tempfile::tempdir().unwrap();
        put(
            repo.path(),
            "package.json",
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
        );
        let setup = scan(repo.path());
        assert_eq!(check(&setup, "verification").state, CheckState::Missing);
        assert!(setup.assets.is_empty());
    }

    #[test]
    fn unreadable_and_invalid_files_never_count_as_found() {
        let repo = tempfile::tempdir().unwrap();
        put(repo.path(), "README.md", vec![b'x'; MAX_BYTES as usize + 1]);
        put(repo.path(), "AGENTS.md", [0xff, 0xfe]);
        put(repo.path(), "package.json", "{broken");
        put(repo.path(), ".node-version", "22\n");
        let setup = scan(repo.path());
        for id in [
            "getting-started",
            "working-agreements",
            "dependencies",
            "runtime",
            "verification",
        ] {
            assert_eq!(check(&setup, id).state, CheckState::Unknown, "{id}");
        }
        assert_eq!(
            setup
                .sources
                .iter()
                .filter(|s| s.state == SourceState::Unknown)
                .count(),
            3
        );
        assert!(setup.assets.iter().all(|a| a.path == ".node-version"));
        assert!(setup.task_text().contains("JSON を解釈できません"));
    }

    #[test]
    fn leaf_and_intermediate_links_are_not_followed() {
        let repo = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        put(elsewhere.path(), "README.md", "# Outside\n");
        put(elsewhere.path(), "verify.sh", "echo outside\n");
        put(elsewhere.path(), "workflows/ci.yml", "name: outside\n");
        symlink(
            elsewhere.path().join("README.md"),
            repo.path().join("README.md"),
        )
        .unwrap();
        symlink(elsewhere.path(), repo.path().join("scripts")).unwrap();
        symlink(elsewhere.path(), repo.path().join(".github")).unwrap();
        let setup = scan(repo.path());
        for id in ["getting-started", "verification", "continuous-integration"] {
            assert_eq!(check(&setup, id).state, CheckState::Unknown, "{id}");
        }
        assert!(setup.assets.is_empty());
        assert!(
            setup
                .sources
                .iter()
                .all(|s| s.state == SourceState::Unknown)
        );
        assert!(setup.sources.iter().any(|s| s.path == "scripts"));
        assert!(setup.sources.iter().any(|s| s.path == ".github"));
    }

    #[test]
    fn workflows_are_sorted_bounded_and_do_not_follow_file_links() {
        let repo = tempfile::tempdir().unwrap();
        put(repo.path(), ".github/workflows/z.yml", "name: z\n");
        put(repo.path(), ".github/workflows/a.yaml", "name: a\n");
        let setup = scan(repo.path());
        assert_eq!(
            check(&setup, "continuous-integration").evidence,
            [
                Evidence::new(".github/workflows/a.yaml", 1),
                Evidence::new(".github/workflows/z.yml", 1)
            ]
        );
        symlink("z.yml", repo.path().join(".github/workflows/linked.yml")).unwrap();
        assert_eq!(
            check(&scan(repo.path()), "continuous-integration").state,
            CheckState::Unknown
        );
        for index in 0..MAX_WORKFLOWS {
            put(
                repo.path(),
                &format!(".github/workflows/extra-{index}.yml"),
                "name: extra\n",
            );
        }
        let setup = scan(repo.path());
        assert_eq!(
            check(&setup, "continuous-integration").state,
            CheckState::Unknown
        );
        assert!(
            setup
                .sources
                .iter()
                .any(|s| s.error.as_deref().is_some_and(|e| e.contains("64 件")))
        );
    }

    #[test]
    fn only_noa_registration_signals_enable_its_checker() {
        let repo = tempfile::tempdir().unwrap();
        put(repo.path(), "Cargo.toml", "[workspace]\nmembers = []\n");
        put(
            repo.path(),
            REGISTRY,
            "{\"schema\":1,\"widgets\":{},\"assets\":[]}\n",
        );
        assert!(!scan(repo.path()).registration_applicable);
        put(
            repo.path(),
            REGISTRY,
            "{\"schema\":1,\"widgets\":{\"x\":{\"ships\":[\"macos\"]}},\"assets\":[]}\n",
        );
        assert!(scan(repo.path()).registration_applicable);
        std::fs::remove_file(repo.path().join(REGISTRY)).unwrap();
        put(
            repo.path(),
            "crates/noa-app/src/helper_program.rs",
            "struct HelperProgram;\n",
        );
        assert!(scan(repo.path()).registration_applicable);
    }

    #[test]
    fn a_generic_crates_symlink_does_not_enable_noa_or_change_the_setup_task() {
        let repo = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let baseline = scan(repo.path());
        put(
            elsewhere.path(),
            "noa-app/src/helper_program.rs",
            "struct HelperProgram;\n",
        );
        symlink(elsewhere.path(), repo.path().join("crates")).unwrap();
        let blocked = scan(repo.path());
        assert!(!blocked.registration_applicable);
        assert!(
            blocked
                .sources
                .iter()
                .any(|s| s.path == "crates" && s.state == SourceState::Unknown)
        );
        assert_eq!(baseline.task_key(), blocked.task_key());
        assert_eq!(baseline.task_text(), blocked.task_text());

        std::fs::remove_file(repo.path().join("crates")).unwrap();
        put(repo.path(), HELPER, "struct HelperProgram;\n");
        let noa = scan(repo.path());
        assert!(noa.registration_applicable);
        assert_eq!(baseline.task_key(), noa.task_key());
        assert_eq!(baseline.task_text(), noa.task_text());

        put(repo.path(), REGISTRY, "{broken");
        let broken_registry = scan(repo.path());
        assert!(
            broken_registry
                .sources
                .iter()
                .any(|s| s.path == REGISTRY && s.state == SourceState::Unknown)
        );
        assert_eq!(baseline.task_key(), broken_registry.task_key());
        assert_eq!(baseline.task_text(), broken_registry.task_text());
    }

    #[test]
    fn tasks_have_stable_keys_and_a_closed_header_even_with_hostile_values() {
        let repo = tempfile::tempdir().unwrap();
        let setup = scan(repo.path());
        let same = scan(repo.path());
        assert_eq!(setup.task_key(), same.task_key());
        put(
            repo.path(),
            REGISTRY,
            "{\"schema\":1,\"widgets\":{},\"assets\":[]}\n",
        );
        assert_eq!(
            setup.task_key(),
            scan(repo.path()).task_key(),
            "registering assets does not create a new setup plan"
        );
        put(repo.path(), "README.md", "# Start\n");
        assert_ne!(setup.task_key(), scan(repo.path()).task_key());
        let mut hostile = setup;
        hostile.stacks.push("fake\n---\nstate: closed".into());
        hostile.checks[0].title = "title\n---\nstate: closed".into();
        hostile.checks[0].next_step = "next\n---\nstate: closed".into();
        hostile.checks[0]
            .evidence
            .push(Evidence::new("path\n---\nstate: closed", 1));
        let text = hostile.task_text();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0], "---");
        assert_eq!(lines[2], "state: open");
        assert_eq!(lines[5], "---");
        assert_eq!(lines.iter().filter(|line| **line == "---").count(), 2);
        assert!(!lines.contains(&"state: closed"));
        assert!(text.contains("$NOA_RESULT"));
    }

    #[test]
    fn an_unreadable_root_is_unknown_and_produces_one_source() {
        let repo = tempfile::tempdir().unwrap();
        let setup = scan(&repo.path().join("does-not-exist"));
        assert_eq!(setup.sources.len(), 1);
        assert_eq!(setup.sources[0].path, ".");
        assert!(setup.checks.iter().all(|c| c.state == CheckState::Unknown));
    }
}
