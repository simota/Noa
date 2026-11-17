# エージェント・オーケストレーション

| | |
|---|---|
| slug | agent-orchestration |
| status | implemented（API/CLI フェーズ、2026-11-16）。owner 未サインオフ。実機での受け入れは未実施（A1・A2・A6） |
| owner | simota |
| as-of | 2026-11-16（HEAD 1d316c7） |
| scope decision | 「エージェント駆動 API」と「人間向け内蔵 UI」の両方。API を先に固定し、UI は後段フェーズ（elicited 2026-11-16） |

## L0：Vision

- Problem：1 つのペインで動く親エージェントが、ほかのペインで動くエージェントを束ねる手段がない。部品はほぼ揃っているが、つながっていない。
  - ワーカーの状態（`running`/`permission`/`input`/`finished`/`error`）は `SessionCard.agent_status` にあるが（`crates/noa-app/src/session_store.rs:140`）、`card_to_panel` が落とすので外から見えない（`crates/noa-app/src/ipc_bridge.rs:604-627`、`docs/AGENT_WORKFLOW.md` 末尾「remote client metadata is unchanged」）。
  - `noa.newTab`/`noa.split` はコマンドも cwd も env も受け取れない（`crates/noa-ipc/src/backend.rs:74-76`）。AppleScript 側には `SurfaceConfig` がある（`crates/noa-app/src/macos_applescript.rs:297`）。
  - ペインの中のプロセスが自分の paneId を知る手段がない。`NOA_SURFACE_ID` は env に入るだけで（`crates/noa-app/src/app.rs:762-777`）、API のどこにも出てこない。
  - シェルから叩けるクライアントがない。`noa_ipc::Client` には `send_text` も `subscribe` もない（`crates/noa-ipc/src/client.rs:120-221`）。
- Job to be done：オーケストレーターがシェルコマンドだけで複数のペインを追加・操作・削除し、タスクの内容に応じて claude / codex / agy を使い分けて遂行できること（elicited 2026-11-16）。1 つのワーカーについては「起動 → 指示を送る → ターンの終わり（または承認待ち）を待つ → 出力を読む → 閉じる」を回す。どのタスクにどのエージェントを使うかはオーケストレーターが決め、Noa は決めない。人間はその全体を Noa の中で一覧し、止まっているワーカーに介入できること。
- Success definition：`Noa +ctl spawn` → `send` → `wait` → `read` → `close` が 1 本のシェルスクリプトとして完走する（`scripts/agent-orchestration-smoke.sh`）。

## Glossary

| Term | Definition | Code identifier |
|---|---|---|
| orchestrator | `Noa +ctl` を実行しているプロセスが属するペイン | `parentPaneId` の指す先 |
| worker | `parentPaneId` を持つペイン | `Surface.orchestration.parent` |
| turn | エージェント状態が 1 回報告されるごとの区切り。`seq` が 1 進む | `SessionCard.agent_seq` |
| terminal state | 待機が解ける状態：`finished`、`permission`、`input`、`error`、プロセス終了 | — |

## 決定

### D1：Panel に 5 フィールドを足す（追加のみ、`PROTOCOL_VERSION` は 3 のまま）

```jsonc
{
  "surfaceId": "0x00c0ffee00c0ffee",          // NOA_SURFACE_ID と同じ値
  "agent": { "kind": "claude", "state": "finished", "detail": "", "seq": 7 },  // kind は claude|codex|agy。既知のエージェントでなく報告もなければキーごと省略。state/detail は報告があるときだけ
  "exit": { "code": 0 },                       // 子が終了しペインが残っているときだけ
  "parentPaneId": "3", "label": "fix-tests"    // spawn 時に指定されたときだけ
}
```

