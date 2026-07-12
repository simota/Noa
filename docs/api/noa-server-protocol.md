# noa-server クライアント API 仕様 (protocolVersion 1)

外部クライアント(CLI・ダッシュボード・iOS アプリ等)から noa に接続するためのプロトコルリファレンス。
運用手順(有効化・トークン・トラブルシューティング)は `docs/runbooks/noa-server.md`、設計背景は `docs/specs/noa-server.md` を参照。

## 1. トランスポート

- **WebSocket over TCP**: `ws://127.0.0.1:<server-port>/`(既定ポート `61771`)。TLS なし(loopback 限定 bind。リモートからは SSH/Tailscale 等のトンネル終端で到達させる)。
- メッセージは WS テキストフレームの **JSON-RPC 2.0**。1 メッセージ ≤ 1 MiB、1 フレーム ≤ 256 KiB(超過で接続クローズ)。
- 同時接続上限 32(超過 accept は即クローズ)。
- **接続期限**: WS handshake は接続から 5 秒以内(絶対期限。数バイトずつ小刻みに送り続けて 1 回ごとの読み取りタイムアウトを回避する接続でも、この期限は超えられない)、`noa.hello` の成功は接続から 10 秒以内。超過した接続はサーバー側でクローズされる。

## 2. JSON-RPC 規約

- リクエスト: `{"jsonrpc":"2.0","id":<number|string>,"method":"...","params":{...}}`
- 成功応答: `{"jsonrpc":"2.0","id":<echo>,"result":{...}}` / 失敗: `{"jsonrpc":"2.0","id":<echo>,"error":{"code":...,"message":"..."}}`
- サーバー→クライアント通知は `id` なし: `{"jsonrpc":"2.0","method":"noa.stateChanged","params":{...}}`
- **前方互換 (additive-only)**: 既知メソッドの未知フィールドはエラーにならず無視される。未知メソッドは `-32601` を返すが**接続は維持される**。破壊的変更時のみ `protocolVersion` の major が上がる。クライアントは未知フィールド・未知通知を無視するよう実装すること。
- `id` は仕様上 `number | string` のみ。それ以外の `id`(欠落・`null`・オブジェクト・配列・真偽値)は `noa.hello` を含む**全メソッドで** `-32600` `InvalidRequest` となり dispatch されない(副作用のあるメソッドも実行されない)。接続は維持される。

### ID 表現

`windowGroupId` / `windowId` / `paneId` / `subscriptionId` は u64 だが、wire 上は **10 進文字列**(例 `"42"`)。JS の安全整数(2^53)を超え得るため。受信は文字列・整数どちらも許容されるが、送信は文字列を推奨。ID の階層:

```
windowGroup (論理ウィンドウ) ─▶ window (ネイティブタブ) ─▶ pane
```

`paneId` はサーバーセッション内で安定・再利用されない。パネルが閉じられた後の使用は `-32002`。

## 3. 接続確立フロー

1. WS upgrade。任意で `Authorization: Bearer <token>` ヘッダ(送ると事前認証)。
2. **`noa.hello` を最初に送る**(必須)。ヘッダ未使用なら `params.token` でトークン提示。
3. hello 成功後、`grantedScopes` の範囲でメソッド呼び出し可能。

hello 前の他メソッドは `-32001`。major 不一致は `-32006`。

## 4. スコープ

| スコープ | 対象メソッド |
|---------|-------------|
| `read` | listPanels / getText / getGrid / subscribe / unsubscribe |
| `control` | focusPane / newTab / split / closePane |
| `input` | sendText |

`grantedScopes` = hello の `scopes`(要求)∩ サーバー設定 `server-scopes`。`control`/`input` はサーバー側で明示許可されている場合のみ付与される。未付与スコープのメソッドは `-32003`。

## 5. メソッドリファレンス

### noa.hello

| params | 型 | 必須 | 説明 |
|--------|----|------|------|
| `protocolVersion` | number | ✓ | クライアントの major。現行 `1` |
| `token` | string | ヘッダ認証時は省略可 | Bearer トークン |
| `scopes` | string[] | — (省略 = `[]`) | 要求スコープ |

result: `{"protocolVersion":1,"grantedScopes":["read"],"serverVersion":"0.1.2"}`

### noa.listPanels — 要 read

params: `{}` / result: `{"panels":[Panel]}`(全ウィンドウグループ配下の全パネル。Quick Terminal のパネルはサイドバー同様に対象外)

### noa.getText — 要 read

