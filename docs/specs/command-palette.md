# Spec: コマンドパレット (command-palette)

## Metadata

- slug: `command-palette`
- title: コマンドパレット (Command Palette)
- status: `locked`(Accord+Scribe spec 作成 2026-07-04 / apex Phase 1-4 圧縮)
- owner: simota
- parity: **REQ-MACOS-002 / IMPL-MACOS-002**(`docs/roadmaps/ghostty-parity-roadmap.md`)— "Add command palette backed by the action registry"
- recipe: apex(Discovery→Ideate→Verdict→Spec 圧縮)。scope mode = **Standard**(17 要件 = 12 functional + 5 NFR、新規 enum variant + 新規モーダルセッション + レンダラオーバーレイ + フィルタロジック)。
- build-path: 未定(LOCK 後に orbit loop / titan 等を指示で選択)。**本 spec はコードを書かない。**

## L0 — Vision

1. **問題**: noa の全アプリコマンド(`AppCommand`)は macOS メニューとキーバインドからのみ起動できる。キーバインドを覚えていない/メニューを辿るのが遅いユーザーは、実行したい操作(分割、フォント、スクロール等)に素早く到達できない。Ghostty は 1.1.0 でコマンドパレットを導入済みで、乗り換えユーザーはこの検索実行サーフェスを期待する。
2. **対象**: Ghostty からの乗り換えユーザーおよびキーボード駆動のヘビーユーザー。
3. **Job-to-be-done**: キー一発でパレットを開き、コマンド名を数文字打って絞り込み、Enter 一発で実行する。実行できるコマンドと(あれば)そのキーバインドを一覧で確認できる。
4. **成功条件**: `cmd+shift+p` でパレットが開き、タイトルへのファジー(部分列)マッチで `AppCommand` 一覧が絞り込まれ、Enter でハイライト中コマンドを実行して閉じ、Esc で実行せず閉じる。パレットのセッション状態はタブ/ウィンドウ閉鎖でリークしない(search_prompt の初期リークを繰り返さない)。
5. **制約**: キーボード駆動(v1 マウス操作なし)/ search_prompt のオーバーレイ機構(グリッド整列モーダル、インクリメンタル・`FrameSnapshot` 経由描画)を再利用/新規重量級依存なし・純 Rust(新規 FFI なし)/ macOS ファースト。
6. **アクションレジストリ**: パレットが露出するのは既存の `AppCommand` enum(`crates/noa-app/src/commands.rs`)そのもの。これがロードマップの言う "action registry"。

### パリティ忠実度メモ(Fidelity)

- Ghostty のパレットは大量のアクション(config-reload、inspector、goto_tab:N 等)を列挙する。noa のパレットは **noa が実装済みの `AppCommand` のみ**を列挙する。これは「Ghostty 挙動からの逸脱」ではなく「noa の実装済みアクション集合が Ghostty より小さい」だけであり、観測挙動(開く/絞る/実行/閉じる)は忠実に再現する。
- **明示的パリティ例外(意図的スコープ差)**: (a) Ghostty の `command_palette_entry`(ユーザー定義カスタムエントリ)は v1 で **CUT**。(b) `SelectTab(1..9)`(Go to Tab N)は 9 個の冗長エントリのためパレット表示から **CUT**(タイトルレジストリには保持、`cmd+1..9` で直接到達可能)。いずれも本 L0 に記録する。
- **忠実ギャップ**: Ghostty のパレットはファジーマッチのスコアリング/最近使用順を持ちうるが、v1 は部分列マッチ+レジストリ宣言順のみ(ランキング・recency は CUT、下記 Scope 参照)。

## FRAME — 再利用資産と制約(Lens 調査 2026-07-04)

### 既存資産(search_prompt が最近傍アナログ)

- **`AppCommand` レジストリ**(`commands.rs:8-31`)= 露出対象。`action_name()`(commands.rs:217)が安定機械名、`menu_id()`/`from_menu_id()` が往復済み。→ パレットの安定 ID として `action_name` を流用可能。
- **`KeybindEngine`**(`commands.rs:344-455`)= `AppCommand → chord` の唯一の正。`KeybindEngine::default()` の spec 表(commands.rs:350-437)から逆写像 `AppCommand → Option<chord>` を導ける。
- **`SearchPromptSession`**(`app.rs:193-197`)= 単一 app-wide モーダルセッションの前例。`window_id`/`pane_id`/バッファ。open-guard(`app.rs:2415`)、KeyboardInput 先取りルーティング(`app.rs:1890-1897`)、Esc/Enter/Backspace/text の modal 処理(`handle_search_prompt_key`, `app.rs:2453-2509`)。
- **セッション・クリーンアップ前例(リーク修正済み)**: `close_tab`(`app.rs:848-854`)と `close_pane`(`app.rs:928-935`)で対象ウィンドウ/ペインの search_prompt を `None` にする。**この2箇所がパレットでも守るべきリーク非発生契約**。
- **`FrameSnapshot::search_prompt`**(`app.rs:399-400`)= 状態→GPU シーム。ロック最小・自己完結スナップショットにオーバーレイ payload を足す前例。
- **`CommandScope`**(`app.rs:3331-3388`)+ `handle_app_command`(`app.rs:526`)+ `overview_command_scope` ガード(`app.rs:527`)= コマンド発火のスコープ解決。`ToggleTabOverview`/`ToggleSplitZoom` がトグルコマンド追加の同型パターン。
- **`macos_menu.rs`**(View メニュー、`ToggleTabOverview` は `app.rs`/`macos_menu.rs:198`)= ネイティブメニュー項目追加パターン。

