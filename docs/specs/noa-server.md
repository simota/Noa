# noa-server — locked specification

- slug: `noa-server`
- status: `locked`(2026-07-11 サインオフ)
- owner: simota
- build-path decision: **apex**(/nexus apex — 単一ランで design→実装→AC 検証→ship。fallback: orbit / feature)
- quality gate: PASS(Judge+Attest 2026-07-11、blocker 0)

## L0 — Vision

**問題:** noa は GUI 内でしかパネル(ウィンドウ→タブ→ペイン)状態を扱えず、外部プログラムからの制御は macOS 専用の AppleScript に限られる。CLI ツール・外部アプリ・AI エージェント等の「接続クライアント」が、稼働中の noa に接続してパネル一覧と各状態(タイトル・プロセス・busy・出力内容など)を閲覧し、特定パネルへのアクション実行やテキスト入力を行えるサーバー機能を提供する。(FRAME チェックポイントでユーザー確認済み 2026-07-11)

**対象(who):**
- 外部アプリ/ダッシュボード(セッション一覧のリモート監視・操作)
- iOS アプリ(→ ネットワーク越しアクセスが必須)
- noa 自身の将来機能(リモートウィンドウ・セッション共有等の内部基盤)

**成功定義:** パネル一覧・状態取得・アクション・入力に加え、**状態変化・出力のリアルタイム push(購読)** までクライアントが行える。

## 再利用資産・制約 (Lens スキャン 2026-07-11)

**流用可能:**
- 状態モデル: `SessionStore`/`SessionCard`(name/cwd/branch/process/busy/attention/preview)、`AppStateSnapshot`(AppleScript 用 main-thread 読み取り射影, `macos_applescript.rs:31`)
- アクション注入: `UserEvent`(WriteText/SpawnTab/ClosePane/AppCommand 等)を `EventLoopProxy` 経由で送出 — AppleScript と同一経路
- アクション語彙: `command_from_applescript_action`
- 入力注入: `write_pane_pty_bytes` / 有界 `PtyInputQueue`
- テキスト化: `Screen::scrollback_text()` / `selected_text()` / preview_spans / `FrameSnapshot`
- ライフサイクル雛形: `Registration::install(proxy, snapshot)`
- config 追加は 5 箇所定型(`noa-config/src/lib.rs` + `parser/overrides.rs`)

**制約:**
- `App` 本体は main-thread 専有。サーバーは別スレッド、ミューテーション=EventLoopProxy / 読み取り=共有 `Arc<Mutex<Snapshot>>` の二経路厳守
- `Terminal` ロックは短時間(pty feed と競合)
- serde/tokio 等ネットワーク系依存は未導入(完全新規増分)
- ID はポインタ由来 u64 → API では 64bit/文字列で扱う
- 認証・露出範囲のセキュリティ前例なし。sandbox で socket bind 不可の可能性

## SHAPE — 提案 (Spark 2026-07-11, ユーザー確認前)

**ソリューション:** 新クレート `noa-ipc` — JSON-RPC over WebSocket サーバー。sync tungstenite + thread-per-connection + crossbeam(非同期ランタイム不使用)。読取 = main-thread 公開 `Arc<Mutex<Snapshot>>`、変異 = `EventLoopProxy<UserEvent>`(AppleScript と同一経路)、output = `feed.rs` タップ + sidebar 抽出流用の構造化行差分。認証 = loopback TCP 上の WS + 必須トークン、変異系別スコープ。handshake で `protocolVersion` 交換(additive-only)。

```
 ┌─────────── main thread (App) ───────────────────┐
 │  Terminal ─about_to_wait─▶ Arc<Mutex<Snapshot>> │─read──┐
 │       ▲                                         │       ▼
 │       │ feed.rs tap ─▶ output diff broadcast ───┼─push─▶ ┌──────────────┐
 │  EventLoopProxy<UserEvent> ◀─mutate─────────────┼───────│ noa-ipc      │◀═WS═▶ client
 └─────────────────────────────────────────────────┘       │ (tungstenite │      (CLI/iOS/
                                              token auth    │  per-conn)   │       dashboard)
                                                            └──────────────┘
```

