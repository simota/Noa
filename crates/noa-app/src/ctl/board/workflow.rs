//! A live view of delegation, using the same pane relationships as the board.
//! Arrows mean that one pane spawned another; they do not imply task
//! dependencies, execution order, or that a completed turn passed review.

use super::*;

/// Below this width, keep the selected worker's card full-width and put its
/// delegation path above it instead of squeezing three cards side by side.
const FLOW_COLUMNS: usize = 90;
const CARD_ROWS: usize = 4;

struct Card {
    pane: Option<u64>,
    role: String,
    name: String,
    state: String,
    detail: String,
    color: Rgb,
    state_color: Rgb,
}

impl Board {
    pub(crate) fn open_workflow(&mut self) {
        if self.in_timeline() {
            self.leave_timeline(View::Workflow);
        } else {
            self.view = View::Workflow;
        }
        self.overview = View::Workflow;
        self.result_shown = false;
        self.result_scroll = 0;
        self.past = 0;
    }

    pub(super) fn toggle_workflow(&mut self) {
        if self.view == View::Workflow {
            self.view = View::List;
            self.overview = View::List;
            // The flow shows folded workers too. Reveal the selected one
            // when returning to the list, without changing its number.
            if let Some(pane) = self.selected {
                self.select(pane);
            }
        } else {
            self.open_workflow();
        }
    }

    pub(super) fn focus_orchestrator(&mut self) -> Option<Action> {
        let pane = self
            .orchestrator()
            .and_then(|pane| self.panel(pane))
            .filter(|panel| panel.exit.is_none())
            .map(|panel| panel.pane_id.0);
        if let Some(pane) = pane {
            return Some(Action::Focus(pane));
        }
        self.message = Some(Notice::warn(
            self.lang
                .pick(
                    "移動できるオーケストレーターのペインがありません",
                    "no live orchestrator pane to go to",
                )
                .to_string(),
        ));
        None
    }

    pub(super) fn render_workflow(&self, cols: usize, rows: usize, now: Instant) -> Vec<String> {
        let mut lines = self.header(cols, now);
        let room = rows.saturating_sub(lines.len() + 1);
        let nodes = self.tree();
        if nodes.is_empty() && self.lead().is_none() {
            lines.extend(self.empty_lines(cols, room, now));
            return lines;
        }

        let mut body = Vec::new();
        let mut selected_line = None;
        let mut selected_start = 0;
        let mut selected_end = 0;
        // The lead can itself hold up every worker. Give it its own card
        // while it needs an answer, including in the narrow layout.
        if let Some(lead) = self.lead() {
            let card = self.flow_lead(Some(lead.pane_id.0), now);
            body.extend(self.flow_card(&card, cols));
            if self.selected == Some(lead.pane_id.0) {
                selected_line = Some(2);
                selected_end = body.len();
            }
        }
        for node in &nodes {
            let cards = self.flow_path(node, now);
            let selected = Some(node.panel.pane_id.0) == self.selected;
            let start = body.len();
            if cols >= FLOW_COLUMNS {
                let gap = 3;
                let card_cols = (cols - gap * (cards.len() - 1)) / cards.len();
                let drawn: Vec<Vec<String>> = cards
                    .iter()
                    .map(|card| self.flow_card(card, card_cols))
                    .collect();
                for row in 0..CARD_ROWS + 2 {
                    let mut line = String::new();
                    for (index, card) in drawn.iter().enumerate() {
                        if index > 0 {
                            let mut arrow = Line::default();
                            arrow.fg(BORDER, if row == 2 { " → " } else { "   " });
                            line.push_str(&arrow.finish(gap));
                        }
                        line.push_str(&card[row]);
                    }
                    line.push_str(
                        &" ".repeat(cols - card_cols * cards.len() - gap * (cards.len() - 1)),
                    );
                    body.push(line);
                }
                if selected {
                    selected_line = Some(start + 2);
                }
            } else {
                // Keep the target's name/state readable in a narrow pane.
                // Ancestors stay in the breadcrumb in actual parent order.
                let ancestors = cards[..cards.len() - 1]
                    .iter()
                    .map(|card| card.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" → ");
                let mut path = Line::default();
                path.plain(" ").fg(
                    DIM,
                    &fit(
                        &format!("{}: {ancestors} ↓", self.lang.pick("委譲元", "from")),
                        cols.saturating_sub(1),
                    ),
                );
                body.push(path.finish(cols));
                body.extend(self.flow_card(cards.last().unwrap(), cols));
                if selected {
                    selected_line = Some(start + 3);
                }
            }
            if selected {
                selected_start = start;
                selected_end = body.len();
            }
        }

        // A minimum-height pane still shows the selected worker, not only
        // the top border of a card. Taller panes keep its entire path visible.
        let mut available = room;
        if room >= 4 {
            let picks = self.picks();
            let position = self
                .selected
                .and_then(|selected| picks.iter().position(|pane| *pane == selected))
                .map_or(0, |index| index + 1);
            let title = self.lang.pick(
                format!(
                    " 委譲フロー {position}/{} · 指揮 → ワーカー → 子ワーカー",
                    picks.len()
                ),
                format!(
                    " Delegation flow {position}/{} · orchestrator → worker → child",
                    picks.len()
                ),
            );
            let mut heading = Line::default();
            heading.fg(BLUE, &fit(&title, cols));
            lines.push(heading.finish(cols));
            available -= 1;
        }
        let anchor = selected_line.unwrap_or(0);
        let offset = if body.len() <= available {
            0
        } else if selected_end.saturating_sub(selected_start) <= available {
            selected_start.min(body.len().saturating_sub(available))
        } else {
            anchor.min(body.len().saturating_sub(available))
        };
        lines.extend(body.into_iter().skip(offset).take(available));
        lines
    }

