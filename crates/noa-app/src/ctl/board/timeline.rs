//! The board's timeline view (`docs/specs/board-timeline.md` AC-3, L2-9):
//! a board's posts, newest first, with a preview or a full post, filters, a
//! query line and the count of posts since the person last looked. A pure
//! model and renderer like the worker list; `board.rs` does the I/O.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use noa_ipc::Panel;
use noa_ipc::timeline::{
    ChangesParams, ContextResult, Filter, GetParams, GetResult, LIST_MAX, ListParams,
    MAX_QUERY_CHARS, Outcome, PageResult, PostKind, PostSummary, Provenance, Seq,
};
use unicode_width::UnicodeWidthStr;

use super::{
    BLUE, BORDER, DIM, GREEN, GREY, Key, Lang, Line, ORANGE, RED, Rgb, STACKED_COLS, STACKED_ROWS,
    WIDE_COLS, clean, fit, rule, wrap,
};

/// How often the board reads the timeline's context and new posts while
/// the timeline is shown (D11: polling only).
pub(crate) const POLL_EVERY: Duration = Duration::from_secs(2);
/// The most `changes` pages read in one poll.
pub(crate) const CHANGES_PAGES: usize = 5;
/// Post bodies kept for the preview and the full post.
const BODY_CACHE: usize = 32;
/// Summary lines a list item shows.
const SUMMARY_LINES: usize = 3;

pub(crate) type PostId = String;

const KINDS: [PostKind; 5] = [
    PostKind::Summary,
    PostKind::Milestone,
    PostKind::Blocker,
    PostKind::Decision,
    PostKind::Handoff,
];
const OUTCOMES: [Outcome; 5] = [
    Outcome::Succeeded,
    Outcome::Partial,
    Outcome::Failed,
    Outcome::Cancelled,
    Outcome::Unknown,
];

/// A worker of the board whose binding matches a pane in this Noa's
/// current listing (Glossary "live link").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveWorker {
    pub(crate) pane: u64,
}

/// The line under the header while the person types into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InputLine {
    /// `/`: matched against title, summary and body (`list.query`).
    Query(String),
    /// `f`: one pick per field (worker, agent, kind, outcome); 0 is any,
    /// `n` the field's option `n - 1` in `options`, each field's options
    /// as they were when `f` was pressed: a poll that learns of another
    /// worker meanwhile does not move a pick onto it.
    Filter {
        field: usize,
        picks: [usize; 4],
        options: Vec<Vec<(String, String)>>,
    },
}

/// What the `list` in flight was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    /// The head page with the filter and query.
    Reload,
    /// The page older than what is loaded.
    Older,
    /// New posts the query must judge (see [`Timeline::take_probe`]).
    Probe,
}

/// What a key or a poll asks `board.rs` to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TlAction {
    /// Read a post whole (`noa.timeline.get`).
    Fetch(GetParams),
    /// Read a page of posts (`noa.timeline.list`).
    Page(ListParams),
    /// Focus a live worker's pane.
    Focus(u64),
    /// Show a worker's pane in the board's detail view.
    ShowWorker(u64),
    /// Store this visit's boundary.
    SaveVisit(u64),
}

pub(crate) struct Timeline {
    pub(crate) lang: Lang,
    /// Seconds east of UTC at a Unix time: the offset each time is shown
    /// in, its own date's (daylight saving time included), not today's.
    pub(crate) offset_at: fn(i64) -> i64,
    /// The server lacks `boardTimelineV1`.
    pub(crate) unsupported: bool,
    board_id: Option<String>,
    /// The board was named by id (`--board-id`), not found by pane.
    pinned: bool,
    name: String,
    archived: bool,
    /// The server answered for the board: its context, or that it does
    /// not exist.
    known: bool,
    /// The calling pane is on no board (yet).
    no_board: bool,
    /// The board named by id does not exist.
    missing: bool,
    /// Shown posts, newest first.
    posts: Vec<PostSummary>,
    /// Posts that arrived while the person read older ones, newest first.
    pending: Vec<PostSummary>,
    head_seq: u64,
    /// The highest seq `changes` has given (its next `afterSeq`).
    recv_cursor: u64,
    /// New posts that passed the filter while a query is set, by their
    /// lowest and highest seq: only the server matches a query against a
    /// body, which a summary lacks, so a `list` decides them.
    probe: Option<(u64, u64)>,
    /// A reload's page came in.
    loaded: bool,
    /// A reload (head page with the filter and query) is wanted.
    reload: bool,
    /// What the `list` in flight was for.
    requested: Option<Request>,
    /// The next `beforeSeq` while older posts remain.
    older: Option<u64>,
    visit_boundary: u64,
    visit_known: bool,
    /// The highest seq listed during this visit, which a later search or
    /// filter that lists fewer posts does not take back.
    seen_max: u64,
    selected: usize,
    /// The first item drawn; set right as the list is drawn.
    scroll: Cell<usize>,
    detail_scroll: usize,
    detail_reach: Cell<usize>,
    filter: Filter,
    query: Option<String>,
    input: Option<InputLine>,
    detail: Option<PostId>,
    bodies: HashMap<PostId, GetResult>,
    body_order: Vec<PostId>,
    /// The post whose body was last asked for, so a failing read is not
    /// asked again for the same post.
    tried: Option<PostId>,
    live: HashMap<String, LiveWorker>,
    labels: HashMap<String, String>,
    agents: HashMap<String, String>,
    last_ok_unix: Option<i64>,
    stale: bool,
    message: Option<String>,
}

impl Timeline {
    pub(crate) fn new(lang: Lang, offset_at: fn(i64) -> i64) -> Self {
        Self {
            lang,
            offset_at,
            unsupported: false,
            board_id: None,
            pinned: false,
            name: String::new(),
            archived: false,
            known: false,
            no_board: false,
            missing: false,
            posts: Vec::new(),
            pending: Vec::new(),
            head_seq: 0,
            recv_cursor: 0,
            probe: None,
            loaded: false,
            reload: true,
            requested: None,
            older: None,
            visit_boundary: 0,
            visit_known: false,
            seen_max: 0,
            selected: 0,
            scroll: Cell::new(0),
            detail_scroll: 0,
            detail_reach: Cell::new(0),
            filter: Filter::default(),
            query: None,
            input: None,
            detail: None,
            bodies: HashMap::new(),
            body_order: Vec::new(),
            tried: None,
            live: HashMap::new(),
            labels: HashMap::new(),
            agents: HashMap::new(),
            last_ok_unix: None,
            stale: false,
            message: None,
        }
    }

    pub(crate) fn board_id(&self) -> Option<&str> {
        self.board_id.as_deref()
    }

    /// The board to show (`board --timeline --board-id B`).
    pub(crate) fn set_board_id(&mut self, board_id: Option<String>) {
        self.pinned = board_id.is_some();
        self.board_id = board_id;
    }

    /// Whether the board was named by id rather than found by pane.
    pub(crate) fn pinned(&self) -> bool {
        self.pinned
    }

    /// Whether the server answered for the board yet.
    pub(crate) fn context_known(&self) -> bool {
        self.known
    }

    pub(crate) fn archived(&self) -> bool {
        self.archived
    }

    /// Whether `pane` is a live worker of the board here (Glossary).
    pub(crate) fn is_live_pane(&self, pane: u64) -> bool {
        self.live.values().any(|live| live.pane == pane)
    }

