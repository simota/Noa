# Spec: エージェントCLI自動承認モード (auto-approve-mode)

- slug: `auto-approve-mode`
- status: `locked` (2026-07-08 サインオフ)
- owner: simota
- build-path decision: **apex** (`/nexus apex` — 実機AC: T-1署名採取・AC-11/12/13のGUI目視は手動残)

## L0 — Vision

- **問題:** noaのタブ内でClaude Code / Codex / agyを走らせると、ツール実行のたびに承認プロンプト(y/n・番号メニュー・Enter確認)で停止し、ユーザが端末に戻って打鍵するまでエージェントが待ち続ける。複数タブ並行運用では承認待ちがスループットを支配する。
- **対象:** noa上で複数のAIエージェントCLIを並行運用するユーザ(=simota本人のワークフロー)。
- **ジョブ:** opt-inしたタブに限り、認識済みエージェントCLIの承認プロンプトをnoaが検出して自動的に肯定応答を送信し、放置運用を可能にする。
- **成功の定義:** 自動承認ONのタブでエージェントが承認待ちで停止しない。誤検出による意図しない打鍵がゼロ。
- **最大リスク:** 誤検出による破壊的操作の自動承認。検出精度とゲート設計が仕様の中心。

### FRAME確定事項 (ユーザ回答 2026-07-08)

- 方式の軸: **画面検出+合成打鍵** (CLIフラグ注入ではない)
- 粒度: **per-tab opt-in**
- 安全ガード(全採用): 既知パターンのみ承認 / 危険操作は承認しない / ユーザ打鍵で一時停止 / 視覚インジケータ必須

### 再利用資産・制約 (Lens reuse-scan)

| 資産 | 場所 | 用途 |
|------|------|------|
| エージェント検出 `classify_agent` → `AgentKind{ClaudeCode,Codex,Agy,Generic}` | `crates/noa-app/src/sidebar.rs:734` | 自動承認のゲート(認識エージェントのみ) |
| フォアグラウンドプロセスprobe (1秒poll) | `crates/noa-pty/src/pty.rs:213,269`, `branch_poll.rs:44,283` | エージェント在席判定 |
| 画面末尾N行読み取りパターン `preview_rows` | `crates/noa-app/src/io_thread.rs:315` | プロンプト検出スキャンの雛形 |
| pty合成打鍵 `write_pane_pty_bytes` → `PtyInputQueue` | `crates/noa-app/src/app/input_ops/terminal.rs:162`, `io_thread.rs:200` | "y\r"等の注入経路(キーエンコード不要) |
| attention状態 (OSC 9/777通知・Bell昇格) | `app/event_loop.rs:96-121`, `sidebar.rs:775` | 「エージェント応答待ち」の既存シグナル=トリガ候補 |
| io_thread feedのロックセクション+throttle | `io_thread.rs:395,426` | 出力直後スキャンの相乗り先(ロック再取得ゼロ) |
| OSC 133 shell marks / `has_running_program` | `crates/noa-grid/src/terminal.rs:63,345,467` | CLI起動中判定(ただしCLI内部プロンプトは不可視) |
| 設定・コマンド基盤 (Config bool / AppCommand / palette / keybind) | `app/config.rs`, `app/commands.rs:23`, `command_palette.rs:103` | トグルの実装先。per-tabは`Surface`にフラグ新設 |

**スレッド制約:** `Terminal`はArc<Mutex>(parking_lot)。検出=io_thread(既存ロック内)、打鍵=UserEvent経由でmain threadの`write_pane_pty_bytes`、の2段構成が既存設計に整合。合成打鍵は`PTY_INPUT_OVERFLOW_BYTE_CAP`をユーザ入力と共有。node ラッパー越しのCLIは`Generic`に落ちる既知の限界あり(`sidebar.rs:731`)。

## 候補案 (EXPAND)