| params | 型 | 必須 | 説明 |
|--------|----|------|------|
| `paneId` | string | ✓ | |
| `source` | `"screen"` \| `"scrollback"` | ✓ | screen=可視画面のみ / scrollback=scrollback+可視画面の全体 |
| `maxBytes` | number | — (既定 262144) | UTF-8 バイト上限。サーバー側で **1 MiB (1048576 バイト) にクランプ**される(それ以上を要求してもリジェクトはされない) |

result: `{"paneId":"1","text":"..."}` — 上限超過時は**末尾優先**で切り詰められ `"truncated":true` が付く(`truncated` は true のときのみ出現)。

### noa.getGrid — 要 read

| params | 型 | 必須 | 説明 |
|--------|----|------|------|
| `paneId` | string | ✓ | |
| `startRow` | number | ✓ | 絶対行。行 0 = scrollback 最古行 |
| `rowCount` | number | ✓ | 1 リクエスト実効上限 2048 行 |

result: `{"paneId":"1","cols":80,"startRow":0,"rows":[Row],"hasMore":false}`

応答は直列化 256 KiB 以内に丸められる。`hasMore:true` なら `startRow = 前回startRow + rows.length` で続きを取得。1 行単体が上限を超える場合は `-32005`。

### noa.sendText — 要 input

params: `{"paneId":"1","text":"ls\n","paste":true}` / result: `{"ok":true}`

UTF-8 テキストを対象パネルの pty へ注入する。改行を送るには `\n` を含める。

- `paste`(省略可。既定 `true`): パネルが bracketed paste モード中は自動的に paste としてラップされる、既存の挙動。
- `paste:false`: bracketed paste ラップなしの生注入。`text` の UTF-8 バイト列がそのまま pty に書き込まれる。単独の Enter を送りたい場合は `{"text":"\r","paste":false}`(TUI アプリへのキー入力エミュレーション用途)。

### noa.focusPane — 要 control

params: `{"paneId":"1"}` / result: `{"ok":true}` — ウィンドウ前面化+パネルフォーカス。

### noa.newTab — 要 control

params: `{"windowId":"..."}`(省略可。`windowId` またはウィンドウグループ id のどちらでも解決される。省略時はアクティブウィンドウ)
result: `{"paneId":"7"}` — 生成されたタブの初期パネル id。

### noa.split — 要 control

params: `{"paneId":"1","direction":"horizontal"|"vertical"}` — horizontal=左右、vertical=上下。
result: `{"paneId":"8"}` — 生成ペイン id。

### noa.closePane — 要 control

params: `{"paneId":"1"}` / result: `{"ok":true}`

`control` スコープは認可済み自動化とみなされ、実行中プロセスがあっても GUI の確認ダイアログを経由せず即座にペインを閉じる(通常の cmd+w 等が出す確認ダイアログはスキップされる)。`ok:true` はクローズが実際にディスパッチされた後にのみ返る。

### noa.subscribe — 要 read

| params | 型 | 必須 | 説明 |
|--------|----|------|------|
| `events` | (`"state_changed"` \| `"output"`)[] | ✓ | 購読するイベント種別 |
| `paneIds` | string[] | — | 省略 = 全パネル。指定時は `state_changed` / `output` 両方のイベントをこの集合にフィルタする |

result: `{"subscriptionId":"1"}`

接続ごとに最大 16 件まで(`unsubscribe` されたぶんは即座に枠が空く)。超過した `subscribe` 呼び出しは接続を切らずに `-32005`("subscription limit exceeded")を返す。

### noa.unsubscribe — 要 read

params: `{"subscriptionId":"1"}` / result: `{"ok":true}`

### ミューテーションの実行セマンティクス

`sendText` / `focusPane` / `newTab` / `split` / `closePane` は UI スレッドへの往復で実行され、2 秒でタイムアウトすると `-32603`(internal)を返す。**タイムアウト後も操作が遅延実行される可能性がある(at-least-once)**。失敗応答での盲目的リトライは二重実行になり得る。

## 6. 通知 (サーバー → クライアント)

### noa.stateChanged

```json
{"jsonrpc":"2.0","method":"noa.stateChanged","params":{"panels":[Panel]}}
```

パネルメタデータ変化時に**変化した/追加された Panel のみ**を配信。busy / attention / name の変化は即時、cwd / preview の変化は最大 500ms 遅延で反映。**パネル削除の通知は v1 には無い** — 既知 paneId への操作が `-32002` を返したら `noa.listPanels` で再同期すること。`subscribe` の `paneIds` を指定した場合、この配列はその集合に含まれる Panel のみへフィルタされる(集合内の変化が 0 件なら通知自体を送らない)。オプションで `"dropped":true` が付くことがある(`output` と同じ、購読キュー溢れ時のマーカー — 後述)。

