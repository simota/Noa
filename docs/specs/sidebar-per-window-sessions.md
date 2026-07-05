# Spec: Sidebar per-window session display

- **slug:** sidebar-per-window-sessions
- **status:** locked (2026-07-05)
- **owner:** simota
- **build-path decision:** apex（単発自律ラン）— fallback: feature / 手動実装
- **quality gate:** PASS（Judge 所見 S1〜S5 全反映済み）

## L0 — Vision

- **問題:** 複数ウィンドウ利用時、各ウィンドウのサイドバーに全ウィンドウのセッションが混在表示され、「このウィンドウのセッション」を把握できない。
- **対象:** noa をマルチウィンドウで使うユーザー（= 開発者本人）。
- **Job-to-be-done:** サイドバーを見れば、いま操作しているウィンドウに属するセッションだけが並び、そのウィンドウの状態（busy / attention）が一目で分かる。
- **成功definition:** 各ウィンドウのサイドバーが自ウィンドウのセッションのみを表示・カウント・ヒットテストし、既存の GC / delta / スクロール挙動が退行しない。

## Reuse / constraint findings (Lens scan, 2026-07-05)

- データモデルは既に per-window キー付け済み: `SessionCardId { window_id: SessionWindowId, pane_id }` (`crates/noa-app/src/session_store.rs:55-65`)。
- 全ウィンドウ横断表示の原因は描画側のみ: `sidebar_draw_model` が `session_store.ordered_ids()`（全カード）を無フィルタ使用 (`crates/noa-app/src/app/sidebar.rs:772`)。
- 同じ `ordered_ids()` を使う変更対象: hit_test (`sidebar.rs:532`)、card_rect 逆引き (`sidebar.rs:693`)、draw model (`sidebar.rs:765,772`)。三者は同一リストを共有する不変条件あり。
- ヘッダーカウントも全体集計: `attention_count()` (`sidebar.rs:797`)、`busy_count()` (`sidebar.rs:813`)。
- 変更不要と判明: GC (`reconcile_session_store`, 全ウィンドウ横断で正しい)、スクロール (`WindowState.sidebar_scroll` 既に per-window)、delta 経路 (`io_thread.rs:457` が window_id 埋め込み済み)、`clear_session_bell_for_window` (既にフィルタ済み)。
- 制約 (NFR): `session_store.rs` は GUI 非依存（winit/wgpu import 禁止、自己スキャンテストあり）。フィルタ引数は `SessionWindowId` で受けること。
- `SessionStore` は main スレッド単独所有・ロック無し。ウィンドウ毎分割よりフィルタメソッド追加が最小侵襲。
- 関連: Overview (`app/overview.rs`) は同じ store を全ウィンドウ横断で参照。既存仕様 `docs/specs/session-sidebar.md`。

## FRAME 確定事項 (user confirmed)

1. Problem Statement: 上記 L0 で確定。
2. **Overview は全ウィンドウ表示を維持**（サイドバー=自ウィンドウ、Overview=全体俯瞰という役割分担。可視集合のずれは意図的設計）。
3. **attention は完全 per-window**（自ウィンドウ分のみ表示・カウント。他ウィンドウの attention はそのウィンドウで気づく）。
4. **sidebar_visible トグルは現状維持（アプリ全体一括）**。per-window トグル化は Open Question として記録。

## EXPAND — 候補と選択

- **A: SessionStore にフィルタメソッド追加**（`ordered_ids_for_window` / `busy_count_for_window` / `attention_count_for_window`）+ sidebar.rs 呼び出し側5箇所差し替え — **採用（user pick）**。最小侵襲、フィルタロジック1箇所集約、ユニットテスト可能。
- B: Store を per-window 分割（`HashMap<SessionWindowId, SessionStore>`）— **却下**: GC・delta・Overview 全経路に改修が波及、Overview 全体表示維持との相性も悪い。
- C: View 層フィルタ（App 側で retain）— **却下**: 3箇所にフィルタ重複、hit_test/draw の同一リスト不変条件が壊れやすい、テスト不能。

## CHALLENGE — ストレステスト結果と確定方向 A′ (user confirmed)

当初案 A への修正3点（Void/Ripple/Omen パネル、コード実査に基づく）:

1. **フィルタ粒度は `WindowGroupId`（論理ウィンドウ）**。macOS ネイティブタブは同一論理ウィンドウでも winit 上は別 `WindowId`（`app.rs:125-151`）。winit WindowId 単位だと兄弟タブが表示されず1枚リストに退化する。App 側でフォーカスウィンドウの group に属する winit WindowId 群 → `HashSet<SessionWindowId>` を算出して store に渡す（前例: `group_running_program_count` `app.rs:1534-1541`）。
2. **呼び出し箇所は6箇所**。当初5箇所に加え `handle_sidebar_wheel` の `session_store.len()`（`sidebar.rs:719`）— 放置するとスクロール可能域が実カード数とドリフト。
3. **追加メソッドは `ordered_ids_for_windows(&HashSet<SessionWindowId>) -> Vec<SessionCardId>` の1本のみ**。busy/attention カウントは `sidebar_draw_model` 内でフィルタ済み `ids` からインライン集計（専用カウントメソッド2本は YAGNI）。wheel 用件数はフィルタ結果の `.len()`。

追加所見:
- Overview は `get(&card_id)` のみで `ordered_ids` 非使用（`overview.rs:484,1272`）→「Overview グローバル維持」は変更ゼロで自動成立。
- `selected_id` は常に自 group 内（`sidebar.rs:781`）→ ダングリングなし。
- attention blink/float は per-card 状態で駆動 → グローバル依存なし。他ウィンドウ attention 非表示はスコープ決定どおりの意図的損失。
- 既存テスト非破壊（`ordered_ids`/`busy_count`/`attention_count` 本体は残置）。

呼び出し箇所 完全リスト（全て同一フィルタ集合で一貫させる不変条件）:
| # | 箇所 | 位置 |
|---|------|------|
| 1 | hit_test | `sidebar.rs:532` |
| 2 | card_menu_anchor 逆引き | `sidebar.rs:693` |
| 3 | scroll content_height の件数 | `sidebar.rs:719` |
| 4 | draw model | `sidebar.rs:765`（→766 layout へ流入） |
| 5 | attention カウント（draw 内集計へ） | `sidebar.rs:790-797` |
| 6 | busy カウント（draw 内集計へ） | `sidebar.rs:806-813` |

## SHAPE — 提案 (user confirmed)

- **Problem:** 複数ウィンドウ利用時、サイドバーに全ウィンドウのセッションが混在。
- **Solution:** サイドバーの表示・ヒットテスト・スクロール・ヘッダカウントを自論理ウィンドウ（`WindowGroupId`、ネイティブタブ含む）のセッションに絞る。`SessionStore::ordered_ids_for_windows(&HashSet<SessionWindowId>)` 1本追加 + App 側 group→SessionWindowId 集合ヘルパー + `sidebar.rs` 6箇所差し替え。
- **In-scope:** カード一覧 / hit_test / scroll 域 / busy・attention カウントの group 絞り込み、App 側 group→window 集合ヘルパー（導出ロジックは純関数化）、ユニットテスト。
- **Out-of-scope:** Overview（グローバル維持・変更ゼロ）、`sidebar_visible` per-window 化、他ウィンドウ attention インジケータ、GC/delta/スクロール状態の構造変更。
- **Assumptions:** 「ウィンドウ」= `WindowGroupId`（AppKit 論理ウィンドウ）。空フィルタ結果はヘッダのみ描画で許容（既存空ストア挙動と同等）。対象 window_id が `windows` マップに不在・group 解決不能の場合は空集合として扱う（= ヘッダのみ描画に縮退）。

## L1 — Requirements

| ID | 種別 | 要件 |
|----|------|------|
| R1 | 機能 | サイドバーは、描画対象ウィンドウの `WindowGroupId` に属するセッションカードのみを表示する。 |
| R2 | 機能 | 同一論理ウィンドウ内のネイティブタブ（別 winit `WindowId`）のセッションは、兄弟タブのサイドバーにも表示される。 |
| R3 | 機能 | hit_test・カードメニューアンカー逆引き・draw model は同一のフィルタ済みリストを使用する（リスト不変条件の維持）。 |
| R4 | 機能 | サイドバーのスクロール可能域（content_height）はフィルタ後のカード件数から算出する。 |
| R5 | 機能 | ヘッダの busy / attention カウントは自 group のセッションのみを集計する。 |
| R6 | 機能 | フィルタ後もカード順序は既存ソート（attention float → window_id → pane_id）を維持する。 |
| R7 | 非機能 | `session_store.rs` は GUI 非依存を維持する（winit/wgpu import 禁止、フィルタ引数は `SessionWindowId` ベース）。 |
| R8 | 非機能 | GC（reconcile）・delta 経路・per-window スクロール状態・Overview の挙動に変更を加えない。既存テストは無修正で通過する。 |

