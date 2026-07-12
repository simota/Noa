# noa-server 運用 Runbook

対象: `noa-ipc` — JSON-RPC 2.0 over WebSocket サーバー(仕様: `docs/specs/noa-server.md`)。
クライアントから稼働中の noa に接続し、パネル一覧・テキスト/グリッド取得・操作・入力・リアルタイム購読を行う。

## 1. 有効化

デフォルトでは**完全に無効**(ポートを一切開かない)。`~/.config/noa/config`(または `$XDG_CONFIG_HOME/noa/config`)に:

```
server-enable = true
# 以下は省略可(デフォルト値)
server-port = 61771
server-bind = 127.0.0.1
server-scopes = read
```

| キー | 型 / デフォルト | 意味 |
|------|----------------|------|
| `server-enable` | bool / `false` | サーバー起動ゲート(FR-1) |
| `server-port` | u16 / `61771` | bind ポート(FR-2) |
| `server-bind` | IP アドレス文字列 / `127.0.0.1` | bind するインターフェース。既定は loopback 限定。`0.0.0.0` 等を指定すると LAN 上の他ホストからも直接到達できる(v2 の opt-in。詳細は本節末の「LAN 公開手順」) |
| `server-token` | string / なし | 認証トークンの明示指定。設定時はトークンファイルを生成・読取しない |
| `server-scopes` | csv / `read` | 付与可能スコープの上限。`read,control,input` の部分集合。`control`(focus/tab/split/close)と `input`(sendText)は**明示列挙時のみ**付与可能 |

再起動して有効化を確認:

```sh
lsof -nP -iTCP:61771 -sTCP:LISTEN   # noa が 127.0.0.1:61771 で LISTEN していれば OK
```

bind 失敗(ポート衝突等)ではアプリは落ちず、警告ログのみ:
`noa-ipc: failed to bind <server-bind>:<port>: <err>`。

設定ファイルの直接編集に代えて、Settings パネル(`⌘,` などから開く Settings モード)からも
`server-enable`/`server-port`/`server-bind`/`server-scopes` を操作できる。パネル上の行名は「Server」
「Server Port」「Server Bind」(Server Port の直後)「Server Scopes」で、いずれも Save 適用(ON SAVE
バッジ)——保存すると `ConfigWatcher` がファイル変更を検知し、1 秒以内にサーバーを再起動/停止/再bind
する。「Server Bind」行は `←`/`→` で `127.0.0.1` ⇔ `0.0.0.0` の2値をトグルする(手編集した config の
値がこの2値のどちらでもない場合は、その値をそのまま表示しつつ次の ←/→ で2値のどちらかへ着地する)。
`server-token` の値自体はシークレットのため意図的にパネルへ出していない(§2 の「Server Token」行はコ
ピー専用で、値は一切表示しない)。

「Server」行の直後には読み取り専用の「Server Status」行があり、サーバーの現在状態を
`Running (<bind>:<port>, <N> client(s))` / `Stopped` / `Bind failed: <reason>` の3形式で表示する
(値の編集・Reset は無効、Save 対象外、常に LIVE バッジ)。パネルの描画パスは `App` の生存状態を
直接参照できないため、この行はセッションが開いている間 `App` が `install_ipc_server_if_needed`/
`restart_ipc_server` を再実行するたびに(config reload の `server-enable`/`server-port`/
`server-bind`/`server-scopes` 変更を検知した際を含む)明示的に書き戻す方式——パネル上のトグルを保存
してから実際の表示更新まで、`ConfigWatcher` の 500ms ポーリング1周分以内に反映される。

### LAN 公開手順

既定の loopback 限定 bind を、同一 LAN 上の他ホストから直接到達できるよう opt-in で広げる手順。
**TLS は無いため、信頼できるネットワーク(自宅 LAN 等)でのみ使うこと**。信頼できないネットワーク
(公衆 Wi-Fi・オフィス共有ネットワーク等)では従来どおり SSH ポートフォワード/Tailscale 等のトンネ
ル経由にすること — トークン認証だけでは平文通信そのものは守れない。

1. `~/.config/noa/config` に `server-bind = 0.0.0.0`(または特定インターフェースの IP)を追記(Settings
   パネルの「Server Bind」行から `←`/`→` で切り替えても同じ)。
2. noa を再起動、または `server-enable` が既に true なら config 保存後 1 秒以内に自動で再bindされる。
3. bind を確認:
   ```sh
   lsof -nP -iTCP:61771 -sTCP:LISTEN   # ADDRESS が *:61771 になっていれば 0.0.0.0 bind 成功
   ```
4. クライアント側から LAN IP で接続(トークンは loopback 時と同じものを使う):
   ```sh
   websocat ws://<LAN-IP>:61771/
   ```
5. 不要になったら `server-bind` を消す(または `127.0.0.1` に戻す)— デフォルトへ復帰する。



## 2. トークン