### 制約

- search_prompt は**単一行**バッファ(`search_prompt.rs`)。パレットは**複数行リスト+ハイライト+絞り込み**が必要 → オーバーレイ描画は search_prompt の「グリッド整列モーダル」パターンを踏襲しつつ行リスト描画は新規(noa-render)。
- `cmd+shift+p` は空き(検証済み: `cmd+shift+o`=overview、`cmd+shift+d`=split-down、`cmd+shift+g`=find-prev、`cmd+shift+enter`=zoom、`cmd+shift+[`/`]`=tab、`cmd+shift+plus`=font)。Ghostty macOS 既定と一致。
- クレート依存規則: パレットのタイトル表・フィルタ・セッション状態は可能な限り GUI 非依存にし、`noa-grid` 以下は無改変。winit/wgpu は `noa-app`/`noa-render` に閉じ込める。

## L1 — Requirements

優先度タグ: **[MH]** = must-have(v1 ブロッカー)、**[NH]** = nice-to-have。

### 機能要件 (Functional)

**コマンド定義・配線**

- **R-1** [MH]: 新規 `AppCommand::ToggleCommandPalette` variant を追加する。`action_name()` = `"command-palette.toggle"`、`menu_id()` = `"noa.view.toggle-command-palette"` を割り当て、`from_action_name`/`from_menu_id`/`menu_id` の網羅マッチを更新して往復可能にする。`KeybindEngine::default()` に `("cmd+shift+p", AppCommand::ToggleCommandPalette)` を追加し、`macos_menu.rs` の View メニューに項目を追加する。`command_scope` は **`CommandScope::App`**(どのタブからでも開ける)。

**タイトルレジストリ**

- **R-2** [MH]: 純関数 `command_palette_title(AppCommand) -> &'static str` を定義し、**すべての `AppCommand` variant**(下記「コマンドタイトルレジストリ」表の全行、`SelectTab(1..9)` と `ToggleCommandPalette` を含む)に人間可読タイトルを与える。網羅マッチ(`match` の `_` ワイルドカード禁止)とし、variant 追加時にコンパイルエラーで未タイトルを検出する(NFR-4 と対)。

- **R-3** [MH]: パレット表示コマンド集合を決定的順序の関数 `command_palette_entries() -> &'static [AppCommand]` として定義する。**除外**: (a) `ToggleCommandPalette` 自身(自己参照除外、overview 自己除外と同型)、(b) `SelectTab(1..9)`(v1 CUT)。順序はレジストリ表の宣言順に一致させる(ファジー同点時の tie-break にも用いる)。

- **R-4** [NH]: 純関数 `command_palette_keybind(AppCommand) -> Option<String>` を `KeybindEngine` の現行バインドから逆引きし、各エントリ行に右寄せでキーバインドヒントを表示する(バインド無しは非表示)。macOS グリフ整形(⌘⇧ 等)は NH の内側 NH(テキスト chord 表記で可)。

**セッション・モーダル動作**

- **R-5** [MH]: `AppCommand::ToggleCommandPalette` はトグル。閉じている時に発火→単一 app-wide `CommandPaletteSession` を **focused ウィンドウにバインド**して生成(空クエリ、`command_palette_entries()` 全件、selected=0)。開いている時に再発火→閉じる(Ghostty `toggle_command_palette` と同型)。同時に複数セッションは存在しない。

- **R-6** [MH]: パレットが focused ウィンドウをターゲットしている間、`KeyboardInput` はキー入力をパレットハンドラへ**先取りルーティング**する(通常の keybind-resolve→pty-encode 経路より前、search_prompt の `app.rs:1890-1897` と同型)。パレット開時はいかなるキーストロークも pty に到達しない(modal)。