**In-scope (v1):** RPC `list_panels` / `get_text` / `get_grid`(viewport 限定・ページング・色属性付き) / `send_text` / `focus_pane` / `new_tab` / `split` / `close_pane`、`subscribe` → `state_changed` + `output`(構造化行差分)、config `server-enable`/`server-port`/`server-token`、NFR 5 項(CHALLENGE 節)。

**Out-of-scope (v1):** LAN 直接 bind / in-process TLS / keybind 全語彙 / per-client 購読ポリシー / 生 VT ストリーム / CRDT・オフライン / multi-noa リモーティング。

**前提:** iOS はトンネル経由で到達 / sandbox 下でも loopback bind 可(不可なら degrade)/ ID は u64 で安定表現 / `about_to_wait` 更新頻度で push レイテンシ要件を満たす。

## L1 — Requirements

### Functional (FR)

- **FR-1 (lifecycle/gate):** サーバーは config `server-enable`(bool, default `false`)が真のときのみ起動し、偽なら一切のポートを開かない。
- **FR-2 (bind):** サーバーは `127.0.0.1:<server-port>`(default `61771`)にのみ WebSocket を bind し、非 loopback インターフェースを listen しない。bind 失敗時はアプリを継続し警告ログのみ残す。
- **FR-3 (token provisioning):** 初回起動時にトークンを自動生成して 0600 権限のファイルに保存し、以降は再利用する。config `server-token` が設定されていればそれを優先しファイル生成をスキップする。
- **FR-4 (handshake):** 接続確立後、サーバーとクライアントは `protocolVersion`(整数 major)を交換する。major 不一致の接続はエラーで拒否する。
- **FR-5 (auth):** クライアントは `Authorization: Bearer <token>` ヘッダまたは最初の `noa.hello` メッセージでトークンを提示し、一致しない接続は確立を拒否する。
- **FR-6 (scopes):** 認可は 3 スコープ `read` / `control` / `input`。config `server-scopes`(カンマ区切りリスト, default `read`)に列挙されたスコープのみ付与可能で、クライアントが `noa.hello` で要求したスコープとの積集合を付与する。付与されないスコープを要するメソッド呼び出しは拒否する。`control`(focus/tab/split/close)と `input`(send_text)はそれぞれ独立に `server-scopes` へ明示列挙されたときのみ付与可能。
- **FR-7 (list_panels):** `noa.listPanels` は全ウィンドウグループ配下のパネル一覧を、ID とメタデータ(name/cwd/branch/process/busy/attention/preview)付きで返す。要 `read`。
- **FR-8 (get_text):** `noa.getText` は指定パネルのテキストを返す。`source=screen` は可視画面のみ、`source=scrollback` は scrollback **と可視画面を含む全体**(`scrollback_text()` 相当)。応答は `maxBytes`(省略時 256KB)で有界化し、超過時は**末尾側を優先して切り詰め** `truncated:true` を返す(NFR-4 整合)。要 `read`。
- **FR-9 (get_grid):** `noa.getGrid` は指定パネルのグリッドを行レンジページング(`startRow`/`rowCount`)で返し、各セルをテキスト + 色ラン(PreviewSpan 形式)で表現する。1 応答は有界サイズ内に収める。要 `read`。
- **FR-10 (focus_pane):** `noa.focusPane` は指定パネルを前面化・フォーカスする。要 `control`。
- **FR-11 (new_tab):** `noa.newTab` は指定ウィンドウ(省略時アクティブ)に新規タブを生成し、生成パネル ID を返す。要 `control`。
- **FR-12 (split):** `noa.split` は指定パネルを指定方向(`horizontal`/`vertical`)に分割し、生成ペイン ID を返す。要 `control`。
- **FR-13 (close_pane):** `noa.closePane` は指定パネルを閉じる。要 `control`。
- **FR-14 (send_text):** `noa.sendText` は指定パネルの pty へ UTF-8 テキストを注入する。要 `input`。
- **FR-15 (subscribe):** `noa.subscribe` / `noa.unsubscribe` はイベント種別(`state_changed` / `output`)とパネルフィルタを指定して push 購読を開始・停止する。要 `read`。
- **FR-16 (state_changed):** パネルメタデータ変化時、購読クライアントへ `noa.stateChanged` 通知を送る(sidebar 抽出と同一の構造化スナップショット)。
- **FR-17 (output):** パネル出力更新時、購読クライアントへ `noa.output` 通知を色ラン付き行差分で送る。取りこぼしが生じた購読には `dropped` マーカーを含めて通知する。
- **FR-18 (errors):** 全メソッドは JSON-RPC 2.0 エラーオブジェクトで失敗を返し、認証失敗・未知パネル・スコープ不足・パネル消滅・ペイロード超過・バージョン不一致に固有コードを割り当てる。
- **FR-19 (versioning):** プロトコルは additive-only で拡張し、major bump は破壊的変更時のみ行う。「無害」の定義: 未知メソッドは標準エラー `-32601` を返し**接続は維持**する。既知メソッド内の未知フィールドはエラーにせず無視する。

