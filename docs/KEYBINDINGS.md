# Noa キーボードショートカット一覧

Noa が処理するキーボードショートカットの全数リファレンス(シェル側のキーは含まない)。
既定値の実装源は `crates/noa-app/src/commands/keybind.rs` の `KeybindEngine::default()`。
config の `keybind =` はこの既定表に順番に適用される。有効なバインド一覧は CLI からも確認できる:

```bash
noa +list-keybinds
```

## config の `keybind =`

`keybind = <chord>=<action>` で既定表へ追加または上書きできる。同じ chord は後勝ち。
`keybind = <chord>=unbind` はその chord を解除し、`keybind = clear` はそれ以前の全バインドを消去する。

```text
keybind = cmd+i=prompt_surface_title
keybind = cmd+t=unbind
keybind = cmd+shift+n=tab.new
```

`<chord>` は `+` 区切り。修飾キー別名は `cmd`/`command`/`super`/`meta`、`ctrl`/`control`、`alt`/`option`、`shift`。キーは単一文字、`plus`、`arrowup`/`up` 等の矢印(短縮別名可)、`pageup`、`pagedown`、`home`、`end`、`enter`/`return`、`grave`/`backtick`(`` ` ``)を受け付ける。

`<action>` は下の「canonical action 一覧」にある名前を使う。`noa +list-keybinds` は現在有効な
バインドだけを表示するため、既定で未バインドの action は出力しない。互換入力として
`new_tab`、`prompt_surface_title`、`toggle_quick_terminal` など一部の Ghostty 風 action 名も
受け付ける。

### canonical action 一覧

| カテゴリ | action |
|---|---|
| アプリ | `about`, `preferences`, `config.reload`, `app.quit` |
| 編集 | `copy`, `paste`, `pane.send-selection` |
| 端末 | `terminal.clear`, `terminal.clear-scrollback`, `terminal.select-all`, `terminal.export-scrollback`, `terminal.pipe-scrollback-to-pager` |
| フォント | `font-size.increase`, `font-size.decrease`, `font-size.reset` |
| 検索 | `search.find`, `search.next`, `search.previous`, `search.clear` |
| スクロール | `scroll.line-up`, `scroll.line-down`, `scroll.page-up`, `scroll.page-down`, `scroll.top`, `scroll.bottom`, `scroll.prev-prompt`, `scroll.next-prompt` |
| タブ | `tab.new`, `tab.close`, `tab.next`, `tab.previous`, `tab.set-title`, `tab.select-1` … `tab.select-9` |
| ウィンドウ | `window.new`, `window.close`, `fullscreen.toggle` |
| 分割 | `split.new-left`, `split.new-right`, `split.new-up`, `split.new-down`, `split.focus-left`, `split.focus-right`, `split.focus-up`, `split.focus-down`, `split.resize-left`, `split.resize-right`, `split.resize-up`, `split.resize-down`, `split.equalize`, `split.toggle-zoom` |
| UI | `session-overview.toggle`, `command-palette.toggle`, `quick-terminal.toggle`, `secure-keyboard-entry.toggle`, `sidebar.toggle`, `auto-approve.toggle`, `theme-settings.open` |

`tab-overview.toggle` も `session-overview.toggle` の互換名として受け付ける。入力に `_` が
含まれる場合は `-` に置き換えた名前も照合される。Ghostty 風 alias の完全な対応表は
`crates/noa-app/src/commands/keybind.rs` の `ghostty_action_alias` が真実源。

## グローバル(ターミナルフォーカス時)

### アプリ / ウィンドウ / タブ

| キー | アクション |
|---|---|
| ⌘Q | 終了 |
| ⌘T | 新規タブ |
| ⌘N | 新規ウィンドウ |
| ⌘W | タブを閉じる |
| ⌘⇧W | ウィンドウを閉じる |
| ⌘⌃F | フルスクリーン切替 |
| ⌘1 〜 ⌘9 | タブ 1〜9 を選択 |
| ⌘⇧] | 次のタブ |
| ⌘⇧[ | 前のタブ |

### 分割ペイン(Splits)

| キー | アクション |
|---|---|
| ⌘D | 右にペイン追加 |
| ⌘⇧D | 下にペイン追加 |
| ⌘⌃← / → / ↑ / ↓ | 分割フォーカス移動 |
| ⌘⌥← / → / ↑ / ↓ | 分割フォーカス移動(別名) |
| ⌘⌃⇧← / → / ↑ / ↓ | 分割リサイズ |
| ⌘⌃= | 分割を均等化 |
| ⌘⇧Enter | 分割ズームのトグル |

Add Pane Left / Add Pane Up はデフォルトキーバインドなし。コマンドパレットまたは右クリックコンテキストメニューから実行できる。
ペイン追加は各行/列最大3枚、1タブあたり最大9ペインまで。上限到達時の追加は no-op。
コマンドパレットと右クリックコンテキストメニューでは、これ以上作成できない Add Pane 方向は disabled になる。
分割系はメニューにはなく、キーバインドと右クリックコンテキストメニュー(Add Pane Left / Add Pane Right / Add Pane Up / Add Pane Down / Equalize Splits / Toggle Split Zoom)からのみ到達可能。

### 編集 / 端末 / フォント

| キー | アクション |
|---|---|
| ⌘C | コピー |
| ⌘V | ペースト |
| ⌘⇧M | 選択範囲をペインへ送信 |
| ⌘A | すべて選択 |
| ⌘K | 画面クリア |
| ⌘= / ⌘⇧+ | フォント拡大 |
| ⌘- | フォント縮小 |
| ⌘0 | フォントサイズをリセット |

### 検索

| キー | アクション |
|---|---|
| ⌘F | 検索プロンプトを開く |
| ⌘G | 次を検索 |
| ⌘⇧G | 前を検索 |

⌘⇧F は将来用に意図的に未割り当て。

### スクロール(ビューポート操作、pty へは送らない)

| キー | アクション |
|---|---|
| ⇧↑ / ⇧↓ | 1 行スクロール |
| ⇧PageUp / ⇧PageDown | 1 ページスクロール |
| ⇧Home / ⇧End | 先頭 / 末尾へ |
| ⌘↑ / ⌘↓ | 前 / 次のプロンプトへジャンプ(シェル統合 OSC 133 が前提) |

Shift 単独スクロールは他の修飾キーが付くと発動しない。

### オーバーレイ起動

| キー | アクション |
|---|---|
| ⌘⇧O | セッションオーバービュー(タブ俯瞰)のトグル |
| ⌘⇧P | コマンドパレットのトグル |
| ⌘⇧S | サイドバーのトグル |

既定キーバインドがない action もコマンドパレット / メニューから実行できる。主なものは
Reload Configuration、Clear Scrollback、Toggle Quick Terminal、Secure Keyboard Entry、About、
Open Preferences、Open Theme & Settings、Export Scrollback、Pipe Scrollback to Pager、
Toggle Auto Approve、Set Tab Title。

> 未バインドの ⌘ 併用キーは pty へ漏らさず握り潰される。

## グローバルシステムホットキー

Carbon `RegisterEventHotKey` によるシステム全域ホットキー。アプリが非フォーカスでも発火する。config で変更可能。

| config キー | 既定値 | アクション |
|---|---|---|
| `quick-terminal-hotkey` | `cmd+grave`(⌘`) | Quick Terminal のトグル |
| `sidebar-hotkey` | なし(無効) | サイドバーのトグル |