| 案 | トリガ | 検出器 | 応答 | 危険判定 | 規模 |
|----|--------|--------|------|----------|------|
| A: Attention-Gated Minimal | attention立ち上がりエッジのみ | CLI別ハードコード署名 | 常に最初の肯定選択肢 | 静的キーワード | S |
| B: Debounced Feed-Scan + Regex Table | feed毎+デバウンス | 設定regexテーブル(拡張可) | 種別ごと応答マップ | コマンド抽出分類 | L |
| C: 1s-Poll Matrix | branch_poll 1秒tick便乗 | AgentKind×プロンプト種別マトリクス | don't-ask-again優先 | 承認回数レート制限 | M |
| D: Hybrid State-Machine | attentionエッジ→バーストスキャン | マトリクス+regex二層 | 種別ごと応答マップ | 抽出分類+レート制限 | L |

Riff所感: v1最小リスク=C、将来拡張前提=D。Bの設定regex公開はv1過剰。

### Flux知見 (設計に取り込むべき安全機構)

- 行確定+数フレーム安定(~120ms)後のみ照合、カーソルがプロンプト行にある時のみ (iTerm2/部分描画対策)
- 打鍵後は同一署名にデバウンス+消費フラグ、画面変化まで再武装しない (二重打鍵対策)
- 打鍵→画面が「進んだ」確認まで次弾禁止 (tmux盲撃ち対策)
- alt screenフラグ+viewport bottomを前提条件に (スクロールアウト対策)
- 未知バージョン文言は自動無効化+通知 (fail-safe)
- IMEアクティブ・ペースト中・直近ユーザ入力中は送出抑止
- 逆張り: 端末側でやる価値=「全CLI一律ポリシー・per-tab可視化・監査」に絞れ / 既定を自動yesでなく自動エスカレーションにする道 / OSC構造化チャネル標準をCLIに出させる道(VS Code方式)

## 選定と却下案 (CHALLENGE)

**選定 (ユーザ確定 2026-07-08): 案C (1s-Poll Matrix) + Flux安全機構**

- 応答ポリシー: 常に最初の肯定選択肢 (Claude Codeなら "1. Yes")。don't-ask-again系はCLI側設定を汚すため選ばない。
- OSC構造化チャネル: Open Questionにpark。検出器をtrait化して将来差替え可能な構造だけ担保。

### CHALLENGE修正 (Omen/Ripple実査 2026-07-08)

**トリガ層の差替え(条件付きGO):** branch_poll便乗は実コード上不成立 — workerは`Arc<Mutex<Terminal>>`非保持(branch_poll.rs:220-243)、per-tabフラグ(Surface, main thread所有)不可視、window/pane解決不能。よって:
1. **検出はio_thread `feed_terminal_batch`内へ移設** (ロック既保持・ペイン既知・自然throttle, io_thread.rs:395付近)。スキャンは2連続一致デバウンスで半端描画誤照合(RPN80)を遮断。
2. **注入は新規`UserEvent::AutoApprove{..}`** → main threadでcard→(window,pane)権威解決 → 既存`write_pane_pty_bytes`。SessionDeltaはstore適用専用で流用不可。
3. **許可リスト方式に反転**: 既知無害プロンプト署名のみ承認。承認後は当該領域cell-hashロック+連続承認上限で再武装ループ(RPN45)遮断。

**Voidスコープ圧縮 — v1安全機構は6系統+1加算:**
署名照合(未知文言=不発のfail-safe内包) / タブバッジ / 消費フラグ(cell-hash・画面前進確認を吸収) / alt screen+viewport末尾追従中のみ武装 / IME・ペースト・直近ユーザ入力中(3s)抑止 / ローリング窓6回で自動OFF+attention。加算=**自動承認の監査ログ**(リングバッファ、直近N件)。
CUT: 危険語全文パーサ(署名を無害種別に限定して代替) / 行安定120ms(ポーリング…feed駆動でも2連続一致が代替) / 検出器trait化(YAGNI)。

**Omen残リスク上位:** 矢印UI版で番号無効(RPN60)→番号+ハイライト位置の二重署名・不一致は不発 / 打鍵先ペイン誤り(RPN30)→注入直前に対象再スキャン / "1"vs"1\r"の版差→agent×署名ごとに注入バイト列を表で固定し実機検証。