### Non-Functional (CFR, CHALLENGE より昇格)

- **NFR-1 (security):** default で loopback 限定 bind + 全メソッド認可必須。変異系(`control`/`input`)は `read` と別スコープの opt-in。
- **NFR-2 (non-blocking):** 端末・io_thread はクライアントを絶対に待たない。push は有界 `try_send` + drop-oldest + 欠落マーカーで、slow/stall クライアントが描画や pty feed を遅延させない。
- **NFR-3 (concurrency model):** 非同期ランタイム不使用。sync tungstenite + thread-per-connection + crossbeam のみ(io_thread と同一並行モデル)。
- **NFR-4 (bounded serialization):** 直列化は有界。viewport/行レンジ限定・ページング必須・dirty 合流 ≥16ms。全 scrollback 一括ダンプを禁止。
- **NFR-5 (versioned protocol):** プロトコルはバージョン付き。handshake で `protocolVersion` を交換し、major 不一致は接続を拒否する。

## L2 — Detail

### Transport & Handshake

- WebSocket over TCP、`ws://127.0.0.1:61771/`(`server-port` で可変)。TLS なし(loopback 前提、iOS はトンネル終端)。
- 認証: WS upgrade の `Authorization: Bearer <token>` ヘッダ、またはヘッダ非対応クライアント向けに接続直後の `noa.hello` リクエスト(`params.token`)。いずれも FR-3 のトークンと定数時間比較。
- handshake: クライアントが `noa.hello { protocolVersion, token, scopes }` を送り、サーバーが `{ protocolVersion, grantedScopes, serverVersion }` を返す。`protocolVersion` は現行 `1`。`grantedScopes` = 要求スコープ ∩ `server-scopes`(config, default `read` のみ)。
- 認証・バージョン確立前の他メソッドは `-32001`(auth)/`-32006`(version)で拒否。

### JSON-RPC 2.0 メソッド表

| メソッド | 要スコープ | params | result(概略) |
|----------|-----------|--------|----------------|
| `noa.hello` | — | `{ protocolVersion, token, scopes:[…] }` | `{ protocolVersion, grantedScopes:[…], serverVersion }` |
| `noa.listPanels` | read | `{}` | `{ panels:[Panel] }` |
| `noa.getText` | read | `{ paneId, source:"screen"|"scrollback", maxBytes? }` | `{ paneId, text, truncated? }` |
| `noa.getGrid` | read | `{ paneId, startRow, rowCount }` | `{ paneId, cols, startRow, rows:[Row], hasMore }` |
| `noa.sendText` | input | `{ paneId, text }` | `{ ok:true }` |
| `noa.focusPane` | control | `{ paneId }` | `{ ok:true }` |
| `noa.newTab` | control | `{ windowId? }` | `{ paneId }` |
| `noa.split` | control | `{ paneId, direction:"horizontal"|"vertical" }` | `{ paneId }` |
| `noa.closePane` | control | `{ paneId }` | `{ ok:true }` |
| `noa.subscribe` | read | `{ events:["state_changed","output"], paneIds?:[…] }` | `{ subscriptionId }` |
| `noa.unsubscribe` | read | `{ subscriptionId }` | `{ ok:true }` |
| 通知 `noa.stateChanged` | — | `{ panels:[Panel] }`(変化分) | (通知・応答なし) |
| 通知 `noa.output` | — | `{ paneId, lines:[Row], dropped?:true }` | (通知・応答なし) |

### ID モデル & Panel メタデータ