## L2 — Detail

- **SessionStore API**（`crates/noa-app/src/session_store.rs`）:
  ```rust
  /// ordered_ids と同一ソート順で、windows に含まれる window_id のカードのみ返す。
  pub fn ordered_ids_for_windows(
      &self,
      windows: &HashSet<SessionWindowId>,
  ) -> Vec<SessionCardId>

  /// windows に属するカードの (busy, attention) 件数。
  pub fn counts_for_windows(
      &self,
      windows: &HashSet<SessionWindowId>,
  ) -> (usize, usize)
  ```
  既存 `ordered_ids` / `busy_count` / `attention_count` は残置（既存テスト保全）。`HashSet` は `std::collections`（session_store.rs で既使用、GUI 依存なし）。
- **group→window 集合の導出（純関数）:** `(SessionWindowId, WindowGroupId)` ペア列と対象 group を受け、同 group の `HashSet<SessionWindowId>` を返す純関数として切り出す（winit 非依存でユニットテスト可能）。App 側の薄いラッパーが `windows` マップ + `window_order` からペア列を組み立てて呼ぶ。前例: `group_running_program_count`（`app.rs:1534-1541`）。
- **呼び出し側差し替え（6箇所、全て同一集合由来）:**
  1. hit_test（`sidebar.rs:532`）
  2. card_menu_anchor 逆引き（`sidebar.rs:693`）
  3. `handle_sidebar_wheel` の件数（`sidebar.rs:719`、`session_store.len()` → フィルタ後 `.len()`）
  4. `sidebar_draw_model`（`sidebar.rs:765`、→766 layout へ流入）
  5. attention カウント → `counts_for_windows` へ（`:790-797`）
  6. busy カウント → 同上（`:806-813`）
- **空フィルタ結果**: ヘッダのみ描画（既存の空ストア挙動と同一パス）。

## L3 — Acceptance Criteria

| ID | 検証する要件 | 基準 | 検証方法 |
|----|--------------|------|----------|
| AC-1 | R1 | 2つの group A/B にカードがあるとき、`ordered_ids_for_windows({A の window 群})` は B のカードを一切含まない。 | unit test |
| AC-2 | R2 | 同一 group 内の複数 `SessionWindowId`（ネイティブタブ相当）のカードが全て結果に含まれる。 | unit test |
| AC-3 | R6 | フィルタ結果の順序が `ordered_ids` の相対順（attention float → window_id → pane_id）と一致する。 | unit test |
| AC-4 | R3 | `sidebar.rs` 内で `self.session_store.ordered_ids()` と `self.session_store.len()` の grep が 0 件になり、6箇所全てがフィルタ済み集合由来になる（テスト内ローカル `store.len()` 等は対象外）。 | grep + code review |
| AC-5 | R4 | wheel ハンドラの content_height 算出がフィルタ後件数を使い、スクロールクランプが表示カード数と一致する。 | code review + 手動確認 |
| AC-6 | R5 | 他 group にのみ busy/attention セッションがある場合、`counts_for_windows` が (0, 0) を返し、自ウィンドウのヘッダカウントが 0 表示になる。 | unit test + 手動確認 |
| AC-7 | R7 | `session_store.rs` の GUI 非依存自己スキャンテストが通過する。 | `cargo test -p noa-app` |
| AC-8 | R8 | `cargo test --workspace` が全通過し、既存 `session_store.rs` テストは無修正のまま通る。`cargo clippy --workspace` クリーン。 | CI 相当コマンド |
| AC-9 | R1/R2 | 実機 GUI: 2 論理ウィンドウ + 片方に cmd+t タブを作成し、各サイドバーが自 group のセッションのみ表示・兄弟タブ分は表示されることを目視確認。 | 手動 GUI 確認 |
| AC-10 | R2 | group→window 集合導出の純関数が、混在ペア列（複数 group、複数タブ）から対象 group の `SessionWindowId` のみを過不足なく返す。 | unit test |

## Open Questions / Deferred Decisions

- sidebar_visible の per-window 化（今回スコープ外、将来検討）。