- `kind` は前面プロセス名から決まる（`classify_agent`、`crates/noa-app/src/sidebar.rs:740`）ので、フックを設定していないエージェントでも出る。
- `seq` はペインごとの単調増加カウンタで、OSC 777 の報告（`clear` を含む）が届くたびに 1 進む。前のターンの `finished` が残っていても、「`seq` が N より大きい terminal state」を待てば取り違えない。`agent_status_at` は時刻であって順序の保証がないので使わない。
- バージョンを上げない理由：hello は厳密一致で弾く（`crates/noa-ipc/src/server.rs:1478`）ので、上げると既存の v3 クライアントが全滅する。`Panel` は未知フィールドを拒否しない。
- `noa.stateChanged` は `Panel` の `PartialEq` 差分で配信される（`crates/noa-ipc/src/protocol.rs:163-165`）ので、フィールドを足せば状態変化はそのまま push される。

### D2：`noa.newTab`/`noa.split` に `config` を足す（新メソッドは作らない）

```jsonc
{"paneId":"1","direction":"horizontal",
 "config":{"command":"claude","cwd":"/repo","env":{"K":"V"},"initialInput":"…","waitAfterCommand":true},
 "label":"fix-tests","parentPaneId":"1","focus":false}
```

- AppleScript v2 の `with configuration` と同じ spawn 経路（`ScriptSpawn`）を使う。`macos_applescript` は macOS 外でも no-op shim としてコンパイルされるので、型を移す必要はなかった。
- `config.command` は文字列ではなく argv。シェルを通さずに起動し、`argv[0]` は `config.env.PATH` で引く（portable-pty が子の env の `PATH` を使う）。`+ctl spawn` はオーケストレーター自身の `PATH` を渡すので、GUI アプリの最小 `PATH` で見つからない問題が起きない。
- `config.command`/`initialInput`/`env` のどれかがあれば `control` に加えて `input` スコープも要る。コマンド実行は入力と同じ権限だからである。
- 自動配置は新パラメータではなく `noa.split` の `direction:"auto"` にした（既存の `direction` と排他になるため）。`paneId` を親とする最初の worker は親の右に分割し、2 つ目以降は同じ親を持つ worker のうち最も大きいペインを長辺方向に分割する。どれにも余地がなければ同じウィンドウの新しいタブに開く。`Panel` に寸法がなく、クライアントでは決められないのでアプリ側で選ぶ。
- 拡張を何も含まない要求は従来の `IpcBackend::new_tab`/`split` に流し、拡張付きだけを既定実装つきの `IpcBackend::spawn` に流す。既存のバックエンド実装はソース互換のまま。
- `focus:false` のときは、生成後に元の focus へ戻す。今の IPC split は対象ペインに focus を移してから分割する（`crates/noa-app/src/app/ipc.rs:349-356`）ので、何もしないと人間が打っているペインから focus を奪う。

### D3：待機はサーバーに作らず、クライアントが subscribe で組む

手順は「`subscribe(state_changed, paneIds)` → `listPanels` → 条件判定 → 通知ごとに再判定」。dropped マーカーを受けたら、その通知の状態を判定してから `listPanels` をやり直す（dropped は前の通知が欠けた印で、その通知自体は有効）。`noa_ipc::Client` に `send_text`、`subscribe`、通知の受信を足した。応答待ちの間に届いた通知は捨てずにバッファする（上限 1024、溢れたら次の通知に dropped を立てる）。