    /// The board's name, else its id.
    pub(crate) fn board_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.board_id.clone().unwrap_or_default()
        } else {
            clean(&self.name)
        }
    }

    /// Whether the filter line is open, the one place Tab means anything.
    pub(crate) fn takes_tab(&self) -> bool {
        matches!(self.input, Some(InputLine::Filter { .. }))
    }

    /// Drop what the timeline's last key said, for a key the board handles
    /// (its own prompt or note takes the line then).
    pub(crate) fn clear_message(&mut self) {
        self.message = None;
    }

    /// The post shown whole, if any.
    pub(crate) fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Whether the person types into the input line, which takes every key.
    pub(crate) fn typing(&self) -> bool {
        self.input.is_some()
    }

    /// Whether the line under the header is the timeline's own (its input
    /// line or what its last key said).
    pub(crate) fn owns_notice(&self) -> bool {
        self.input.is_some() || self.message.is_some()
    }

    pub(crate) fn visit_known(&self) -> bool {
        self.visit_known
    }

    pub(crate) fn set_visit_boundary(&mut self, seq: u64) {
        self.visit_boundary = seq;
        self.visit_known = true;
    }

    /// The highest seq shown during the visit, which becomes the visit
    /// boundary when the person leaves (never lower than the one stored).
    pub(crate) fn leave(&mut self) -> Option<TlAction> {
        self.board_id.as_ref()?;
        if !self.loaded {
            return None;
        }
        self.note_shown();
        self.visit_boundary = self.visit_boundary.max(self.seen_max);
        self.visit_known = true;
        Some(TlAction::SaveVisit(self.visit_boundary))
    }

    /// Keep the highest seq listed, before the list is replaced or left.
    fn note_shown(&mut self) {
        let shown = self.posts.iter().map(|post| post.seq.0).max().unwrap_or(0);
        self.seen_max = self.seen_max.max(shown);
    }

    /// "前回閲覧以降 N 件": posts with a seq over the boundary. Seqs are
    /// contiguous per board, so the head tells it without reading them:
    /// the board's context alone, which the worker views read too.
    pub(crate) fn since_last_visit(&self) -> usize {
        if !self.visit_known {
            return 0;
        }
        self.head_seq.saturating_sub(self.visit_boundary) as usize
    }

    /// The head page to read, when one is wanted and none is in flight.
    pub(crate) fn take_reload(&mut self) -> Option<ListParams> {
        if !self.reload || self.requested.is_some() {
            return None;
        }
        let params = self.list_params(None)?;
        self.reload = false;
        self.requested = Some(Request::Reload);
        Some(params)
    }

    /// The `list` that decides the new posts a query must judge: the
    /// query and filter, from just above the newest of them, as many as a
    /// page holds. The posts it returns in their range match; the others
    /// in it do not. Asked for after `changes`, until none is left.
    pub(crate) fn take_probe(&mut self) -> Option<ListParams> {
        let (_, hi) = self.probe?;
        if self.requested.is_some() {
            return None;
        }
        let mut params = self.list_params(Some(hi.saturating_add(1)))?;
        params.limit = Some(LIST_MAX);
        self.requested = Some(Request::Probe);
        Some(params)
    }

    /// The next `changes` call, once the head page came in.
    pub(crate) fn changes_params(&self) -> Option<ChangesParams> {
        if !self.loaded {
            return None;
        }
        Some(ChangesParams {
            after_seq: Seq(self.recv_cursor),
            board_id: self.board_id.clone()?,
            limit: None,
        })
    }

    /// The body the preview or the full post needs, when not read yet.
    pub(crate) fn wanted_body(&mut self, cols: usize, rows: usize) -> Option<GetParams> {
        let previews = cols >= WIDE_COLS || (cols >= STACKED_COLS && rows >= STACKED_ROWS);
        let post_id = match &self.detail {
            Some(id) => id.clone(),
            None if previews => self.posts.get(self.selected)?.post_id.clone(),
            None => return None,
        };
        if self.bodies.contains_key(&post_id) || self.tried.as_ref() == Some(&post_id) {
            return None;
        }
        self.tried = Some(post_id.clone());
        Some(GetParams {
            board_id: self.board_id.clone()?,
            post_id,
        })
    }

    fn list_params(&self, before: Option<u64>) -> Option<ListParams> {
        let filter = self.filter_set().then(|| self.filter.clone());
        Some(ListParams {
            before_seq: before.map(Seq),
            board_id: self.board_id.clone()?,
            filter,
            limit: None,
            query: self.query.clone(),
        })
    }

    fn filter_set(&self) -> bool {
        self.filter != Filter::default()
    }

    /// Ask for the head page with the current filter and query.
    fn reload_now(&mut self) -> Option<TlAction> {
        self.reload = true;
        self.requested = None;
        self.take_reload().map(TlAction::Page)
    }

    fn mark_ok(&mut self) {
        // A body that failed to load while the connection did is asked
        // for again.
        if self.stale {
            self.tried = None;
        }
        self.last_ok_unix = Some(unix_now());
        self.stale = false;
    }

    /// A `list` result: the head page (a reload), an older page, or the
    /// new posts a query judged.
    pub(crate) fn on_page(&mut self, page: PageResult) {
        let request = self.requested.take();
        self.mark_ok();
        self.head_seq = self.head_seq.max(page.head_seq.0);
        let next = page
            .has_more
            .then_some(page.next_before_seq)
            .flatten()
            .map(|seq| seq.0);
        match request {
            Some(Request::Older) => {
                // Asked for by a step past the last post: still there, the
                // selection takes that step onto the first of the page.
                let stepped = self.selected + 1 == self.posts.len();
                let before = self.posts.len();
                let last = self.posts.last().map_or(u64::MAX, |post| post.seq.0);
                self.posts
                    .extend(page.posts.into_iter().filter(|post| post.seq.0 < last));
                if stepped && self.posts.len() > before {
                    self.selected += 1;
                }
                self.older = next;
            }
            Some(Request::Probe) => self.on_probe(page.posts, next),
            Some(Request::Reload) | None => {
                // The post read stays selected when the new list has it
                // (another filter or query that still matches it).
                let kept = self
                    .posts
                    .get(self.selected)
                    .map(|post| post.post_id.clone());
                self.note_shown();
                self.posts = page.posts;
                self.pending.clear();
                self.probe = None;
                self.recv_cursor = page.head_seq.0;
                match kept.and_then(|id| self.posts.iter().position(|post| post.post_id == id)) {
                    Some(at) => {
                        self.selected = at;
                        self.scroll.set(self.scroll.get().min(at));
                    }
                    None => {
                        self.selected = 0;
                        self.scroll.set(0);
                    }
                }
                self.loaded = true;
                self.older = next;
            }
        }
    }

    /// The query's verdict on the new posts in `probe`: those of its range
    /// the page has match; what is below the page is asked for next.
    fn on_probe(&mut self, posts: Vec<PostSummary>, next: Option<u64>) {
        let Some((lo, hi)) = self.probe else {
            return;
        };
        self.probe = next.filter(|&next| next > lo).map(|next| (lo, next - 1));
        let new = posts
            .into_iter()
            .filter(|post| (lo..=hi).contains(&post.seq.0))
            .collect();
        self.merge_new(new);
    }

    /// A `changes` page (ascending): new posts merge at once at the latest
    /// position, else wait in `pending` with the selection where it is.
    /// While a query is set the server judges them first ([`Self::take_probe`]).
    pub(crate) fn on_changes(&mut self, page: PageResult) {
        self.mark_ok();
        self.head_seq = self.head_seq.max(page.head_seq.0);
        if !self.loaded {
            return;
        }
        let mut new = Vec::new();
        for post in page.posts {
            if post.seq.0 <= self.recv_cursor {
                continue;
            }
            self.recv_cursor = post.seq.0;
            // A correction marks the post it corrects.
            if let Some(of) = &post.supersedes_post_id {
                for old in self.posts.iter_mut().chain(self.pending.iter_mut()) {
                    if &old.post_id == of && !old.superseded_by.contains(&post.post_id) {
                        old.superseded_by.push(post.post_id.clone());
                    }
                }
            }
            if !self.admits(&post) {
                continue;
            }
            if self.query.is_some() {
                let seq = post.seq.0;
                self.probe = Some(
                    self.probe
                        .map_or((seq, seq), |(lo, hi)| (lo.min(seq), hi.max(seq))),
                );
            } else {
                new.push(post);
            }
        }
        if let Some(seq) = page.next_after_seq {
            self.recv_cursor = self.recv_cursor.max(seq.0);
        }
        new.reverse();
        self.merge_new(new);
    }

    /// Take in new posts (newest first): at the latest position into the
    /// list, with the ones held before; else held in `pending`. Kept newest
    /// first and never twice.
    fn merge_new(&mut self, new: Vec<PostSummary>) {
        let known: HashSet<u64> = self
            .posts
            .iter()
            .chain(&self.pending)
            .map(|post| post.seq.0)
            .collect();
        let mut new: Vec<PostSummary> = new
            .into_iter()
            .filter(|post| !known.contains(&post.seq.0))
            .collect();
        if new.is_empty() {
            return;
        }
        new.append(&mut self.pending);
        if self.at_latest() {
            new.append(&mut self.posts);
            new.sort_by_key(|post| std::cmp::Reverse(post.seq.0));
            self.posts = new;
        } else {
            new.sort_by_key(|post| std::cmp::Reverse(post.seq.0));
            self.pending = new;
        }
    }

    /// The list shows its newest post selected and is not scrolled, and no
    /// post is open.
    fn at_latest(&self) -> bool {
        self.selected == 0 && self.scroll.get() == 0 && self.detail.is_none()
    }

    /// Whether a post that came through `changes` passes the filter, as the
    /// server applies it.
    fn admits(&self, post: &PostSummary) -> bool {
        let f = &self.filter;
        let any = |list: &[String], value: &Option<String>| {
            list.is_empty() || value.as_ref().is_some_and(|value| list.contains(value))
        };
        any(&f.worker_ids, &post.worker_id)
            && any(&f.agent_kinds, &post.author.agent_kind)
            && (f.kinds.is_empty() || f.kinds.contains(&post.kind))
            && (f.outcomes.is_empty() || f.outcomes.contains(&post.outcome))
    }

    /// The board's context: its id (when it was asked by pane), name, and
    /// which workers are live here. A worker is live only when the context
    /// came from this Noa (`same_instance`), and the listing has its pane
    /// with the same surface (§5).
    pub(crate) fn on_context(
        &mut self,
        ctx: &ContextResult,
        listing: &[Panel],
        same_instance: bool,
    ) {
        if self.board_id.is_none() {
            self.board_id = Some(ctx.board_id.clone());
        }
        self.known = true;
        self.no_board = false;
        self.missing = false;
        self.name = ctx.name.clone();
        self.archived = ctx.archived;
        self.head_seq = self.head_seq.max(ctx.head_seq.0);
        self.live.clear();
        for worker in &ctx.workers {
            if let Some(label) = &worker.label {
                self.labels.insert(worker.worker_id.clone(), label.clone());
            }
            if let Some(kind) = &worker.agent_kind {
                self.agents.insert(worker.worker_id.clone(), kind.clone());
            }
            let Some(pane) = worker.pane_id else {
                continue;
            };
            let here = listing.iter().any(|panel| {
                panel.pane_id == pane
                    && panel.surface_id.is_some()
                    && panel.surface_id == worker.surface_id
            });
            if same_instance && worker.live && here {
                self.live
                    .insert(worker.worker_id.clone(), LiveWorker { pane: pane.0 });
            }
        }
    }

    /// The calling pane is on no board yet.
    pub(crate) fn on_no_board(&mut self) {
        self.no_board = true;
    }

    /// The server answered that the board named by id does not exist.
    pub(crate) fn on_missing(&mut self) {
        self.missing = true;
        self.known = true;
        self.mark_ok();
    }

    /// A post read whole. It says nothing of new posts: a list that stopped
    /// updating stays marked so, as of its own last success.
    pub(crate) fn on_get(&mut self, result: GetResult) {
        let id = result.post.post_id.clone();
        if self.bodies.insert(id.clone(), result).is_none() {
            self.body_order.push(id);
        }
        while self.body_order.len() > BODY_CACHE {
            let old = self.body_order.remove(0);
            self.bodies.remove(&old);
        }
    }

    /// A read failed: what is shown stays, marked as of the last success.
    pub(crate) fn on_failure(&mut self) {
        self.stale = true;
        // A head page that did not come is asked for again by the next
        // poll, new posts a query was to judge too; an older page by the
        // next `j` at the end.
        let reload = self.requested.take() == Some(Request::Reload);
        if reload || !self.loaded {
            self.reload = true;
        }
    }

    /// Noa cannot be reached, so no read is made: what is shown is marked
    /// as of the last success. Returns whether that is new.
    pub(crate) fn on_unreachable(&mut self) -> bool {
        !std::mem::replace(&mut self.stale, true)
    }

    pub(crate) fn key(&mut self, key: Key) -> Option<TlAction> {
        self.message = None;
        if let Some(input) = self.input.clone() {
            return self.input_key(input, key);
        }
        match key {
            Key::Char('j') | Key::Down if self.detail.is_some() => {
                self.detail_scroll = (self.detail_scroll + 1).min(self.detail_reach.get());
                None
            }
            Key::Char('k') | Key::Up if self.detail.is_some() => {
                self.detail_scroll = self
                    .detail_scroll
                    .min(self.detail_reach.get())
                    .saturating_sub(1);
                None
            }
            Key::Char('j') | Key::Down => self.move_selection(1),
            Key::Char('k') | Key::Up => self.move_selection(-1),
            Key::Enter if self.detail.is_none() => {
                let post = self.posts.get(self.selected)?;
                let id = post.post_id.clone();
                self.detail = Some(id.clone());
                self.detail_scroll = 0;
                if self.bodies.contains_key(&id) {
                    return None;
                }
                self.tried = Some(id.clone());
                Some(TlAction::Fetch(GetParams {
                    board_id: self.board_id.clone()?,
                    post_id: id,
                }))
            }
            Key::Esc | Key::Char('h') | Key::Left => {
                self.detail = None;
                None
            }
            Key::Char('/') => {
                self.input = Some(InputLine::Query(self.query.clone().unwrap_or_default()));
                None
            }
            Key::Char('f') => {
                let picks = self.current_picks();
                let options = (0..4).map(|field| self.options(field)).collect();
                self.input = Some(InputLine::Filter {
                    field: 0,
                    picks,
                    options,
                });
                None
            }
            Key::Char('G') => {
                let mut pending = std::mem::take(&mut self.pending);
                pending.append(&mut self.posts);
                pending.sort_by_key(|post| std::cmp::Reverse(post.seq.0));
                self.posts = pending;
                self.selected = 0;
                self.scroll.set(0);
                self.detail = None;
                None
            }
            Key::Char('g') => {
                let pane = self.post_worker()?;
                Some(TlAction::Focus(pane))
            }
            Key::Char('w') => {
                let pane = self.post_worker()?;
                Some(TlAction::ShowWorker(pane))
            }
            _ => None,
        }
    }

    /// The live pane of the selected (or open) post's worker. Says why
    /// there is none, except on an archived board, where `g`/`w` are inert.
    fn post_worker(&mut self) -> Option<u64> {
        if self.archived {
            return None;
        }
        let post = self.current()?;
        let live = post
            .worker_id
            .as_ref()
            .and_then(|worker| self.live.get(worker))
            .map(|live| live.pane);
        if live.is_none() {
            self.message = Some(not_on_board(self.lang).to_string());
        }
        live
    }

    /// The post open, else the selected one.
    fn current(&self) -> Option<&PostSummary> {
        match &self.detail {
            Some(id) => self
                .posts
                .iter()
                .chain(&self.pending)
                .find(|post| &post.post_id == id),
            None => self.posts.get(self.selected),
        }
    }

    fn move_selection(&mut self, step: isize) -> Option<TlAction> {
        if self.posts.is_empty() {
            return None;
        }
        let last = self.posts.len() - 1;
        if step > 0 && self.selected == last {
            // The end of what is loaded: read the next older page.
            let before = self.older?;
            if self.requested.is_some() {
                return None;
            }
            let params = self.list_params(Some(before))?;
            self.requested = Some(Request::Older);
            return Some(TlAction::Page(params));
        }
        self.selected = self.selected.saturating_add_signed(step).min(last);
        if self.selected < self.scroll.get() {
            self.scroll.set(self.selected);
        }
        None
    }

    fn input_key(&mut self, input: InputLine, key: Key) -> Option<TlAction> {
        match input {
            InputLine::Query(mut text) => {
                match key {
                    Key::Char(ch) => push_capped(&mut text, &ch.to_string()),
                    Key::Paste(paste) => push_capped(&mut text, &paste),
                    Key::Backspace => {
                        text.pop();
                    }
                    Key::CtrlU => text.clear(),
                    Key::CtrlW => {
                        let kept = text.trim_end().len();
                        let word = text[..kept]
                            .char_indices()
                            .rfind(|(_, ch)| ch.is_whitespace())
                            .map_or(0, |(at, ch)| at + ch.len_utf8());
                        text.truncate(word);
                    }
                    Key::Enter => {
                        self.input = None;
                        let query = text.trim();
                        self.query = (!query.is_empty()).then(|| query.to_string());
                        self.detail = None;
                        return self.reload_now();
                    }
                    Key::Esc | Key::CtrlC => {
                        self.input = None;
                        return None;
                    }
                    _ => {}
                }
                self.input = Some(InputLine::Query(text));
                None
            }
            InputLine::Filter {
                mut field,
                mut picks,
                options,
            } => {
                match key {
                    Key::Tab => field = (field + 1) % 4,
                    Key::Right | Key::Left => {
                        let count = options[field].len() + 1;
                        picks[field] = if key == Key::Right {
                            (picks[field] + 1) % count
                        } else {
                            (picks[field] + count - 1) % count
                        };
                    }
                    Key::Enter => {
                        self.input = None;
                        self.filter = filter_of(&options, picks);
                        self.detail = None;
                        return self.reload_now();
                    }
                    Key::Esc | Key::CtrlC => {
                        self.input = None;
                        return None;
                    }
                    _ => {}
                }
                self.input = Some(InputLine::Filter {
                    field,
                    picks,
                    options,
                });
                None
            }
        }
    }

    /// A field's options as (value, label): workers and agents the board
    /// knows of, every kind and outcome.
    fn options(&self, field: usize) -> Vec<(String, String)> {
        match field {
            0 => {
                let mut ids: BTreeSet<&String> = self.labels.keys().collect();
                ids.extend(self.posts.iter().filter_map(|post| post.worker_id.as_ref()));
                let mut options: Vec<(String, String)> = ids
                    .into_iter()
                    .map(|id| {
                        let label = self.labels.get(id).map_or(id.as_str(), String::as_str);
                        (id.clone(), clean(label))
                    })
                    .collect();
                options.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                options
            }
            1 => {
                let mut kinds: BTreeSet<&String> = self.agents.values().collect();
                kinds.extend(
                    self.posts
                        .iter()
                        .filter_map(|post| post.author.agent_kind.as_ref()),
                );
                kinds
                    .into_iter()
                    .map(|kind| (kind.clone(), clean(kind)))
                    .collect()
            }
            2 => KINDS
                .iter()
                .map(|kind| (String::new(), kind_label(*kind, self.lang).to_string()))
                .collect(),
            _ => OUTCOMES
                .iter()
                .map(|outcome| {
                    (
                        String::new(),
                        outcome_label(*outcome, self.lang).to_string(),
                    )
                })
                .collect(),
        }
    }

    fn current_picks(&self) -> [usize; 4] {
        let pick = |field: usize, value: Option<&String>| {
            value.map_or(0, |value| {
                self.options(field)
                    .iter()
                    .position(|(v, _)| v == value)
                    .map_or(0, |at| at + 1)
            })
        };
        let f = &self.filter;
        [
            pick(0, f.worker_ids.first()),
            pick(1, f.agent_kinds.first()),
            f.kinds
                .first()
                .and_then(|kind| KINDS.iter().position(|k| k == kind))
                .map_or(0, |at| at + 1),
            f.outcomes
                .first()
                .and_then(|outcome| OUTCOMES.iter().position(|o| o == outcome))
                .map_or(0, |at| at + 1),
        ]
    }

    // -----------------------------------------------------------------------
    // Drawing

    /// The timeline as `rows - 1` lines of exactly `cols` columns (the
    /// board draws the footer under them).
    pub(crate) fn render(&self, cols: usize, rows: usize, now: Instant) -> Vec<String> {
        let mut lines = self.header(cols, now);
        let room = rows.saturating_sub(lines.len() + 1);
        if let Some(id) = &self.detail {
            lines.extend(self.post_lines(id, cols, room, true));
            return lines;
        }
        if self.no_board || self.missing || !self.loaded || self.posts.is_empty() {
            let missing;
            let note: &str = if self.no_board {
                self.lang.pick(
                    "このペインのボードはまだありません",
                    "this pane is on no board yet",
                )
            } else if self.missing {
                let id = self.board_id.as_deref().map(clean).unwrap_or_default();
                missing = self.lang.pick(
                    format!("ボード {id} は見つかりません"),
                    format!("no board {id}"),
                );
                missing.as_str()
            } else if !self.loaded {
                self.lang.pick("読み込み中…", "Loading…")
            } else if self.filter_set() || self.query.is_some() {
                self.lang
                    .pick("条件に合う投稿はありません", "no post matches")
            } else {
                self.lang.pick("投稿はまだありません", "no post yet")
            };
            let mut line = Line::default();
            line.plain("   ")
                .fg(DIM, &fit(note, cols.saturating_sub(3)));
            lines.push(Line::default().finish(cols));
            lines.push(line.finish(cols));
            return lines;
        }
        if cols >= WIDE_COLS {
            let right = cols * 2 / 5;
            let left = cols - right - 1;
            let list = self.list_lines(left, room);
            let side = self.preview_lines(right, room);
            let mut bar = Line::default();
            bar.fg(BORDER, "│");
            let bar = bar.finish(1);
            for (list, side) in list.into_iter().zip(side) {
                lines.push(format!("{list}{bar}{side}"));
            }
        } else if cols >= STACKED_COLS && rows >= STACKED_ROWS {
            let height = (room / 3).clamp(3, 8);
            lines.extend(self.list_lines(cols, room - height));
            lines.extend(self.preview_lines(cols, height));
        } else {
            lines.extend(self.list_lines(cols, room));
        }
        lines
    }

    fn header(&self, cols: usize, now: Instant) -> Vec<String> {
        let _ = now;
        let mut title = Line::default();
        title
            .plain(" ")
            .bold(self.lang.pick("タイムライン", "TIMELINE"))
            .plain("  ");
        let name = self.board_name();
        title.fg(DIM, &fit(&name, cols.saturating_sub(title.width + 1)));

        let l = |ja: String, en: String| self.lang.pick(ja, en);
        let mut status: Vec<(Rgb, String)> = Vec::new();
        if self.stale {
            let at = self
                .last_ok_unix
                .map(|unix| clock(unix + (self.offset_at)(unix)))
                .unwrap_or_else(|| "--:--:--".to_string());
            status.push((ORANGE, l(format!("更新停止 {at}"), format!("stalled {at}"))));
        }
        if !self.pending.is_empty() {
            let n = self.pending.len();
            status.push((ORANGE, l(format!("新着 {n} (G)"), format!("{n} new (G)"))));
        }
        let since = self.since_last_visit();
        if since > 0 {
            status.push((
                BLUE,
                l(
                    format!("前回閲覧以降 {since} 件"),
                    format!("{since} since last visit"),
                ),
            ));
        }
        if self.archived {
            status.push((
                GREY,
                l(
                    "アーカイブ: ライブworkerなし".to_string(),
                    "archive: no live worker".to_string(),
                ),
            ));
        } else if self.loaded {
            let n = self.live.len();
            status.push((DIM, l(format!("ライブ {n}"), format!("{n} live"))));
        }
        if self.filter_set() {
            status.push((
                DIM,
                l(
                    format!("絞り込み {}", self.filter_text()),
                    format!("filter {}", self.filter_text()),
                ),
            ));
        }
        if let Some(query) = &self.query {
            let query = clean(query);
            status.push((
                DIM,
                l(format!("検索 “{query}”"), format!("query “{query}”")),
            ));
        }
        let mut chips = Line::default();
        chips.plain(" ");
        for (at, (color, text)) in status.iter().enumerate() {
            if at > 0 {
                chips.fg(BORDER, " · ");
            }
            chips.fg(*color, text);
        }
        vec![
            title.finish(cols),
            chips.finish(cols),
            self.notice_line(cols),
        ]
    }

    fn filter_text(&self) -> String {
        let picks = self.current_picks();
        (0..4)
            .filter(|&field| picks[field] > 0)
            .filter_map(|field| {
                let (_, label) = self.options(field).get(picks[field] - 1)?.clone();
                Some(format!("{}={label}", field_name(field, self.lang)))
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn notice_line(&self, cols: usize) -> String {
        let mut line = Line::default();
        line.plain(" ");
        let room = cols.saturating_sub(1);
        match &self.input {
            Some(InputLine::Query(text)) => {
                line.fg(BLUE, self.lang.pick("検索 › ", "search › "));
                let text = clean(text);
                let room = room.saturating_sub(line.width + 1);
                // The end of a long query, where the typing happens.
                let mut tail = String::new();
                let mut used = 0;
                for ch in text.chars().rev() {
                    let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if used + w > room {
                        break;
                    }
                    tail.insert(0, ch);
                    used += w;
                }
                line.plain(&tail).plain("█");
            }
            Some(InputLine::Filter {
                field,
                picks,
                options,
            }) => {
                let prefix = self.lang.pick("絞り込み ", "filter ");
                let texts: Vec<String> = picks
                    .iter()
                    .enumerate()
                    .map(|(at, &pick)| {
                        let value = match pick {
                            0 => self.lang.pick("すべて", "any").to_string(),
                            n => options[at]
                                .get(n - 1)
                                .map(|(_, label)| label.clone())
                                .unwrap_or_default(),
                        };
                        format!(" {}: {value} ", field_name(at, self.lang))
                    })
                    .collect();
                // Each field is drawn two columns wider ("[…]" or " … ").
                let whole =
                    prefix.width() + texts.iter().map(|text| text.width() + 2).sum::<usize>();
                if whole <= room {
                    line.fg(BLUE, prefix);
                    for (at, text) in texts.iter().enumerate() {
                        if at == *field {
                            line.fg(BLUE, "[").bold(text).fg(BLUE, "]");
                        } else {
                            line.fg(DIM, &format!(" {text} "));
                        }
                    }
                } else {
                    // Too narrow for every field: the one being changed.
                    let text = fit(&texts[*field], room.saturating_sub(2));
                    line.fg(BLUE, "[").bold(&text).fg(BLUE, "]");
                }
            }
            None => match &self.message {
                Some(message) => {
                    line.fg(ORANGE, &fit(&clean(message), room));
                }
                None => {
                    let hint = self.lang.pick(
                        "t でワーカー一覧へ  ·  ? キー一覧",
                        "t back to the workers  ·  ? keys",
                    );
                    line.fg(DIM, &fit(hint, room));
                }
            },
        }
        line.finish(cols)
    }

    /// The list from the first item drawn, keeping the selected one in
    /// view; `room` lines of `cols` columns.
    fn list_lines(&self, cols: usize, room: usize) -> Vec<String> {
        // One line for what is below, when anything is.
        let body = room.saturating_sub(1).max(1);
        // Too low for a title under an item's first line: one line a post,
        // its seq and title, so the posts `j`/`k` move between differ.
        let compact = body < 2;
        let items: Vec<Vec<String>> = self
            .posts
            .iter()
            .enumerate()
            .map(|(at, post)| {
                let selected = at == self.selected;
                if compact {
                    vec![self.compact_line(post, cols, selected)]
                } else {
                    self.item_lines(post, cols, selected)
                }
            })
            .collect();
        let mut start = self.scroll.get().min(self.selected);
        let fits = |start: usize| -> bool {
            items[start..=self.selected]
                .iter()
                .map(Vec::len)
                .sum::<usize>()
                <= body
        };
        while start < self.selected && !fits(start) {
            start += 1;
        }
        self.scroll.set(start);
        let mut out: Vec<String> = Vec::new();
        let mut shown = start;
        // Whether the selected post is shown in part only.
        let mut cut = compact;
        for item in &items[start..] {
            if out.len() + item.len() > body {
                // A selected item taller than the pane is cut (its blank
                // last line is no part of it).
                if out.is_empty() {
                    out.extend(item.iter().take(body).cloned());
                    shown += 1;
                    cut |= item.len() - 1 > body;
                }
                break;
            }
            out.extend(item.iter().cloned());
            shown += 1;
        }
        let below = items.len() - shown;
        if below > 0 || self.older.is_some() || cut {
            let mut more = Line::default();
            let text = match (below, self.older.is_some(), self.lang) {
                (0, false, Lang::Ja) => "… Enter で全文".to_string(),
                (0, false, Lang::En) => "… Enter shows it whole".to_string(),
                (0, _, Lang::Ja) => "↓ さらに古い投稿 (j)".to_string(),
                (0, _, Lang::En) => "↓ older posts (j)".to_string(),
                (n, false, Lang::Ja) => format!("↓ あと {n} 件"),
                (n, false, Lang::En) => format!("↓ {n} more"),
                (n, true, Lang::Ja) => format!("↓ あと {n} 件 · さらに古い投稿あり"),
                (n, true, Lang::En) => format!("↓ {n} more · older posts"),
            };
            more.plain("   ")
                .fg(DIM, &fit(&text, cols.saturating_sub(3)));
            out.push(more.finish(cols));
        }
        while out.len() < room {
            out.push(Line::default().finish(cols));
        }
        out.truncate(room);
        out
    }

    /// A list item in one line: its seq and title.
    fn compact_line(&self, post: &PostSummary, cols: usize, selected: bool) -> String {
        let mut line = Line::default();
        if selected {
            line.fg(BLUE, "▌ ");
        } else {
            line.plain("  ");
        }
        line.fg(DIM, &format!("#{} ", post.seq.0));
        let room = cols.saturating_sub(line.width);
        line.bold(&fit(&clean(&post.title), room));
        line.finish(cols)
    }

    /// One list item: who and what, the title, up to three summary lines,
    /// and what it carries; then a blank line.
    fn item_lines(&self, post: &PostSummary, cols: usize, selected: bool) -> Vec<String> {
        let mark = |line: &mut Line| {
            if selected {
                line.fg(BLUE, "▌ ");
            } else {
                line.plain("  ");
            }
        };
        let inner = cols.saturating_sub(2);
        let mut out = Vec::new();

        let mut head = Line::default();
        mark(&mut head);
        head.fg(DIM, &self.time_text(&post.created_at)).plain("  ");
        let who = self.who(post);
        head.plain(&fit(
            &who,
            inner.saturating_sub(head.width).saturating_sub(12),
        ));
        head.fg(BORDER, " · ")
            .fg(DIM, kind_label(post.kind, self.lang))
            .fg(BORDER, " · ")
            .fg(
                outcome_color(post.outcome),
                outcome_label(post.outcome, self.lang),
            );
        out.push(head.finish(cols));

        let mut title = Line::default();
        mark(&mut title);
        title.bold(&fit(&clean(&post.title), inner));
        out.push(title.finish(cols));

        let summary = clean(&post.summary);
        let mut lines = wrap(&summary, inner.max(1));
        if lines.len() > SUMMARY_LINES {
            lines.truncate(SUMMARY_LINES);
            let last = lines.pop().unwrap_or_default();
            lines.push(fit(&format!("{last}…"), inner));
        }
        for text in lines {
            let mut line = Line::default();
            mark(&mut line);
            line.plain(&text);
            out.push(line.finish(cols));
        }

        let mut aux = Line::default();
        mark(&mut aux);
        for (at, (color, text)) in self.aux_pieces(post).iter().enumerate() {
            if at > 0 {
                aux.fg(BORDER, " · ");
            }
            aux.fg(*color, text);
        }
        out.push(aux.finish(cols));
        out.push(Line::default().finish(cols));
        out
    }

    /// "api · claude · worker": the author, its agent and its role.
    fn who(&self, post: &PostSummary) -> String {
        let label = post
            .author
            .label
            .clone()
            .or_else(|| {
                post.worker_id
                    .as_ref()
                    .and_then(|worker| self.labels.get(worker).cloned())
            })
            .unwrap_or_else(|| post.producer.id.clone());
        let mut parts = vec![clean(&label)];
        if let Some(kind) = &post.author.agent_kind {
            parts.push(clean(kind));
        }
        if let Some(role) = &post.author.role {
            parts.push(clean(role));
        }
        parts.join(" · ")
    }

    /// What a post carries, beside its outcome: its task and run, its
    /// checks (verification is not the outcome, §6), issues, artifacts,
    /// corrections and where it came from.
    fn aux_pieces(&self, post: &PostSummary) -> Vec<(Rgb, String)> {
        let l = |ja: String, en: String| self.lang.pick(ja, en);
        let mut pieces = Vec::new();
        if let Some(task) = &post.task_ref {
            pieces.push((DIM, format!("task {}", clean(&task.id))));
        }
        if let Some(run) = &post.run_id {
            pieces.push((DIM, format!("run {}", short_id(run))));
        }
        let CheckCountsView {
            passed,
            failed,
            not_run,
            inconclusive,
        } = CheckCountsView::of(post);
        if passed + failed + inconclusive == 0 {
            pieces.push((
                GREY,
                l("検証未実施".to_string(), "not verified".to_string()),
            ));
        } else {
            let color = if failed > 0 { RED } else { GREEN };
            let mut text = l(
                format!("検証 ✓{passed} ×{failed} -{not_run}"),
                format!("checks ✓{passed} ×{failed} -{not_run}"),
            );
            if inconclusive > 0 {
                text.push_str(&format!(" ?{inconclusive}"));
            }
            pieces.push((color, text));
        }
        if post.open_issues > 0 {
            let n = post.open_issues;
            pieces.push((ORANGE, l(format!("課題 {n}"), format!("{n} issues"))));
        }
        if post.artifacts > 0 {
            let n = post.artifacts;
            pieces.push((DIM, l(format!("成果物 {n}"), format!("{n} artifacts"))));
        }
        if !post.superseded_by.is_empty() {
            pieces.push((ORANGE, l("訂正あり".to_string(), "corrected".to_string())));
        }
        if post.supersedes_post_id.is_some() {
            pieces.push((BLUE, l("訂正".to_string(), "correction".to_string())));
        }
        if post.source_namespace == "result-file" {
            pieces.push((
                DIM,
                l(
                    "結果ファイルから取り込み".to_string(),
                    "imported from a result file".to_string(),
                ),
            ));
        }
        if post.provenance == Provenance::Unverified {
            pieces.push((
                ORANGE,
                l("出所未照合".to_string(), "unverified source".to_string()),
            ));
        }
        pieces
    }

    /// The selected post beside or under the list.
    fn preview_lines(&self, cols: usize, height: usize) -> Vec<String> {
        match self.posts.get(self.selected) {
            Some(post) => self.post_lines(&post.post_id, cols, height, false),
            None => (0..height).map(|_| Line::default().finish(cols)).collect(),
        }
    }

    /// A post whole (`full`: the PostDetail view, scrolled by
    /// `detail_scroll`), or as a preview, in `height` lines.
    fn post_lines(&self, id: &str, cols: usize, height: usize, full: bool) -> Vec<String> {
        let post = self
            .posts
            .iter()
            .chain(&self.pending)
            .find(|post| post.post_id == id);
        let body = self.bodies.get(id);
        let inner = cols.saturating_sub(2).max(1);
        let mut text: Vec<(Option<Rgb>, String)> = Vec::new();
        let push_wrapped = |text: &mut Vec<(Option<Rgb>, String)>, color, s: &str| {
            for line in wrap(s, inner) {
                text.push((color, line));
            }
        };
        let seq = post.map(|post| post.seq.0).or(body.map(|b| b.post.seq.0));
        let title = match seq {
            Some(seq) => vec![(
                None,
                self.lang
                    .pick(format!("投稿 #{seq}"), format!("post #{seq}")),
            )],
            None => vec![(None, self.lang.pick("投稿", "post").to_string())],
        };
        let hint = full.then(|| self.lang.pick("Esc で一覧", "Esc list"));
        let head = rule(cols, &title, hint);
        if let Some(post) = post {
            push_wrapped(&mut text, None, &clean(&post.title));
            let meta = format!(
                "{} · {} · {} · {}",
                self.time_text(&post.created_at),
                self.who(post),
                kind_label(post.kind, self.lang),
                outcome_label(post.outcome, self.lang),
            );
            push_wrapped(&mut text, Some(DIM), &meta);
            let aux: Vec<String> = self
                .aux_pieces(post)
                .into_iter()
                .map(|(_, text)| text)
                .collect();
            push_wrapped(&mut text, Some(DIM), &aux.join(" · "));
            text.push((None, String::new()));
            push_wrapped(&mut text, None, &clean(&post.summary));
        }
        match body {
            Some(result) => {
                let input = &result.post.input;
                let l = |ja: &'static str, en: &'static str| self.lang.pick(ja, en);
                if !input.checks.is_empty() {
                    text.push((None, String::new()));
                    text.push((Some(DIM), l("検証", "Checks").to_string()));
                    for check in &input.checks {
                        let (mark, color) = match check.result {
                            noa_ipc::timeline::CheckResult::Passed => ("✓", GREEN),
                            noa_ipc::timeline::CheckResult::Failed => ("×", RED),
                            noa_ipc::timeline::CheckResult::NotRun => ("-", GREY),
                            noa_ipc::timeline::CheckResult::Inconclusive => ("?", ORANGE),
                        };
                        let line = format!(
                            "{mark} {} ({})",
                            clean(&check.name),
                            clean(&check.reported_by)
                        );
                        push_wrapped(&mut text, Some(color), &line);
                    }
                }
                for (name, items) in [
                    (l("未解決の課題", "Open issues"), &input.open_issues),
                    (l("次の作業", "Next actions"), &input.next_actions),
                ] {
                    if items.is_empty() {
                        continue;
                    }
                    text.push((None, String::new()));
                    text.push((Some(DIM), name.to_string()));
                    for item in items {
                        push_wrapped(&mut text, None, &format!("・{}", clean(&item.text)));
                    }
                }
                if !input.artifacts.is_empty() {
                    text.push((None, String::new()));
                    text.push((Some(DIM), l("成果物", "Artifacts").to_string()));
                    for artifact in &input.artifacts {
                        let line = format!(
                            "{} {} {}",
                            clean(&artifact.kind),
                            clean(&artifact.label),
                            clean(&artifact.r#ref)
                        );
                        push_wrapped(&mut text, None, line.trim());
                    }
                }
                if !input.body_markdown.is_empty() {
                    text.push((None, String::new()));
                    for line in input.body_markdown.lines() {
                        push_wrapped(&mut text, None, &clean(line));
                    }
                }
                // Where it came from (`sourceRef`), last.
                let source = &input.source_ref;
                text.push((None, String::new()));
                text.push((Some(DIM), l("出典", "Source").to_string()));
                let mut lines = vec![
                    clean(&source.namespace),
                    format!("session {}", clean(&source.session_id)),
                ];
                if let Some(run) = &source.run_id {
                    lines.push(format!("run {}", clean(run)));
                }
                lines.push(format!("event {}", clean(&source.event_id)));
                if let Some(at) = &source.locator {
                    lines.push(format!("{} {}", l("位置", "at"), clean(at)));
                }
                for line in lines {
                    push_wrapped(&mut text, None, &line);
                }
            }
            None if full => {
                text.push((None, String::new()));
                text.push((
                    Some(DIM),
                    self.lang
                        .pick("本文を読み込み中…", "Loading the body…")
                        .to_string(),
                ));
            }
            None => {}
        }
        let room = height.saturating_sub(1);
        let offset = if full {
            let reach = text.len().saturating_sub(room);
            self.detail_reach.set(reach);
            self.detail_scroll.min(reach)
        } else {
            0
        };
        let mut out = vec![head];
        for (color, line) in text.into_iter().skip(offset).take(room) {
            let mut l = Line::default();
            l.plain(" ");
            match color {
                Some(color) => l.fg(color, &line),
                None => l.plain(&line),
            };
            out.push(l.finish(cols));
        }
        while out.len() < height {
            out.push(Line::default().finish(cols));
        }
        out.truncate(height);
        out
    }

    /// "11/26 14:03" in the viewer's time zone, or the text as is when it
    /// is no RFC 3339 time.
    fn time_text(&self, at: &str) -> String {
        match unix_secs(at) {
            Some(unix) => {
                let clock =
                    crate::session_store::civil_from_unix_secs(unix + (self.offset_at)(unix));
                format!(
                    "{:02}/{:02} {:02}:{:02}",
                    clock.month, clock.day, clock.hour, clock.minute
                )
            }
            None => clean(at),
        }
    }

    /// The keys the footer lists in the timeline, most important first.
    pub(crate) fn footer_keys(&self) -> Vec<(&'static str, &'static str, &'static str)> {
        let l = |ja, en| self.lang.pick(ja, en);
        match &self.input {
            Some(InputLine::Query(_)) => vec![
                ("Enter", l("検索", "search"), l("検索", "search")),
                ("Esc", l("取り消し", "cancel"), l("取消", "cancel")),
                ("^U", l("全部消す", "clear"), l("全消", "clear")),
            ],
            Some(InputLine::Filter { .. }) => vec![
                ("Tab", l("項目", "field"), l("項目", "field")),
                ("←→", l("値", "value"), l("値", "value")),
                ("Enter", l("適用", "apply"), l("適用", "apply")),
                ("Esc", l("取り消し", "cancel"), l("取消", "cancel")),
            ],
            None if self.detail.is_some() => vec![
                ("Esc", l("一覧", "list"), l("一覧", "list")),
                ("j/k", l("スクロール", "scroll"), l("スクロール", "scroll")),
                ("g", l("移動", "go"), l("移動", "go")),
                ("w", l("ワーカー", "worker"), l("ワーカー", "worker")),
                ("t", l("ワーカー一覧", "workers"), l("一覧", "workers")),
                ("q", l("終了", "quit"), l("終了", "quit")),
            ],
            None => vec![
                ("Enter", l("開く", "open"), l("開く", "open")),
                ("j/k", l("選択", "select"), l("選択", "select")),
                ("/", l("検索", "search"), l("検索", "search")),
                ("f", l("絞り込み", "filter"), l("絞込", "filter")),
                ("G", l("最新へ", "newest"), l("最新", "newest")),
                ("g", l("移動", "go"), l("移動", "go")),
                ("w", l("ワーカー", "worker"), l("ワーカー", "worker")),
                ("t", l("ワーカー一覧", "workers"), l("一覧", "workers")),
                ("q", l("終了", "quit"), l("終了", "quit")),
            ],
        }
    }
}

/// A post's check counts, as the list says them.
struct CheckCountsView {
    passed: u32,
    failed: u32,
    not_run: u32,
    inconclusive: u32,
}

impl CheckCountsView {
    fn of(post: &PostSummary) -> Self {
        let counts = &post.checks;
        Self {
            passed: counts.passed,
            failed: counts.failed,
            not_run: counts.not_run,
            inconclusive: counts.inconclusive,
        }
    }
}

/// Said when `g`/`w` find no live pane for the post's worker.
pub(crate) fn not_on_board(lang: Lang) -> &'static str {
    lang.pick(
        "このボードにそのワーカーはいません",
        "worker is not on this board",
    )
}

/// The filter `picks` name among each field's `options`.
fn filter_of(options: &[Vec<(String, String)>], picks: [usize; 4]) -> Filter {
    let value = |field: usize| -> Vec<String> {
        match picks[field] {
            0 => Vec::new(),
            n => options[field]
                .get(n - 1)
                .map(|(value, _)| vec![value.clone()])
                .unwrap_or_default(),
        }
    };
    Filter {
        agent_kinds: value(1),
        kinds: picks[2]
            .checked_sub(1)
            .and_then(|at| KINDS.get(at))
            .map(|kind| vec![*kind])
            .unwrap_or_default(),
        outcomes: picks[3]
            .checked_sub(1)
            .and_then(|at| OUTCOMES.get(at))
            .map(|outcome| vec![*outcome])
            .unwrap_or_default(),
        worker_ids: value(0),
    }
}

fn field_name(field: usize, lang: Lang) -> &'static str {
    match field {
        0 => "worker",
        1 => lang.pick("agent", "agent"),
        2 => lang.pick("種類", "kind"),
        _ => lang.pick("結果", "outcome"),
    }
}

fn kind_label(kind: PostKind, lang: Lang) -> &'static str {
    match kind {
        PostKind::Summary => lang.pick("要約", "summary"),
        PostKind::Milestone => lang.pick("節目", "milestone"),
        PostKind::Blocker => lang.pick("障害", "blocker"),
        PostKind::Decision => lang.pick("決定", "decision"),
        PostKind::Handoff => lang.pick("引き継ぎ", "handoff"),
    }
}

