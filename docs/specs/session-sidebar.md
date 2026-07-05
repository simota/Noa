# Session Sidebar — Specification

## Metadata
- slug: `session-sidebar`
- title: セッションサイドバー（新規タブリストUI）
- status: `locked` (2026-07-05)
- owner: simota
- build-path: **apex**（`/nexus apex` — 設計→リスクゲート→実装ループ→AC検証→ship。L3 AC が検証契約。[manual] AC 7本は人手確認）
- source mockup: `~/Downloads/ChatGPT Image 2026年7月5日 08_43_15.png`

## L0 — Vision
noa の現行タブは macOS ネイティブタブで、一覧性は別ウィンドウの Tab Overview のみ。複数プロジェクトを並行作業する際「どのセッションで何が起きているか」（cwd・ブランチ・最終出力・実行状態）を常時把握し即切替する手段がない。そこで、セッションカード（アイコン・名前・cwd・ブランチ・更新時刻・状態ドット・最終出力2行プレビュー）を並べる常設左サイドバーを追加する。

- **audience**: 複数プロジェクト/セッションを並行作業する開発者（＝作者自身）
- **job-to-be-done**: ターミナルを離れず全セッションの状態を把握し、1クリックで切り替える
- **success**: 全セッションの cwd・ブランチ・最終出力・状態がサイドバーで常時見え、クリック切替が機能する

### FRAME 決定事項
1. **セッション単位**: 全ウィンドウ横断のサイドバー。ただし**モード切替**で従来のネイティブタブモードも維持（sidebar mode / native-tab mode）。
2. **ヘッダーバー**: スコープに含める（「✳ Claude Code」風の実行中プログラムラベル＋中央タイトル＋右端セッション名ピル）。
3. **git ブランチ表示**: 含める。新規実装 — cwd に対する throttled な `git -C <cwd> branch --show-current`（キャッシュ、描画パス外）。

### Lens 再利用/制約所見（2026-07-05 スキャン）
**再利用資産**
- `noa-render/src/blit.rs` — 丸角カードパイプライン（`overlay_texture_cards`、`CardStyle`、border/focus-glow）
- `noa-app/src/tab_overview.rs` — グリッド計算・10Hzスロットル・フィルタ・ヒットテスト（純ロジック、ユニットテスト済）
- `Surface.overview_snapshot`（app.rs:298）— io スレッドが `FrameSnapshot` を publish するスロット。Terminal ロック不要で最終出力プレビュー実現可
- `Terminal.cwd`（OSC 7、noa-grid/src/terminal.rs:49）・`Terminal.title`（OSC 0/2）
- `relayout_and_resize_window`（app.rs）— grid-first リサイズパス
- `noa-config`: `StartupConfig` + parser.rs のキー追加パターン

**未実装（新規作業）**
- git ブランチ検出／最終出力タイムスタンプ（io スレッドで `Instant` スタンプ可）／構造化ステータス（busy 検出は `group_running_program_count` が部分的に存在）

**ハード制約**
1. テキストラベル用プリミティブなし — サイドバー文字は全てターミナルセル描画（overview 方式の専用小 Renderer）か事前ラスタライズテクスチャ
2. レンダラは Terminal をロックしない — snapshot publish スロット経由（sidebar 可視時のみ gate）
3. サイドバー幅変更は grid-first リサイズ必須（grid → pty winsize の順）
4. wgpu は noa-app/noa-render のみ、winit は noa-app のみ。レイアウトロジックは tab_overview.rs 同様に純粋・テスト可能に
5. blit.rs シェーダ拡張時は std140 / bind-group visibility の罠に注意

## EXPAND — 候補方向（2026-07-05 確定）
検討5方向: A=Window Aggregator（現行構造維持・focus切替）／B=Session Host（1ウィンドウ多重Terminal・active差替）／C=Shared Registry（中央SessionStore＝真実源、UIはread-onlyビュー、切替ポリシー差替可）／D=Overlay Projection（Tab Overview縦積み最小改修）／E=Attention Switcher（召喚オーバーレイ＋attentionシグナル）。

**生存候補: C（Shared Registry）** — ユーザー採択。