- `stateChanged` には削除イベントがないので、閉じたペインは通知では分からない。そのため待機中に通知が 1 秒来なければ `listPanels` で読み直す。完全なポーリングなしという当初の成功条件からはこの点だけ外れる。
- `attention` は、待機中に一度 `false` を観測してから `true` になったときだけ解ける。前のターンの通知が残っているだけでは解けない。`send --wait` は送信前に取ったパネルを基準にするので、送信直後に立った `attention` も取りこぼさない。IPC の `sendText` は、入力が受け付けられたときだけそのペインの通知を既読にする（focus と同じ扱い。終了済みのペインなどで入力が拒まれたら未読のまま）。フックのないエージェントでも、送るたびに `attention` が一度下がるので、2 ターン目以降の `send --wait` も `attention` で解ける。
- 一覧の読み直しは、通知が来ていても前回から 1 秒経てば行う（ほかのペインの通知が続いていても、消えたペインを見落とさないため）。
- `Panel` はセッションカード（最初の出力で作られる）から作るので、そのままでは出力前のワーカーが一覧に出ず、`wait` が閉じたと誤判定する。カードがなくても一覧に載せる（`parentPaneId` か `label` 付きのワーカーに加え、出力前のオーケストレーター自身が `NOA_SURFACE_ID` で自分を見つけられるよう、カードを持てるウィンドウの通常ペインも。quick terminal と scratch ポップアップ、float は除く）。spawn は応答の前に一覧を作り直す。
- `seq` は出力バッチではなく報告の数を数える（同じバッチに 2 回報告が入っても 2 進む）。
- 終了して残されたペイン（`Panel.exit` あり）は、消えたペインと同じく二度と報告しないので、`--for` に関係なく `exit` で待機を解く。ただし待機の開始時点ですでに終了していたペインは、`--all` なしの待機ではほかに待つペインが残っている限り待機を解かない（そうしないと、残された 1 つが以降の `wait --children` をすべて即座に終わらせる）。
- 同じペインを重ねて指定しても 1 つとして待つ。巨大な `--timeout` は引数エラーか「期限なし」として扱い、panic しない。
- 生成に失敗した spawn（分割の余地がない固定方向など）は、`focus` の指定にかかわらず元の focus に戻す。分割は余地を確かめる前に対象へ focus を移すため。
- spawn が focused pane を動かしたタブ（spawn 先や、余地のなかった `auto` の候補。OS フォーカスのあるタブも含む）は、新しいペインに focus を渡すタブを除いてすべて元のペインに戻す。新しいペインに focus を渡すとき、そのペインが背面のタブにできたらそのタブを前に出す。macOS で別の OS ウィンドウのグループに新しいタブを作った場合、そのグループで選択中のタブは戻さない（未対応）。
- `auto` の候補は、親と同じタブに限らず、親が起動したすべてのワーカー（新しいタブに溢れたものを含む）から選ぶ。
- `--cwd` の相対パスは CLI 側でオーケストレーターの cwd を基準に絶対パスにしてから送る（Noa は自分のプロセスの cwd で解決するため）。
- 待機中、通知で受け取った状態は一覧の読み直しより先に判定する（読み直しが遷移を上書きしないように）。
- 未対応：ワーカー数の上限は CLI が spawn の前に一覧で数えるだけなので、同じ親から並行して spawn すると超えうる。サーバー側で親ごとに直列化すれば防げるが、GUI なしでは再現できず、まだ入れていない。
- spawn の配置で focus が動いても、未読通知（`attention` とベル）は既読にしない。分割の候補に focus を移すと `focus_pane` が通知を消すため、spawn の前の未読フラグを spawn の後に戻す。
- focus を戻すときにタブを前に出しても、アプリは activate しない（AppleScript 経路の ASSUME-6 と同じく、別のアプリで作業中のユーザーから focus を奪わない）。

### D4：CLI は `Noa +ctl`（既存の `+action` 機構に載せる）

当初は `noa ctl` と書いていたが、バイナリには Ghostty 互換の `Noa +<action>` 機構（`crates/noa-app/src/cli.rs`）が既にあるので、それに合わせた。

