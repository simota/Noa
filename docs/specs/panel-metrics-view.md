# panel-metrics-view

- slug: panel-metrics-view
- title: ペイン毎の実行プロセス/CPU/メモリ一覧ビュー(プロセスモニタ)
- status: locked (2026-07-11)
- owner: simota
- build-path: apex(単一有人ラン。fallback: feature / orbit)

## L0 — Vision

noa はタブ/分割ペインごとに独立した pty セッションを持つが、各ペインで「今何が動いていて、どれだけリソースを食っているか」を横断的に見る手段がない。エージェント(Claude Code/codex 等)を多数並走させる使い方では、暴走・高負荷セッションの特定や整理判断のために、ペイン毎の実行プロセス名・CPU使用率・メモリ使用量を一覧できるビューが必要。

- 対象: noa 上でエージェント/ビルドを多数並走させるユーザー(=作者自身)
- Job: 高負荷/暴走ペインを数秒で特定し、対処(kill/整理)の判断材料を得る
- 成功: 全ライブペインの process/CPU/mem が 1s 鮮度で一覧でき、高負荷ペインを数秒で特定できる
- 前提: macOS ファースト(非macOSは全列 "—" の劣化表示)
- 計測対象: **フォアグラウンドのプロセスツリー**(フォアグラウンドプロセスグループ + その子孫)— ratified

## 再利用/制約(Lens スキャン結果)

### 再利用可能資産
- `ForegroundProcessProbe` (noa-pty/src/pty.rs:213) — dup した fd 保持・Send・既にバックグラウンドでポーリング。`poll_metrics()` 拡張の起点
- `branch_poll.rs` メタデータワーカー — 1–4s 適応ポーリング、`SessionCardId` 毎の probe map。metrics tick の追加先
- `SessionStore` + `SessionDelta` (session_store.rs) — `SessionDelta::Metrics` を追加し `Process` (:582) をミラー
- `SessionCard` (session_store.rs:130) — `process` の隣に metrics フィールド追加
- サイドバー行モデル/レンダ (sidebar.rs:138, app/sidebar/render.rs)
- macOS syscall テンプレ: tcgetpgrp + proc_name + sysctl(KERN_PROCARGS2) (pty.rs:270-324)

### 制約
- 収集は io read loop・UI スレッド禁止 → branch_poll ワーカーで実施
- winit/wgpu は noa-app(+noa-render)のみ。オーバーレイは wgpu スナップショット方式
- CPU% は2回サンプリングの差分が必要 → 表示中は固定1sスケジュールで解決
- `sysinfo` 依存なし(現行は libc 手書き流儀)。`kinfo_proc` は libc crate に未定義のため sysctl(KERN_PROC_ALL) は不採用
- 純ロジック/GUI 分離が repo 規約(pure model + snapshot)

## Assumption Ledger
- ASSUME-1 (ratified): CPU% の分母は「1コア=100%」(Activity Monitor/top 方式、マルチスレッドは100%超え)
- ASSUME-2 (ratified): メモリは物理フットプリント(`ri_phys_footprint`)のツリー合算
- ASSUME-3 (ratified, 改訂): 非macOSでは全列 "—"(既存 probe がプロセス名も返さないため。行自体は表示)
- ASSUME-4 (resolved): 起動キーバインドは v1 スコープ外(コマンドパレット項目のみ)。keybind config 基盤が整い次第の将来対応
- ASSUME-5 (ratified): 行の絞り込み検索は v1 不要(ペイン数 ≤ 数十の想定)

## 方向の確定(CHALLENGE)

- **採用: B — 専用オーバーレイ「プロセスモニタ」**(ソート可能な表形式モーダル + ペインへジャンプ)— ratified
- 考慮したが不採用:
  - A サイドバー拡張 — カード過密・ソート不可・サイドバー非表示時に見えない(データ層は共通なので将来増築可)
  - C Tab Overview 統合 — タイルは視覚確認向きで数値比較・ソート不向き
  - D ハイブリッド(B+軽量A警告) — v1 スコープ過大。B のデータ層の上に後日増築可能

## SHAPE — 提案(ratified)

- 名称(仮): **プロセスモニタ** オーバーレイ
- コマンドパレット/テーマ設定と同型の wgpu モーダル。全ウィンドウ・全タブの全ライブペインを1行=1ペインで表形式一覧
- 列: プロセス名 / CPU% / メモリ / プロセス数 / 経過時間 / 所属(タブ名・ペイン位置)。値はフォアグラウンドプロセスツリー合算
- 操作: ↑↓選択・Enterジャンプ・Esc閉じる。ソート列サイクルキーあり。破壊操作なし
- 収集: 表示中のみ・固定1s・branch_poll ワーカー

## L1 — Requirements