### EXPAND 追加決定
- 可視性: **トグル式常設**（keybind＋config。表示中は常設、トグル時に grid-first リサイズ）。auto-hide は不採用。
- スケール: 初版はスクロールのみ。repo-root グルーピング／折畳は Open Question へ。

### Flux 警告（設計に反映すべき）
1. 固定パネルは全ターミナルを狭める → トグル必須（採用済）
2. 共有レジストリなしの鮮度更新は git spawn ストーム → C の採択理由（更新はセッション数 N 回で済む）
3. text-is-cells 税: カード1枚≒6テキストラン。20件超は未対応（Open Question）

## CHALLENGE — 採択 / 棄却（2026-07-05 確定）

### 採択: C（Shared Registry）＋ Magi 裁定4項目（全 3-0 承認）
1. **ウィンドウモデル = A-flavor**: 1タブ=1 WindowState 維持。切替=ウィンドウフォーカス。B-flavor（active-swap）は切替ポリシーの継ぎ目の裏に温存（v2 で差し替え可能、store には触れない）。
2. **SessionStore = channel-delta 型**: io スレッドが差分を送信、メインスレッドが store を所有。クロススレッドロック無し。既存 UserEvent poke パターンを踏襲。
3. **描画 = per-window**: 各ウィンドウが同じ store を read-only に描く。「メインウィンドウ」という特権概念は作らない。
4. **可視性 = per-window トグル**: config がアプリ全体の初期値、keybind はフォーカス中ウィンドウのみ反転。
- 「モード切替（sidebar↔native）」は**サイドバー可視性トグルに縮退**（A-flavor ではネイティブタブが無傷のため、非表示=従来モード）。

### Ripple: GO-with-conditions（リスク6.5/10、影響~6領域・新規2ファイル・700-1100 LOC）
拘束条件として採用:
- (a) git spawn は io 読取ループ厳禁 — 専用 branch-poll スレッド、cwd 毎キャッシュ（≥1s throttle）、OSC-7 cwd 変化時のみ、非 git ディレクトリはネガティブキャッシュ、結果は UserEvent で post
- (b) SessionStore は overview_snapshot と同型の publish-slot 読取モデル。Terminal をロックしない
- (c) GC は全5 teardown サイト（close_pane / close_pane_after_pty_exit / close_tab / window remove / Quit）に併置＋store サイズが overview_tiles に追随する回帰テスト
- (d) sidebar レイアウトは tab_overview.rs 同様に純粋・ユニットテスト可能に
- (e) **quick-terminal ウィンドウはサイドバー対象外**（inset も掛けない）
- (f) 実装は 4 PR 以上に分割（config+store → layout → render → git）
- リサイズ順序: トグル時、**フォーカス中 WindowState** について pane_bounds_for_size の inset → relayout_and_resize_window（grid → pty winsize）→ request_redraw（可視性は per-window のため他ウィンドウは触らない）

### v1 スコープ再裁定（Void 攻撃へのユーザー裁定）
- **v1 に含める（FRAME 維持・フルモック忠実）**: git ブランチ／ヘッダーバー3要素／updated-time／+ ボタン／プロジェクトアイコン／… メニュー
- Void KEEP はそのまま: 名前・cwd・状態ドット・2行プレビュー・クリック切替
- CUT 採用: pluggable 切替ポリシーの**ユーザー向け設定化**（内部の継ぎ目としてのみ保持）
- スケール対応（グルーピング/折畳/バッジ）: Open Question 維持

### Considered but rejected（EXPAND 由来）
- A: Window Aggregator 単独 — store なしでは git/snapshot 更新が N×ウィンドウ数に爆発（Flux 警告②）
- B: Session Host — app.rs のウィンドウ=タブ前提の大改修・最大工数（Magi 却下、継ぎ目で将来到達可能に）
- D: Overlay Projection — 早期頭打ち、フル要素を足すと二度手間
- E: Attention Switcher — 常時ダッシュボードという JTBD に不適合

## SHAPE — 提案（2026-07-05 承認）

### Solution
中央 `SessionStore`（channel-delta 型・メインスレッド所有）を真実源に、各ウィンドウが read-only に描くトグル式常設左サイドバー。io スレッドは `overview_snapshot` と同型の publish-slot でセッション状態を送り、専用 branch-poll スレッドが git ブランチを供給。切替＝ウィンドウフォーカス（A-flavor）。