| コマンド | 動作 |
|---|---|
| `self` | `NOA_SURFACE_ID` と一致する `surfaceId` のペインを返す |
| `ls [--children]` | Panel 一覧（`--children` は自分が親のものだけ） |
| `spawn [--cwd D] [--env K=V] [--label L] [--input T] [--no-wait-after] [--focus] [--no-board] [--max-children N] [-- CMD…]` | D2 の `direction:"float"` を呼ぶ（D8・D9。ワーカーは常に float）。`parentPaneId` は自分、`focus` は false、cwd は自分の cwd。終了した worker は既定で残す（exit code を読めるように。`--no-wait-after` で無効）。自分の worker が 8 以上なら拒否する（`--max-children N` で変更）。ボードがなければ開く（`--no-board` で開かない）。`--split`/`--tab` は拒否 |
| `send PANE [--enter] [--raw] [--wait …] [--answer] [--any-pane] TEXT\|-` | `noa.sendText`。承認待ち（`permission`）のペインには送らない。送る直前と Enter の直前に確かめる（ボードと同じ理由、D7）。`--answer` で送る。`--enter` は貼り付けの 150 ms 後に `\r` を `paste:false` で送る。送信前の `seq` を出力する。`--wait` は購読してから送信し、そのまま待つ（取りこぼしがない推奨経路） |
| `wait PANE…\|--children [--for finished,permission,input,error,exit,attention] [--after SEQ \| --after PANE=SEQ …] [--all] [--timeout S]` | D3。`--after` を省くと待機開始時点の `seq` を基準にする（別プロセスの `send` 直前の値は知りようがないため。取りこぼしたくなければ `send --wait` か `--after`）。`--after SEQ` は単一ペインのときだけで、複数ペインには `--after PANE=SEQ` を繰り返す。タイムアウト行は待機中の各ペインの基準を `baseline` に出すので、それを `--after` に渡せば、間に届いた報告を取りこぼさずに待ち直せる。消えたペインは `exit`（`closed:true`）として必ず返す |
| `read PANE [--scrollback] [--max-bytes N]` | `noa.getText` |
| `close PANE…\|--children [--any-pane]` | `noa.closePane`。一覧にないペインは `gone:true` をつけて成功とし、1 つ失敗しても残りを閉じる（失敗があれば exit 1）。自分の worker でない生存ペインは、何も閉じる前に拒否する |
| `report STATE [DETAIL]` | 制御端末に `OSC 777;noa-agent` を書く。各エージェントのフックから呼ぶ（D6） |

- 出力は 1 行 1 JSON（`read` だけはテキスト）。終了コードは 0（成立）、1（引数・未知のペインなど）、2（タイムアウト）、3（サーバー無効・接続不可・スコープ不足。直し方の設定行を stderr に出す）。
- 接続先は `noa-config` の `server-bind`/`server-port`（未指定アドレスは同じアドレス族の loopback に読み替え。`0.0.0.0` は `127.0.0.1`、`::` は `::1`。v6only の `::` は IPv4 を受けないため）と、`server-token` またはトークンファイル（`noa_config::server_token_path()`）から引く。トークンファイルは読むだけで作らない。
- 自分のペインが要るコマンド（`self`、`spawn`、`--children`、`send`/`close` の確認）は、自分のペインが一覧にまだなければ最長 3 秒（100 ms 間隔で 30 回）読み直して待つ。`Noa -e` で起動したスクリプトは、ペインが最初に一覧に載る前に走りうるため。一覧に載らないペイン（float など）からの `ls`/`read` は待たない。
- `send`/`close` は、`parentPaneId` が自分でないペインを既定で拒否する（`--any-pane` で解除）。これは誤爆防止であって、セキュリティ境界ではない（下記「採らなかった案」）。
- 子の PATH に `noa` を入れるコードは見つかっていない（`crates/noa-pty/src`、`crates/noa-app/src/app.rs`、`shell-integration/`、`scripts/bundle-macos.sh` を grep して 0 件。実機では未確認、A4）。そのため `NOA_SURFACE_ID` と同じ場所で `NOA_BIN`（実行ファイルの絶対パス）を子に渡す。

### D5：UI フェーズとの継ぎ目は `Surface` と `SessionCard`

当初は 3 つとも `SessionCard` に置く予定だったが、`SessionCard` は io スレッドの delta でしか更新されない。spawn 時に main スレッドで決まる `parent`/`label`（と surface id・終了情報）は `Surface.orchestration` などに置き、報告ごとに増える `agent_seq` だけを `SessionCard` に置いた。`Panel` はどちらも写すだけで、UI フェーズも同じ 2 か所を読む。UI の中身は別 spec（`docs/specs/orchestration-board.md`）で決めるが、前提にする範囲は次のとおり。