機能要件:
- **FR-1** コマンドパレット項目からプロセスモニタを開閉できる(専用キーバインドはスコープ外)
- **FR-2** 全ウィンドウ・全タブの全ライブペインを1行=1ペインで一覧表示する
- **FR-3** 各行に プロセス名 / CPU% / メモリ / プロセス数 / 経過時間 / 所属(タブ名・ペイン位置)を表示する
- **FR-4** CPU/メモリ/プロセス数はフォアグラウンドプロセスツリー(フォアグラウンド pgid に属す全プロセス ∪ その子孫)の合算値とする。CPU% は 1コア=100% 基準
- **FR-5** デフォルトは CPU 降順ソート。ソートキーで CPU(降順)→ メモリ(降順)→ プロセス名(昇順)をサイクルできる
- **FR-6** ↑↓ で行選択、Enter で該当ペインへジャンプ(ウィンドウ・タブ・ペインをフォーカスしオーバーレイを閉じる)、Esc で閉じる
- **FR-7** metrics 収集(プロセス一覧列挙 + rusage 系呼び出し)はオーバーレイ表示中のみ・固定1s間隔で行う。非表示中はこれらを一切発行しない(既存のプロセス名適応ポーリングは従来どおり継続)
- **FR-8** 値が取得できない場合(非macOS / プロセス消滅 / 初回サンプル前のCPU%)は "—" を表示し、クラッシュ・行欠落しない。非macOSは全列 "—"

非機能要件:
- **NFR-1** 収集は branch_poll ワーカースレッドで実施(io read loop・UIスレッド禁止)
- **NFR-2** プロセス一覧の列挙は 1 tick あたり1回とし、全ペインで結果を共有する(設計目標: ペイン数30規模で 1 tick 50ms 未満 — 参考値、ACでは列挙1回のみを検証)
- **NFR-3** ソート・書式・選択・行構築の純ロジックは GUI 非依存モジュールに置き単体テスト可能とする
- **NFR-4** 新規の重量依存(sysinfo 等)を追加しない。libc 直呼びの既存流儀に従う
- **NFR-5** winit/wgpu への依存は noa-app / noa-render に閉じる(既存規約)

## L2 — Detail

### 収集層(noa-pty + branch_poll)
- `ForegroundProcessProbe` を拡張し `poll_metrics(&mut self, snapshot: &ProcSnapshot) -> Option<PaneMetrics>` を追加
- **プロセススナップショット**(tick 冒頭に1回、全ペイン共有): `proc_listallpids` で全 pid を列挙し、各 pid の `proc_pidinfo(PROC_PIDTBSDINFO)`(`proc_bsdinfo`: `pbi_ppid` / `pbi_pgid` / `pbi_start_tvsec`)で ppid・pgid・開始時刻を得る(いずれも libc crate 定義済み)
- **ツリー構築**: `tcgetpgrp(fd)` のフォアグラウンド pgid に対し、`pbi_pgid == pgid` の全プロセス ∪ それらの子孫(ppid 走査)。中間親の exit で launchd に reparent された孫や、pgid リーダー死亡後の生存メンバーも取りこぼさない
- **各 pid の計測**: `proc_pid_rusage(RUSAGE_INFO_V4)` で `ri_user_time + ri_system_time`(CPU時間, mach ticks→ns 換算)と `ri_phys_footprint`(メモリ)を取得
- CPU% = ツリー合算 CPU 時間の前回 tick との差分 ÷ 実経過時間(1コア=100%)。初回サンプルは "—"。tick 間に消滅した pid は 0 扱いで合算続行
- 経過時間 = pgid リーダーの `pbi_start_tvsec` 起点。リーダー死亡時はグループ内最古プロセスの開始時刻にフォールバック、それも無ければ "—"
- `PaneMetrics { cpu_permille: Option<u32>, mem_bytes: u64, proc_count: u32, started_at: Option<SystemTime> }`。ペイン単位で取得不能な場合は `SessionDelta::Metrics { metrics: None }`(全列 "—")
- branch_poll に `ProbeControl::MetricsActive(bool)` を追加。true の間のみ metrics tick(固定1s、既存のプロセス名適応ポーリングとは独立したスケジュール)。結果は `SessionDelta::Metrics { id, metrics: Option<PaneMetrics> }` で post(`SessionDelta::Process` の :582 パターンをミラー)
- 非macOS: `poll_metrics` は常に `None`(既存 probe と同じ degradation)

### 状態層(session_store)
- `SessionCard` に `metrics: Option<PaneMetrics>` を追加。`SessionDelta::Metrics` 適用で更新
- オーバーレイクローズ時に全カードの metrics をクリア(stale 値の再表示防止)

### UI層(noa-app + noa-render)
- 新規純ロジックモジュール `crates/noa-app/src/process_monitor.rs`: 行モデル(SessionStore → 行リスト構築)・ソート状態・選択状態・値フォーマッタを GUI 非依存で実装
  - フォーマッタ: CPU% は整数%(100%超可)、メモリは MB/GB 自動単位、経過時間は `mm:ss`、1時間以上は `hh:mm:ss`