- ID(`windowGroupId` / `windowId` / `paneId`)はいずれも内部 u64 を **10 進文字列**として表現(JS の 53bit 安全整数を越えるため)。
- 階層は windowGroup(論理ウィンドウ)→ window(ネイティブタブ)→ pane。`noa.newTab` は**タブ + その初期 pane を生成し、初期 pane の `paneId` を返す**(タブは `noa.split` により複数 pane を持ち得る — 1:1 前提ではない)。
- `Panel` = `{ windowGroupId, windowId, paneId, name, cwd, branch, process, busy, attention, preview }` — `SessionCard` を鏡写しにする(`sidebar` 抽出流用)。`preview` は色ラン付き。

### Grid ペイロード

- `Row` = `{ row, spans:[{ text, fg?, bg?, attrs? }] }`。`fg`/`bg` は `#rrggbb` または palette index、`attrs` は bold/italic/underline 等のフラグ集合。**同一スタイルの連続セルを text を保持したまま 1 span に畳む**(PreviewSpan 相当)。
- ページング: `startRow`/`rowCount` で行レンジを指定。応答が上限(既定 256KB 目安)に達すると `hasMore:true` を返し、超過分は次リクエストに委ねる。上限超過単発は `-32005` で拒否。

### Push パイプライン

- output: `feed.rs` の feed 後タップ(O(1)・追加ロックなし)で dirty 行を集め、≥16ms で合流してから購読ごとの有界 broadcast チャネルへ `try_send`。満杯時は drop-oldest し次送信に `dropped:true` を立てる。行差分は sidebar 抽出と同じ色ラン化ロジックを再利用。
- state_changed: `about_to_wait` で更新される `Arc<Mutex<Snapshot>>` から sidebar 抽出を流用し、変化検知時のみ差分 `panels` を配信。
- io_thread/main-thread は broadcast への `try_send` のみ行い、購読者スレッドが直列化・送信を担う(NFR-2)。

### Config キー & トークンファイル

- `server-enable`(bool, default `false`)/ `server-port`(u16, default `61771`)/ `server-token`(string, 省略時は自動生成)/ `server-scopes`(カンマ区切り `read,control,input` の部分集合, default `read`)。追加は既存 5 箇所定型(`noa-config/src/lib.rs` + `parser/overrides.rs`)。
- トークンファイル: config ディレクトリ配下 `server-token`(例 `$XDG_CONFIG_HOME/noa/server-token`)、権限 `0600`、初回のみ生成。`server-token` config 指定時は生成・読込ともスキップ。

### エラーコード表

| code | 意味 | 契機 |
|------|------|------|
| `-32001` | auth failure | トークン不一致・未認証メソッド呼び出し |
| `-32002` | unknown pane | 存在しない `paneId`/`windowId` |
| `-32003` | scope denied | 未付与スコープのメソッド呼び出し |
| `-32004` | pane closed | 実行途中でパネルが消滅 |
| `-32005` | payload too large | 応答/リクエストが上限超過 |
| `-32006` | version mismatch | `protocolVersion` の major 不一致 |

(`-326xx` は JSON-RPC 実装定義レンジ。標準 `-32600`〜`-32603` はパース/不正リクエスト/未知メソッド/不正パラメータに使用。)

### クレート配置 & 統合点

- 新クレート `noa-ipc`(DAG 上は `noa-app` から利用、`wgpu`/`winit` 非依存)。
- 変異注入: `noa-app/src/events.rs` の `UserEvent` に IPC 用バリアント(既存 WriteText/SpawnTab/ClosePane/AppCommand を再利用、不足分のみ追加)を足し、`EventLoopProxy` 経由で送出(AppleScript と同一経路)。
- 状態読取: `noa-app/src/app.rs` の `about_to_wait` で公開する `Arc<Mutex<Snapshot>>`(`macos_applescript.rs` の `AppStateSnapshot` 射影を流用)。
- output タップ: `noa-app/src/io_thread.rs` の feed ループ(`feed.rs`)に O(1) タップを挿入し broadcast へ。
- config: `noa-config/src/lib.rs` + `noa-config/src/parser/overrides.rs`。

## L3 — Acceptance Criteria