- ~~サイドバーで worker カードを orchestrator カードの下に入れ子にし、親に集計（running / 要対応 / 完了 / error）を出す。新しいウィンドウやボードは作らない。~~ D7 で専用ペインに改めた（2026-11-17）。
- Compose Prompt の宛先に「この orchestrator の worker 全部」を足す。送信経路は `+ctl send` と同じ。
- 親が閉じた worker は最上位に戻す。ipc id は再利用されない（`crates/noa-app/src/ipc_bridge.rs:51-60`）ので、古い `parentPaneId` が別のペインを指すことはない。

### D7：UI は専用ペインの TUI `+ctl board`（2026-11-17）

人間向けの UI は、サイドバーの入れ子（D5 の当初案）ではなく、オーケストレーターの横に置く専用ペインにした。画面モックを見たうえで owner が決めた。

- Noa のペインはすべて `Terminal` を持つ `Surface` で、ターミナル以外のペインはない。新しい種類のペインを足すと、レイアウト・入力・描画・セッション保存のすべてに手が入る。float のタイトル帯がすでに UI をセルで描いているのにならい、`Noa +ctl board` という TUI を普通のペインで動かす（owner が TUI 案を選んだ）。
- データは `+ctl` と同じサーバーから読む。0.5 秒ごとに `listPanels`、1 秒ごとに表示中のワーカーの `getText`。購読はしない（描画中に読まれない通知がたまるため）。
- 並びは要対応（permission / input / error / 未読通知）→ 作業中（running と報告なし）→ 完了 → 終了。finished は通知を伴うことが多いので、未読通知より finished を優先する（実機で、完了したワーカーが要対応に出た）。
- 操作は移動（`noa.focusPane`）、送信（`+ctl send --enter` と同じ貼り付け + `\r`）、閉じる（確認つき）。承認はボードからはせず、ペインへ移動して人が答える。承認待ち（permission）のワーカーには送信欄を開かず、入力中に承認待ちになったら送らない。送る直前と、貼り付けから 150 ms 後の Enter の直前にも一覧を取り直して確かめる（Enter が既定の選択肢で承認するプロンプトがあるため）。貼り付けは bracketed paste で受け、改行を含めて送信欄に入れる（改行を Enter として読むと、残りの文字が操作キーとして実行されるため）。
- `--open` で開くボードは `parentPaneId` を持たない。ワーカーとして数えず、`close --children` でも閉じない。
- カードのプレビューは、画面の下部にある行幅いっぱいの区切り線のまとまり（エージェントの入力欄）より上の行を使う。入力欄とステータスラインは作業の中身ではないため。区切り線がなければ画面全体。detail のない報告は、直前の detail か、状態と経過時間を出す。`seq` は報告の数でターン数ではないので「報告 N」と出す。状態が変わった時刻を覚え、「作業中 2 分」「完了 3 分前」と出す（開いた時点の状態は「開始時から」）。
- 報告の履歴は、ボードを開いてから見た報告だけで、ボードの中にしかない。開いた時点の状態は「開始時」と表示する（時刻が分からないため）。
- 未対応：ワーカーの入力欄の書きかけの検出（画面から推定する方法を確かめていない）、GUI のキーバインドやメニューからの起動、履歴の永続化。
- UI 改善（2026-11-17、owner がボードに範囲を絞って依頼）：枠つきカード（1 件 5 行で、8 ワーカーが 1 画面に収まらなかった）を、1 件 1 行の行と、その下に作業の末尾を出す形に改めた（作業の末尾は通常 1 行、選択中は 3 行、完了と終了は選択中のときだけ出す）。名前・種別・状態の列幅はワーカー間で揃え、種別の列は異なるプログラムが混ざるときだけ出す。`pane N` と「報告 N」は詳細画面だけに出す。ヘッダはオーケストレーターをペイン名で出す。キーに `1`〜`9`（番号で選ぶ）、`n`（次の要対応）、`?`（ヘルプ）を足し、フッターは幅に入るだけのキーと `? ヘルプ` を出す。スクロールで隠れたワーカー（行の先頭が見えないものだけを数える）は「↑/↓ あと N 件 · うち要対応 M」と出し、隠れた閉じた欄は「閉じた K 件」と出す。0 件の部分は出さない。最後のワーカーを選ぶと閉じた欄まで見え、入りきらなければ最後のワーカーでさらに `j` を押すと閉じた欄の先までスクロールする（`k` はまずスクロールを戻す。選択は最後のワーカーのまま）。ワーカーが 0 件になったら `j`/`k` で閉じた欄をスクロールする。ボードを開いてから閉じたワーカーは、最後の状態と detail を「閉じた」欄に 5 件まで残す。送信欄は ^U（全消去）と ^W（1 語消去）を受け、宛先と操作を 1 行上に出す。作業中のワーカーを閉じる確認ではそのことを告げる。ペインのタイトル（OSC 2）を「ボード · 要対応 N」にし、終了時に `CSI 22/23 t` で元に戻す。

