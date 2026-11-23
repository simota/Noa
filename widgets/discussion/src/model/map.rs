//! The discussion as a mind map: the topic at the top left, its issues to
//! the right of it, sub-issues to the right of theirs, and the minutes items
//! that belong to an issue as leaves under it. Pure layout: every node gets
//! a rectangle, every edge joins a parent to a child.

use std::collections::HashMap;

use super::types::{IssueId, MinuteSection, Session};

/// Node sizes and spacing, in logical pixels.
pub const TOPIC_W: f32 = 220.0;
pub const TOPIC_H: f32 = 96.0;
pub const ISSUE_W: f32 = 260.0;
pub const ISSUE_H: f32 = 104.0;
pub const LEAF_W: f32 = 240.0;
pub const LEAF_H: f32 = 48.0;
/// Between columns (where the edges run) and between stacked nodes.
pub const COL_GAP: f32 = 56.0;
pub const ROW_GAP: f32 = 14.0;
/// Around the whole map.
pub const MARGIN: f32 = 20.0;

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Topic,
    Issue(IssueId),
    /// A minutes item: its section and its text.
    Minute {
        section: MinuteSection,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    pub depth: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Node {
    /// Where an edge leaves this node (right edge, middle) and where one
    /// arrives (left edge, middle).
    pub fn out_point(&self) -> (f32, f32) {
        (self.x + self.w, self.y + self.h / 2.0)
    }

    pub fn in_point(&self) -> (f32, f32) {
        (self.x, self.y + self.h / 2.0)
    }
}

/// Parent and child, as indices into [`MindMap::nodes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MindMap {
    /// The topic first, then depth-first in the order issues were added.
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// The size the nodes take, margins included.
    pub width: f32,
    pub height: f32,
}

/// What a node is before it is placed.
struct Draft {
    kind: NodeKind,
    children: Vec<Draft>,
    span: f32,
}

impl Draft {
    fn new(kind: NodeKind, children: Vec<Draft>) -> Self {
        let own = Self::size_of(&kind).1;
        let span = if children.is_empty() {
            own
        } else {
            let child_span: f32 = children.iter().map(|c| c.span).sum::<f32>()
                + ROW_GAP * (children.len() - 1) as f32;
            own.max(child_span)
        };
        Self {
            kind,
            children,
            span,
        }
    }

    fn size_of(kind: &NodeKind) -> (f32, f32) {
        match kind {
            NodeKind::Topic => (TOPIC_W, TOPIC_H),
            NodeKind::Issue(_) => (ISSUE_W, ISSUE_H),
            NodeKind::Minute { .. } => (LEAF_W, LEAF_H),
        }
    }

    fn size(&self) -> (f32, f32) {
        Self::size_of(&self.kind)
    }
}

/// The x where column `depth` starts.
fn column_x(depth: usize) -> f32 {
    match depth {
        0 => MARGIN,
        d => MARGIN + TOPIC_W + COL_GAP + (d - 1) as f32 * (ISSUE_W + COL_GAP),
    }
}

/// The map of `session`. A minutes item hangs under the first issue it
/// names, or under the topic when it names none.
pub fn layout(session: &Session) -> MindMap {
    let issues = &session.issues.issues;
    let n = issues.len();
    let mut by_id: HashMap<&str, usize> = HashMap::with_capacity(n);
    for (idx, issue) in issues.iter().enumerate() {
        by_id.insert(issue.id.as_str(), idx);
    }
    let mut roots = Vec::new();
    let mut issue_children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (idx, issue) in issues.iter().enumerate() {
        match &issue.parent {
            None => roots.push(idx),
            Some(parent) => {
                if let Some(&pidx) = by_id.get(parent.as_str()) {
                    issue_children[pidx].push(idx);
                }
            }
        }
    }
    let mut issue_leaves: Vec<Vec<NodeKind>> = vec![Vec::new(); n];
    let mut topic_leaves: Vec<NodeKind> = Vec::new();
    for section in MinuteSection::ALL {
        for item in session.minutes.section(section) {
            let kind = NodeKind::Minute {
                section,
                text: item.text.clone(),
            };
            if let Some(&idx) = item.issues.iter().find_map(|id| by_id.get(id.as_str())) {
                issue_leaves[idx].push(kind);
            } else {
                topic_leaves.push(kind);
            }
        }
    }

    fn issue_draft(
        idx: usize,
        issues: &[super::types::Issue],
        issue_children: &[Vec<usize>],
        issue_leaves: &mut [Vec<NodeKind>],
    ) -> Draft {
        let mut children: Vec<Draft> = issue_children[idx]
            .iter()
            .map(|&child| issue_draft(child, issues, issue_children, issue_leaves))
            .collect();
        for kind in std::mem::take(&mut issue_leaves[idx]) {
            children.push(Draft::new(kind, Vec::new()));
        }
        Draft::new(NodeKind::Issue(issues[idx].id.clone()), children)
    }

    let mut children: Vec<Draft> = roots
        .iter()
        .map(|&idx| issue_draft(idx, issues, &issue_children, &mut issue_leaves))
        .collect();
    for kind in topic_leaves {
        children.push(Draft::new(kind, Vec::new()));
    }
    let root = Draft::new(NodeKind::Topic, children);

    let total_nodes = 1
        + n
        + MinuteSection::ALL
            .iter()
            .map(|s| session.minutes.section(*s).len())
            .sum::<usize>();
    let mut map = MindMap {
        nodes: Vec::with_capacity(total_nodes),
        edges: Vec::with_capacity(total_nodes.saturating_sub(1)),
        width: 0.0,
        height: root.span + 2.0 * MARGIN,
    };
    place(root, 0, MARGIN, None, &mut map);
    map.width = map.nodes.iter().map(|n| n.x + n.w).fold(0.0, f32::max) + MARGIN;
    map
}

