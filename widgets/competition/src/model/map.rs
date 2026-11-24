//! Each contestant's direction as a mind map: the request at the top left,
//! the contestants to the right of it, and each contestant's accepted
//! submissions, round by round, to the right of theirs. Pure layout: every
//! node gets a rectangle, every edge joins a parent to a child.

use super::types::{PartId, RoundNo, Session};

/// Node sizes and spacing, in logical pixels.
pub const REQUEST_W: f32 = 220.0;
pub const REQUEST_H: f32 = 96.0;
pub const PART_W: f32 = 190.0;
pub const PART_H: f32 = 72.0;
pub const ENTRY_W: f32 = 320.0;
pub const ENTRY_H: f32 = 96.0;
/// Between columns (where the edges run) and between stacked nodes.
pub const COL_GAP: f32 = 56.0;
pub const ROW_GAP: f32 = 12.0;
/// Extra space between one contestant's branch and the next.
pub const BRANCH_GAP: f32 = 24.0;
/// Around the whole map.
pub const MARGIN: f32 = 20.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Request,
    Contestant(PartId),
    /// One accepted submission of a contestant.
    Entry {
        part: PartId,
        round: RoundNo,
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
    /// The request first, then each contestant followed by its entries.
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// The size the nodes take, margins included.
    pub width: f32,
    pub height: f32,
}

const PART_X: f32 = MARGIN + REQUEST_W + COL_GAP;
const ENTRY_X: f32 = PART_X + PART_W + COL_GAP;

/// The map of `session`, contestants in `order` (the ones not in the session
/// are skipped). Each contestant opens level with its first entry, its
/// entries stacked below in round order.
pub fn layout(session: &Session, order: &[PartId]) -> MindMap {
    let mut map = MindMap {
        nodes: vec![Node {
            kind: NodeKind::Request,
            depth: 0,
            x: MARGIN,
            y: MARGIN,
            w: REQUEST_W,
            h: REQUEST_H,
        }],
        edges: Vec::new(),
        width: MARGIN + REQUEST_W + MARGIN,
        height: MARGIN + REQUEST_H + MARGIN,
    };
    let mut top = MARGIN;
    for part in order.iter().filter(|p| session.is_contestant(p)) {
        let index = map.nodes.len();
        map.nodes.push(Node {
            kind: NodeKind::Contestant(part.clone()),
            depth: 1,
            x: PART_X,
            y: top,
            w: PART_W,
            h: PART_H,
        });
        map.edges.push(Edge { from: 0, to: index });
        let mut y = top;
        for round in session
            .rounds
            .iter()
            .filter(|r| r.submissions.contains_key(part))
        {
            let entry = map.nodes.len();
            map.nodes.push(Node {
                kind: NodeKind::Entry {
                    part: part.clone(),
                    round: round.no,
                },
                depth: 2,
                x: ENTRY_X,
                y,
                w: ENTRY_W,
                h: ENTRY_H,
            });
            map.edges.push(Edge {
                from: index,
                to: entry,
            });
            y += ENTRY_H + ROW_GAP;
        }
        let bottom = (y - ROW_GAP).max(top + PART_H);
        top = bottom + ROW_GAP + BRANCH_GAP;
    }
    for node in &map.nodes {
        map.width = map.width.max(node.x + node.w + MARGIN);
        map.height = map.height.max(node.y + node.h + MARGIN);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{
        AgentKind, Participant, Phase, Role, Round, RoundKind, SessionId, Submission,
    };
    use std::collections::BTreeMap;

    fn session(entries: &[(&str, &[RoundNo])]) -> Session {
        let p = |id: &str, role| Participant {
            id: PartId::new(id),
            kind: AgentKind::Claude,
            role,
            pane_id: None,
            setup: Default::default(),
        };
        let last = entries
            .iter()
            .flat_map(|(_, rounds)| rounds.iter().copied())
            .max()
            .unwrap_or(0);
        let rounds = (1..=last)
            .map(|no| Round {
                no,
                kind: RoundKind::Blind,
                phase: Phase::Closed,
                started_at: 0,
                briefs: Vec::new(),
                submissions: entries
                    .iter()
                    .filter(|(_, rounds)| rounds.contains(&no))
                    .map(|(id, _)| {
                        (
                            PartId::new(*id),
                            Submission {
                                part: PartId::new(*id),
                                summary: format!("{id} r{no}"),
                                body: String::new(),
                                artifacts: Vec::new(),
                                at: 0,
                            },
                        )
                    })
                    .collect::<BTreeMap<_, _>>(),
                judge_brief: None,
                co_judge_briefs: Vec::new(),
                scorecards: BTreeMap::new(),
                verdict_brief: None,
                outcome: None,
            })
            .collect();
        Session {
            id: SessionId::new("s"),
            request: "build it".into(),
            workdir: None,
            max_rounds: None,
            reveal_interval: 0,
            rev: 1,
            created_at: 0,
            judge: p("judge", Role::Judge),
            co_judges: Vec::new(),
            contestants: entries
                .iter()
                .map(|(id, _)| p(id, Role::Contestant))
                .collect(),
            messages: Vec::new(),
            rounds,
            winner: None,
            closed: false,
            hold: false,
        }
    }

    fn order(ids: &[&str]) -> Vec<PartId> {
        ids.iter().map(|id| PartId::new(*id)).collect()
    }

    #[test]
    fn contestants_branch_from_the_request_and_entries_follow_round_order() {
        let s = session(&[("claude", &[1, 2, 3]), ("codex", &[1, 3])]);
        let map = layout(&s, &order(&["codex", "claude"]));
        let kinds: Vec<_> = map.nodes.iter().map(|n| n.kind.clone()).collect();
        let entry = |part: &str, round| NodeKind::Entry {
            part: PartId::new(part),
            round,
        };
        assert_eq!(
            kinds,
            vec![
                NodeKind::Request,
                NodeKind::Contestant(PartId::new("codex")),
                entry("codex", 1),
                entry("codex", 3),
                NodeKind::Contestant(PartId::new("claude")),
                entry("claude", 1),
                entry("claude", 2),
                entry("claude", 3),
            ]
        );
        assert_eq!(map.edges.len(), map.nodes.len() - 1, "a tree");
        assert_eq!(map.edges[0], Edge { from: 0, to: 1 });
        assert_eq!(map.edges[1], Edge { from: 1, to: 2 });
        assert_eq!(map.edges[3], Edge { from: 0, to: 4 });
    }

    #[test]
    fn nothing_overlaps_and_everything_fits() {
        let s = session(&[("a", &[1, 2, 3, 4]), ("b", &[]), ("c", &[2])]);
        let map = layout(&s, &order(&["a", "b", "c"]));
        for edge in &map.edges {
            let (from, to) = (&map.nodes[edge.from], &map.nodes[edge.to]);
            assert_eq!(to.depth, from.depth + 1);
            assert!(from.out_point().0 + COL_GAP <= to.in_point().0 + 0.5);
        }
        for (i, a) in map.nodes.iter().enumerate() {
            assert!(a.x + a.w + MARGIN <= map.width + 0.5);
            assert!(a.y + a.h + MARGIN <= map.height + 0.5);
            for b in &map.nodes[i + 1..] {
                let apart =
                    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn unknown_ids_are_skipped_and_an_empty_session_is_the_request_alone() {
        let s = session(&[]);
        let map = layout(&s, &order(&["ghost"]));
        assert_eq!(map.nodes.len(), 1);
        assert_eq!(map.width, MARGIN + REQUEST_W + MARGIN);
    }
}