### D8：ワーカーを float に置ける（2026-11-17）

ボードを見ているあいだワーカーのペインを隠したい、という owner の求めで、`spawn --float`（`noa.split` の `direction:"float"`）を足した。

- ワーカーはオーケストレーターのペインにアンカーした float として開き、`focus:false`（`+ctl spawn` の既定）ならタイトル帯だけにたたんだ状態で、`focus:true`（`--focus`）なら開いてフォーカスを持った状態で始まる。`direction:"float"` は `parentPaneId` を必須にする（ないと invalid params）。一覧に載るのはワーカーの float だけなので、親のない float を IPC から作れないようにした。タイトル帯はエージェントの状態と要対応の色を出すので、たたんだままでも止まったワーカーが分かる。開くのはタイトル帯か、ボードの `g`（`focus_pane` がたたんだ float を開く）。
- floating-panes の REQ-023 を改め、IPC は `parentPaneId` を持つ float（ワーカー）だけを列挙する。AppleScript、AX、Overview はこれまでどおりすべての float を出さない。ワーカーでない float は IPC からも見えない。
- `auto` の分割候補から float のワーカーを外す（分割レイアウトにないため）。
- 1 つのペインに置ける float は 8 つ（`MAX_PER_ANCHOR`）で、超えると spawn はエラーになる。`--max-children` の既定と同じ数。
- セッション復元では orchestration の情報を戻さないので、復元した float はワーカーでなくなり、IPC からは見えなくなる。

### D9：ワーカーは常に float、最初の spawn でボードを開く（2026-11-17）

owner の指示で、`+ctl spawn` はワーカーを必ず float として起動し、`--split` と `--tab` を拒否する（`--float` は互換のため受け付けて何もしない）。オーケストレーションを始めたら、ボードも開く。

- 強制するのは `+ctl` の層である。サーバーの `noa.split`（`horizontal`/`vertical`/`auto`）と `noa.newTab` の拡張は、ほかの API クライアントのために残す。
- spawn の後、そのオーケストレーターのボードがなければ `board --open` と同じ配置（右に分割、余地がなければ新しいタブ）で開く。ボードは `board:<オーケストレーターの pane id>` というラベルで見分け、すでにあれば開かない。開けなくても、警告を出すだけで spawn は成功させる。`--no-board` で開かない。
- ボードはワーカーではない（`parentPaneId` なし）ので、`close --children` では閉じない。閉じるのは人。
- ワーカーが分割しなくなったので、分割に伴うズームの解除（未再現の既知の制約）はオーケストレーションでは起きない。
- `--max-children` の既定 8 は、1 つのペインに置ける float の数（8）と同じ。

### D6：エージェントごとの差はフックの設定だけに閉じ込める

