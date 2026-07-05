# Agent Attention Notification — Specification

## Metadata
- slug: `agent-attention`
- title: エージェント応答待ち通知（サイドバー点滅・タブオーバービュー・Dock）
- status: `locked` (2026-07-05)
- owner: simota
- related: [`session-sidebar`](session-sidebar.md) FR-16（本 spec は FR-16 を拡張・具体化する上位ドキュメント）
- build-path: **feature**（既存 FR-16 基盤への差分実装。設計→実装→AC 検証。[manual] 視覚 AC は人手確認）

## L0 — Vision
Claude Code / Codex / agy を複数セッションで並行実行していると、あるセッションが「ユーザーの入力・判断を待って停止している」状態に気づけず、作業が滞る。noa は既に FR-16 で OSC 9/777 通知を受けて非フォーカスのセッションカードに静的な赤マーク（`· 応答待ち`）とタブオーバービューの `●` を出すが、(1) 静的なため見落としやすく、(2) 検知経路が OSC 9/777 のみで、ベル（BEL）で待機を知らせるエージェントを拾えない。本 spec は **点滅による能動的な注意喚起** と **BEL 検知の追加** を定義する。

- **audience**: 複数のエージェントセッションを並行運用する開発者（＝作者自身）
- **job-to-be-done**: どのセッションが応答待ちかを、ターミナルを注視していなくても即座に気づける
- **success**: エージェントが対話を要求した瞬間、該当セッションがサイドバー／タブオーバービューで点滅し、数秒後に静的マークへ収束、Dock も1回バウンスする。フォーカスで全て消える

### 既存基盤（このセッションで実装済み — 再利用）
- `session_store.rs`: `SessionCard { unread_bell, attention, busy, process, … }`、`StatusDot { Blue, Green, Yellow, Red }`、`status_dot()` 優先度 **attention > bell > busy > idle**
- `SessionDelta::{ Bell, Attention }` — `apply` がフラグを立て、`Upsert` は保持、`clear_bell_for_window` がフォーカスで両方クリア
- `io_thread.rs`: `sidebar_bell = sidebar_visible && term.take_pending_bell()` → `SessionDelta::Bell`／`pending_notifications`（OSC 9/777）→ `UserEvent::Notify`
- `event_loop.rs`: `Notify` → `should_notify` ゲート下で `post_notification` ＋ `apply_session_delta(Attention)`（非フォーカスウィンドウのみ）
- `notification.rs`: `post_notification` が OS 通知センター投函 ＋ `request_dock_attention()`（`NSInformationalRequest` 単発バウンス）
- `overview.rs`: `overview_tile_label` が `attention || unread_bell` で `●` プレフィックス
- `app/sidebar.rs`: `process_badge` ＋ attention 時に `· 応答待ち` を `SIDEBAR_DOT_RED` で付記／`classify_agent(process) → AgentKind`
- **アニメーションタイマ基盤**: `cursor_blink_visible` / `cursor_blink_deadline` / `tick_cursor_blink` ＋ `about_to_wait` の `WaitUntil` 起床機構（入力で `true` にスナップ）。点滅もこの単一タイマ源に相乗りする

### ハード制約（session-sidebar spec から継承）
1. レンダラ／描画パスは `Terminal` をロックしない（publish スロット経由、NFR-1）
2. `session_store.rs` / `sidebar.rs` は GUI 非依存（winit/wgpu 禁止、NFR-6）。点滅の「位相計算」は純関数化しユニットテスト可能に
3. サイドバー文字は全てセル描画。マークは既存ドット／ラベルのプリミティブを流用（新シェーダ不要）
4. `Instant`/時刻はメインスレッド（App）が所有。`session_store` は `WallClock` のみ保持、単調時計は App 側で扱う

## FRAME — 決定事項（AskUserQuestion 2026-07-05）
- **視覚表現**: 点滅 → 数秒後に静的（`点滅→数秒後に静的`）。通知直後だけ点滅で注意喚起し、以降は静的マークへ収束
- **通知範囲**: サイドバーのカード ＋ タブオーバービュー ＋ Dock バウンス/OS 通知（3経路）
- **検知トリガ**: OSC 9/777 通知（実装済）＋ ベル（BEL）