- レンダリングはコマンドパレット/テーマ設定と同型: `noa_render` にスナップショット型を追加し wgpu オーバーレイ描画(モーダル追加の既存定型 — パレット登録・入力ルーティング・スナップショット・描画・Esc ハンドリング — に従う)
- 所属列 = ウィンドウ/タブタイトル + ペイン位置(SessionCard 既存メタを流用)
- ジャンプ = サイドバー/Overview の既存ペインフォーカス経路を再利用

## L3 — Acceptance Criteria

| ID | 検証内容 | 検証手段 | 対応要件 |
|----|---------|---------|---------|
| AC-1 | コマンドパレットに「プロセスモニタ」項目が存在し、実行でオーバーレイが開き、Esc で閉じる | 実機 | FR-1, FR-6 |
| AC-2 | 複数ウィンドウ×タブ×分割ペイン構成で、行数が全ライブペイン数と一致する | 単体テスト(行構築) | FR-2 |
| AC-3 | あるペインで `yes > /dev/null` 実行中、2s(±1 tick)以内にそのペインの行の CPU% が 90% 以上を表示する | 実機(手動ライブ確認) | FR-3, FR-4, FR-7 |
| AC-4 | `sh -c 'yes > /dev/null'` のように子プロセスへ負荷が逃げるケースでも合算 CPU% に反映される | 実機 | FR-4 |
| AC-5 | メモリ列がツリー合算値を MB/GB 自動単位で表示する | フォーマッタ単体テスト + 実機 | FR-3, FR-4 |
| AC-6 | 初期表示は CPU 降順。ソートキーで メモリ(降順)→プロセス名(昇順)→CPU(降順)とサイクルし順序が追随する | 単体テスト | FR-5 |
| AC-7 | ↑↓ で選択が移動し、Enter で該当ペインのウィンドウ/タブ/ペインがフォーカスされオーバーレイが閉じる | 実機 | FR-6 |
| AC-8 | `MetricsActive=false` の間、branch_poll ワーカーの tick で metrics 収集経路(列挙・rusage)が呼ばれない。収集は branch_poll ワーカー上でのみ実行される | ワーカー単体テスト + コードレビュー | FR-7, NFR-1 |
| AC-9 | 表示中、値は 1s±0.5s 間隔で更新される(2回目 tick 以降 CPU% が数値になる) | 実機 | FR-7 |
| AC-10 | フォアグラウンドプロセス群が exit した直後の tick で該当行が "—" 表示になりパニックしない(消滅 pid を含むスナップショットでの行構築を含む) | 単体テスト | FR-8 |
| AC-11 | ソート・選択・フォーマッタ(mm:ss / hh:mm:ss 繰り上がり含む)・行構築の純ロジックが `cargo test -p noa-app` で検証される | 単体テスト | NFR-3 |
| AC-12 | プロセス一覧の列挙(`proc_listallpids` + `proc_pidinfo`)が 1 tick あたり1回で、全ペインが同一スナップショットを共有する | 単体テスト(スナップショット共有)+ コードレビュー | NFR-2 |
| AC-13 | 行モデルが6フィールド(プロセス名/CPU%/メモリ/プロセス数/経過時間/所属)を全て保持し、所属がタブ名+ペイン位置で構築される | 単体テスト | FR-3 |
| AC-14 | 新規依存が追加されていない(Cargo.toml diff)こと、wgpu/winit 依存が noa-app/noa-render に閉じていることをレビューで確認 | コードレビュー | NFR-4, NFR-5 |
| AC-15 | 非macOS ターゲットで `poll_metrics` が `None` を返し、行が全列 "—" で表示される(cfg ゲートの単体テスト) | 単体テスト | FR-8 |

## Scope

**In-scope**: 上記 FR/NFR 全て(macOS 実測、非macOSは全列 "—" の劣化表示まで)。
**Out-of-scope**: kill 等の破壊操作 / 履歴・グラフ表示 / ディスク・ネットワーク I/O / 非macOS での実測 / 専用キーバインド(keybind config 基盤待ち) / サイドバー高負荷警告(将来 D 案) / ペイン内プロセスの個別行展開 / 行の絞り込み検索。

## Open Questions / Deferred Decisions

- 専用キーバインドのデフォルト割当(keybind config 基盤が整い次第。v1 はコマンドパレットのみ)
- サイドバー高負荷警告インジケータ(D 案)— 本 spec のデータ層(`SessionDelta::Metrics`)の上に増築可能だが、常時収集への切替判断が必要
- NFR-2 の 50ms 目標は設計参考値(AC 検証対象外)。実測で問題が出た場合にベンチ AC 化を検討

## 品質ゲート結果(2026-07-11)

Judge スペックレビュー: **GATE_PASS**(high 0 / medium 8 / low 9)。medium 全件をドラフトへ反映済み: kinfo_proc 非採用→proc_listallpids 化、pgid ツリー定義是正、非macOS 全列 "—" 化、キーバインドのスコープ外化、FR-3/NFR-1/NFR-4/5 の AC 追加(AC-13〜15)、AC-8 の NFR-1 兼務化、NFR-2 の参考値化。