構文は `+` 区切りのチョード(例: `cmd+shift+t`)。修飾キー別名:
`cmd`/`command`/`super`/`meta`、`ctrl`/`control`、`alt`/`option`、`shift`。
キーは英字、数字、および次の token を受け付ける。

- 記号: `=`/`equal`, `-`/`minus`, `[`/`leftbracket`, `]`/`rightbracket`,
  `;`/`semicolon`, `,`/`comma`, `.`/`period`, `/`/`slash`
- 基本キー: `enter`/`return`, `tab`, `space`, `escape`/`esc`
- backtick: `grave`, `backtick`, `` ` ``
- backslash: `backslash` または `\`。ANSI `\` と JIS `¥` / `ろ` を同時登録
- JIS 個別指定: `yen`/`jis-yen`/`intl-yen`,
  `underscore`/`jis-underscore`/`intl-ro` (`_` と `-` の別名も可)

in-app の `keybind` と異なり、global hotkey は矢印、`PageUp` / `PageDown`、`Home` / `End`
を受け付けない。`none` / `off` / `false` / 空値で hotkey を無効化できる。

## オーバーレイ内のキー操作

各オーバーレイはモーダルで、表示中のキー入力は pty に到達しない。

### 検索プロンプト(⌘F)

| キー | 動作 |
|---|---|
| Escape | 閉じてクエリをクリア |
| Enter / ⇧Enter | 開いたまま次 / 前のマッチへ移動 |
| ⌘G / ⌘⇧G | 開いたまま次 / 前へ |
| ⌘F(再押下) | 閉じる(ハイライトとアクティブマッチは維持) |
| Backspace | 1 文字削除 |
| 印字文字 | クエリに追記 |

### コマンドパレット(⌘⇧P)

| キー | 動作 |
|---|---|
| Escape | 実行せず閉じる |
| Enter | 選択中のコマンドを実行 |
| ↑ / ↓ | 選択移動 |
| ⌘⇧P | 閉じる(トグル) |
| 印字文字 | クエリに追記(サブシーケンス絞り込み) |

### セッションオーバービュー(⌘⇧O)

| キー | 動作 |
|---|---|
| ← / → / ↑ / ↓ | タイル選択の移動 |
| Enter | 選択タブを開く |
| Escape | 2 段階: 検索クエリがあればクリア、なければ閉じる |
| Tab | quick-look ズームのトグル |
| ⌘1 〜 ⌘9 | タブへ即切替 |
| 印字文字 | 検索クエリに追記 |

### 確認ダイアログ(ペースト保護 / OSC 52 / クローズ確認)

| キー | 動作 |
|---|---|
| Enter / y | 確定・実行 |
| Escape / n | キャンセル |

### サイドバーのインラインリネーム

| キー | 動作 |
|---|---|
| Enter | 確定(空文字はキャンセル扱い) |
| Escape | キャンセル |

## マウス + 修飾キー

| 操作 | 動作 |
|---|---|
| ⇧ + クリック / ドラッグ / ホイール | マウストラッキングモードをバイパスしてローカル選択 / スクロール |
| ⌘ + ホバー | リンク(OSC 8 / 自動検出 URL)上でポインタ化 + 下線 |
| ⌘ + 左クリック | ホバー中のリンクを開く |
| 左ダブルクリック | 単語選択 |
| 左トリプルクリック | 行選択 |
| 右クリック | ペインをフォーカスし分割コンテキストメニューを表示 |

## 主要ソース

- `crates/noa-app/src/commands/keybind.rs` — `KeybindEngine`・既定バインド・config適用(真実源)
- `crates/noa-app/src/commands/command.rs` — `AppCommand`・action名の相互変換
- `crates/noa-app/src/commands/key_token.rs` — チョードパーサー・キー別名
- `crates/noa-app/src/commands.rs` — 上記モジュールのfacade / re-export
- `crates/noa-app/src/macos_menu.rs` — メニューアクセラレータ + コンテキストメニュー
- `crates/noa-app/src/app/event_loop.rs` — キー / マウスのルーティング
- `crates/noa-app/src/app/input_ops.rs` — 検索プロンプト / コマンドパレット / 確認ダイアログ
- `crates/noa-app/src/macos_hotkey.rs` — グローバルホットキー
- `docs/CONFIGURATION.md` — config キー・値・既定値の全数リファレンス