- **AC-1 (FR-1):** `server-enable=false`(既定)で起動すると `61771` は listen されず(`lsof`/接続試行が拒否)、真にすると listen される。
- **AC-2 (FR-2):** サーバーは `127.0.0.1` のみ bind し、LAN IP への接続は拒否される。bind 失敗時もアプリは起動を続行する。
- **AC-3 (FR-3):** 初回起動後、トークンファイルが権限 `0600` で存在し内容が非空。`server-token` を config 指定した起動ではファイルを生成せず指定値が有効。
- **AC-4 (FR-5, NFR-1):** 有効トークンを提示しない接続は `-32001` で拒否され、いかなる `read`/`control`/`input` メソッドも実行されない。
- **AC-5 (FR-6, NFR-1):** `read` のみ付与のクライアントが `noa.sendText`/`noa.focusPane` を呼ぶと `-32003` を返し、pty 注入・フォーカス変更が起きない。
- **AC-6 (FR-6):** `input` 未付与クライアントの `noa.sendText` は `-32003`。`control` 付与のみでは `noa.sendText` も拒否される(input は別枠)。
- **AC-7 (FR-4, FR-19, NFR-5):** `protocolVersion` major が不一致の `noa.hello` は `-32006` で拒否され、一致時のみ `grantedScopes` が返る。未知フィールド付きリクエストはエラーにならず無視される。
- **AC-8 (FR-7):** `noa.listPanels` が全パネルを返し、各要素が `paneId` と name/cwd/branch/process/busy/attention/preview を含む。ID は 10 進文字列。
- **AC-9 (FR-8):** `noa.getText source=screen` は可視行のみ、`source=scrollback` は scrollback+可視画面の全体を返し、いずれも実端末内容と一致する。`maxBytes` 超過分は末尾優先で切り詰められ `truncated:true` が立つ(一括無限ダンプは発生しない)。
- **AC-10 (FR-9, NFR-4):** `noa.getGrid startRow/rowCount` が指定行レンジのみを色ラン付きで返し、レンジ外行を含まない。全画面超のグリッドは `hasMore:true` でページングされ、単一応答が上限内。
- **AC-11 (FR-10..13):** `focusPane`/`newTab`/`split`/`closePane` がそれぞれフォーカス変更・タブ生成・分割・クローズを実行し、生成系は新 `paneId` を返す。存在しない `paneId` または `windowId` の指定は `-32002`。
- **AC-12 (FR-14):** `input` 付与クライアントの `noa.sendText` 後、対象パネルの pty にテキストが到達しシェルが受理する。
- **AC-13 (FR-15, FR-16):** `state_changed` 購読中にパネルの busy/attention/name が変化すると `noa.stateChanged` が差分 `panels` で届く。`unsubscribe` 後は届かない。
- **AC-14 (FR-17):** `output` 購読中にパネル出力が更新されると `noa.output` が色ラン付き行差分で届く。
- **AC-15 (FR-18):** 消滅済みパネルへの操作は `-32004`、上限超過ペイロードは `-32005` を返す。
- **AC-16 (NFR-2):** pty 出力 10MB/s・60 秒のフラッド下で、`output` 購読クライアント有無による feed スループット(バイト/秒)の低下が **5% 以内**。購読チャネル溢れ時は drop-oldest され次通知に `dropped:true` が立つ。
- **AC-17 (NFR-2):** 応答を読まず stall したクライアント 1 接続 + pty 出力 10MB/s・60 秒の条件下で、メインスレッドの **p99 フレーム時間の増分がベースライン比 ≤5% かつ ≤1ms**。
- **AC-18 (NFR-3):** 実装は tokio 等の async ランタイムに依存せず(`cargo tree` に非出現)、接続ごとに 1 スレッド + crossbeam チャネルで動作する。
- **AC-19 (NFR-4):** `getGrid` が巨大 scrollback を一括ダンプせず、ページング/レンジで有界応答を返す。dirty 合流間隔が ≥16ms。
- **AC-20 (FR-6, NFR-1):** `server-scopes` 未設定(default)の起動では `noa.hello` で `control`/`input` を要求しても `grantedScopes` は `["read"]` のみ。`server-scopes = read,input` 設定時は `input` が付与され `control` は付与されない。
- **AC-21 (FR-19, NFR-5):** 未知メソッド(例 `noa.nonexistent`)の呼び出しは `-32601` を返し、同一接続上の後続の既知メソッドは正常に処理される(接続維持)。

## Scope