## L1 — Requirements

### Functional
- **FR-A1 点滅→静的収束（サイドバー）**: セッションカードの attention マーク（赤ドット ＋ `· 応答待ち` ラベル）は、attention が `false→true` に遷移した時刻を起点に **`ATTENTION_BLINK_DURATION`（既定 6秒）** の間 **`ATTENTION_BLINK_HZ`（既定 1.5Hz）** で可視/不可視をトグルし、経過後は静的な可視（赤）へ収束する。収束後 attention はフォーカスまで持続（FR-16 準拠）。点滅対象はドットとラベルのみ（カード他要素・テキストは常時可視）。
- **FR-A2 点滅→静的収束（タブオーバービュー）**: タイトルバンドの `●` プレフィックスも FR-A1 と同一位相・同一パラメータで点滅→静的収束する。オーバービュー表示中のみ再描画（既存 due-tile 機構に相乗り）。
- **FR-A3 BEL 検知の attention 昇格**: セッションの前景プロセスが既知エージェント（`classify_agent` が `ClaudeCode`/`Codex`/`Agy`）の場合、そのセッションの BEL（`take_pending_bell`）は **`SessionDelta::Attention`** に昇格する。前景プロセスが `Generic` の BEL は従来通り `SessionDelta::Bell`（黄ドット、未読ベル）に留める — 汎用プログラムのベルを応答待ちと誤認しないため。
- **FR-A4 BEL 検知の常時化**: BEL は io スレッドで **サイドバー可視状態に依らず常時 drain・送信**する（従来 `sidebar_bell` は `sidebar_visible` ゲート付きだった）。分類は前景プロセスを持つメインスレッドが行う（io スレッドは process を知らないため）— 既知エージェントは attention へ昇格（常時反映、Dock/オーバービュー経路に乗る）、`Generic` は `unread_bell` を立てる（フラグはサイドバー可視時のみ描画され、フォーカスでクリア）。実装上の帰結: サイドバー非表示中に鳴った generic ベルは「後で開いた時に遅延表示」ではなく、可視時のみ描画に変わる（軽微な仕様逸脱、許容）。
- **FR-A5 Dock/OS 通知**: attention への遷移時、非フォーカスウィンドウなら Dock を1回バウンスする（`request_dock_attention`）。OSC 9/777 経由の attention は従来通り OS 通知センターにも投函する。**BEL 昇格経由の attention は Dock バウンスのみ**とし OS 通知センターには投函しない（ベルは頻度が高く通知過多を避ける）。
- **FR-A6 クリア**: attention・点滅状態は該当ウィンドウのフォーカス取得で解除される（`clear_bell_for_window` が attention/unread_bell を両クリア、FR-16 準拠）。点滅中でも即座に消える。フォーカス中ウィンドウは attention を立てない（FR-16 準拠）。
- **FR-A7 多重発火の扱い**: 既に attention（点滅済み or 収束済み）のカードに再度 attention デルタが届いた場合、**点滅位相を再スタートしない**（収束後に再点滅させない）。ただしフォーカスでクリア後の新規発火は新しい点滅を開始する。

### Non-Functional
- **NFR-A1 描画パス非ロック**: 点滅可視性は App 側の単調時計から算出し、`Terminal` をロックしない。
- **NFR-A2 バウンドされた再描画**: 点滅は `ATTENTION_BLINK_DURATION` で必ず停止し、収束後は再描画を要求しない（アイドル復帰）。点滅の起床は cursor-blink と同じ `WaitUntil` 単一タイマ源に統合し、二重タイマを作らない。attention カードが無ければ点滅タイマは非武装。
- **NFR-A3 純粋・テスト可能**: 点滅位相の算出（`elapsed → visible: bool`／収束判定）は winit/wgpu 非依存の純関数として `sidebar.rs`（または純モジュール）に置き、ユニットテストする。
- **NFR-A4 誤検知の抑制**: BEL 昇格は前景プロセス分類が既知エージェントの場合に限定（FR-A3）。分類は `process` 未解決時（非 macOS／未ポーリング）には昇格しない（安全側 = 従来の黄ベル）。