- `server-token` 未設定なら初回起動時に自動生成: **`~/.config/noa/server-token`**(権限 0600、hex 64 文字)。
- 権限が 0600 より緩いファイルを検出すると自動修復(chmod 0600 + 警告ログ)。
- 回転(v1): ファイルを削除して noa を再起動 → 新トークン生成。接続中クライアントには影響しない(認証は接続確立時のみ)。

```sh
TOKEN=$(cat ~/.config/noa/server-token)
```

Settings パネル(Settings モード)の「Server Token」行(Server Scopes の直後)は、`←`/`→` または
`Enter` で押すたびに今使われるトークン——`server-token` が設定されていればその値、なければ
上記のファイル(未作成なら自動生成)——をクリップボードへコピーする。値自体は画面に一切表示せず、
行の表示は「Copy to clipboard」→「Copied ✓」(失敗時は「Copy failed」)の3状態のみ。値を持たない
アクション行のため Save 対象外で、常に LIVE バッジ(押した瞬間に完了、保存不要)。

## 3. 接続と handshake

認証は 2 方式(どちらか):
1. WS upgrade 時の `Authorization: Bearer <token>` ヘッダ
2. 接続直後の `noa.hello` の `params.token`

いずれの場合も**最初に `noa.hello` が必須**(他メソッドは `-32001`)。`protocolVersion` は現行 `1`(major 不一致は `-32006`)。

```sh
# websocat (brew install websocat) での対話例
websocat ws://127.0.0.1:61771/
{"jsonrpc":"2.0","id":1,"method":"noa.hello","params":{"protocolVersion":1,"token":"<TOKEN>","scopes":["read","control","input"]}}
# → {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"grantedScopes":["read"],"serverVersion":"0.1.2"}}
```

`grantedScopes` = 要求スコープ ∩ `server-scopes`。デフォルト設定では `control`/`input` を要求しても `["read"]` しか返らない(AC-20)。

## 4. メソッド早見表

ID(`windowGroupId`/`windowId`/`paneId`)は全て **10 進文字列**。

```json
{"jsonrpc":"2.0","id":2,"method":"noa.listPanels","params":{}}
{"jsonrpc":"2.0","id":3,"method":"noa.getText","params":{"paneId":"1","source":"scrollback","maxBytes":65536}}
{"jsonrpc":"2.0","id":4,"method":"noa.getGrid","params":{"paneId":"1","startRow":0,"rowCount":50}}
{"jsonrpc":"2.0","id":5,"method":"noa.sendText","params":{"paneId":"1","text":"ls\n"}}
{"jsonrpc":"2.0","id":6,"method":"noa.focusPane","params":{"paneId":"1"}}
{"jsonrpc":"2.0","id":7,"method":"noa.newTab","params":{"windowId":"..."}}
{"jsonrpc":"2.0","id":8,"method":"noa.split","params":{"paneId":"1","direction":"horizontal"}}
{"jsonrpc":"2.0","id":9,"method":"noa.closePane","params":{"paneId":"1"}}
{"jsonrpc":"2.0","id":10,"method":"noa.subscribe","params":{"events":["state_changed","output"],"paneIds":["1"]}}
{"jsonrpc":"2.0","id":11,"method":"noa.unsubscribe","params":{"subscriptionId":"..."}}
```

補足:
- `getText` の `source`: `screen` = 可視画面のみ / `scrollback` = scrollback+可視画面全体。`maxBytes`(既定 256KiB、サーバー側上限 1MiB クランプ)超過は**末尾優先**で切り詰め、`truncated:true`。
- `getGrid`: 行 0 = scrollback 最古行の絶対座標。1 リクエスト最大 2048 行 + 応答 256KiB 上限。切れた場合 `hasMore:true` → `startRow` を進めて続きを取得。
- `sendText` の `paste`(省略可、既定 `true`): `false` で bracketed paste ラップなしの生注入。単独 Enter を送るなら `{"text":"\r","paste":false}`。
- `newTab` の `windowId`: ネイティブウィンドウ id・ウィンドウグループ id のどちらでも解決される。省略時はアクティブウィンドウ。
- `split` の `direction`: `horizontal` = 左右分割 / `vertical` = 上下分割。
- 通知: `noa.stateChanged`(変化した Panel のみ)/ `noa.output`(変化行のみの色ラン付き差分、≥16ms 合流)。購読チャネル溢れ時は古いものから破棄され、次通知に `dropped:true`。

## 5. エラーコード

| code | 意味 | 主な対処 |
|------|------|---------|
| `-32001` | 認証失敗 / hello 前のメソッド呼出 | トークン確認・先に `noa.hello` |
| `-32002` | 不明な paneId/windowId | `noa.listPanels` で再取得(パネルは閉じられると消える) |
| `-32003` | スコープ不足 | `server-scopes` に必要スコープを追加して noa 再起動 + hello で要求 |
| `-32004` | 実行中にパネル消滅 | リトライ不要、対象喪失 |
| `-32005` | ペイロード超過 | `maxBytes`/`rowCount` を下げる |
| `-32006` | protocolVersion major 不一致 | クライアント更新 |
| `-32601` | 未知メソッド | 接続は維持される(additive-only 互換動作) |

## 6. 運用上の注意