- **R-7** [MH]: インクリメンタル・ファジーフィルタ。printable text 入力はクエリ末尾に追記、Backspace は1文字 pop。フィルタは**タイトルに対する大文字小文字無視の部分列(subsequence)マッチ**。各キーストロークで表示リストを再計算し、selected を先頭(0)へリセットする。マッチ集合の順序は `command_palette_entries()` の宣言順を保つ(v1 はスコアリングなし)。

- **R-8** [MH]: ナビゲーションと実行。Up/Down 矢印は絞り込み後リスト内で selected を移動(両端でクランプ、ラップなし)。Enter はハイライト中コマンドを `handle_app_command` 経由で実行し、パレットを閉じる。Esc は実行せず閉じる。

- **R-9** [MH]: 空結果ハンドリング。フィルタ結果が 0 件の時、Enter は no-op(パレットは開いたまま)でパニックしない。selected は空リストで無効参照しない。

- **R-10** [MH]: 実行セマンティクス。実行は既存 `handle_app_command`/`command_scope` を通し、コマンドは自身のスコープでターゲットを再解決する(FocusedTab 系は focused タブに効く)。**別モーダルを開くコマンド(`Search(Find)` → search_prompt)を実行する場合、パレットは副作用の前に閉じる**(2モーダル同時開きを防ぐ)。overview が focused の間はパレットを開かない(v1、下記 Scope 参照)。

**クリーンアップ・描画**

- **R-11** [MH]: **セッションリーク非発生契約(search_prompt パリティ)**。パレットセッションは、そのターゲットウィンドウが閉じられた時(`close_tab` 経路)に `None` へクリアされる(`app.rs:848-854` と同型)。閉じられたウィンドウはキーを配送できず Esc すら届かないため、クリアしないと将来の全 `cmd+shift+p` を open-guard 相当が塞ぐ/デッドウィンドウ参照が残る。パレットはウィンドウ束縛のみ(ペイン非依存)のため、タブ全体が閉じない単なるペイン閉鎖ではクリア不要。

- **R-12** [MH]: 描画シーム。`FrameSnapshot` に `command_palette: Option<CommandPaletteSnapshot>`(クエリ文字列、絞り込み後エントリのタイトル+キーバインドヒント、selected インデックス)を追加し、`FrameSnapshot::from_terminal` 相当の構築点でロック最小に埋める。noa-render は search_prompt オーバーレイと同じグリッド整列モーダルパターンで行リストを描画する。

### 非機能要件 (NFR)

- **NFR-1** [MH](純 Rust・依存衛生): ファジーマッチは手書きの部分列判定で実装し、`fuzzy-matcher`/`nucleo` 等の新規クレートを追加しない。新規 FFI を導入しない。
- **NFR-2** [NH](コスト): フィルタ+スナップショット構築は `AppCommand` レジストリ(~40 エントリ)に対し O(N) で、キーストロークごとに知覚可能な遅延を追加しない(参考: 1 キーあたり < 1ms、健全性チェック)。
- **NFR-3** [MH](依存規則): パレットのタイトル表・フィルタ・セッション状態ロジックは可能な限り GUI 非依存に保つ。`noa-grid`/`noa-vt`/`noa-core` は無改変で `wgpu`/`winit` に依存しない(`cargo tree` で検証可)。winit/wgpu は `noa-app`/`noa-render` に限定。
- **NFR-4** [MH](レジストリ完全性): `command_palette_title` は網羅 `match`(`_` ワイルドカード禁止)であり、`AppCommand` に variant を追加すると未タイトルのままではコンパイルできない(コンパイル時ゲート)。
- **NFR-5** [MH](品質ゲート): 本変更後も `cargo test --workspace` と `cargo clippy --workspace` がクリーン。新規 `#[allow(...)]` によるもみ消し禁止。

> Must 比率メモ: 17 要件中 [MH] = 14(82%)。モーダル正しさ・リーク非発生・実行セマンティクスが v1 の中核のため高比率は妥当。[NH] は R-4(キーバインドヒント表示)・NFR-2(perf 健全性)の 2 件。

## L2 — Detail

per-crate のシームのみ定義する(コードは書かない)。

### noa-app / commands.rs

- `AppCommand` に `ToggleCommandPalette` を追加(R-1)。`action_name`/`from_action_name`/`menu_id`/`from_menu_id` の各網羅マッチへ 1 行ずつ追加。`ABOUT_MENU_ID` 群に `TOGGLE_COMMAND_PALETTE_MENU_ID: &str = "noa.view.toggle-command-palette"` を追加。
- `KeybindEngine::default()` の spec 配列に `("cmd+shift+p", AppCommand::ToggleCommandPalette)` を追加。
- タイトル/エントリ/キーバインド逆引きの純関数(R-2/R-3/R-4)は `commands.rs` もしくは新規 `command_palette.rs` に置く(GUI 非依存、ユニットテスト可)。`command_palette_keybind` は `KeybindEngine` を参照するため、`KeybindEngine::binding_for(AppCommand) -> Option<&KeyTrigger>` 相当の逆引き API を追加するか、既定 spec 表を再走査する。

