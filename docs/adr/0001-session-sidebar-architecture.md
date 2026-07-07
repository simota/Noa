---
# CI fitness-function projection (derived; the narrative below is the primary artifact)
status: Accepted
date: 2026-07-05
constraints:
  - session_store.rs and sidebar.rs are GUI-agnostic (no winit/wgpu import)
  - render path never calls terminal.lock()
  - git spawn never runs on the io read loop (feed_terminal)
affected:
  - crates/noa-app/src/session_store.rs   # new
  - crates/noa-app/src/sidebar.rs         # new
  - crates/noa-app/src/io_thread.rs
  - crates/noa-app/src/events.rs
  - crates/noa-app/src/app.rs
  - crates/noa-render/src/blit.rs
  - crates/noa-config/src/{lib.rs,parser.rs}
tests:
  - AC-2  (source-scan: no winit/wgpu in session_store.rs + sidebar.rs)
  - AC-17 (source-scan: no terminal.lock() in sidebar render path)
  - AC-18 (source-scan: no git spawn in io read loop)
---

# ADR 0001 — Session Sidebar Architecture

Status: Accepted (2026-07-05) · Owner: simota · Spec: `docs/specs/session-sidebar.md` (locked)

これは軽量 ADR である。決定は spec 対話（EXPAND → CHALLENGE → SHAPE）で確定済みで、本書はその決定と代替案・帰結を監査可能な形で固定する。実装詳細は spec の L2/L3 が保持する（本 ADR には転記しない — Mega-ADR 回避）。

## Context

現行 noa のタブは macOS ネイティブタブで、複数プロジェクト並行時に「どのセッションで何が起きているか」（cwd・ブランチ・最終出力・実行状態）を常時把握し即切替する手段がない。常設左サイドバー（セッションカードの縦積み）を追加したい。

作用している力:

- **鮮度更新のコスト**: git ブランチ・最終出力プレビューをカード毎に持つと、更新頻度がウィンドウ数 × セッション数で爆発しうる（Flux 警告②）。
- **描画パスのロック規律**: レンダラは `Terminal` をロックしない（NFR-1）。既存の `overview_snapshot` publish-slot が唯一の合法な読取経路。
- **テキストプリミティブ不在**: サイドバー文字は全てターミナルセル描画に帰着する（"text-is-cells"）。カード1枚 ≒ 6テキストラン、20件超は未対応（Open Question 2）。
- **crate 境界**: wgpu は noa-app/noa-render のみ、winit は noa-app のみ。レイアウトロジックは `tab_overview.rs` 同様に GUI 非依存・ユニットテスト可能に保つ（NFR-6）。
- **リサイズ規律**: サイドバー幅の増減は grid-first リサイズ（grid → pty winsize）必須。

## Decision

**中央 `SessionStore`（channel-delta 型・メインスレッド所有）を真実源とし、各ウィンドウが read-only に描くトグル式常設左サイドバー**（EXPAND 生存候補 C = Shared Registry）を採る。Magi 3-0 承認の 4 裁定でウィンドウモデルと同期方式を確定する。

1. **ウィンドウモデル = A-flavor**: 1タブ = 1 `WindowState` を維持。切替 = ウィンドウフォーカス。B-flavor（1ウィンドウ多重 Terminal の active-swap）は切替ポリシーの継ぎ目の裏に温存し、store には触れず v2 で差し替え可能にする。
2. **SessionStore = channel-delta 型**: io スレッドが差分を送信、メインスレッドが store を所有。クロススレッドロック無し。既存 `UserEvent` poke パターン（`events.rs`）を踏襲。
3. **描画 = per-window**: 各ウィンドウが同じ store を read-only に描く。「メインウィンドウ」という特権概念は作らない。
4. **可視性 = per-window トグル**: config `sidebar-enabled` がアプリ全体の初期値、hotkey はフォーカス中ウィンドウのみ反転。カードの最終出力プレビュー行数は `sidebar-preview-lines`（既定 3）で制御する。「モード切替（sidebar↔native）」はこのトグルに縮退（A-flavor ではネイティブタブが無傷 = 非表示が従来モード）。

**SessionDelta は 5 種で閉じた enum**（`Upsert / Remove / Branch / Rename / Bell`）とする。閉包により store の `apply` が総当たり網羅でき、将来の状態追加はこの enum の拡張として明示化される（make-the-implicit-explicit）。

**git 取得は専用 branch-poll スレッド**が担う。OSC-7 由来の cwd 変化でトリガ、cwd 毎に `(branch, Instant)` をキャッシュ（≥1s throttle）、非 git は negative-cache、結果は `UserEvent`（`SessionDelta::Branch`）で post。アイコン判定（cwd マーカー first-match）も cwd 変化時のみ同居再判定。io 読取ループ（`feed_terminal`）では git を一切 spawn しない（Ripple 拘束条件 a）。

