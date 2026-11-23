//! The one key router: every key and text event the dashboard sees goes
//! through `route` before the tree, so a focused filter field types digits
//! instead of toggling panels, and the help overlay keeps keys from the
//! screen below. `BINDINGS` is also what the help lists.

use noa_gpui_core::{Event, Key, Modifiers};

use super::panel::Panel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Nothing typed into: keys move the selection and toggle panels.
    Browse,
    /// The filter field has the focus: keys not bound here edit it.
    Filter,
    /// The drawer's send field has the focus: Enter sends, Esc leaves it.
    Send,
    /// The help overlay is open: keys not bound here are swallowed.
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Trigger {
    /// A key with no modifier held (`Key::Char` is lowercase).
    Key(Key),
    /// Committed text, whatever the modifiers (`?` needs shift).
    Text(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Next,
    Prev,
    NextWaiting,
    Open,
    ToggleDetail,
    StartSend,
    SubmitSend,
    CancelSend,
    ClosePane,
    CloseEnded,
    ViewDiff,
    CycleGrouping,
    NextTab,
    StartFilter,
    Back,
    TogglePanel(Panel),
    OpenHelp,
    ClearFilter,
    CommitFilter,
    CloseHelp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Binding {
    pub(super) mode: Mode,
    pub(super) trigger: Trigger,
    /// The keycaps the help shows for this trigger.
    pub(super) keys: &'static [&'static str],
    pub(super) action: Action,
    pub(super) title: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    Act(Action),
    /// Nobody gets the event.
    Swallow,
    /// The tree gets the event as usual.
    Tree,
}

const fn bind(
    mode: Mode,
    trigger: Trigger,
    keys: &'static [&'static str],
    action: Action,
    title: &'static str,
) -> Binding {
    Binding {
        mode,
        trigger,
        keys,
        action,
        title,
    }
}

use Action::*;
use Mode::{Browse, Filter, Help};
use Trigger::{Key as K, Text as T};

const NEXT: &str = "次のワーカーを選ぶ";
const PREV: &str = "前のワーカーを選ぶ";
const MOVE: &str = "入力したまま選択を動かす";
const CLOSE_HELP: &str = "ヘルプを閉じる";
/// The five panel keys share one help row (one keycap, 1–5), so every row
/// fits the smallest window.
const PANELS: &str = "パネル表示の切替（要対応・作業中・完了・推移・関連）";