状態の報告経路は 3 種とも `OSC 777;noa-agent` に揃える。各エージェントのフックが `"$NOA_BIN" +ctl report` を呼ぶだけにし、Noa 本体と `+ctl wait` には種別ごとの分岐を持たせない。`scripts/noa-agent-hook.py` は既存の利用者の設定を壊さないよう残し、`+ctl report` は同じ処理（制御端末へ書く、端末がなければ何もせず成功）を持つ。

| kind | `running`/`finished` の出どころ | 承認・回答待ちの出どころ | 裏付け |
|---|---|---|---|
| claude | フック（`UserPromptSubmit`、`Stop` ほか） | フック（`PermissionRequest`、`Notification`） | リポジトリ内に実装あり（`scripts/noa-agent-hook.py`、`docs/AGENT_WORKFLOW.md`） |
| codex | フック（`UserPromptSubmit`、`Stop`） | 対応するフックは確認できていない。TUI の通知（OSC 9）が `attention` になるので、それで代用する | 外部記事のみ。有効化の方法と設定場所は版によって違う。未検証（A6） |
| agy | フック（`PreInvocation`、`Stop`。`.agents/hooks.json`） | 確認できていない。`attention` で代用する | 公式 docs の検索結果のみ。`Stop` をターン終了とみなすのは推測。未検証（A6） |

- `attention` は「通知が出た」であって「承認待ち」の証明ではない（`docs/specs/agent-attention.md` L0）。codex と agy では、wait が `attention` で解けたら、オーケストレーターが `read` で画面を見て判断する。
- 承認待ちで止まらないようにする手段は 2 つあり、どちらも使うかどうかは人間が決める：各エージェント自身の自動承認フラグを `spawn` の引数で渡す。Noa の auto-approve モードを worker のタブで有効にする。この spec はどちらも既定では有効にしない。
- 送信の作法（貼り付けの後に `\r`）が 3 種で同じかは未検証（A1）。違った場合は `+ctl send` が `agent.kind` を見て切り替える。分岐を置くのはここだけにする。
- codex と agy のフックの設定書式はドキュメントに載せていない（版ごとに違い、確認できていないため）。`docs/AGENT_WORKFLOW.md` には「どのイベントで何を呼ぶか」だけを書いた。

## 採らなかった案

- サーバー側の `noa.waitFor`：接続ごとに 1 スレッドの同期サーバーで（`crates/noa-ipc/src/lib.rs` の NFR-3）、待機中その接続が塞がる。subscribe と同じことを二重に持つことにもなる。
- 新メソッド `noa.spawn`：`newTab`/`split` と配置の指定が重複する。AppleScript と形が分かれる。
- ペイン単位のトークンで「自分の子孫しか触れない」ように閉じ込める：同じユーザーのプロセスは、トークンなしの `osascript` で任意のペインに入力できる（`docs/specs/agent-automation.md` L0）。境界にならないものに `auth.rs` の変更を払わない。この判断が成り立つのは macOS で `macos-applescript` が既定の `true` のときだけである（`docs/CONFIGURATION.md:264`、`crates/noa-app/src/app/applescript.rs:189`）。`false` にした環境と Linux ではトークンが唯一の関門になるので、そこを守りたくなったら再検討する（A5）。
- サーバー側の run/task オブジェクト（タスクキュー、依存関係、再試行）：呼び出し側は 1 つで、順序はそのシェルスクリプトが持てる。
- AppleScript を土台にする：macOS 限定で、イベント購読がなくポーリングになる。`wait for` は既に却下済み（`docs/specs/agent-automation.md`「Considered but rejected」）。
- MCP サーバー：シェルを持つエージェントなら CLI で足りる。
- 画面の文言から承認待ちを推定する（auto-approve の検出器 `crates/noa-app/src/auto_approve.rs:45-54` には codex と agy のシグネチャがある）：今は auto-approve が有効なタブでしか走らない（`crates/noa-app/src/io_thread/auto_approve.rs:54`）。全 worker で走らせると pty の feed ごとに画面を走査することになり、出力スループットの保証（`docs/positioning.md` Guardrails）に触れる。`attention` で足りないと分かってから検討する。
- Noa がタスクに合うエージェントを選ぶ：選ぶ基準を持っているのはオーケストレーターで、Noa ではない。
- エージェントごとの起動プロファイル（`--agent codex` のような別名）：`-- codex …` と書けば済む。