カード構成: `[icon] name … ●dot` ／ `cwd … branch` ／ ANSI 最終出力2行 ／ 相対 updated-time。
ヘッダーバー: busy ラベル（縮退版）＋中央タイトル＋セッション名ピル。サイドバー上部に + ボタンと … メニュー。

### Open Question 解決（ユーザー承認済）
- **アイコン**: Nerd Font グリフをセル描画。判定 = cwd のマーカー first-match（`Cargo.toml`→rust, `package.json`→node, `*.tf`→terraform, `go.mod`→go, `pyproject.toml`→python, `.git`のみ→git, なし→folder）。cwd 変化時のみ再判定（branch-poll スレッド同居）。
- **… メニュー**: v1 は close / rename の2アクション（rename は SessionStore の名前オーバーライド）。
- **+ ボタン**: フォーカス中ウィンドウに新規タブ、cwd はアクティブセッションから継承（既存 new-tab パス再利用）。
- **状態ドット**: busy（OSC 133 `has_running_program`）=青／idle=緑／bell-attention（未読ベル）=黄。
- **updated-time**: 相対表示（"3分前"、24h 超は "昨日 23:47" 形式）。io スレッドが最終出力時に Instant＋wall-clock スタンプ。
- **サイドバー幅**: config `sidebar-width`（points、既定 360pt）。grid-first リサイズで換算。
- **ヘッダーラベル**: 当初 park したフォアグラウンドプロセス名検出は、ユーザー要望により 2026-07-05 実装（`Pty::foreground_probe` = master fd dup → tcgetpgrp → libproc `proc_name`、session-metadata ワーカーで 1s ポーリング・サイドバー可視時のみ）。ラベルは実プロセス名（`✳ <proc>`）、未検出時は Running/Idle にフォールバック。

### Assumptions
- A1: Nerd Font/emoji はフォント fallback カスケード（face.rs）でセル描画可。未インストール時は tofu/emoji にデグレード
- A2: 同時セッション数 ≤ ~20
- A3: macOS のみ
- A4: busy/bell は OSC 133 シェル統合前提。未導入シェルは busy=false にデグレード

### MoSCoW（PR 順序）
- **Must** (PR1-2): SessionStore＋GC＋回帰テスト／純粋 sidebar layout／トグル＋grid-first リサイズ／カード基本描画（名前・cwd・ドット・click-switch）
- **Should** (PR3-4): 2行プレビュー／git ブランチ／updated-time／アイコン
- **Could** (PR4+): ヘッダーバー（縮退版）／… メニュー／+ ボタン

## L1 — Requirements