### noa-app / app.rs

- 新規 `struct CommandPaletteSession { window_id: WindowId, query: String, filtered: Vec<AppCommand>, selected: usize }`(SearchPromptSession と同型、ただし `pane_id` は持たない=ウィンドウ束縛、R-11)。`App` に `command_palette: Option<CommandPaletteSession>` フィールドを追加(`search_prompt` の隣、初期値 `None`)。
- **フィルタは純ロジックに切り出す**: `command_palette_filter(query: &str) -> Vec<AppCommand>`(部分列マッチ、`command_palette_entries()` を走査、宣言順保持、R-7)。`is_subsequence_ci(needle, haystack) -> bool`(大文字小文字無視、NFR-1、手書き)。両者は GUI・Window 不要でユニットテスト可能。
- `command_scope(ToggleCommandPalette) = CommandScope::App`(R-1)。`overview_command_scope(ToggleCommandPalette) = CommandScope::Overview`(overview focused 時は no-op、R-10 の overview ガード)。`handle_app_command` の `ToggleCommandPalette` アームで `toggle_command_palette()` を呼ぶ。
- `toggle_command_palette()`: 開いていれば `self.command_palette = None`; 閉じていれば `self.focused` を window_id に採り、`CommandPaletteSession { query: "", filtered: command_palette_entries().to_vec(), selected: 0 }` を生成し redraw(R-5)。
- `KeyboardInput` ハンドラ(`app.rs:1882` 付近)に、IME/ search_prompt 分岐の直後・keybind-resolve の直前で「`command_palette` が `window_id` をターゲットしていれば `handle_command_palette_key` へ委譲し return」を挿入(R-6、search_prompt の 1890-1897 と同型)。
- `handle_command_palette_key(event_loop, window_id, event)`(`handle_search_prompt_key` と同型、R-7/R-8/R-9):
  - Esc → `self.command_palette = None`(実行せず、R-8)。
  - Enter → 絞り込み後リストが空なら no-op(R-9)、非空なら `filtered[selected]` を取り出し `self.command_palette = None`(**副作用の前に閉じる**、R-10)→ `handle_app_command(event_loop, command)`。
  - ArrowUp/ArrowDown → `selected` をクランプ移動、redraw(R-8)。
  - Backspace → `query.pop()` → `filtered = command_palette_filter(&query)`、`selected = 0`、redraw(R-7)。
  - `cmd`-held combos → swallow(search と同じ規約)。再度 `cmd+shift+p` は toggle で閉じる。
  - printable text → `query.push_str(filtered_text)` → 再フィルタ、`selected = 0`、redraw(R-7)。
- **クリーンアップ(R-11)**: `close_tab`(`app.rs:848-854`)に「`command_palette` が閉鎖ウィンドウをターゲットしていれば `None`」を追加。`close_pane` はペイン束縛でないため追加不要だが、タブ全体閉鎖経路は `close_tab` を通るのでカバーされる。
- **スナップショット(R-12)**: `FrameSnapshot` 構築点(`app.rs:399` 付近)で `command_palette` セッションから `CommandPaletteSnapshot { query, rows: Vec<(String /*title*/, Option<String> /*keybind*/)>, selected }` を構築。ロックは持たない(パレットは端末状態非依存)。

### noa-render

- `FrameSnapshot` に `command_palette: Option<CommandPaletteSnapshot>` を追加(search_prompt 隣接、R-12)。
- レンダラは search_prompt オーバーレイのグリッド整列モーダル描画を拡張し、**行リスト**(タイトル左寄せ + キーバインドヒント右寄せ + selected 行の反転/ハイライト背景 + 上部にクエリ入力行)を描画する。既存セル命令パイプラインで矩形+テキストを重畳(新規 GPU パイプライン不要)。
- 描画は既存オーバーレイ機構の範囲内で完結させ、`noa-render/tests/pipeline.rs` の検証対象(bind-group visibility / std140)を新規に増やさない(既存パイプライン流用)。

### noa-font / noa-pty / noa-grid / noa-vt / noa-core

- 無改変。パレットは端末状態に非依存(NFR-3)。`noa-grid` 以下に winit/wgpu を導入しない。

## L3 — Acceptance Criteria

各 AC は Given/When/Then で対応 `R-*`/`NFR-*` を明記する。[unit] = `cargo test -p noa-app`(GPU/Window 不要な純ロジック)、[manual] = 実 GUI 目視、[inspection] = 型・構造の静的検査/コンパイル境界、[headless] = `noa-render/tests/pipeline.rs`。