**却下:**
- 案A (Attention-Gated Minimal) — attention未発火プロンプトの取りこぼしを許容できない
- 案B (Regex Table) — 設定regex公開はv1過剰・暴発リスク・設計負債
- 案D (Hybrid State-Machine) — 実装・検証コストL。安全層(armed/cooldown・二重ゲート)の要素はCに選択的に取り込む
- CLIフラグ注入方式 — FRAMEで却下(起動経路への介入が必要)
- don't-ask-again優先応答 — セッションを越えてCLI側allowlistに残る副作用

## 提案 (SHAPE)

### 解決策 (C改)

per-tab opt-inの「自動承認モード」。ONのタブでは、io_threadのfeed処理内(出力直後・Terminalロック既保持)で可視ビューポートをスキャンし、認識エージェント(ClaudeCode/Codex/Agy)×既知の**無害プロンプト署名**(Edit/Write/Read承認・Enter確認)にマッチした場合のみ、`UserEvent::AutoApprove`経由でmain threadから該当ペインのptyへ固定バイト列("1"等)を注入する。未知の文言・Bash承認・未認識エージェントには何もしない(fail-safe)。

### In-scope (v1)

1. per-tabトグル: `Surface`フラグ + `AppCommand::ToggleAutoApprove` + パレット項目 + keybind(グローバル既定はconfig、既定OFF)
2. 検出器: AgentKind×プロンプト種別の署名マトリクス(ハードコード)。番号+ハイライト位置の二重署名。2連続feed一致で確定
3. 前提ゲート: alt screen中 or viewport末尾追従中のみ武装。カーソル行条件は署名に内包
4. 注入: `UserEvent::AutoApprove{card,bytes}` → main thread権威解決(注入直前に対象再確認) → `write_pane_pty_bytes`。agent×署名ごとの注入バイト列固定表
5. 消費フラグ: 承認後は当該領域cell-hash変化まで再武装しない + 連続承認上限
6. 入力競合抑止: IME preedit中・ペースト中・直近ユーザ入力(3s)は不発
7. 暴走遮断: ローリング窓(60s)6回で自動OFF + attention通知
8. 可視化: タブ/サイドバーバッジ(モードON) + 発火時フラッシュ
9. 監査ログ: 直近N件の自動承認記録(リングバッファ + 表示面)

### Out-of-scope

- Bashコマンド実行承認の自動化(v2候補: denylist約20行で拡張可能な設計余地のみ残す)
- 設定ファイルregexテーブル / ユーザ定義パターン
- CLIフラグ注入(--dangerously-skip-permissions等)方式
- OSC構造化チャネル(park済み)
- nodeラッパー等で`Generic`に落ちるエージェントの検出改善
- 危険語全文パーサ

### 前提 (Assumptions)

- 対象CLIの承認プロンプト文言は既知バージョン範囲で安定(変わったら不発=安全側)
- Claude Codeの番号メニューは数字打鍵を受け付ける(実機検証をSPECIFYのACに含める)
- 検出署名の初期セットは実機のプロンプト採取で作る(Claude Code優先、Codex/agyは採取でき次第)

### SHAPE確定事項 (ユーザ回答 2026-07-08)

- **フォーカス中のタブでも発火する**。衝突回避は直近ユーザ入力抑止(3s)+IME/ペーストガードに委ねる
- In-scopeの正準リストは「## Scope」節の10項目(本SHAPE節の9項目+フォーカス中発火)とする
- 監査ログ表示面: **サイドバーカード内**(直近承認件数/最新項目。専用モーダルはv2送り)

## L1 — Requirements

### 機能要件 (FR)