### Functional
- **FR-1 SessionStore**: 全ウィンドウ横断のセッションカード状態を保持する中央 `SessionStore` を真実源とし、io スレッドからの差分(delta)をメインスレッドが適用・所有する(クロススレッドロックなし)。
- **FR-2 Card rendering**: 各カードは `●dot [icon] name … 相対updated-time` / `cwd … branch` / 実行中プロセス行(`✳ proc`=busy / `❯ shell`=idle) の3行構成で描画する（2026-07-05 ユーザー裁定: 最終出力2行プレビューをプロセス行に置換。preview 配管は store/io_thread に温存・描画のみ停止）。
- **FR-3 Click-to-switch**: カードクリックで該当セッションの `{window_id, pane_id}` にウィンドウフォーカスを移す(A-flavor、active-swap しない)。
- **FR-4 Toggle + resize**: hotkey/config でサイドバー可視性を**フォーカス中ウィンドウ単位**でトグルし、トグル時に grid-first リサイズ(grid → pty winsize)を**そのウィンドウの全 pane**（quick-terminal ウィンドウは対象外）に適用する。他ウィンドウの可視性・グリッドには影響しない。
- **FR-5 Header bar**: サイドバー上部に実行状態ラベル（フォーカスセッションの実プロセス名 `✳ <proc>`、未検出時 Running/Idle）＋中央タイトル＋右端セッション名ピルを描画する。
- **FR-6 + button**: フォーカス中ウィンドウに新規タブを開き、cwd をアクティブセッションから継承する(既存 new-tab パス再利用)。
- **FR-7 … menu**: カードごとに close アクションを提供し、rename は SessionStore の名前オーバーライドとして保持する。close は既存の close_pane/close_tab teardown パス（confirm ダイアログ・pty 終了・GC choke-point を含む）に委譲する — カードは per-pane（SessionCardId が pane_id を持つ）ため close_pane が正、最終 pane では close_tab へカスケード（Judge 裁定 2026-07-05）。rename の inline 入力 UI は v1 deferral（Open Question 5 参照。store 層 Rename は実装・テスト済 = AC-9）。
- **FR-8 Git branch**: cwd に対する `git -C <cwd> branch --show-current` を throttled に取得し、結果を SessionStore に供給する。
- **FR-9 Icon detection**: cwd のマーカー first-match でプロジェクトアイコンを判定する(`Cargo.toml`→rust, `package.json`→node, `*.tf`→terraform, `go.mod`→go, `pyproject.toml`→python, `.git`のみ→git, なし→folder)。
- **FR-10 Updated-time**: 最終出力時刻を相対表示する("3分前"、24h 超は "昨日 23:47" 形式)。
- **FR-11 Status dots**: busy(OSC 133 `has_running_program`)=青 / idle=緑 / 未読ベル=黄 を状態ドットで表す。未読ベルは `Terminal::take_pending_bell`(terminal.rs:305、BEL 由来)を io スレッドが drain して SessionDelta で送り、該当セッションのウィンドウがフォーカスされた時点でクリアする。
- **FR-12 GC/teardown**: セッション終了時に全5 teardown サイトで SessionStore から該当エントリを除去する。
- **FR-13 Config keys**: `sidebar-enabled`(bool 初期値)・`sidebar-width`(points、既定 360)・`sidebar-hotkey`(トグル用チョード、`quick-terminal-hotkey` の既存パース/ディスパッチパターンを踏襲)を noa-config に追加する。汎用 keybind→action システムは導入しない。
- **FR-14 Quick-terminal exclusion**: quick-terminal ウィンドウはサイドバー対象外とし、inset も掛けない。
- **FR-15 Scroll**: カード数がサイドバーの表示領域を超える場合、縦スクロール（スクロールオフセットのクランプ付き）で全カードに到達できる。グルーピング/折畳は行わない。

### Non-Functional
- **NFR-1 No render-path lock**: 描画パスは Terminal をロックせず、`overview_snapshot` と同型の publish-slot 経由でのみセッション状態を読む。
- **NFR-2 No git on io loop**: git spawn を io 読取ループで実行しない(専用 branch-poll スレッド)。
- **NFR-3 Throttles**: 最終出力プレビューは最小レンダ間隔(~10Hz)で再利用、branch-poll は cwd 毎 ≥1s throttle・非 git は negative cache。
- **NFR-4 Pure layout**: サイドバーのレイアウト/ヒットテスト/スクロール計算は `tab_overview.rs` 同様に純粋・ユニットテスト可能なモジュールに置く。
- **NFR-5 Graceful degradation**: Nerd Font 未導入→tofu/emoji、OSC 133 未導入シェル→busy=false、非 git cwd→ブランチ非表示にデグレードする。
- **NFR-6 Crate boundaries**: wgpu は noa-app/noa-render のみ、winit は noa-app のみ。SessionStore/layout は GUI 非依存に保つ。

## L2 — Detail