- **露出範囲**: 既定は `127.0.0.1` 限定 bind。`server-bind`(例 `0.0.0.0`)で LAN 直 bind への opt-in が可能(v2、本節冒頭「LAN 公開手順」参照)——ただし TLS は依然として無いため平文通信になる点は変わらない。信頼できないネットワークからのリモート(iOS 等)アクセスは、LAN bind を使わず従来どおり SSH ポートフォワード / Tailscale 等のトンネル経由で到達させること。
- **変異系スコープは opt-in**: 自動化エージェントに渡すトークンの権限は `server-scopes` で最小化する(閲覧のみなら `read` のまま)。
- **ミューテーションのタイムアウト**: focus/newTab/split/close/sendText は main thread 往復で実行され、2 秒でタイムアウト(Internal error)。**タイムアウト後も遅延実行される可能性がある**(at-least-once)。エラー時に盲目的リトライすると二重実行になり得る点に注意。
- **性能**: 端末側はクライアントを待たない設計(有界 try_send + drop-oldest)。stall したクライアントは通知を欠落する(`dropped:true`)だけで描画・pty には影響しない。
- **接続上限**: 同時 32 接続。超過分は即クローズ。1 メッセージ 1MiB / 1 フレーム 256KiB 上限。購読上限 16/接続(`noa.subscribe` 超過は接続を切らず `-32005`)。
- **config reload での反映**: `server-enable`/`server-port`/`server-bind`/`server-token`/`server-scopes` の変更は config ファイル書き換え(500ms ポーリング)で即座に反映される — サーバーを再起動(既存接続は ~50ms 以内に自己終了)し、新しい設定で再バインドする。無効化した場合はそのまま起動しない。それ以外のキー(server 系以外)は再起動しない。broadcaster はアプリのライフタイムで 1 つだけ保持され、サーバー再起動をまたいで使い回されるため、再起動より前に spawn 済みのペインの出力プッシュも、再起動後の `noa.output` 購読者へ引き続き届く(ペインの開き直しは不要)。**サーバーがずっと無効だった状態から `server-enable` を有効化した場合も同様**: 既に開いていたペインは無効な間もそれぞれ出力プッシュ用のタップを保持しており(タップの有無ではなく購読者の有無でゲートされる)、有効化後にクライアントが `noa.subscribe` で `output` を購読すれば、その場で開き直しなしに配信対象になる。
- **Quick Terminal は非対象**: サイドバー除外と同じ理由で、Quick Terminal のペインは `noa.listPanels` にも出力プッシュにも現れない(v1 の意図的な仕様)。
- **closePane は確認をスキップする**: `noa.closePane` は `control` スコープ = 認可済み自動化とみなし、実行中プロセスがあっても GUI の確認ダイアログを出さず即座にペインを閉じる(cmd+w 等の通常操作が出す確認ダイアログとは異なる)。誤ったペイン id を渡すと確認なしで作業中プロセスごと閉じるため、自動化側で対象 id の妥当性を確認してから呼ぶこと。

## 7. トラブルシューティング

| 症状 | 確認 |
|------|------|
| ポートが開かない | `server-enable` が true か / ログに `noa-ipc: failed to bind` がないか / `lsof -iTCP:61771` |
| 接続即切断 | メッセージ/フレームサイズ上限(1MiB/256KiB)超過、接続数 32 超過、または `noa.hello` を接続から 10 秒以内に完了していない(handshake 自体は 5 秒期限) |
| 全メソッドが -32001 | `noa.hello` を先に送っているか / トークンがファイルと一致するか(`server-token` 設定時はそちらが優先) |
| sendText が -32003 | `server-scopes` に `input` が明示列挙されているか(`control` では不可) |
| stateChanged が来ない | `subscribe` の `events` に `state_changed` を含めたか / busy・attention・name 変化は即時、cwd・preview 変化は最大 500ms 遅延 |
| LAN から繋がらない | `server-bind` がまだ `127.0.0.1`(既定)のままになっていないか(§1「LAN 公開手順」参照)/ macOS のファイアウォールが接続をブロックしていないか(システム設定 → ネットワーク → ファイアウォール、または `noa` アプリの着信接続許可を確認) |
| 開発時: テストが PermissionDenied | `cargo test -p noa-ipc` の TCP テストはサンドボックス内では loopback bind 不可 → サンドボックス無効で実行(noa-pty と同様) |

## 8. 動作確認スモーク(手動)

```sh
# 1. config に server-enable=true を追記して noa を起動
lsof -nP -iTCP:61771 -sTCP:LISTEN                      # LISTEN 確認
TOKEN=$(cat ~/.config/noa/server-token)
websocat ws://127.0.0.1:61771/ <<EOF
{"jsonrpc":"2.0","id":1,"method":"noa.hello","params":{"protocolVersion":1,"token":"$TOKEN","scopes":["read"]}}
{"jsonrpc":"2.0","id":2,"method":"noa.listPanels","params":{}}
EOF
```

`server-enable = false` に戻して再起動 → `lsof` で LISTEN が消えることも確認(FR-1)。