### noa.output

```json
{"jsonrpc":"2.0","method":"noa.output","params":{"paneId":"1","lines":[Row]}}
```

パネル出力の更新を **≥16ms 間隔に合流した可視領域の変化行のみ**(色ラン付き)で配信。`Row.row` は絶対行番号。行の全置換として扱う(パッチではない)。

### dropped マーカー

購読キューが溢れると古い通知から破棄され、同種の次の通知に `"dropped":true` が付く(true のときのみ出現)。受信したらその購読対象の完全な状態を `listPanels` / `getGrid` で再取得することを推奨。

## 7. データ型

### Panel

```json
{
  "windowGroupId": "1", "windowId": "140234...", "paneId": "3",
  "name": "zsh", "cwd": "/Users/me/src",
  "branch": "main", "process": "vim",
  "busy": true, "attention": false,
  "preview": [Row]
}
```

`branch` / `process` は不明時**キーごと省略**される。`preview` はサイドバー相当の末尾数行(色ラン付き)。`preview` の各 `Row.row` は**絶対行番号ではなく** 0 始まりのプレビュー行インデックス(先頭行が 0)— `noa.getGrid` の `Row.row`(絶対行)とは意味が異なるので注意。

### Row / Span

```json
{ "row": 120, "spans": [
    { "text": "cargo build", "fg": "#c6d0f5", "attrs": ["bold"] },
    { "text": " done", "fg": 2 }
] }
```

| Span フィールド | 型 | 説明 |
|----------------|----|------|
| `text` | string | 同一スタイル連続セルを畳んだテキスト |
| `fg` / `bg` | `"#rrggbb"` \| number | truecolor は hex 文字列、パレット色は 0-255 の整数。**端末デフォルト色はキー省略** — クライアントのテーマ既定色で描画する |
| `attrs` | string[] | 省略 = なし。値: `bold` `faint` `italic` `underline` `double_underline` `curly_underline` `dotted_underline` `dashed_underline` `blink` `inverse` `invisible` `strikethrough` `overline` |

## 8. エラーコード

| code | 意味 |
|------|------|
| `-32700` / `-32600` / `-32601` / `-32602` / `-32603` | JSON-RPC 標準 (parse / invalid request / method not found / invalid params / internal) |
| `-32001` | 認証失敗(トークン不一致・hello 前のメソッド呼び出し) |
| `-32002` | 不明な paneId / windowId |
| `-32003` | スコープ不足 |
| `-32004` | 実行中にパネル消滅 |
| `-32005` | ペイロード超過(リクエスト/応答) |
| `-32006` | protocolVersion major 不一致 |

## 9. セッション例(全文)

```json
→ {"jsonrpc":"2.0","id":1,"method":"noa.hello","params":{"protocolVersion":1,"token":"<hex64>","scopes":["read","input"]}}
← {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"grantedScopes":["read","input"],"serverVersion":"0.1.2"}}
→ {"jsonrpc":"2.0","id":2,"method":"noa.listPanels","params":{}}
← {"jsonrpc":"2.0","id":2,"result":{"panels":[{"windowGroupId":"1","windowId":"105553...","paneId":"1","name":"zsh","cwd":"/Users/me","busy":false,"attention":false,"preview":[]}]}}
→ {"jsonrpc":"2.0","id":3,"method":"noa.subscribe","params":{"events":["output"],"paneIds":["1"]}}
← {"jsonrpc":"2.0","id":3,"result":{"subscriptionId":"1"}}
→ {"jsonrpc":"2.0","id":4,"method":"noa.sendText","params":{"paneId":"1","text":"echo hi\n"}}
← {"jsonrpc":"2.0","id":4,"result":{"ok":true}}
← {"jsonrpc":"2.0","method":"noa.output","params":{"paneId":"1","lines":[{"row":42,"spans":[{"text":"hi"}]}]}}
```

## 10. クライアント実装チェックリスト

- [ ] 未知フィールド・未知通知を無視する(FR-19 前提)
- [ ] ID は文字列として保持(u64 を number にパースしない — 2^53 超過あり)
- [ ] `-32002` 受信時に `listPanels` で再同期(削除通知は無い)
- [ ] `dropped:true` 受信時にフル再取得
- [ ] ミューテーション失敗時の自動リトライは at-least-once を考慮
- [ ] `fg`/`bg` 省略・`truncated`/`dropped`/`hasMore` の条件付き出現を処理
- [ ] 再接続時は hello からやり直し(購読は接続ごとに消える)