pub(super) const BINDINGS: &[Binding] = &[
    bind(Browse, K(Key::Char('j')), &["J"], Next, NEXT),
    bind(Browse, K(Key::ArrowDown), &["↓"], Next, NEXT),
    bind(Browse, K(Key::Char('k')), &["K"], Prev, PREV),
    bind(Browse, K(Key::ArrowUp), &["↑"], Prev, PREV),
    bind(
        Browse,
        K(Key::Char('n')),
        &["N"],
        NextWaiting,
        "次の要対応ワーカーを選ぶ",
    ),
    bind(
        Browse,
        K(Key::Enter),
        &["↩"],
        Open,
        "選んだワーカーのペインへ移動",
    ),
    bind(
        Browse,
        K(Key::Space),
        &["Space"],
        ToggleDetail,
        "選んだワーカーの詳細を開く・閉じる",
    ),
    // On its text, as / is: the key's `Text("s")` is then taken here,
    // not typed into the field this focuses.
    bind(
        Browse,
        T("s"),
        &["S"],
        StartSend,
        "選んだワーカーへメッセージ送信",
    ),
    bind(
        Browse,
        K(Key::Char('x')),
        &["X"],
        ClosePane,
        "選んだワーカーのペインを閉じる（2 回で確認）",
    ),
    // On its text, as S is: Shift+X is the key `x` with Shift held, which
    // no `Key` binding takes, then the text "X".
    bind(
        Browse,
        T("X"),
        &["⇧X"],
        CloseEnded,
        "絞り込み中の完了・失敗をまとめて閉じる（2 回で確認）",
    ),
    bind(
        Browse,
        K(Key::Char('d')),
        &["D"],
        ViewDiff,
        "選んだワーカーの作業ツリーを Repo Browser で開く（差分を見る）",
    ),
    bind(
        Browse,
        K(Key::Char('g')),
        &["G"],
        CycleGrouping,
        "グループを切り替え",
    ),
    bind(
        Browse,
        K(Key::Char('t')),
        &["T"],
        NextTab,
        "タブを切り替え（運用 / 統計 / マップ / タイムライン）",
    ),
    bind(
        Browse,
        T("/"),
        &["/"],
        StartFilter,
        "絞り込む（state: / need: / parent: / -否定 に対応）",
    ),
    bind(
        Browse,
        K(Key::Escape),
        &["Esc"],
        Back,
        "詳細を閉じる → 絞り込みを消す → 選択を外す",
    ),
    bind(
        Browse,
        K(Key::Char('1')),
        &["1–5"],
        TogglePanel(Panel::Queue),
        PANELS,
    ),
    bind(
        Browse,
        K(Key::Char('2')),
        &[],
        TogglePanel(Panel::Running),
        PANELS,
    ),
    bind(
        Browse,
        K(Key::Char('3')),
        &[],
        TogglePanel(Panel::Done),
        PANELS,
    ),
    bind(
        Browse,
        K(Key::Char('4')),
        &[],
        TogglePanel(Panel::Timeline),
        PANELS,
    ),
    bind(
        Browse,
        K(Key::Char('5')),
        &[],
        TogglePanel(Panel::Graph),
        PANELS,
    ),
    bind(Browse, T("?"), &["?"], OpenHelp, "このヘルプ"),
    bind(
        Filter,
        K(Key::Escape),
        &["Esc"],
        ClearFilter,
        "絞り込みを消して入力を終える",
    ),
    bind(
        Filter,
        K(Key::Enter),
        &["↩"],
        CommitFilter,
        "絞り込みを残して入力を終える",
    ),
    bind(Filter, K(Key::ArrowDown), &["↓"], Next, MOVE),
    bind(Filter, K(Key::ArrowUp), &["↑"], Prev, MOVE),
    bind(Help, K(Key::Escape), &["Esc"], CloseHelp, CLOSE_HELP),
    bind(Help, T("?"), &["?"], CloseHelp, CLOSE_HELP),
];

/// No modifier held: `Modifiers::default()` and `Modifiers::on_apple()` both
/// qualify (the latter only says which key is the command key).
fn plain(modifiers: &Modifiers) -> bool {
    !(modifiers.shift || modifiers.alt || modifiers.ctrl || modifiers.command || modifiers.meta)
}

fn matches(trigger: Trigger, event: &Event) -> bool {
    match (trigger, event) {
        (Trigger::Key(want), Event::KeyDown { key, modifiers }) => *key == want && plain(modifiers),
        (Trigger::Text(want), Event::Text(text)) => text == want,
        _ => false,
    }
}

/// Where `event` goes in `mode`. While an IME composition is in progress in
/// a text field (`Filter` or `Send`), every event goes to the field.
pub(super) fn route(mode: Mode, composing: bool, event: &Event) -> Route {
    if matches!(mode, Mode::Filter | Mode::Send) && composing {
        return Route::Tree;
    }
    if mode == Mode::Send {
        if matches(Trigger::Key(Key::Enter), event) {
            return Route::Act(Action::SubmitSend);
        }
        if matches(Trigger::Key(Key::Escape), event) {
            return Route::Act(Action::CancelSend);
        }
        return Route::Tree;
    }
    if let Some(binding) = BINDINGS
        .iter()
        .find(|b| b.mode == mode && matches(b.trigger, event))
    {
        return Route::Act(binding.action);
    }
    let typing = matches!(
        event,
        Event::KeyDown { .. } | Event::Text(_) | Event::Composition { .. } | Event::ImeSelect(_)
    );
    if mode == Mode::Help && typing {
        Route::Swallow
    } else {
        Route::Tree
    }
}