- **FR-1** per-tab opt-inトグル: 自動承認モードを`Surface`単位でON/OFFできる(`AppCommand::ToggleAutoApprove`、コマンドパレット項目、keybind、`config`既定値)。既定はOFF。
- **FR-2** エージェントゲート: `classify_agent`が`ClaudeCode`/`Codex`/`Agy`を返すペインでのみ武装。`Generic`・未認識は不発。
- **FR-3** プロンプト検出: `AgentKind`×プロンプト種別の**ハードコード署名マトリクス**で可視ビューポートを照合。二重署名 = ①アンカー文言+番号ラベル ②**選択マーカー条件**(選択カーソル文字「❯」等が最初の肯定選択肢の行頭にあること。グリッド文字ベース判定、SGR属性非依存)。いずれか不一致なら不発。
- **FR-4** 2連続一致デバウンス: 同一署名が連続2回のfeedスキャンで一致した時のみ確定(半端描画の誤照合遮断)。
- **FR-5** 前提ゲート: alt screen中 or viewport末尾追従中(**scrollback表示オフセット==0**、すなわちライブ末尾を表示中)のみ武装。
- **FR-6** 肯定応答注入: 確定時、`UserEvent::AutoApprove`経由でmain threadが(window,pane)を権威解決し、agent×署名ごとに固定した注入バイト列を`write_pane_pty_bytes`で送出。
- **FR-7** 注入直前の再確認: main threadで注入する直前に対象ペインを再スキャンし、署名が消えていれば送出中止(打鍵先ペイン誤り・状態陳腐化の防止)。
- **FR-8** 消費フラグ: 承認後は**署名マッチ行範囲(アンカー行〜選択肢最終行)のセル内容ハッシュ**が変化するまで再武装しない + 連続承認上限。
- **FR-9** 入力競合抑止: IME preedit中・ペースト中・直近ユーザ入力(3s以内)は不発。
- **FR-10** 暴走遮断: ローリング窓(60s)内M回(既定6回)の承認で自動OFF + attention通知。
- **FR-11** 危険操作の非承認: Bash実行承認・未知文言はマトリクス署名に含めず、常に不発(fail-safe)。
- **FR-12** 可視化: モードONのタブ/サイドバーカードにバッジ、発火時にフラッシュ。
- **FR-13** 監査ログ: 直近N件(既定16件)の自動承認をリングバッファに記録し、サイドバーカード内に直近承認件数/最新項目を表示。

### 非機能要件 (NFR)

- **NFR-1** 性能: 検出スキャンは`feed_terminal_batch`(`io_thread.rs:395`)内の既保持ロックに相乗りし、Terminalロックの再取得ゼロ。スキャン範囲は可視ビューポート行数に限定し、feed毎の追加コストを`preview_rows`相当のO(rows×cols)以内に抑える。
- **NFR-2** fail-safe原則: 未知=不発。署名不一致・未知バージョン文言・前提ゲート未成立は一貫して「何もしない」に倒す。
- **NFR-3** スレッド安全: 検出はio_thread(ロック内・ペイン既知)、注入は`UserEvent::AutoApprove`→main threadの2段構成。合成打鍵は`PTY_INPUT_OVERFLOW_BYTE_CAP`をユーザ入力と共有。
- **NFR-4** CLI側非汚染: don't-ask-again系応答は選ばず、常に最初の肯定選択肢のみ送出(CLI側allowlistに副作用を残さない)。
- **NFR-5** 拡張余地: 署名マトリクスは後日Bash denylist等を加算できるデータ構造とするが、v1では公開設定・trait化はしない(YAGNI)。

## L2 — Detail

### 検出器 (純関数コア + io_thread glue)

- **純関数seam(テスト境界)**: 検出コアは副作用なしの純関数として切り出す —
  `detect(viewport_rows: &[RowText], cursor: CursorPos, agent: AgentKind, now: Timestamp, state: &AutoApproveState) -> Decision`(`Decision = Fire{signature_id, bytes} | Hold | Suppressed{reason}`)。時刻・直近ユーザ入力タイムスタンプ・IME/ペースト状態はすべて引数注入し、AC-2..9はこの純関数の単体テストとして検証する。io_thread/pty/GUIは不要。