- **SessionStore** (`crates/noa-app/src/session_store.rs`, 新規): `SessionCardId{window_id, pane_id}`(既存 `OverviewTileId` を踏襲)を鍵に `SessionCard`(name/cwd/branch/icon/dot/updated/preview-slot 参照)を保持。更新は `enum SessionDelta`(**Upsert / Remove / Branch / Rename / Bell / Process の6種で閉じる** — Process はプロセス名検出の 2026-07-05 昇格時に追加)を既存 `UserEvent` チャネル(`events.rs`)で post、メインスレッドが `apply` する。
- **io-thread publishing** (`crates/noa-app/src/io_thread.rs`): `publish_overview_snapshot` 近傍で `Instant`＋wall-clock を最終出力時にスタンプし SessionDelta::Upsert を送る。同じロック区間で `Terminal::take_pending_bell` を drain し、true なら SessionDelta::Bell を送る（フォーカス時にメインスレッドがクリア）。プレビューは第2スナップショット slot ではなく **SessionDelta::Upsert に同梱**する（実装時 Judge 裁定 2026-07-05: delta 同梱の方が coherence・メモリ・ロック時間で優位。専用 `SidebarPublish` gate＋`decide_sidebar_publish` throttle は `OverviewPublish` の**パターン再利用・インスタンス分離**で実装 — Omen T1）。
- **branch-poll thread** (新規): OSC-7 由来の cwd 変化イベントでトリガ。cwd 毎に `(branch, Instant)` をキャッシュ(≥1s throttle)、非 git は negative-cache。結果は `UserEvent`(SessionDelta::Branch)で post。アイコン判定(FR-9)も同居させ cwd 変化時のみ再判定。
- **sidebar layout** (`crates/noa-app/src/sidebar.rs`, 新規): `tab_overview.rs` を鏡像に、カード矩形の縦積みジオメトリ・スクロールオフセット・`hit_test`(→ SessionCardId)・close/… ボタン矩形を純関数で算出。winit/wgpu 非依存。
- **rendering** (`crates/noa-render/src/blit.rs` 再利用): カード枠は `CardStyle`＋`overlay_texture_cards`。テキスト(name/cwd/branch/preview)は overview 方式の専用小 `Renderer`(1枚を全カード再利用、per-card renderer にしない)。状態ドットは小さな塗りつぶし quad。
- **resize path** (`crates/noa-app/src/app.rs`): トグル時、対象 `WindowState` 群について `pane_bounds_for_size` にサイドバー幅 inset を適用 → `relayout_and_resize_window`(grid → pty winsize)→ `request_redraw` の順(quick-terminal は除外)。
- **config** (`crates/noa-config/src/lib.rs` `StartupConfig`, `parser.rs`): `quick_terminal_hotkey` パターンを踏襲し `sidebar-enabled`/`sidebar-width`/`sidebar-hotkey` を追加。汎用 keybind→action システムは導入しない（parser.rs の keybind は現状 diagnostic のみのため）。
- **header bar** (`sidebar.rs` + rendering): 実行状態は `group_running_program_count` を真偽に縮退。中央タイトルは `WindowState.title`、ピルは SessionCard.name。
- **teardown GC sites** (`crates/noa-app/src/app.rs`): `close_tab`・`close_pane_after_pty_exit`・`close_pane`・ウィンドウ remove・`request_quit` の5箇所で SessionDelta::Remove を送る。

## L3 — Acceptance Criteria