    fn flow_path(&self, node: &Node<'_>, now: Instant) -> Vec<Card> {
        let mut cards = Vec::new();
        match node.branch {
            Branch::Top { .. } => {
                cards.push(self.flow_lead(node.panel.parent_pane_id.map(|id| id.0), now));
                cards.push(self.flow_worker(node.panel, false, now));
            }
            Branch::Kid { parent, .. } => {
                let parent = self.panel(parent).expect("tree parents are listed");
                cards.push(self.flow_lead(parent.parent_pane_id.map(|id| id.0), now));
                cards.push(self.flow_worker(parent, false, now));
                cards.push(self.flow_worker(node.panel, true, now));
            }
        }
        cards
    }

    fn flow_lead(&self, pane: Option<u64>, now: Instant) -> Card {
        let role = self.lang.pick("◆ 指揮", "◆ Orchestrator").to_string();
        let own = self.orchestrates_itself() && pane == self.me;
        if let Some(panel) = pane.and_then(|pane| self.panel(pane)).filter(|_| !own) {
            let status = Status::of(panel);
            return Card {
                pane,
                role,
                name: if panel.name.trim().is_empty() {
                    format!("pane {}", panel.pane_id.0)
                } else {
                    clean(&panel.name)
                },
                state: format!(
                    "{} {}",
                    status.mark(),
                    self.state_text(panel.pane_id.0, status, StateForm::Short, now)
                ),
                detail: clean(&self.last_word(panel)),
                color: BLUE,
                state_color: status.color(),
            };
        }
        Card {
            pane,
            role,
            name: if own {
                self.lang.pick("このボード", "this board").to_string()
            } else {
                pane.map_or_else(
                    || self.lang.pick("不明", "unknown").to_string(),
                    |pane| format!("pane {pane}"),
                )
            },
            state: if own {
                self.lang.pick("ワーカーを統括", "coordinates workers")
            } else {
                self.lang.pick("ペイン未表示", "pane not listed")
            }
            .to_string(),
            detail: String::new(),
            color: BLUE,
            state_color: DIM,
        }
    }