- **AC-1 → R-1** [MH] [unit]: Given `AppCommand::ToggleCommandPalette`。When `menu_id`/`from_menu_id`、`action_name`/`from_action_name`、`from_key(Key::Character("p"), SUPER|SHIFT)` を評価。Then 全経路が往復一致し、`from_key` が `ToggleCommandPalette` を返す。加えて `command_scope(ToggleCommandPalette) == CommandScope::App`。
- **AC-2 → R-1** [MH] [unit]+[inspection]: Given `cmd+shift+p` 以外の既存 `cmd+shift+*` バインド。When `KeybindEngine::default()` を評価。Then `cmd+shift+p` は `ToggleCommandPalette` に解決し、既存バインド(o/d/g/enter/[/]/plus)はいずれも上書きされていない。
- **AC-3 → R-2, NFR-4** [MH] [unit]+[inspection]: Given `command_palette_title`。When すべての `AppCommand` variant(`SelectTab(1..9)`・`ToggleCommandPalette` 含む)に対し呼ぶ。Then 各 variant が非空タイトルを返す(全 variant 到達をテストで列挙)。加えて `match` に `_` ワイルドカードが無いことをコード検査で確認(variant 追加時コンパイルエラー化)。
- **AC-4 → R-3** [MH] [unit]: Given `command_palette_entries()`。When 内容を検査。Then `ToggleCommandPalette` と全 `SelectTab(n)` を**含まず**、残りの `AppCommand` variant を**すべて含み**、順序がレジストリ宣言順に一致する。
- **AC-5 → R-4** [NH] [unit]: Given `command_palette_keybind`。When `Copy`/`NewTab`/`Search(Find)`/`Quit` に対し呼ぶ。Then それぞれ `cmd+c`/`cmd+t`/`cmd+f`/`cmd+q` 相当の chord 文字列を返す。When `ClearScrollback`(バインド無し)に対し呼ぶ。Then `None` を返す。
- **AC-6 → R-5** [MH] [unit]: Given パレット閉状態と focused ウィンドウ。When `ToggleCommandPalette` を dispatch。Then `command_palette` が `Some`(query 空、filtered = 全エントリ、selected=0、window_id = focused)になる。When 再度 dispatch。Then `command_palette` が `None` に戻る(トグル)。
- **AC-7 → R-6** [MH] [unit]+[manual]: Given パレットが window_id をターゲット中。When 当該ウィンドウで文字キーの `KeyboardInput` を処理。Then パレットハンドラが消費し、keybind-resolve/pty-encode 経路に到達しない(unit: ルーティング分岐);When 文字入力(manual)。Then どの pty にもバイトが届かない。
- **AC-8 → R-7** [MH] [unit]: Given `command_palette_filter`。When `"splt"` でフィルタ(部分列)。Then "Split Right"/"Split Down"/"Toggle Split Zoom"/"Equalize Splits" 等タイトルに `s..p..l..t` を部分列として含むエントリのみ返り、順序は宣言順。When 大文字クエリ `"QUIT"`。Then "Quit noa" がマッチ(大文字小文字無視)。When 各フィルタ後、selected は 0。
- **AC-9 → R-7** [MH] [unit]: Given query `"new"` の状態。When Backspace を処理。Then query が `"ne"` になり filtered が再計算され selected=0。
- **AC-10 → R-8** [MH] [unit]: Given filtered 3 件・selected=0。When ArrowDown×2→ArrowDown。Then selected は 1→2→2(末尾クランプ、ラップなし)。When ArrowUp を先頭で。Then selected=0 に留まる。
- **AC-11 → R-8** [MH] [unit]+[manual]: Given filtered 非空・selected=k。When Enter を処理。Then `filtered[k]` が `handle_app_command` に渡され、`command_palette` が `None` になる(unit: dispatch 記録);Given 実 GUI で "New Tab" をハイライトし Enter。Then 新規タブが開きパレットが閉じる(manual)。
- **AC-12 → R-8** [MH] [unit]: Given パレット開。When Esc を処理。Then `command_palette` が `None` になり、いかなる `AppCommand` も `handle_app_command` に渡らない(実行なしで閉じる)。
- **AC-13 → R-9** [MH] [unit]: Given query がどのタイトルにもマッチせず filtered が空。When Enter を処理。Then no-op(パレット開のまま、パニックなし)。selected の空リスト参照が起きない。
- **AC-14 → R-10** [MH] [unit]+[manual]: Given パレットで `Search(Find)` をハイライト。When Enter を処理。Then パレットが**先に**閉じ(`command_palette == None`)その後 `Search(Find)` が dispatch される(順序をテストで検証);Given 実 GUI。Then パレットが閉じてから search_prompt が開き、2モーダルが同時に開かない(manual)。
- **AC-15 → R-10** [MH] [unit]: Given overview 表示中(overview focused)。When `ToggleCommandPalette` を dispatch。Then `overview_command_scope` により no-op に解決され、`command_palette` は `None` のまま(v1: overview 中はパレット非表示)。
- **AC-16 → R-11** [MH] [unit]: Given パレットが window A をターゲット中。When window A を `close_tab` で閉じる。Then `command_palette` が `None` にクリアされ、以降 `ToggleCommandPalette` が正常に再度開ける(デッドウィンドウ参照が残らない/open が塞がれない)。
- **AC-17 → R-11** [MH] [unit]: Given パレットが window A をターゲット中で window A 内に複数ペインがある。When window A 内の 1 ペインのみを閉じる(タブは残る)。Then `command_palette` はクリアされない(ウィンドウ束縛=ペイン非依存)。
- **AC-18 → R-12** [MH] [unit]: Given パレットセッション(query, filtered, selected)。When `FrameSnapshot` を構築。Then `snapshot.command_palette` がクエリ・絞り込み後タイトル行・selected を反映し、端末ロックを取得せず構築できる。
- **AC-19 → R-12** [MH] [headless]+[manual]: Given パレット payload を含む `FrameSnapshot`。When 実アダプタで 1 フレーム描画。Then wgpu 検証エラーなしで完了する(headless、非サンドボックス);Given 実 GUI。Then クエリ行・エントリ一覧・キーバインドヒント・ハイライト行が表示される(manual)。
- **AC-20 → NFR-1** [MH] [inspection]: Given `crates/noa-app/Cargo.toml` と実装。When 依存とファジーマッチ実装を検査。Then 新規ファジーマッチ系クレート(`fuzzy-matcher`/`nucleo` 等)も新規 FFI も追加されておらず、部分列判定が手書きである。
- **AC-21 → NFR-2** [NH] [unit]: Given `command_palette_filter`。When レジストリ全件に対しフィルタ 1 回を実行。Then 計算量が O(N) 相当(レジストリ走査 1 パス)であることをコードレビューで確認し、参考値として 1 キーあたり < 1ms をマイクロベンチで健全性チェック(必須合否基準ではない)。
- **AC-22 → NFR-3** [MH] [unit]: Given 変更後ワークスペース。When `cargo tree -p noa-grid --offline` と `cargo tree -p noa-vt --offline` を実行。Then いずれも `wgpu`/`winit` を含まない(パレット追加で GUI 依存が下層に漏れていない)。
- **AC-23 → NFR-4** [MH] [inspection]: Given `command_palette_title` の `match`。When ソースを検査。Then `_` ワイルドカードアームが無く、`AppCommand` に variant を追加すると当該関数がコンパイルエラーになる(コンパイル時完全性ゲート)。
- **AC-24 → NFR-5** [MH] [unit]+[headless]: Given 本変更を適用したワークスペース。When `cargo test --workspace --offline` と `cargo clippy --workspace --offline` を実行。Then 両方 exit 0 で、変更で新規追加された `#[allow(...)]` が存在しない。