## Scope

- In：D1〜D4 と D6、`docs/api/noa-server-protocol.md` と `docs/AGENT_WORKFLOW.md` の更新、smoke スクリプト。
- Out：UI（D5 の別 spec）、codex と agy のフック設定の自動書き込み（設定例をドキュメントに載せるだけ）、worker の自動承認の既定有効化、`parent`/`label` のセッション復元、LAN 越しのオーケストレーション、`server-enable` の既定値の変更、mutation の冪等キー（at-least-once のまま。`spawn` をタイムアウト後に再送すると二重起動しうる）。

## 前提と反証

| ID | 前提 | 安く確かめる方法 | 外れたら |
|---|---|---|---|
| A1（要） | claude / codex / agy のそれぞれで、フックが毎ターン `running`→`finished` を出し、`sendText` の貼り付け + `\r` でプロンプトが送信される | 今の main で各エージェントのペインに `noa.sendText` を 2 回送り、サイドバーの状態が 2 往復するか見る（codex と agy はフックから `printf` で OSC 777 を出す仮設定で試す） | そのエージェントでは D3 の `seq` 待機が成り立たない。出力の静止検出を足すか、対象から外す |
| A2 | `focus:false` で元の focus に戻しても、IME の変換中や分割アニメーションを壊さない | 変換中に `noa.split` を送る | `focus` を外し、focus を奪わない分割経路を別に作る |
| A3 | `agent`/`exit` の変化だけでも `stateChanged` が遅延なく出る | `card_to_panel` の差分テストに 1 ケース足す | 配信の間引き条件を直す |
| A4 | ペインの子プロセスの PATH に `noa` が入っていない | 実機のペインで `command -v noa` | 入っていれば `NOA_BIN` は要らない |
| A5 | トークンなしの `osascript` で任意ペインに入力できる（spec とゲートのコードを読んだだけで、実行はしていない） | `osascript -e 'tell application "Noa" to input text …'` を別ペインに向けて実行 | 閉じ込め案を再検討する |
| A6 | codex と agy に、プロンプト送信時とターン終了時に外部コマンドを呼ぶフックがある | 手元の版で公式 docs と `/hooks` を確認する | そのエージェントは `exit` と `attention` でしか待てない |

A1 と A6 は未検証。この設計全体がこの 2 つに乗っている。

## 実装記録（2026-11-16）

- `noa-ipc`：`Panel` の 5 フィールド、`SpawnConfig`/`SpawnParams`/`SplitPlacement`、`IpcBackend::spawn`、`Client` の `spawn_tab`/`spawn_split`/`send_text`/`subscribe`/`next_notification`。テストは `crates/noa-ipc/tests/orchestration_tests.rs`。
- `noa-app`：`execute_ipc_spawn` と自動配置（`crates/noa-app/src/app/ipc.rs`）、`Panel` への写像（`crates/noa-app/src/ipc_bridge.rs`）、`SessionCard.agent_seq`、`NOA_BIN`、`Noa +ctl`（`crates/noa-app/src/ctl.rs`）。
- `noa-pty`：`Pty::spawn_env`（surface id を `Surface` に残すため）。
- 自動テストで確かめたこと：引数解析、待機条件（古い報告で解けない、`--after`、attention、exit・消滅）、自動配置の順序、`Panel` への写像、サーバーの振り分けとスコープ、通知のバッファ。
- 確かめていないこと：GUI 上での実際の分割・focus 復元（A2）、各エージェントでの送信と報告（A1・A6）。`scripts/agent-orchestration-smoke.sh` を Noa のペインで実行すると、エージェントなしで spawn → exit 待ち → read → report 待ち → close → ガードまでを確かめられる。

次のフェーズ：UI（D5）の spec を起こす。