    fn flow_worker(&self, panel: &Panel, child: bool, now: Instant) -> Card {
        let pane = panel.pane_id.0;
        let status = Status::of(panel);
        let role = if child {
            self.lang.pick("子ワーカー", "Child worker")
        } else {
            self.lang.pick("ワーカー", "Worker")
        };
        let detail = clean(&self.last_word(panel));
        let detail = if detail.is_empty() {
            match self.insight.task(pane) {
                Some(Task::Sent { text, .. }) => clean(text),
                _ => self.lang.pick("報告なし", "no report yet").to_string(),
            }
        } else {
            detail
        };
        Card {
            pane: Some(pane),
            role: format!("{} {role}", self.number(pane)),
            name: self.name(pane),
            state: format!(
                "{} {}",
                status.mark(),
                self.state_text(pane, status, StateForm::Short, now)
            ),
            detail,
            color: status.color(),
            state_color: status.color(),
        }
    }

    fn flow_card(&self, card: &Card, cols: usize) -> Vec<String> {
        let selected = card.pane.is_some() && card.pane == self.selected;
        let color = if self.offline.is_some() {
            DIM
        } else {
            card.color
        };
        let state_color = if self.offline.is_some() {
            DIM
        } else {
            card.state_color
        };
        let body = [
            (Some(color), card.role.clone()),
            (
                None,
                format!("{}{}", if selected { "▶ " } else { "" }, card.name),
            ),
            (Some(state_color), card.state.clone()),
            (Some(DIM), card.detail.clone()),
        ];
        boxed(
            &body,
            cols,
            CARD_ROWS,
            if selected { color } else { BORDER },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_ipc::{PanelExit, WireId};

    fn panel(id: u64, parent: Option<u64>, label: &str, state: &str) -> Panel {
        serde_json::from_value(serde_json::json!({
            "windowGroupId": "1", "windowId": "1", "paneId": id.to_string(),
            "parentPaneId": parent.map(|id| id.to_string()),
            "name": label, "label": label, "cwd": "/repo", "busy": false,
            "attention": false, "preview": [],
            "agent": {"kind": "claude", "state": state, "seq": 1}
        }))
        .unwrap()
    }

    fn board() -> Board {
        let mut board = Board::new(Some(1), Some(9));
        board.update(
            vec![
                panel(1, None, "lead", "running"),
                panel(2, Some(1), "api", "running"),
                panel(3, Some(2), "api-tests", "permission"),
                panel(4, Some(1), "docs", "finished"),
                panel(7, Some(6), "other-board", "running"),
            ],
            Instant::now(),
        );
        board
    }

    fn plain(line: &str) -> String {
        let mut text = String::new();
        let mut chars = line.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                text.push(ch);
            }
        }
        text
    }

    #[test]
    fn flow_keeps_selection_and_returns_after_details_and_timeline() {
        let mut board = board();
        board.select(3);
        board.key(Key::Char('v'));
        assert_eq!(board.view, View::Workflow);
        assert_eq!(board.selected, Some(3));
        board.key(Key::Enter);
        assert_eq!(board.view, View::Detail(3));
        assert!(plain(&board.render(80, 24, Instant::now())[0]).contains("← フロー"));
        board.key(Key::Esc);
        assert_eq!(board.view, View::Workflow);
        board.key(Key::Char('t'));
        assert!(board.in_timeline());
        board.key(Key::Char('t'));
        assert_eq!(board.view, View::Workflow);
        assert_eq!(board.key(Key::Char('g')), Some(Action::Focus(3)));
        board.key(Key::Char('v'));
        assert_eq!(board.view, View::List);
        assert_eq!(board.selected, Some(3));
    }

    #[test]
    fn flow_navigation_reaches_folded_children_and_finished_workers() {
        let mut board = board();
        board
            .panels
            .iter_mut()
            .find(|panel| panel.pane_id.0 == 3)
            .unwrap()
            .agent
            .as_mut()
            .unwrap()
            .state = Some("running".to_string());
        board.collapsed.insert(2);
        board.open_workflow();
        board.select(2);
        board.key(Key::Char('j'));
        assert_eq!(board.selected, Some(3));
        board.key(Key::Char('j'));
        assert_eq!(board.selected, Some(4));
        board.key(Key::Char('k'));
        assert_eq!(board.selected, Some(3));
        assert!(!board.picks().contains(&7));
    }

