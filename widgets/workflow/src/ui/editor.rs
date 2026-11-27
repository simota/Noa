//! Draft editing is independent of the immutable definition captured by a run.

use workflow::model::{Edge, MAX_NODES, Node, NodeKind, Point, WorkflowDefinition};

pub(super) struct Editor {
    pub definition: WorkflowDefinition,
    pub saved: WorkflowDefinition,
    pub version: u64,
    pub selected: Option<String>,
    pub source: Option<String>,
    pub conflict: bool,
    pub pending_save: Option<(WorkflowDefinition, u64)>,
    past: Vec<WorkflowDefinition>,
    future: Vec<WorkflowDefinition>,
}

fn same_content(left: &WorkflowDefinition, right: &WorkflowDefinition) -> bool {
    let mut normalized = left.clone();
    normalized.revision = right.revision;
    normalized == *right
}

impl Editor {
    pub fn new(definition: WorkflowDefinition, version: u64) -> Self {
        Self {
            selected: definition.nodes.first().map(|n| n.id.clone()),
            saved: definition.clone(),
            definition,
            version,
            source: None,
            conflict: false,
            pending_save: None,
            past: Vec::new(),
            future: Vec::new(),
        }
    }

    pub fn dirty(&self) -> bool {
        !same_content(&self.definition, &self.saved)
    }
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn change(&mut self, edit: impl FnOnce(&mut WorkflowDefinition)) {
        let before = self.definition.clone();
        edit(&mut self.definition);
        self.record(before);
    }

