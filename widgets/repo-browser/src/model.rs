use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    #[default]
    Unstaged,
    Staged,
    VsMain,
}

impl DiffMode {
    pub const ALL: [DiffMode; 3] = [DiffMode::Unstaged, DiffMode::Staged, DiffMode::VsMain];

    pub fn label(self) -> &'static str {
        match self {
            DiffMode::Unstaged => "Unstaged",
            DiffMode::Staged => "Staged",
            DiffMode::VsMain => "vs main",
        }
    }

    pub fn from_index(idx: usize) -> Self {
        Self::ALL.get(idx).copied().unwrap_or(DiffMode::Unstaged)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentActivity {
    None,
    Running,
    Waiting,
    Done,
}

impl AgentActivity {
    pub fn badge(self) -> &'static str {
        match self {
            AgentActivity::None => "",
            AgentActivity::Running => "稼働中",
            AgentActivity::Waiting => "承認待ち",
            AgentActivity::Done => "完了",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSummary {
    pub pane_id: u64,
    pub name: String,
    pub label: Option<String>,
    pub process: Option<String>,
    pub activity: AgentActivity,
    pub busy: bool,
    pub attention: bool,
    pub terminal_tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub added: usize,
    pub deleted: usize,
    pub diff: String,
    pub conflict_worktrees: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub name: String,
    pub cwd: String,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub panes: Vec<PaneSummary>,
    pub files: Vec<ChangedFile>,
    /// Why `files` may be empty for a reason other than "no changes".
    pub note: Option<String>,
}

impl Worktree {
    pub fn primary_pane(&self) -> Option<&PaneSummary> {
        self.panes
            .iter()
            .find(|p| p.attention)
            .or_else(|| {
                self.panes
                    .iter()
                    .find(|p| p.activity == AgentActivity::Running)
            })
            .or_else(|| self.panes.first())
    }

    pub fn has_conflict(&self) -> bool {
        self.files.iter().any(|f| !f.conflict_worktrees.is_empty())
    }

    pub fn activity(&self) -> AgentActivity {
        if self
            .panes
            .iter()
            .any(|p| p.attention || p.activity == AgentActivity::Waiting)
        {
            AgentActivity::Waiting
        } else if self
            .panes
            .iter()
            .any(|p| p.busy || p.activity == AgentActivity::Running)
        {
            AgentActivity::Running
        } else if self.panes.iter().any(|p| p.activity == AgentActivity::Done) {
            AgentActivity::Done
        } else {
            AgentActivity::None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub root: String,
    pub worktrees: Vec<Worktree>,
}

impl Repo {
    pub fn total_files(&self) -> usize {
        self.worktrees.iter().map(|w| w.files.len()).sum()
    }

    pub fn conflict_count(&self) -> usize {
        self.worktrees
            .iter()
            .flat_map(|w| w.files.iter())
            .filter(|f| !f.conflict_worktrees.is_empty())
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoData {
    pub repos: Vec<Repo>,
}

impl RepoData {
    /// Marks files that are modified in more than one worktree of the same repo.
    pub fn compute_conflicts(&mut self) {
        for repo in &mut self.repos {
            // Worktrees are told apart by cwd: branch names are not unique
            // (two detached HEADs are both "(detached)").
            let mut editors: HashMap<String, Vec<(String, String)>> = HashMap::new();
            for wt in &repo.worktrees {
                let mut seen = HashSet::new();
                for f in &wt.files {
                    if seen.insert(f.path.clone()) {
                        editors
                            .entry(f.path.clone())
                            .or_default()
                            .push((wt.cwd.clone(), wt.branch.clone()));
                    }
                }
            }
            for wt in &mut repo.worktrees {
                for f in &mut wt.files {
                    if let Some(editors) = editors.get(&f.path) {
                        f.conflict_worktrees = editors
                            .iter()
                            .filter(|(cwd, _)| cwd.as_str() != wt.cwd.as_str())
                            .map(|(_, branch)| branch.clone())
                            .collect();
                    } else {
                        f.conflict_worktrees.clear();
                    }
                }
            }
        }
    }

    pub fn totals(&self) -> (usize, usize, usize, usize) {
        let repos = self.repos.len();
        let worktrees: usize = self.repos.iter().map(|r| r.worktrees.len()).sum();
        let files: usize = self.repos.iter().map(Repo::total_files).sum();
        let conflicts: usize = self.repos.iter().map(Repo::conflict_count).sum();
        (repos, worktrees, files, conflicts)
    }

    /// Filter repos/worktrees/files according to active token strings.
    pub fn filtered(&self, tokens: &[String]) -> RepoData {
        if tokens.is_empty() {
            return self.clone();
        }
        let mut repos = Vec::new();
        for repo in &self.repos {
            let mut worktrees = Vec::new();
            for wt in &repo.worktrees {
                if !worktree_matches(repo, wt, tokens) {
                    continue;
                }
                let mut cloned_wt = wt.clone();
                if tokens.iter().any(|t| t.eq_ignore_ascii_case("conflict")) {
                    cloned_wt.files.retain(|f| !f.conflict_worktrees.is_empty());
                }
                worktrees.push(cloned_wt);
            }
            if !worktrees.is_empty() {
                repos.push(Repo {
                    name: repo.name.clone(),
                    root: repo.root.clone(),
                    worktrees,
                });
            }
        }
        RepoData { repos }
    }
}

fn worktree_matches(repo: &Repo, wt: &Worktree, tokens: &[String]) -> bool {
    for raw in tokens {
        let t = raw.trim().to_ascii_lowercase();
        if t.is_empty() {
            continue;
        }
        let matched = match t.as_str() {
            "dirty" => !wt.files.is_empty(),
            "conflict" => wt.has_conflict(),
            "agent:running" => wt.activity() == AgentActivity::Running,
            "agent:waiting" => wt.activity() == AgentActivity::Waiting,
            "agent:done" => wt.activity() == AgentActivity::Done,
            _ => {
                if let Some(branch) = t.strip_prefix("branch:") {
                    wt.branch.to_ascii_lowercase().contains(branch)
                } else if let Some(repo_q) = t.strip_prefix("repo:") {
                    repo.name.to_ascii_lowercase().contains(repo_q)
                } else {
                    repo.name.to_ascii_lowercase().contains(&t)
                        || wt.branch.to_ascii_lowercase().contains(&t)
                        || wt.name.to_ascii_lowercase().contains(&t)
                        || wt
                            .files
                            .iter()
                            .any(|f| f.path.to_ascii_lowercase().contains(&t))
                }
            }
        };
        if !matched {
            return false;
        }
    }
    true
}

/// Group panels by repository key and worktree cwd in deterministic order.
pub fn group_repos(entries: Vec<(String, String, Worktree)>) -> RepoData {
    let mut by_repo: BTreeMap<String, (String, Vec<Worktree>)> = BTreeMap::new();
    for (repo_name, repo_root, wt) in entries {
        let entry = by_repo
            .entry(repo_root.clone())
            .or_insert_with(|| (repo_name, Vec::new()));
        entry.1.push(wt);
    }
    let repos = by_repo
        .into_iter()
        .map(|(root, (name, mut worktrees))| {
            worktrees.sort_by(|a, b| a.branch.cmp(&b.branch).then(a.cwd.cmp(&b.cwd)));
            Repo {
                name,
                root,
                worktrees,
            }
        })
        .collect();
    let mut data = RepoData { repos };
    data.compute_conflicts();
    data
}

pub fn demo_data(mode: DiffMode) -> RepoData {
    let suffix = match mode {
        DiffMode::Unstaged => "",
        DiffMode::Staged => " [staged]",
        DiffMode::VsMain => " [vs main]",
    };

    let mut data = RepoData {
        repos: vec![
            Repo {
                name: "Noa".into(),
                root: "/Users/simota/repos/github.com/Noa".into(),
                worktrees: vec![
                    Worktree {
                        name: "feat/pane-subscriptions".into(),
                        cwd: "/Users/simota/repos/github.com/Noa-wt-subs".into(),
                        branch: "feat/pane-subscriptions".into(),
                        ahead: 3,
                        behind: 0,
                        note: None,
                        panes: vec![
                            PaneSummary {
                                pane_id: 12,
                                name: "claude · subs".into(),
                                label: Some("worker-subs".into()),
                                process: Some("claude".into()),
                                activity: AgentActivity::Running,
                                busy: true,
                                attention: false,
                                terminal_tail:
                                    "\x1b[1;36m● Claude Code\x1b[0m  feat/pane-subscriptions\r\n\
                                \x1b[32m✓\x1b[0m cargo test -p noa-ipc (42 passed)\r\n\
                                \x1b[33m→\x1b[0m Editing crates/noa-ipc/src/push.rs\r\n\
                                  Adding FieldMask filter for state_changed...\r\n\
                                \x1b[90m[14:22:08] waiting for clippy check\x1b[0m\r\n"
                                        .into(),
                            },
                            PaneSummary {
                                pane_id: 13,
                                name: "zsh · subs".into(),
                                label: None,
                                process: Some("zsh".into()),
                                activity: AgentActivity::None,
                                busy: false,
                                attention: false,
                                terminal_tail:
                                    "\x1b[32msimota@subs\x1b[0m % cargo test -p noa-ipc\r\n\
                                test result: ok. 42 passed; 0 failed\r\n"
                                        .into(),
                            },
                        ],
                        files: vec![
                            ChangedFile {
                                path: "crates/noa-ipc/src/push.rs".into(),
                                status: "M".into(),
                                added: 28,
                                deleted: 6,
                                diff: format!(
                                    "diff --git a/crates/noa-ipc/src/push.rs b/crates/noa-ipc/src/push.rs{suffix}\n\
                                     index 41a9e12..8b2c091 100644\n\
                                     --- a/crates/noa-ipc/src/push.rs\n\
                                     +++ b/crates/noa-ipc/src/push.rs\n\
                                     @@ -112,9 +112,18 @@ impl Broadcaster {{\n\
                                      pub fn broadcast_state_delta(\n\
                                          &self,\n\
                                     -    changed: Vec<Panel>,\n\
                                     +    changed: Vec<ChangedPanel>,\n\
                                     +    removed: Vec<u64>,\n\
                                      ) {{\n\
                                          let guard = self.inner.lock();\n\
                                     -    for sub in guard.subscribers.values() {{\n\
                                     -        sub.enqueue_panels(&changed);\n\
                                     +    for conn in guard.connections.values() {{\n\
                                     +        let panels = conn.filter_panels(&changed);\n\
                                     +        let removed_ids = conn.filter_removed(&removed);\n\
                                     +        if !panels.is_empty() || !removed_ids.is_empty() {{\n\
                                     +            conn.enqueue_delta(panels, removed_ids);\n\
                                     +        }}\n\
                                          }}\n\
                                      }}\n"
                                ),
                                conflict_worktrees: Vec::new(),
                            },
                            ChangedFile {
                                path: "crates/noa-ipc/src/protocol.rs".into(),
                                status: "M".into(),
                                added: 14,
                                deleted: 2,
                                diff: format!(
                                    "diff --git a/crates/noa-ipc/src/protocol.rs b/crates/noa-ipc/src/protocol.rs{suffix}\n\
                                     --- a/crates/noa-ipc/src/protocol.rs\n\
                                     +++ b/crates/noa-ipc/src/protocol.rs\n\
                                     @@ -640,6 +640,12 @@ pub struct StateChangedParams {{\n\
                                      pub panels: Vec<Panel>,\n\
                                     +#[serde(default, skip_serializing_if = \"Vec::is_empty\")]\n\
                                     +pub removed_pane_ids: Vec<WireId>,\n\
                                      #[serde(default, skip_serializing_if = \"std::ops::Not::not\")]\n\
                                      pub dropped: bool,\n"
                                ),
                                conflict_worktrees: Vec::new(),
                            },
                        ],
                    },
                    Worktree {
                        name: "fix/ipc-reconnect".into(),
                        cwd: "/Users/simota/repos/github.com/Noa-wt-reconnect".into(),
                        branch: "fix/ipc-reconnect".into(),
                        ahead: 1,
                        behind: 1,
                        note: None,
                        panes: vec![PaneSummary {
                            pane_id: 19,
                            name: "codex · reconnect".into(),
                            label: Some("worker-reconnect".into()),
                            process: Some("codex".into()),
                            activity: AgentActivity::Waiting,
                            busy: false,
                            attention: true,
                            terminal_tail: "\x1b[1;33m▲ Codex\x1b[0m  fix/ipc-reconnect\r\n\
                                Modified crates/noa-ipc/src/protocol.rs\r\n\
                                \x1b[33m? Approve running `cargo test -p noa-ipc`? [y/N]\x1b[0m\r\n"
                                .into(),
                        }],
                        files: vec![
                            ChangedFile {
                                path: "crates/noa-ipc/src/protocol.rs".into(),
                                status: "M".into(),
                                added: 9,
                                deleted: 3,
                                diff: format!(
                                    "diff --git a/crates/noa-ipc/src/protocol.rs b/crates/noa-ipc/src/protocol.rs{suffix}\n\
                                     --- a/crates/noa-ipc/src/protocol.rs\n\
                                     +++ b/crates/noa-ipc/src/protocol.rs\n\
                                     @@ -14,7 +14,9 @@\n\
                                     -pub const PROTOCOL_VERSION: u64 = 3;\n\
                                     +pub const PROTOCOL_VERSION: u64 = 3;\n\
                                     +/// Heartbeat interval advertised during hello handshake.\n\
                                     +pub const HEARTBEAT_MS: u64 = 5_000;\n"
                                ),
                                conflict_worktrees: Vec::new(),
                            },
                            ChangedFile {
                                path: "crates/noa-ipc/src/client.rs".into(),
                                status: "M".into(),
                                added: 19,
                                deleted: 4,
                                diff: format!(
                                    "diff --git a/crates/noa-ipc/src/client.rs b/crates/noa-ipc/src/client.rs{suffix}\n\
                                     --- a/crates/noa-ipc/src/client.rs\n\
                                     +++ b/crates/noa-ipc/src/client.rs\n\
                                     @@ -288,6 +288,15 @@ impl Client {{\n\
                                     +    pub fn reconnect_with_backoff(&mut self) -> Result<(), ClientError> {{\n\
                                     +        self.reset_pending();\n\
                                     +        self.handshake()\n\
                                     +    }}\n"
                                ),
                                conflict_worktrees: Vec::new(),
                            },
                        ],
                    },
                    Worktree {
                        name: "main".into(),
                        cwd: "/Users/simota/repos/github.com/Noa".into(),
                        branch: "main".into(),
                        ahead: 0,
                        behind: 0,
                        note: None,
                        panes: vec![PaneSummary {
                            pane_id: 3,
                            name: "zsh · Noa".into(),
                            label: None,
                            process: Some("zsh".into()),
                            activity: AgentActivity::None,
                            busy: false,
                            attention: false,
                            terminal_tail: "\x1b[32msimota@Noa\x1b[0m % git status -sb\r\n\
                                ## main...origin/main\r\n\
                                 M docs/FEATURES.md\r\n"
                                .into(),
                        }],
                        files: vec![ChangedFile {
                            path: "docs/FEATURES.md".into(),
                            status: "M".into(),
                            added: 6,
                            deleted: 1,
                            diff: format!(
                                "diff --git a/docs/FEATURES.md b/docs/FEATURES.md{suffix}\n\
                                 --- a/docs/FEATURES.md\n\
                                 +++ b/docs/FEATURES.md\n\
                                 @@ -118,3 +118,8 @@\n\
                                  - `widgets/agent-dashboard`: agent fleet operations dashboard\n\
                                 +- `widgets/repo-browser`: cross-worktree browser & live diff review\n"
                            ),
                            conflict_worktrees: Vec::new(),
                        }],
                    },
                ],
            },
            Repo {
                name: "noa-gpui".into(),
                root: "/Users/simota/repos/github.com/noa-gpui".into(),
                worktrees: vec![Worktree {
                    name: "feat/diff-highlight".into(),
                    cwd: "/Users/simota/repos/github.com/noa-gpui".into(),
                    branch: "feat/diff-highlight".into(),
                    ahead: 2,
                    behind: 0,
                    note: None,
                    panes: vec![PaneSummary {
                        pane_id: 27,
                        name: "agy · gpui".into(),
                        label: Some("worker-gpui".into()),
                        process: Some("agy".into()),
                        activity: AgentActivity::Done,
                        busy: false,
                        attention: false,
                        terminal_tail: "\x1b[1;35m◆ agy\x1b[0m  feat/diff-highlight\r\n\
                            \x1b[32m✓\x1b[0m cargo test --workspace (184 passed, 0 failed)\r\n\
                            Ready for review.\r\n"
                            .into(),
                    }],
                    files: vec![ChangedFile {
                        path: "crates/ui-core/src/widgets/log_view.rs".into(),
                        status: "A".into(),
                        added: 34,
                        deleted: 0,
                        diff: format!(
                            "diff --git a/crates/ui-core/src/widgets/log_view.rs b/crates/ui-core/src/widgets/log_view.rs{suffix}\n\
                             new file mode 100644\n\
                             --- /dev/null\n\
                             +++ b/crates/ui-core/src/widgets/log_view.rs\n\
                             @@ -0,0 +1,12 @@\n\
                             +pub fn highlight_diff_line(line: &str) -> Option<Color> {{\n\
                             +    if line.starts_with('+') && !line.starts_with(\"+++\") {{\n\
                             +        Some(Color::rgb(0.25, 0.82, 0.45))\n\
                             +    }} else if line.starts_with('-') && !line.starts_with(\"---\") {{\n\
                             +        Some(Color::rgb(0.95, 0.38, 0.38))\n\
                             +    }} else {{\n\
                             +        None\n\
                             +    }}\n\
                             +}}\n"
                        ),
                        conflict_worktrees: Vec::new(),
                    }],
                }],
            },
        ],
    };
    data.compute_conflicts();
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_worktrees_editing_same_file_conflict() {
        let file = |path: &str| ChangedFile {
            path: path.into(),
            status: "M".into(),
            added: 1,
            deleted: 0,
            diff: String::new(),
            conflict_worktrees: Vec::new(),
        };
        let wt = |cwd: &str| Worktree {
            name: "(detached)".into(),
            cwd: cwd.into(),
            branch: "(detached)".into(),
            ahead: 0,
            behind: 0,
            note: None,
            panes: Vec::new(),
            files: vec![file("src/lib.rs")],
        };
        let mut data = RepoData {
            repos: vec![Repo {
                name: "r".into(),
                root: "/r".into(),
                worktrees: vec![wt("/r/a"), wt("/r/b")],
            }],
        };
        data.compute_conflicts();
        for wt in &data.repos[0].worktrees {
            assert_eq!(wt.files[0].conflict_worktrees.len(), 1, "{}", wt.cwd);
        }
        assert_eq!(data.totals().3, 2);
    }

    #[test]
    fn demo_data_marks_cross_worktree_conflict() {
        let data = demo_data(DiffMode::Unstaged);
        let noa = &data.repos[0];
        let subs_proto = noa.worktrees[0]
            .files
            .iter()
            .find(|f| f.path == "crates/noa-ipc/src/protocol.rs")
            .unwrap();
        assert_eq!(subs_proto.conflict_worktrees, vec!["fix/ipc-reconnect"]);
        let (_, _, _, conflicts) = data.totals();
        assert_eq!(conflicts, 2);
    }

    #[test]
    fn token_filter_narrows_by_conflict_and_agent() {
        let data = demo_data(DiffMode::Unstaged);
        let only_conflict = data.filtered(&["conflict".into()]);
        assert_eq!(only_conflict.repos.len(), 1);
        assert_eq!(only_conflict.repos[0].worktrees.len(), 2);
        for wt in &only_conflict.repos[0].worktrees {
            assert_eq!(wt.files.len(), 1);
            assert_eq!(wt.files[0].path, "crates/noa-ipc/src/protocol.rs");
        }

        let only_waiting = data.filtered(&["agent:waiting".into()]);
        assert_eq!(only_waiting.repos.len(), 1);
        assert_eq!(only_waiting.repos[0].worktrees.len(), 1);
        assert_eq!(
            only_waiting.repos[0].worktrees[0].branch,
            "fix/ipc-reconnect"
        );
    }
}