fn outcome_label(outcome: Outcome, lang: Lang) -> &'static str {
    match outcome {
        Outcome::Succeeded => lang.pick("成功", "succeeded"),
        Outcome::Partial => lang.pick("一部", "partial"),
        Outcome::Failed => lang.pick("失敗", "failed"),
        Outcome::Cancelled => lang.pick("中止", "cancelled"),
        Outcome::Unknown => lang.pick("結果不明", "unknown"),
    }
}

fn outcome_color(outcome: Outcome) -> Rgb {
    match outcome {
        Outcome::Succeeded => GREEN,
        Outcome::Partial => ORANGE,
        Outcome::Failed => RED,
        Outcome::Cancelled | Outcome::Unknown => GREY,
    }
}

/// `rn-0123456789abcdef` as `rn-01234567`.
fn short_id(id: &str) -> String {
    clean(id).chars().take(11).collect()
}

/// Append to a query line up to [`MAX_QUERY_CHARS`]; control characters
/// (a pasted newline) are dropped.
fn push_capped(text: &mut String, more: &str) {
    for ch in more.chars().filter(|ch| !ch.is_control()) {
        if text.chars().count() >= MAX_QUERY_CHARS {
            break;
        }
        text.push(ch);
    }
}

/// Seconds east of UTC in the machine's time zone at Unix time `unix`:
/// libc's `localtime_r` resolves `TZ` / `/etc/localtime` for that date, so
/// a winter post read in summer gets its winter offset.
pub(crate) fn local_offset_at(unix: i64) -> i64 {
    let at = unix as libc::time_t;
    // SAFETY: `tm` is a plain C struct for which all-zero is a valid value;
    // `localtime_r` only writes into the buffer we own and is thread-safe.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&at, &mut tm) }.is_null() {
        return crate::localtime::local_offset_seconds();
    }
    tm.tm_gmtoff
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// "14:03:05" of a local Unix time.
fn clock(local: i64) -> String {
    let secs = local.rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// The Unix seconds of an RFC 3339 UTC time `YYYY-MM-DDTHH:MM:SS[.fff]Z`.
fn unix_secs(at: &str) -> Option<i64> {
    let bytes = at.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || !at.ends_with('Z')
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = at.get(from..to)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // Days from the civil date (H. Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

// ---------------------------------------------------------------------------
// The visit boundary (`<data>/noa/timeline-visits.json`, Q5)

/// The boundary stored for `board_id`, or 0 when there is none (a file
/// missing or unreadable reads as no visit).
pub(crate) fn read_visit(path: &Path, board_id: &str) -> u64 {
    read_visits(path)
        .get(board_id)
        .and_then(|seq| seq.as_str())
        .and_then(|seq| seq.parse().ok())
        .unwrap_or(0)
}

fn read_visits(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("boards")?.as_object().cloned())
        .unwrap_or_default()
}

/// Store `seq` as `board_id`'s boundary (never lowering one stored), the
/// file `0600`, replaced whole: staged, synced, renamed, and its directory
/// synced.
pub(crate) fn write_visit(path: &Path, board_id: &str, seq: u64) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut boards = read_visits(path);
    let stored = boards
        .get(board_id)
        .and_then(|seq| seq.as_str()?.parse::<u64>().ok())
        .unwrap_or(0);
    boards.insert(
        board_id.to_string(),
        serde_json::Value::String(stored.max(seq).to_string()),
    );
    let bytes =
        serde_json::to_vec(&serde_json::json!({ "boards": boards })).map_err(io::Error::other)?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("visits path has no directory"))?;
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map_or_else(|| "visits".into(), |name| name.to_string_lossy());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let tmp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        // The mode given at creation is masked by the umask.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_read_as_utc_and_shown_local() {
        assert_eq!(unix_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_secs("2026-11-26T05:03:07.250Z"), Some(1_795_669_387));
        assert_eq!(unix_secs("2026-11-26 05:03:07Z"), None);
        let mut tl = Timeline::new(Lang::Ja, |_| 9 * 3600);
        assert_eq!(tl.time_text("2026-11-26T05:03:07Z"), "11/26 14:03");
        tl.offset_at = |_| 0;
        assert_eq!(tl.time_text("2026-11-26T23:59:00Z"), "11/26 23:59");
        assert_eq!(clock(3661), "01:01:01");
    }

    /// New York: UTC-4 from the second Sunday of March to the first of
    /// November (2026: 03-08 07:00Z to 11-01 06:00Z), else UTC-5.
    fn new_york(unix: i64) -> i64 {
        let dst =
            unix_secs("2026-03-08T07:00:00Z").unwrap()..unix_secs("2026-11-01T06:00:00Z").unwrap();
        if dst.contains(&unix) {
            -4 * 3600
        } else {
            -5 * 3600
        }
    }

    // Review 14: a post's time is shown in its own date's offset, not the
    // one in force when the board is read.
    #[test]
    fn a_winter_post_read_in_summer_keeps_its_winter_offset() {
        let tl = Timeline::new(Lang::Ja, new_york);
        assert_eq!(tl.time_text("2026-01-15T12:00:00Z"), "01/15 07:00");
        assert_eq!(tl.time_text("2026-07-15T12:00:00Z"), "07/15 08:00");
        // The machine's zone, asked for a time, gives today's offset now.
        assert_eq!(
            local_offset_at(unix_now()),
            crate::localtime::local_offset_seconds()
        );
    }

    #[test]
    fn ac3_visits_file_is_private_and_never_lowered() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("noa-visits-{}-{}", std::process::id(), unix_now()));
        let path = dir.join("noa").join("timeline-visits.json");
        assert_eq!(read_visit(&path, "bd-0000000000000001"), 0);
        write_visit(&path, "bd-0000000000000001", 7).unwrap();
        write_visit(&path, "bd-0000000000000002", 3).unwrap();
        write_visit(&path, "bd-0000000000000001", 5).unwrap();
        assert_eq!(read_visit(&path, "bd-0000000000000001"), 7);
        assert_eq!(read_visit(&path, "bd-0000000000000002"), 3);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let leftovers = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            })
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