- 配置(glue): `feed_terminal_batch`のロックセクション内、`preview_rows`スキャンと同居。ビューポート行テキストを抽出して`detect`を呼ぶだけ。ペインは既知、ロック再取得なし。
- スキャン範囲: 可視ビューポート(alt screen or scrollback表示オフセット==0のみ)。
- 署名マトリクス構造: `AgentKind × PromptKind → Signature`。`Signature`は{ アンカー文言, 番号ラベル("1"等), 選択マーカー条件(「❯」等が最初の肯定選択肢行頭にあること・文字ベース判定) } の二重署名 + 注入バイト列。`PromptKind`初期セット(Claude Code): Edit承認 / Write承認 / Read承認 / AskUserQuestion選択 / Enter確認。実文言・実バイト列は前提タスクT-1(署名採取)で確定。
- デバウンス状態: per-paneに「前回一致署名 + 一致回数」を持ち、2連続一致で確定。

### 状態機械 (per-pane `AutoApproveState`)

- フィールド: `armed`(前提ゲート成立中) / `awaiting_change`(承認後、cell-hash変化待ち) / `cooldown`、直近承認領域の`cell_hash`、ローリング窓カウンタ(60s窓の承認タイムスタンプ列)、最後のユーザ入力時刻。
- 遷移: `armed`→署名2連続一致→`UserEvent`発火→`awaiting_change`。署名マッチ行範囲のハッシュ変化で`armed`へ再武装。60s窓内6回超過で`disabled`(自動OFF)+attention。

### 注入経路

- `UserEvent::AutoApprove { card_id, bytes }`(`events.rs`のUserEvent enumに追加、既存variantに倣う)。
- main threadでcard_id→(window, pane)を権威解決 → **注入直前に対象ペインを再スキャン(FR-7)** → 一致継続時のみ`write_pane_pty_bytes`(`app/input_ops/terminal.rs`)へバイト列を渡し`PtyInputQueue`へ。
- 抑止条件(IME/ペースト/直近入力/窓超過)の評価: 検出時(io_thread、早期棄却)と注入直前(main thread、権威判定)の**両方**で評価。

### トグル / 設定

- `Surface`(`app/state.rs:511`)に`auto_approve: bool`フラグ新設。
- `AppCommand::ToggleAutoApprove`(既存`ToggleSidebar`等に倣いメニューID・title・palette登録)。
- `command_palette_entries()`(`command_palette.rs`)に項目追加。
- `config.rs`にグローバル既定`auto_approve: bool`(既存`sidebar_enabled`/`visual_bell`命名流儀、既定`false`)。keybindはconfig経由。

### 可視化

- バッジ: タブタイトルとサイドバーカード(`sidebar.rs`の`CardLines`/process badge行近傍)にモードONバッジ。
- 発火フラッシュ: 承認送出時にカード/タブを短時間ハイライト。
- 監査ログ: per-paneリングバッファ(容量16件、{時刻, agent, PromptKind}を保持)。`SessionCard`内に「自動承認: N件 / 最新: <PromptKind>」を表示。専用モーダルはv2。

## L3 — Acceptance Criteria

前提: AC-2..9は検出純関数`detect(...) -> Decision`(L2参照)の単体テストとして検証する(io_thread/pty/GUI不要)。