## L3 — Acceptance Criteria

- **AC-A1 (FR-A1)**: 点滅位相の純関数 `attention_blink_visible(elapsed, duration, hz)` に対し、(a) `elapsed=0` 近傍で可視、(b) 半周期後に不可視、(c) `elapsed >= duration` で常に可視（収束）を返すことをユニットテストで検証。
- **AC-A2 (FR-A3/NFR-A4)**: BEL 昇格判定の純関数が、前景プロセス `claude`/`codex`/`agy`/`gemini` で `Attention`、`zsh`/`cargo`/`node` および `process=None` で `Bell`（非昇格）を返すことをユニットテストで検証。
- **AC-A3 (FR-A6)**: attention（点滅中含む）のカードを持つウィンドウがフォーカスを得ると `attention=false` かつ点滅停止することをユニットテスト（store）＋ [manual] で検証。
- **AC-A4 (FR-A7)**: 収束済み attention カードへ再 `Attention` デルタを適用しても点滅位相起点が更新されないことをユニットテストで検証（起点タイムスタンプ不変）。
- **AC-A5 (FR-A2) [manual]**: 実機で非フォーカスのエージェントセッションが応答待ちになった時、サイドバーカードとタブオーバービュー `●` が同位相で点滅→約6秒後に静的赤へ収束することを目視確認。
- **AC-A6 (FR-A5) [manual]**: 応答待ち遷移時に Dock が1回バウンスすること、OSC 9/777 経由では OS 通知センターにも出るが BEL 昇格経由では出ないことを目視確認。
- **AC-A7 (NFR-A2)**: 収束後にアイドル（`ControlFlow::Wait`）へ戻り、attention カードが無いときは点滅タイマが非武装であることを [manual] ＋ ログで確認。

## 実装スケッチ（設計メモ — 実装フェーズで確定）
1. **attention 起点の記録**: App 側に `attention_onset: HashMap<SessionCardId, Instant>` を持つ（`session_store` は GUI 非依存のため Instant を持たせない）。`apply_session_delta` の `Attention` 適用で、当該カードが `false→true` の時のみ挿入（FR-A7）。`clear_session_bell_for_window` で該当ウィンドウ分を削除。
2. **点滅タイマ統合**: `cursor_blink_deadline` と同様に `attention_blink_deadline` を `about_to_wait` の `WaitUntil` に合流。武装条件 = 可視サイドバー/オーバービューに点滅中（`elapsed < duration`）の attention カードが1枚以上。
3. **描画**: `app/sidebar.rs` のドット／`· 応答待ち` 描画と `overview.rs` の `●` 付与を、`attention_blink_visible` が `false` の間は抑制（不可視）に。位相計算は純関数。
4. **BEL 昇格**: `io_thread.rs` の `sidebar_bell` 判定を拡張 — 前景プロセス分類が既知エージェントなら `SessionDelta::Attention`（ゲート無し, FR-A4）、それ以外は従来の `SessionDelta::Bell`（サイドバー可視ゲート維持）。分類元 `process` は既存のセッションメタデータワーカ由来。
5. **Dock 分岐**: BEL 昇格経由の attention は Dock バウンスのみ（OS 通知センター投函をスキップ）。OSC 9/777 経由は現状維持。

## Open Questions
- **OQ-1 点滅パラメータ**: `ATTENTION_BLINK_DURATION`（6s）/`ATTENTION_BLINK_HZ`（1.5Hz）は config キー化するか、compile-time 定数（⚠G precedent）に留めるか。初版は定数を推奨。
- **OQ-2 BEL 昇格の対象**: 既知エージェント基準で十分か、あるいは「前景プロセスが shell 以外かつ一定時間出力停止」等のヒューリスティック（AskUserQuestion で不採用）を将来オプション化するか。
- **OQ-3 収束後の持続表現**: 収束後の静的赤を、次のフォーカスまで完全静止にするか、ごく低頻度の「息づき」パルスにするか（初版は完全静止＝現状 FR-16 と同じ）。