- **AC-1 (FR-1)**: SessionDelta::Upsert→Remove を順に apply すると store サイズが増減し、Remove 後に該当 `SessionCardId` が消えることを unit test で検証。
- **AC-2 (FR-1, NFR-6)**: `session_store.rs` と `sidebar.rs` のソースに `use winit`/`use wgpu`/`winit::`/`wgpu::` が現れないことを、モジュールソースを読む `#[test]`（source-scan テスト）でアサートする。
- **AC-3 (FR-2)**: 与えた SessionCard から生成したカードのテキスト行が `[icon] name`・`cwd … branch`・updated-time を含む(layout の行文字列を unit test でアサート)。
- **AC-4 (FR-3)**: `hit_test(point)` がカード領域内の点に対し正しい `SessionCardId` を返し、領域外で `None`(unit test)。
- **AC-4b (FR-3) [manual]**: カードクリックで対象ウィンドウがフォーカスされ、他ウィンドウの Terminal 内容が変化しない。
- **AC-5 (FR-4)**: トグルでフォーカス中ウィンドウの全 pane grid が pty winsize 送信より先にリサイズされる — 既存 `pane_resize_batch_plan` テスト（`multi_pane_resize_batching_resizes_all_grids_before_pty_winsize_sends`, app/helpers/tests.rs:773）と同型で、サイドバー inset 適用時の順序をアサート。
- **AC-6 (FR-4) [manual]**: トグルでサイドバー幅ぶんターミナル描画領域が狭まり、シェルが縮小後の列数にリフローする（`tput cols` で減少を確認）。
- **AC-7 (FR-5) [manual]**: ヘッダーに `● Running`/`Idle`・中央タイトル・右端セッション名ピルが表示される。
- **AC-8 (FR-6) [manual]**: + ボタンでフォーカス中ウィンドウに新規タブが開き、cwd がアクティブセッションを継承する。
- **AC-9 (FR-7)**: rename アクション適用後、SessionCard.name がオーバーライド値になり、以降の Upsert で上書きされない(unit test)。
- **AC-9b (FR-7) [manual]**: … メニューの close で該当セッションが終了しカードが消える。
- **AC-10 (FR-8, NFR-3)**: 純関数 `decide_branch_poll(now, last_poll, cache)` が <1s で Skip、≥1s で Spawn、negative-cache 済み非 git cwd で Hit を返すことを、明示的な `now: Instant` 値でアサート（`decide_overview_publish` の now-as-param パターンを踏襲、wall-clock sleep なし）。
- **AC-11 (FR-9)**: マーカーファイル集合を入力にアイコン判定関数が first-match 表通りに返す(表駆動 unit test)。
- **AC-12 (FR-10)**: updated-time フォーマッタが 3分前 / 昨日 23:47 / 同日時刻の各境界で正しい文字列を返す(unit test)。
- **AC-13 (FR-11)**: `has_running_program`=true→青、false→緑、未読ベル→黄 のドット色マッピングを unit test で検証。
- **AC-14 (FR-12)**: 純関数 `reconcile_sessions(&mut store, live_ids)` が live_ids に無いエントリを除去し store サイズ == live_ids 数となることを unit test で検証。5 teardown サイトが各々これを呼ぶことは実装レビュー＋[manual] 統合確認（`App` はユニットテストから構築不能のため）。
- **AC-15a (FR-13)**: `sidebar-enabled`/`sidebar-width` がパースされ、既定値(width=360)が適用される(parser unit test、`parse_bool`/`quick-terminal-size` パターン)。
- **AC-15b (FR-13)**: `sidebar-hotkey` チョードが `quick-terminal-hotkey` と同じパース経路で受理される(parser unit test)。不正チョードの diagnostic はアプリ層登録時の `parse_hotkey` で発生する — noa-config は noa-app に依存できないため parse 時ではない（quick-terminal-hotkey と同じ前例。Judge 裁定 2026-07-05）。
- **AC-16a (FR-14)**: quick-terminal ウィンドウの `pane_bounds_for_size` にサイドバー inset が適用されない(純関数 unit test)。
- **AC-16b (FR-14)**: サイドバー対象判定述語が quick-terminal ウィンドウに false を返し、store に登録されない(純関数 unit test)。
- **AC-17 (NFR-1)**: `sidebar.rs`＋sidebar 描画経路のソースに `terminal.lock()` が現れないことを source-scan `#[test]` でアサート。プレビューは slot 読取のみ。
- **AC-18 (NFR-2)**: `io_thread.rs` の読取ループ(`feed_terminal`)ソースに `Command::new("git")` 等の git spawn が現れないことを source-scan `#[test]` でアサート（branch-poll の別スレッド性はコードレビューで確認）。
- **AC-19 (NFR-3)**: sidebar 可視時のみプレビュー slot が publish され、最小レンダ間隔で throttle される(既存 throttle テストパターンを sidebar gate に拡張)。
- **AC-20 (NFR-4, FR-15)**: `sidebar.rs` の `card_layout`・`hit_test`・スクロールクランプの各関数に window/GPU 非依存の unit test が存在し、スクロールはカード数超過時のオフセット上限/下限クランプをアサートする。
- **AC-21 (NFR-5) [manual]**: Nerd Font 未導入で tofu、OSC 133 未導入シェルで全ドット緑(idle)、非 git cwd でブランチ欄が空になる。
- **AC-22 (NFR-6)**: AC-2 の source-scan テストが SessionStore・sidebar layout を対象に含むことで充足（`cargo tree` はモジュール境界を見えないため使わない。noa-config は crate 依存として `cargo tree` で wgpu/winit 非依存を確認可）。
- **AC-23 (FR-15)**: ビューポート高を超えるカード数でスクロールオフセットを増減させると、先頭/末尾カードがそれぞれ到達可能で、オフセットが [0, max] にクランプされる(unit test)。