/// Places `draft` in the band of height `draft.span` starting at `top`:
/// the node at the top of its band, level with its first child, and the
/// children stacked below one another. The topic so opens where the map
/// starts, however many issues there are.
fn place(draft: Draft, depth: usize, top: f32, parent: Option<usize>, map: &mut MindMap) {
    let (w, h) = draft.size();
    let index = map.nodes.len();
    map.nodes.push(Node {
        kind: draft.kind,
        depth,
        x: column_x(depth),
        y: top,
        w,
        h,
    });
    if let Some(parent) = parent {
        map.edges.push(Edge {
            from: parent,
            to: index,
        });
    }
    let mut y = top;
    for child in draft.children {
        let span = child.span;
        place(child, depth + 1, y, Some(index), map);
        y += span + ROW_GAP;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::reduce::{self, Event};
    use crate::model::types::{
        AgentKind, IssueOp, MinuteOp, ModeratorOutput, Participant, Role, RoundKind, SessionId,
    };

    fn session() -> Session {
        let p = |id: &str, role| Participant {
            id: crate::model::types::PartId::new(id),
            kind: AgentKind::Claude,
            role,
            pane_id: None,
            setup: Default::default(),
        };
        let mut s = reduce::create(&[Event::Created {
            id: SessionId::new("s"),
            topic: "Pick a database".into(),
            workdir: None,
            max_rounds: None,
            moderator: p("noa", Role::Moderator),
            panelists: vec![p("claude", Role::Panelist)],
            at: 0,
        }])
        .unwrap();
        let add = |id: &str, parent: Option<&str>| IssueOp::Add {
            id: IssueId::new(id),
            parent: parent.map(IssueId::new),
            title: format!("issue {id}"),
        };
        let minute = |section, text: &str, issues: &[&str]| MinuteOp {
            section,
            text: text.into(),
            issues: issues.iter().map(|i| IssueId::new(*i)).collect(),
        };
        reduce::apply(
            &mut s,
            &[
                Event::RoundStarted {
                    no: 1,
                    kind: RoundKind::Open,
                    at: 0,
                },
                Event::ModeratorAccepted {
                    round: 1,
                    output: ModeratorOutput {
                        summary: "s".into(),
                        issue_ops: vec![add("A", None), add("A1", Some("A")), add("B", None)],
                        minutes_ops: vec![
                            minute(MinuteSection::Decisions, "use SQLite", &["A"]),
                            minute(MinuteSection::Actions, "benchmark", &["A1"]),
                            minute(MinuteSection::OpenQuestions, "who owns ops", &[]),
                        ],
                        questions: Default::default(),
                        propose_debate: None,
                        verdict: None,
                        conclusion: None,
                        round_limit: None,
                        close: false,
                    },
                },
            ],
        )
        .unwrap();
        s
    }

    fn find(map: &MindMap, pred: impl Fn(&NodeKind) -> bool) -> (usize, &Node) {
        map.nodes
            .iter()
            .enumerate()
            .find(|(_, n)| pred(&n.kind))
            .unwrap()
    }

    fn parent_of(map: &MindMap, child: usize) -> Option<usize> {
        map.edges.iter().find(|e| e.to == child).map(|e| e.from)
    }

    #[test]
    fn issues_branch_from_the_topic_and_minutes_hang_under_their_issue() {
        let map = layout(&session());
        assert_eq!(map.nodes[0].kind, NodeKind::Topic);
        // Topic, A, A1, B, three minutes items.
        assert_eq!(map.nodes.len(), 7);
        assert_eq!(map.edges.len(), 6, "a tree: one edge per node but the root");
        let issue = |id: &str| {
            let id = IssueId::new(id);
            find(&map, move |k| *k == NodeKind::Issue(id.clone())).0
        };
        let minute = |text: &'static str| {
            find(
                &map,
                move |k| matches!(k, NodeKind::Minute { text: t, .. } if t == text),
            )
            .0
        };
        assert_eq!(parent_of(&map, issue("A")), Some(0));
        assert_eq!(parent_of(&map, issue("B")), Some(0));
        assert_eq!(parent_of(&map, issue("A1")), Some(issue("A")));
        assert_eq!(parent_of(&map, minute("use SQLite")), Some(issue("A")));
        assert_eq!(parent_of(&map, minute("benchmark")), Some(issue("A1")));
        assert_eq!(
            parent_of(&map, minute("who owns ops")),
            Some(0),
            "no issue: under the topic"
        );
    }

    #[test]
    fn columns_go_right_by_depth_and_nothing_overlaps() {
        let map = layout(&session());
        for edge in &map.edges {
            let (from, to) = (&map.nodes[edge.from], &map.nodes[edge.to]);
            assert_eq!(to.depth, from.depth + 1);
            assert!(from.out_point().0 + COL_GAP <= to.in_point().0 + 0.5);
        }
        for (i, a) in map.nodes.iter().enumerate() {
            assert!(a.x >= MARGIN - 0.5 && a.y >= MARGIN - 0.5);
            assert!(a.x + a.w <= map.width && a.y + a.h <= map.height + 0.5);
            for b in &map.nodes[i + 1..] {
                let apart =
                    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn the_topic_and_every_parent_line_up_with_their_first_child() {
        // Many issues: the map is far taller than a window, and the topic
        // still opens at the top, where the map starts.
        let mut s = session();
        for n in 0..12 {
            let mut issue = s.issues.issues[0].clone();
            issue.id = IssueId::new(format!("Z{n}"));
            issue.parent = None;
            s.issues.issues.push(issue);
        }
        let map = layout(&s);
        assert!(map.height > 1500.0);
        assert_eq!(map.nodes[0].y, MARGIN);
        for (index, node) in map.nodes.iter().enumerate() {
            if let Some(first) = map.edges.iter().find(|e| e.from == index) {
                assert_eq!(node.y, map.nodes[first.to].y, "{node:?}");
            }
        }
    }

    #[test]
    fn an_empty_discussion_is_the_topic_alone() {
        let mut s = session();
        s.issues.issues.clear();
        for section in MinuteSection::ALL {
            s.minutes.section_mut(section).clear();
        }
        let map = layout(&s);
        assert_eq!(map.nodes.len(), 1);
        assert!(map.edges.is_empty());
        assert_eq!(map.width, MARGIN + TOPIC_W + MARGIN);
    }

    #[test]
    fn deep_issue_tree_positions_and_edges_are_exact() {
        let mut s = session();
        let add = |id: &str, parent: Option<&str>| IssueOp::Add {
            id: IssueId::new(id),
            parent: parent.map(IssueId::new),
            title: format!("issue {id}"),
        };
        reduce::apply(
            &mut s,
            &[
                Event::IssueChanged {
                    round: 1,
                    op: add("A1a", Some("A1")),
                },
                Event::IssueChanged {
                    round: 1,
                    op: add("A1a1", Some("A1a")),
                },
                Event::IssueChanged {
                    round: 1,
                    op: add("A2", Some("A")),
                },
            ],
        )
        .unwrap();
        let map = layout(&s);
        assert_eq!(map.nodes.len(), 10);
        assert_eq!(map.edges.len(), 9);
        // Pin exact coordinates of every node in depth-first order.
        let coords: Vec<(usize, f32, f32)> =
            map.nodes.iter().map(|n| (n.depth, n.x, n.y)).collect();
        assert_eq!(
            coords,
            vec![
                (0, 20.0, 20.0),
                (1, 296.0, 20.0),
                (2, 612.0, 20.0),
                (3, 928.0, 20.0),
                (4, 1244.0, 20.0),
                (3, 928.0, 138.0),
                (2, 612.0, 200.0),
                (2, 612.0, 318.0),
                (1, 296.0, 380.0),
                (1, 296.0, 498.0),
            ]
        );
        assert_eq!(map.width, 1524.0);
        assert_eq!(map.height, 566.0);
    }
}