**In-scope (v1):**
- サーバーライフサイクル(`server-enable` gate / loopback bind / トークン自動生成 + `server-token` override)。
- handshake + `protocolVersion` 交換 + Bearer トークン認証 + 3 スコープ(`read`/`control`/`input`)認可。
- RPC: `listPanels` / `getText` / `getGrid`(行レンジページング・色ラン付き) / `sendText` / `focusPane` / `newTab` / `split` / `closePane`。
- 購読: `subscribe`/`unsubscribe` → `stateChanged` + `output`(色ラン行差分・`dropped` マーカー)。
- config `server-enable`/`server-port`/`server-token`/`server-scopes`(default `read` のみ)、NFR-1..5、`noa-ipc` クレート。

**Out-of-scope (v1):**
- LAN 直接 bind / in-process TLS(iOS はトンネル終端)/ UDS トランスポート(OQ-1 の degrade 調査対象としてのみ言及)。
- keybind 全語彙パススルー / per-client 購読ポリシー。
- 生 VT ストリーム配信 / PTY attach / CRDT・オフライン同期 / multi-noa リモーティング。

## Open Questions

- **OQ-1 (sandbox bind):** sandbox 実行下で loopback TCP bind が許可されるか未検証(Omen ⑦)。不可の場合の degrade(UDS フォールバック等)は実装フェーズで確認・判断する。
- **OQ-2 (token 失効/回転):** トークンの回転・失効フロー(再生成 CLI/設定リロード時の扱い)は v1 未定。初回生成 + `server-token` override のみで運用開始し、回転は v2 検討。
- **OQ-3 (応答上限値):** `getGrid`/`getText` の 1 応答上限(暫定 256KB)は実測でチューニング要。`-32005` の閾値は config 化するか未決。
- **OQ-4 (subscribe 認可粒度):** 購読は `read` 一括付与で開始。パネル単位の細粒度購読ポリシーは out-of-scope(将来 opt-in)。

(解決済み: ASSUME-1/ASSUME-2 は CHALLENGE 確定判断で loopback TCP + iOS トンネル終端に決着し、FR-2/FR-5/NFR-1 へ昇格。)

## Decision Ledger (Nexus 裁量判断, 全て可逆)

| ID | 判断 | 根拠 |
|----|------|------|
| DEC-1 | 依存スタック = sync tungstenite + thread-per-connection + crossbeam(tokio 不採用) | コードベースは std-threads + crossbeam のみ。非同期パラダイム持込回避 |
| DEC-2 | `getGrid` ページング = 行レンジ単位 | グリッドの自然単位。タイルより単純 |
| DEC-3 | `server-port` 既定 = 固定値 61771 | 探索ファイル方式は v2 検討 |
| DEC-4 | スコープ付与機構 = `server-scopes` config キー(default `read` のみ) | Quality Gate blocker 解消。input 明示 opt-in 判断と整合 |
| DEC-5 | `getText` は `maxBytes`(既定 256KB)で tail 優先切り詰め + `truncated` | Quality Gate minor 解消。NFR-4 整合 |

## Assumption Ledger

| ID | 内容 | 状態 |
|----|------|------|
| ASSUME-1 | スコープ外の明示(remote access / ピクセル共有 / 認証省略)はユーザー未回答 → 未決 | resolved (CHALLENGE 確定判断; Scope 節へ) |
| ASSUME-2 | iOS クライアント要件により localhost/unix-socket 限定は不成立の可能性。ネットワーク待受+認証が必要かは未確定 | resolved (loopback TCP + iOS トンネル; FR-2/FR-5) |

## EXPAND — 候補方向 (Riff + Flux, 2026-07-11)

- **A. Control-Mode 行プロトコル**(tmux/kitty 系): Unix socket + 行指向テキスト、`%` 通知で push。最速・依存ゼロだが独自フレーミングの保守税、iOS 向け型付けが手作業。
- **B. JSON-RPC over WebSocket**(LSP/DAP 系): メソッド + サーバー通知で request/response と購読 push を一本化。iOS/ダッシュボードに素直。serde+tokio+ws の新規依存。
- **C. gRPC/Protobuf mux サーバー**(wezterm 系): server-streaming が一級。remote-window 志向で最強型付けだが最重量(tonic/protoc/証明書)。
- **D. HTTP REST + SSE**: 読み/アクション=REST、push=SSE。クライアント到達性最大だが双方向高頻度に弱い。
- **E. PTY attach 方式**(Flux: "The terminal IS the wire"): ペインを detach 可能 PTY として公開し、クライアントは VT バイト列を受けて自前描画(+入力/winsize 返し)。push は pty read loop そのもので無料。iOS=薄い VT レンダラ。リスク: 描画経路の二重化・生バイト列の認証/背圧。
- **F. CRDT セッションドキュメント**(Flux): パネル状態を複製ドキュメント化し全ピア同期。オフライン閲覧・マルチデバイスが自然に出るが、依存重量と設計の野心度が最大。