### Traceability — R/NFR ↔ AC(双方向)

| 要件 | AC | 優先度 |
|---|---|---|
| R-1 | AC-1, AC-2 | MH |
| R-2 | AC-3 | MH |
| R-3 | AC-4 | MH |
| R-4 | AC-5 | NH |
| R-5 | AC-6 | MH |
| R-6 | AC-7 | MH |
| R-7 | AC-8, AC-9 | MH |
| R-8 | AC-10, AC-11, AC-12 | MH |
| R-9 | AC-13 | MH |
| R-10 | AC-14, AC-15 | MH |
| R-11 | AC-16, AC-17 | MH |
| R-12 | AC-18, AC-19 | MH |
| NFR-1 | AC-20 | MH |
| NFR-2 | AC-21 | NH |
| NFR-3 | AC-22 | MH |
| NFR-4 | AC-3, AC-23 | MH |
| NFR-5 | AC-24 | MH |

**Coverage: 17/17 要件が ≥1 AC にトレース = 100%**(Standard-scope 最小 ≥85%)。AC 総数 **24**(逆方向: 全 AC が発生元 R/NFR を明示)。

## コマンドタイトルレジストリ

`command_palette_title(AppCommand)` の全行(R-2、`AppCommand` の全 variant を網羅)。**表示列** = v1 パレット(`command_palette_entries()`)に現れるか。**キーバインド列** = `KeybindEngine::default()` の現行バインド(空=未バインド)。