- **AC-1 (FR-1)** ユニット: `Surface.auto_approve`をトグルするとON/OFFが反転し、既定OFF。パレットに項目が現れる。
- **AC-2 (FR-2)** ユニット: `agent=Generic`では、既知署名の画面テキストを与えても`detect`が`Fire`を返さない。
- **AC-3 (FR-3, FR-11)** ユニット: マトリクス外の文言(Bash承認プロンプト・任意の未知文言)では`detect`が`Fire`を返さない。
- **AC-4 (FR-3)** ユニット: 選択マーカー「❯」が最初の肯定選択肢の行頭にない画面(別選択肢を指す/マーカー無し)では、アンカー文言が一致しても不発。
- **AC-5 (FR-4)** ユニット: 署名が1回のスキャンでしか一致しない(次スキャンで消える)場合は確定せず不発。2連続一致で`Fire`。
- **AC-6 (FR-5)** ユニット: alt screen外かつscrollback表示オフセット>0の状態では`detect`が`Suppressed`を返す。
- **AC-7 (FR-6, FR-7)** ユニット: 発火後、注入直前の再スキャン(main thread側)で署名が消えていれば`write_pane_pty_bytes`が呼ばれない。
- **AC-8 (FR-8)** ユニット: 承認直後、署名マッチ行範囲のセル内容ハッシュが不変のまま同一署名が残っても再発火しない。ハッシュ変化後に再武装する。
- **AC-9 (FR-9)** ユニット: IME preedit中 / ペースト中 / 直近ユーザ入力タイムスタンプがnowから3s以内、の各引数条件で`detect`が`Suppressed`を返す。
- **AC-10 (FR-10)** ユニット: 60s窓内で6回承認するとモードが自動OFFになり、attention通知が立つ。
- **AC-11 (FR-12, FR-13)** 実機GUI: モードONでタブ/サイドバーにバッジ表示、発火時フラッシュ、カードに承認件数/最新項目が更新される。監査リングバッファは16件で古い順に破棄される(単体テスト併用)。
- **AC-12 (FR-3, FR-6)** 実機GUI: ONのタブでClaude Codeの各PromptKind(Edit/Write/Read/AskUserQuestion/Enter確認)が自動的に肯定応答され、エージェントが停止せず進行することを目視確認。**前提タスクT-1完了が条件。**
- **AC-13 (FR-6)** 実機GUI(版差検証): 各agent×署名の注入バイト列("1" vs "1\r"等)が実機で受理されることを確認し、表に固定。

### 前提タスク (実装フェーズ、ACの前提)

- **T-1 署名採取**: Claude Code(優先)/Codex/agyの実承認プロンプトを実機で採取し、署名マトリクス(アンカー文言・番号ラベル・選択マーカー・注入バイト列)を確定する。未採取のagent×PromptKindはマトリクスに載せない(=不発のまま)。

## Scope

### In-scope (v1)

1. per-tab opt-inトグル(`Surface`フラグ + `AppCommand::ToggleAutoApprove` + パレット + keybind + config既定OFF)
2. `AgentKind`×プロンプト種別のハードコード署名マトリクス(番号+選択マーカー条件の二重署名、2連続feed一致で確定)
3. 前提ゲート(alt screen中 or viewport末尾追従中のみ武装)
4. `UserEvent::AutoApprove`注入経路(main thread権威解決 + 注入直前再確認 + agent×署名ごと固定バイト列)
5. 消費フラグ(cell-hashロック + 連続承認上限)
6. 入力競合抑止(IME/ペースト/直近ユーザ入力3s)
7. 暴走遮断(60s窓6回で自動OFF + attention)
8. 可視化(タブ/サイドバーバッジ + 発火フラッシュ)
9. 監査ログ(リングバッファ16件 + サイドバーカード表示)
10. フォーカス中タブでも発火(衝突回避は6に委譲)

### Out-of-scope

- Bashコマンド実行承認の自動化(v2: denylist拡張の設計余地のみ残す)
- 設定ファイルregexテーブル / ユーザ定義パターン
- CLIフラグ注入(`--dangerously-skip-permissions`等)方式
- OSC構造化チャネル(park済み)
- nodeラッパー等で`Generic`落ちするエージェントの検出改善
- 危険語全文パーサ
- 監査ログ専用モーダル(v2)
- don't-ask-again優先応答

## Open Questions / Deferred Decisions

- **OSC構造化チャネル** (CLIが承認要求を制御シーケンスで申告、VS Code方式): park。v1は画面検出。検出コアが純関数`detect`に隔離されているため、将来の差替えは局所的
- **nodeラッパー越しCLI**が`Generic`に落ちる既知の限界(`sidebar.rs:731`): v1は対象外。検出改善は別件
- **Bash承認のdenylist拡張** (約20行の部分文字列denylist): v2候補。署名マトリクスは加算可能な構造を維持(NFR-5)
- **監査ログ専用モーダル**: v2候補(v1はサイドバーカード内のみ)
- **Codex/agyの署名採取(T-1)が遅れる場合**: Claude Codeのみで先行し、未採取agentは不発のまま(fail-safe設計により安全に部分リリース可)
- 発火フラッシュの具体アニメーション: 実装時に既存anim基盤(UI_ACCENT/トークン)の流儀に従う