## Scope

### In-scope (v1)
- 中央 SessionStore(channel-delta 型、メインスレッド所有)と全5 teardown サイトの GC
- トグル式常設左サイドバー＋ grid-first リサイズ(per-window 可視性)
- カード全要素: アイコン・名前・cwd・状態ドット・ブランチ・2行プレビュー・相対 updated-time
- クリック切替(ウィンドウフォーカス、A-flavor)
- ヘッダーバー(`Running`/`Idle` 縮退ラベル＋タイトル＋セッション名ピル)
- + ボタン(cwd 継承 new-tab)・… メニュー(close/rename)
- git ブランチ検出(専用 branch-poll スレッド、cache/negative-cache)・プロジェクトアイコン検出
- config キー(`sidebar-enabled`/`sidebar-width`/`sidebar-hotkey`)
- 縦スクロール（オフセットクランプ、FR-15）
- quick-terminal ウィンドウの対象外化

### Out-of-scope (deferred / rejected)
- active-swap ウィンドウモデル(B-flavor)— 切替ポリシーの継ぎ目に温存、v2
- pluggable 切替ポリシーのユーザー向け設定化 — 内部シーム保持のみ
- repo-root グルーピング / 折畳 / バッジ / 20件超のスケール対応 — Open Question
- （2026-07-05 解消: フォアグラウンドプロセス名検出は実装済 → FR-2/FR-5 参照）
- auto-hide、ネイティブタブとの別モード切替(可視性トグルに縮退済)
- soft-wrap reflow(inc≥3 の既存範囲)

## Considered but rejected
(未着手)

## Open Questions / Deferred Decisions
1. ~~フォアグラウンドプロセス名検出~~ — **2026-07-05 実装済**（tcgetpgrp＋libproc、session-metadata ワーカー。カード/ヘッダー双方で使用）。残: 最終出力プレビューの再有効化オプション（配管は温存、描画のみ停止）。
2. **20件超のスケール対応** — repo-root/cwd グルーピング・折畳・未読バッジ（Slack 型 UI の生存条件、Flux 警告③）。v1 はスクロールのみ。
3. **B-flavor active-swap** — 1ウィンドウ多重 Terminal での切替。切替ポリシーの内部シームに温存。
4. **+ ボタンの新規ウィンドウ生成バリアント** — v1 は現ウィンドウ新規タブのみ。
5. **… メニューの追加アクション**（複製・cwd コピー等）— v1 は close のみ。**rename の inline テキスト入力 UI も deferred**（サイドバーにテキスト入力サーフェスが無いため。store 層の Rename オーバーライドは実装・テスト済 AC-9。ヘッダー … は v1 no-op、クリックは consume）。
6. **updated-time の絶対表示オプション** — v1 は相対表示固定。
7. **AC-14 の統合テスト化** — `App` がユニットテスト不能なため、teardown サイト呼び出しの機械検証はハーネス導入待ち（現状は実装レビュー＋manual）。

## Quality Gate 結果（2026-07-05）
- 初回: Judge = REQUEST CHANGES（Ambiguity/Completeness/Consistency/Scope FAIL、HIGH 3件）／Attest = NOT-VERIFIABLE 3件・要セットアップ4件・分割2件。
- 修正: HIGH-1 リサイズ対象をフォーカス中ウィンドウに統一（FR-4/L2/CHALLENGE）。HIGH-2 FR-15 スクロール追加＋AC-23。HIGH-3 bell 供給経路を L2 に定義（`take_pending_bell` drain → SessionDelta::Bell、フォーカスでクリア）。SessionDelta を5種で閉包。FR-7 close 意味論定義。FR-13 を `sidebar-hotkey`（quick-terminal パターン）に変更し汎用 keybind 非導入を明記。Attest 指摘の AC-2/5/6/10/14/15/16/17/18/20/22 を書き換え（source-scan テスト・now-as-param・純関数化・分割）。
- 全 FAIL 所見は上記修正で解消、残余（AC-14 の機械検証限界）は Open Question 7 に降格記録。