| # | AppCommand variant | タイトル | キーバインド | 表示 |
|---|---|---|---|---|
| 1 | `About` | About noa | | ✓ |
| 2 | `Preferences` | Open Preferences | | ✓ |
| 3 | `Copy` | Copy to Clipboard | cmd+c | ✓ |
| 4 | `Paste` | Paste from Clipboard | cmd+v | ✓ |
| 5 | `Terminal(Clear)` | Clear Screen | cmd+k | ✓ |
| 6 | `Terminal(ClearScrollback)` | Clear Scrollback | | ✓ |
| 7 | `Terminal(SelectAll)` | Select All | cmd+a | ✓ |
| 8 | `FontSize(Increase)` | Increase Font Size | cmd+= | ✓ |
| 9 | `FontSize(Decrease)` | Decrease Font Size | cmd+- | ✓ |
| 10 | `FontSize(Reset)` | Reset Font Size | cmd+0 | ✓ |
| 11 | `Search(Find)` | Find… | cmd+f | ✓ |
| 12 | `Search(FindNext)` | Find Next | cmd+g | ✓ |
| 13 | `Search(FindPrevious)` | Find Previous | cmd+shift+g | ✓ |
| 14 | `Search(Clear)` | Clear Search | | ✓ |
| 15 | `ScrollViewport(LineUp)` | Scroll Up One Line | shift+↑ | ✓ |
| 16 | `ScrollViewport(LineDown)` | Scroll Down One Line | shift+↓ | ✓ |
| 17 | `ScrollViewport(PageUp)` | Scroll Up One Page | shift+PageUp | ✓ |
| 18 | `ScrollViewport(PageDown)` | Scroll Down One Page | shift+PageDown | ✓ |
| 19 | `ScrollViewport(Top)` | Scroll to Top | shift+Home | ✓ |
| 20 | `ScrollViewport(Bottom)` | Scroll to Bottom | shift+End | ✓ |
| 21 | `NewTab` | New Tab | cmd+t | ✓ |
| 22 | `NewSplitRight` | Split Right | cmd+d | ✓ |
| 23 | `NewSplitDown` | Split Down | cmd+shift+d | ✓ |
| 24 | `FocusDirection(Left)` | Focus Split Left | cmd+alt+← | ✓ |
| 25 | `FocusDirection(Right)` | Focus Split Right | cmd+alt+→ | ✓ |
| 26 | `FocusDirection(Up)` | Focus Split Up | cmd+alt+↑ | ✓ |
| 27 | `FocusDirection(Down)` | Focus Split Down | cmd+alt+↓ | ✓ |
| 28 | `ResizeSplit(Left)` | Resize Split Left | cmd+ctrl+← | ✓ |
| 29 | `ResizeSplit(Right)` | Resize Split Right | cmd+ctrl+→ | ✓ |
| 30 | `ResizeSplit(Up)` | Resize Split Up | cmd+ctrl+↑ | ✓ |
| 31 | `ResizeSplit(Down)` | Resize Split Down | cmd+ctrl+↓ | ✓ |
| 32 | `EqualizeSplits` | Equalize Splits | cmd+ctrl+= | ✓ |
| 33 | `ToggleSplitZoom` | Toggle Split Zoom | cmd+shift+enter | ✓ |
| 34 | `ToggleTabOverview` | Toggle Tab Overview | cmd+shift+o | ✓ |
| 35 | `CloseTab` | Close Tab | cmd+w | ✓ |
| 36 | `SelectTab(1)` | Go to Tab 1 | cmd+1 | — (CUT) |
| 37 | `SelectTab(2)` | Go to Tab 2 | cmd+2 | — (CUT) |
| 38 | `SelectTab(3)` | Go to Tab 3 | cmd+3 | — (CUT) |
| 39 | `SelectTab(4)` | Go to Tab 4 | cmd+4 | — (CUT) |
| 40 | `SelectTab(5)` | Go to Tab 5 | cmd+5 | — (CUT) |
| 41 | `SelectTab(6)` | Go to Tab 6 | cmd+6 | — (CUT) |
| 42 | `SelectTab(7)` | Go to Tab 7 | cmd+7 | — (CUT) |
| 43 | `SelectTab(8)` | Go to Tab 8 | cmd+8 | — (CUT) |
| 44 | `SelectTab(9)` | Go to Tab 9 | cmd+9 | — (CUT) |
| 45 | `NextTab` | Next Tab | cmd+shift+] | ✓ |
| 46 | `PrevTab` | Previous Tab | cmd+shift+[ | ✓ |
| 47 | `CloseWindow` | Close Window | | ✓ |
| 48 | `Quit` | Quit noa | cmd+q | ✓ |
| 49 | `ToggleCommandPalette` *(new, R-1)* | Toggle Command Palette | cmd+shift+p | — (self) |

> `SelectTab(n)` は `action_name` が `tab.select-{n}`(commands.rs:254-265)で index を持つため、タイトルレジストリは 1..9 を個別行として保持する(網羅マッチ完全性、NFR-4)。表示集合からの除外は R-3 の `command_palette_entries()` で行う。

## Scope

### In-scope (v1)

- `AppCommand::ToggleCommandPalette` 新規 variant + `cmd+shift+p` バインド + View メニュー項目 + 往復配線(R-1)。
- 全 variant 網羅のタイトルレジストリ(R-2/NFR-4)+ 表示エントリ集合(R-3)+ キーバインドヒント逆引き(R-4)。
- 単一 app-wide トグルモーダルセッション(R-5)、modal キーボードルーティング(R-6)。
- インクリメンタル部分列ファジーフィルタ(R-7)、矢印ナビ+Enter 実行+Esc 取消(R-8)、空結果 no-op(R-9)。
- 既存 `handle_app_command`/`command_scope` を通す実行、副作用前の閉じ、overview 中の非表示(R-10)。
- タブ/ウィンドウ閉鎖時のセッションクリア(リーク非発生パリティ、R-11)。
- `FrameSnapshot` オーバーレイ payload + search_prompt パターン流用の行リスト描画(R-12)。

### Out-of-scope（YAGNI / void-style scope cut）

- **マウス操作**(クリックで選択・実行、ホバーハイライト)— search_prompt がキーボード専用のため v1 もキーボードのみ。**CUT**。
- **コマンド履歴 / 最近使用順 / 頻度ランキング** — v1 は宣言順のみ。**CUT**。
- **ファジースコアリング / ハイライト(マッチ文字の強調表示)** — v1 は真偽の部分列判定のみ。**CUT**。
- **`SelectTab(1..9)` の表示** — 9 個の冗長エントリ、`cmd+1..9` で直接到達可。タイトルレジストリには保持。**CUT**。
- **ユーザー定義カスタムエントリ**(Ghostty `command_palette_entry`)— **CUT**(パリティ例外、L0 記録)。
- **パレットからの引数入力**(引数を取るアクション、プロンプト連鎖)— noa の `AppCommand` は引数を取らないため不要。**CUT**。
- **overview focused 中のパレット表示** — overview は専用キーマップのため v1 は非表示(no-op)。**DEFER**。
- **キーバインドヒントの macOS グリフ整形(⌘⇧ 等)** — テキスト chord 表記で可、グリフ化は将来増分。**DEFER**。

### 非ゴール

- pty へのキー入力パススルー(パレット開時は完全 modal)。
- コマンドのアンドゥ/確認ダイアログ。
- 設定ファイルによるパレット挙動のカスタマイズ。

## Considered but rejected

- **キーバインド共有: `Search(Find)` の cmd+f を再利用しパレット起動を兼ねる** — 却下: Ghostty は独立した `cmd+shift+p` を持ち、検索とコマンド実行は別 JTBD。混線させない。
- **パレットを noa-render の完全新規パイプラインで描画** — 却下: search_prompt の既存オーバーレイ機構(グリッド整列モーダル)を拡張する方が CLAUDE.md GPU gotcha(bind-group visibility / std140)への新規曝露を避けられ安全。
- **`SelectTab(n)` を "Go to Tab…" 単一エントリ+数字入力に集約** — 却下(v1): 引数入力 UI は CUT スコープ。将来増分候補。
- **ファジーマッチに `nucleo`/`fuzzy-matcher` クレート導入** — 却下: NFR-1(新規重量級依存なし)。~40 エントリに対し手書き部分列判定で十分。
- **パレットセッションを search_prompt と同じ `pane_id` 束縛にする** — 却下: パレットのコマンドは自身のスコープでターゲット再解決するため(R-10)ペイン非依存。ウィンドウ束縛のみでリーククリーンアップが単純化(R-11、close_pane 追加不要)。

## Open Questions / Deferred Decisions

- 矢印ナビのラップ挙動(両端クランプ採用、ラップは将来検討)— v1 はクランプで確定。
- キーバインドヒントの表示形式(テキスト chord vs ⌘グリフ)— v1 テキスト、グリフ化は DEFER。
- overview focused 中のパレット併用(v1 no-op)— overview 側キーマップ拡張は将来増分。
- 将来増分(deferred): マウス操作 / recency 順 / マッチ文字ハイライト / `SelectTab` 集約エントリ / カスタムエントリ / グリフ整形。

## Next Actions

- ハンドオフ先: **atlas**(design + risk gate)。本 spec の R-1..R-12 / NFR-1..5 と 24 AC を machine-checkable DONE ゲートの母集合とする。
- 主要リスク: (1) KeyboardInput ルーティング挿入位置(IME/search_prompt/keybind の順序、`app.rs:1882-1901`)、(2) リーククリーンアップの `close_tab` 追加漏れ(R-11、search_prompt の実績パスに追従)、(3) レンダラ行リスト描画の既存オーバーレイ流用範囲。
- **本 spec はコードを書かない — loop の生成・起動は別途指示で実行。**