## 経過

- 2026-07-11 FRAME: 問題ステートメント確認済み。EXPAND へ。
- 2026-07-11 EXPAND: 候補 A〜F 生成(Riff A-D / Flux E-F)。
- 2026-07-11 CHALLENGE 入口: ユーザーが **B (JSON-RPC over WebSocket)** を単独選択。閲覧深度 = メタデータ+プレビュー / 画面全体テキスト / 色・属性付きグリッド(生 VT ストリームは不採用)。

## CHALLENGE — ストレステスト結果 (2026-07-11)

**Void+Magi (スコープ):** v1 から TLS・トークン認証(UDS 案なら)・TCP 待受・per-client 購読ポリシー・keybind 全語彙パススルーを削減。不可欠 = `list_panels` / 全文テキスト取得 / `send_text`+`focus_pane` / WS push(output+state_changed)。

**Ripple (実現性):** 新クレート `noa-ipc`(仮)。既存 2 シーム(`AppStateSnapshot`+`about_to_wait` 公開 / `EventLoopProxy<UserEvent>` 注入)流用で blast radius 低〜中。依存は **sync tungstenite + thread-per-connection + crossbeam**(tokio は非同期パラダイム持込のため却下)。output push は `feed.rs` の feed 後タップ(O(1)・追加ロックなし)、state は `sidebar.rs` の抽出流用。直列化は dirty 合流 ≥16ms・viewport/range 有界。

**Omen (pre-mortem, RPN 順):** ①無認証 LAN bind=入力注入 RCE(432) ②output 洪水で io_thread/クライアント溶解(245) ③slow client 背圧で端末停止(200) ④proxy 洪水(180) ⑤大 scrollback 直列化ジャンク(150) ⑥iOS と version skew(120) ⑦sandbox で bind 不可(63)。

**NFR 候補(スペックに必須):**
1. デフォルト loopback/UDS 限定 + 全メソッドに認可。入力/変異系は読取と別スコープ(opt-in)。
2. 端末/io_thread はクライアントを絶対に待たない — 有界 `try_send`・drop-oldest + 欠落マーカー。
3. 非同期ランタイム不使用 — sync tungstenite + thread-per-connection(io_thread と同一並行モデル)。
4. 直列化は有界 — viewport/range 限定・ページング・dirty 合流 ≥16ms。全 scrollback 一括ダンプ禁止。
5. プロトコルはバージョン付き・additive-only — handshake に `protocolVersion`、major 不一致は拒否。

## CHALLENGE — 確定判断 (ユーザー裁定 2026-07-11)

| 論点 | 決定 |
|------|------|
| トランスポート/認証 | **loopback TCP (127.0.0.1) の WS + 必須トークン**。iOS はトンネル(SSH/Tailscale 等)経由。LAN 直受けは v2 の opt-in |
| 色/属性付きグリッド | **v1 に含める**(viewport 限定・ページング必須。Void の v2 送り推奨をオーバーライド) |
| push の output 形式 | **構造化行/プレビュー差分**(sidebar 抽出流用、クライアント VT 解釈不要) |
| アクション語彙 | **最小 5 種**: focus_pane / new_tab / split / close_pane / send_text |
| 依存スタック | **sync tungstenite + thread-per-connection + crossbeam**(DEC-1, 技術判断: tokio 不採用) |

## Considered but rejected (EXPAND 選別)

- A. Control-Mode 行プロトコル — 独自フレーミング保守税、型付きクライアント(iOS)に不向き。
- C. gRPC mux — 重量級(tonic/protoc/mTLS)、近時ニーズに過剰。
- D. REST+SSE — 双方向高頻度に弱い、2 つのメンタルモデル。
- E. PTY attach — 生 VT 配信は不採用(クライアント側 VT 解釈を要求するため)。
- F. CRDT ドキュメント — 野心度・依存過大。「scrollback=ログ/viewport=スナップショット」の状態分割の発想のみ設計に拝借可。