    #[test]
    fn delegation_paths_and_selected_worker_fit_narrow_and_wide_panes() {
        let mut board = board();
        board.open_workflow();
        board.select(3);
        for lang in [Lang::Ja, Lang::En] {
            board.lang = lang;
            for cols in [24, 40, 69, 80, 90, 120, 180] {
                for rows in [6, 8, 12, 24, 40] {
                    let screen = board.render(cols, rows, Instant::now());
                    assert_eq!(screen.len(), rows);
                    for line in &screen {
                        assert_eq!(plain(line).width(), cols, "{cols}x{rows}: {line:?}");
                    }
                    let text = screen
                        .iter()
                        .map(|line| plain(line))
                        .collect::<Vec<_>>()
                        .join("\n");
                    assert!(text.contains("▶ api-tests"), "{cols}x{rows}: {text}");
                    assert!(!text.contains("other-board"));
                }
            }
        }
        board.lang = Lang::En;
        let node = board
            .tree()
            .into_iter()
            .find(|node| node.panel.pane_id.0 == 3)
            .unwrap();
        let cards = board.flow_path(&node, Instant::now());
        assert_eq!(
            cards
                .iter()
                .map(|card| card.name.as_str())
                .collect::<Vec<_>>(),
            ["lead", "api", "api-tests"]
        );
        assert_eq!(cards[0].role, "◆ Orchestrator");
        assert!(cards[2].role.contains("Child worker"));
    }

    #[test]
    fn a_selected_lead_is_visible_in_narrow_panes_while_workers_are_present() {
        let mut board = board();
        let mut panels = board.panels.clone();
        panels[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(panels, Instant::now());
        board.open_workflow();
        board.select(1);
        for (cols, rows) in [(24, 6), (40, 8), (80, 24), (120, 24)] {
            let text = board
                .render(cols, rows, Instant::now())
                .iter()
                .map(|line| plain(line))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("▶ lead"), "{cols}x{rows}: {text}");
            assert!(text.contains("承認"), "{cols}x{rows}: {text}");
            assert_eq!(board.key(Key::Char('g')), Some(Action::Focus(1)));
        }
    }

    #[test]
    fn closed_workers_leave_the_flow_and_exited_workers_remain_inspectable() {
        let mut board = board();
        board.open_workflow();
        board.select(3);
        board.key(Key::Enter);
        let mut panels = board.panels.clone();
        panels.retain(|panel| panel.pane_id.0 != 3);
        panels
            .iter_mut()
            .find(|panel| panel.pane_id.0 == 2)
            .unwrap()
            .exit = Some(PanelExit { code: Some(1) });
        board.update(panels, Instant::now());
        assert_eq!(board.view, View::Workflow);
        assert!(!board.picks().contains(&3));
        assert!(board.picks().contains(&2));
        board.select(2);
        board.key(Key::Char('s'));
        assert_eq!(board.mode, Mode::Normal);
        assert!(board.message.is_some());
    }

    #[test]
    fn an_empty_flow_can_scroll_its_closed_workers() {
        let mut board = board();
        board.open_workflow();
        board.update(Vec::new(), Instant::now());
        assert!(board.picks().is_empty());
        let before = board.render(80, 6, Instant::now());
        for _ in 0..10 {
            board.key(Key::Char('j'));
        }
        let after = board.render(80, 6, Instant::now());
        assert_ne!(before, after);
        assert!(after.iter().any(|line| plain(line).contains("閉じた")));
        let past = board.past;
        board.key(Key::Char('k'));
        assert!(board.past < past);
    }

    #[test]
    fn orchestrator_shortcut_needs_a_live_scoped_orchestrator() {
        let mut board = board();
        assert_eq!(board.key(Key::Char('o')), Some(Action::Focus(1)));
        board.of = None;
        assert_eq!(board.key(Key::Char('o')), None);
        board.of = Some(1);
        board.panels[0].exit = Some(PanelExit { code: Some(0) });
        assert_eq!(board.key(Key::Char('o')), None);
        board.panels[0].exit = None;
        board
            .timeline
            .set_board_id(Some("bd-000000000000000a".to_string()));
        assert_eq!(board.key(Key::Char('o')), None);
        board.open_workflow();
        assert!(board.picks().is_empty());
        // A malformed cyclic parent list cannot bring a foreign worker back.
        board.panels[0].parent_pane_id = Some(WireId(2));
        assert!(board.picks().is_empty());
    }
}