    pub fn record(&mut self, before: WorkflowDefinition) {
        self.definition.revision = before.revision;
        if same_content(&before, &self.definition) {
            return;
        }
        self.past.push(before);
        if self.past.len() > 100 {
            self.past.remove(0);
        }
        self.future.clear();
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.past.pop() {
            self.future
                .push(std::mem::replace(&mut self.definition, previous));
            self.definition.revision = self.saved.revision;
            self.reselect();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.future.pop() {
            self.past
                .push(std::mem::replace(&mut self.definition, next));
            self.definition.revision = self.saved.revision;
            self.reselect();
        }
    }

    fn reselect(&mut self) {
        if self
            .selected
            .as_ref()
            .is_none_or(|id| self.definition.node(id).is_none())
        {
            self.selected = self.definition.nodes.first().map(|n| n.id.clone());
        }
        self.source = None;
    }

    pub fn begin_save(&mut self) -> Result<(WorkflowDefinition, u64), String> {
        if self.conflict {
            return Err("保存済みの内容が変わっています。競合を確認してください".into());
        }
        if self.pending_save.is_some() {
            return Err("前の保存が確定するまでお待ちください".into());
        }
        if !self.dirty() {
            return Err("変更内容はすでに保存されています".into());
        }
        let pending = (self.definition.clone(), self.version);
        self.pending_save = Some(pending.clone());
        Ok(pending)
    }

    pub fn reject_save(&mut self) {
        self.pending_save = None;
    }

    /// Store revisions are assigned on commit, not on individual edits. A
    /// save acknowledgement advances the base while preserving edits made
    /// since submission and any un-applied, focused inspector form.
    pub fn receive(&mut self, definition: &WorkflowDefinition, version: u64, protected: bool) {
        if version < self.version {
            return;
        }
        if self
            .pending_save
            .as_ref()
            .is_some_and(|(snapshot, base)| version > *base && same_content(definition, snapshot))
        {
            self.saved = definition.clone();
            self.definition.revision = definition.revision;
            self.version = version;
            self.conflict = false;
            self.pending_save = None;
        } else if same_content(definition, &self.saved)
            || (same_content(definition, &self.definition) && self.pending_save.is_none())
        {
            self.saved = definition.clone();
            self.definition.revision = definition.revision;
            self.version = version;
            self.conflict = false;
        } else if !self.dirty() && !protected && self.pending_save.is_none() {
            self.definition = definition.clone();
            self.saved = definition.clone();
            self.version = version;
            self.past.clear();
            self.future.clear();
            self.reselect();
        } else {
            self.conflict = true;
        }
    }

    pub fn add(&mut self, kind: NodeKind) -> Result<(), String> {
        if self.definition.nodes.len() >= MAX_NODES {
            return Err(format!("工程は最大{MAX_NODES}件です"));
        }
        let mut number = self.definition.nodes.len() + 1;
        while self.definition.node(&format!("node-{number}")).is_some() {
            number += 1;
        }
        let id = format!("node-{number}");
        let y = self
            .definition
            .nodes
            .iter()
            .map(|n| n.position.y)
            .fold(48.0, f32::max)
            + 176.0;
        self.change(|d| d.nodes.push(Node::new(&id, kind, 32.0, y)));
        self.selected = Some(id);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) {
        self.change(|d| {
            d.nodes.retain(|n| n.id != id);
            d.edges.retain(|e| e.from != id && e.to != id);
        });
        self.reselect();
    }

    pub fn connect(&mut self, from: &str, to: &str) -> Result<(), String> {
        let source = self.definition.node(from).ok_or("接続元がありません")?;
        let target = self.definition.node(to).ok_or("接続先がありません")?;
        let artifact = source.kind.output_kind();
        if from == to {
            return Err("同じ工程には接続できません".into());
        }
        if !target.kind.accepts(artifact) {
            return Err(format!(
                "{}は{}を受け取れません",
                target.title,
                artifact.label()
            ));
        }
        if self
            .definition
            .edges
            .iter()
            .any(|e| e.from == from && e.to == to)
        {
            return Err("この接続は既にあります".into());
        }
        if self.definition.descendants(to).contains(from) {
            return Err("循環する接続です。修正の繰り返しは実行時の回数制限で管理します".into());
        }
        if self.definition.edges.len() >= workflow::model::MAX_EDGES {
            return Err("接続の上限に達しました".into());
        }
        self.change(|d| {
            d.edges.push(Edge {
                from: from.into(),
                to: to.into(),
                artifact,
            })
        });
        self.source = None;
        Ok(())
    }

    pub fn arrange(&mut self) {
        self.change(|d| {
            let mut occupied = std::collections::BTreeSet::new();
            for node in &mut d.nodes {
                let mut slot = match node.kind {
                    NodeKind::Trigger => 0,
                    NodeKind::Plan => 1,
                    NodeKind::Implement => 2,
                    NodeKind::Review => 4,
                    NodeKind::Verify => 5,
                    NodeKind::Approval => 7,
                    NodeKind::PullRequest => 8,
                };
                while occupied.contains(&slot) {
                    slot += 3;
                }
                occupied.insert(slot);
                node.position = Point {
                    x: 32.0 + (slot % 3) as f32 * 252.0,
                    y: 48.0 + (slot / 3) as f32 * 212.0,
                };
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_connections_do_not_mutate_history() {
        let mut editor = Editor::new(WorkflowDefinition::standard(), 1);
        let before = editor.definition.clone();
        assert!(editor.connect("task", "pr").is_err());
        assert!(editor.connect("plan", "plan").is_err());
        assert!(editor.connect("task", "plan").is_err());
        assert_eq!(editor.definition, before);
        assert!(!editor.can_undo());
    }
    #[test]
    fn deletion_undo_restores_connections_and_redo_removes_them() {
        let mut editor = Editor::new(WorkflowDefinition::standard(), 1);
        let before = editor.definition.clone();
        editor.remove("review");
        assert!(
            editor
                .definition
                .edges
                .iter()
                .all(|e| e.from != "review" && e.to != "review")
        );
        assert!(!editor.definition.validate().is_empty());
        editor.undo();
        assert_eq!(editor.definition, before);
        editor.redo();
        assert!(editor.definition.node("review").is_none());
    }
    #[test]
    fn runtime_updates_preserve_draft_but_external_edits_conflict() {
        let saved = WorkflowDefinition::standard();
        let mut editor = Editor::new(saved.clone(), 4);
        editor.change(|d| d.name = "My draft".into());
        editor.receive(&saved, 5, false);
        assert_eq!(editor.version, 5);
        assert_eq!(editor.definition.name, "My draft");
        let mut other = saved;
        other.name = "Another editor".into();
        editor.receive(&other, 6, false);
        assert!(editor.conflict);
        assert_eq!(editor.version, 5);
        assert_eq!(editor.definition.name, "My draft");
    }

    #[test]
    fn multiple_edits_acknowledge_the_real_store_revision() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap().join("workflow");
        let store = workflow::store::Store::open(root).unwrap();
        let first = store
            .save(0, &WorkflowDefinition::standard(), None)
            .unwrap();
        let mut editor = Editor::new(first.definition, first.version);
        editor.change(|definition| definition.name = "Edited flow".into());
        editor.change(|definition| definition.nodes[0].title = "Edited task".into());
        assert_eq!(editor.definition.revision, 1);
        let (snapshot, version) = editor.begin_save().unwrap();
        assert!(editor.begin_save().is_err());
        let saved = store.save(version, &snapshot, None).unwrap();
        assert_eq!(saved.definition.revision, 2);
        editor.receive(&saved.definition, saved.version, false);
        assert!(!editor.dirty());
        assert!(!editor.conflict);
        assert!(editor.pending_save.is_none());
        assert_eq!(editor.definition, saved.definition);
    }

    #[test]
    fn save_acknowledgement_preserves_edits_made_after_submission() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap().join("workflow");
        let store = workflow::store::Store::open(root).unwrap();
        let initial = store
            .save(0, &WorkflowDefinition::standard(), None)
            .unwrap();
        let mut editor = Editor::new(initial.definition, initial.version);
        editor.change(|definition| definition.name = "Submitted name".into());
        let (snapshot, version) = editor.begin_save().unwrap();
        editor.change(|definition| definition.nodes[0].instruction = "Later instructions".into());
        let committed = store.save(version, &snapshot, None).unwrap();
        editor.receive(&committed.definition, committed.version, false);
        assert_eq!(editor.saved, committed.definition);
        assert_eq!(editor.definition.nodes[0].instruction, "Later instructions");
        assert!(editor.dirty());
        assert!(!editor.conflict);
        let (snapshot, version) = editor.begin_save().unwrap();
        let next = store.save(version, &snapshot, None).unwrap();
        editor.receive(&next.definition, next.version, false);
        assert!(!editor.dirty());
    }

    #[test]
    fn an_unapplied_form_blocks_unexpected_definition_replacement() {
        let initial = WorkflowDefinition::standard();
        let mut editor = Editor::new(initial.clone(), 4);
        let mut external = initial.clone();
        external.nodes[0].instruction = "Another window".into();
        external.revision += 1;
        editor.receive(&external, 5, true);
        assert!(editor.conflict);
        assert_eq!(editor.definition, initial);
        assert_eq!(editor.version, 4);
    }

    #[test]
    fn own_acknowledgement_normalizes_only_revision_and_protects_a_form() {
        let mut editor = Editor::new(WorkflowDefinition::standard(), 8);
        editor.change(|definition| definition.name = "Submitted".into());
        let (mut acknowledged, _) = editor.begin_save().unwrap();
        acknowledged.revision = 42;
        editor.receive(&acknowledged, 9, true);
        assert_eq!(editor.definition.revision, 42);
        assert_eq!(editor.saved, acknowledged);
        assert!(!editor.conflict);
        assert!(editor.pending_save.is_none());
        editor.undo();
        assert!(editor.dirty());
        assert_eq!(editor.definition.revision, 42);
    }

    #[test]
    fn rejected_save_keeps_the_draft_and_the_latest_runtime_version() {
        let initial = WorkflowDefinition::standard();
        let mut editor = Editor::new(initial.clone(), 1);
        editor.change(|definition| definition.name = "Retained draft".into());
        editor.begin_save().unwrap();
        editor.receive(&initial, 2, false);
        editor.reject_save();
        assert!(editor.dirty());
        assert_eq!(editor.definition.name, "Retained draft");
        assert_eq!(editor.begin_save().unwrap().1, 2);
    }
}