## Considered alternatives（EXPAND 由来 / spec "Considered but rejected"）

- **A — Window Aggregator 単独**: store なしでは git/snapshot 更新が N × ウィンドウ数に爆発（Flux 警告②）。→ 却下（C の採択理由そのもの）。
- **B — Session Host（1ウィンドウ多重 Terminal・active-swap）**: app.rs のウィンドウ=タブ前提を大改修、最大工数。→ 却下。継ぎ目に温存し将来到達可能（裁定 1）。
- **D — Overlay Projection（Tab Overview 縦積み最小改修）**: 早期頭打ち、フル要素追加で二度手間。→ 却下。
- **E — Attention Switcher（召喚オーバーレイ＋attention シグナル）**: 常時ダッシュボードという JTBD に不適合。→ 却下。

## Consequences

**Positive**

- 鮮度更新が「セッション数 N 回」で済み、ウィンドウ数に対してスケールしない（Flux 警告②の解消）。
- クロススレッドロック無し（channel-delta）で io/main の既存二スレッド規律を崩さない。
- A-flavor 維持により既存の close_tab / new-tab / focus パスを再利用でき、破壊的変更が小さい（Ripple: リスク 6.5/10、新規2ファイル・700-1100 LOC）。
- SessionDelta の閉包と純関数レイアウトにより主要ロジックが window/GPU 非依存でユニットテスト可能。

**Negative**

- **二つ目の共有状態面**: `Terminal` の overview_snapshot に加え SessionStore という第二の publish-slot 面が増える。GC を全5 teardown サイト（close_pane / close_pane_after_pty_exit / close_tab / window remove / Quit）に併置しないとリークする（拘束条件 c）。
- **text-is-cells 税**: カード1枚 ≒ 6テキストラン。20件超のスケールは v1 未対応（Open Question 2）。
- **per-window 描画コスト**: 各ウィンドウが独立にサイドバーを描くため、多ウィンドウ時に同一 store を複数回レンダする冗長がある（特権メインウィンドウを作らない裁定 3 の代償）。

## Compliance hooks（fitness functions）

spec の source-scan AC を CI 拘束条件として固定する。各非推奨化まで維持され、AI 生成コードの境界侵犯（AI-Accelerated Drift）を機械検出する:

- **AC-2 / AC-22 (NFR-6)**: `session_store.rs` と `sidebar.rs` のソースに `use winit` / `use wgpu` / `winit::` / `wgpu::` が現れないことを `#[test]` でアサート。noa-config の wgpu/winit 非依存は `cargo tree` で確認（モジュール境界は cargo tree では見えないため source-scan と併用）。
- **AC-17 (NFR-1)**: `sidebar.rs`＋サイドバー描画経路に `terminal.lock()` が現れないことを source-scan `#[test]` でアサート（プレビューは slot 読取のみ）。
- **AC-18 (NFR-2)**: `io_thread.rs` の読取ループ（`feed_terminal`）に `Command::new("git")` 等が現れないことを source-scan `#[test]` でアサート。
- 既存ゲート継続: `cargo clippy --workspace` クリーン、`cargo test --workspace`。
- 純関数化された AC-4/5/10/11/12/13/14/16/20/23（hit_test・resize batch 順序・decide_branch_poll の now-as-param・reconcile_sessions・スクロールクランプ等）が window/GPU 非依存テストとして存在すること。

## モジュール依存方向の検証

依存方向は合法を保つ。新規 `session_store.rs` と `sidebar.rs` は noa-app 内に置くが、いずれも純ロジック（store の apply / delta 適用、カード矩形ジオメトリ・hit_test・スクロールクランプ）であり `tab_overview.rs` と同格で winit/wgpu を import しない — DAG に新たな下向き依存は生じない。wgpu 使用は既存どおり noa-app（app.rs / blit 呼出）と noa-render（`blit.rs` の `CardStyle`/`overlay_texture_cards` 再利用）に限定、winit は noa-app のみ。noa-config への追加キー（`sidebar-enabled`/`sidebar-width`/`sidebar-hotkey`/`sidebar-preview-lines`）は `StartupConfig`＋parser の既存パターン踏襲で、noa-config は wgpu/winit を引き込まない（確認済 = Cargo.toml に依存なし）。したがって「noa-grid 以下は GUI 非依存」「wgpu は noa-app/noa-render のみ、winit は noa-app のみ」の規則は本 ADR で不変。
