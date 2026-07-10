# Noa 設定リファレンス

この文書は、現在の `noa-config` 実装が受け付ける config キー、値、既定値のリファレンス。
キーバインドの既定表と action 名は [KEYBINDINGS.md](KEYBINDINGS.md) を参照。

## 読み込み場所と書式

Noa は起動時に `$XDG_CONFIG_HOME/noa/config` を読み込む。`XDG_CONFIG_HOME` が未設定なら
`~/.config/noa/config` を使う。旧 `config.toml` は内容を読み込まず、移行警告だけを表示する。

書式は Ghostty 互換の行指向 `key = value`。空行と、行頭の空白を除いて `#` で始まる行は
無視される。値全体を二重引用符で囲むことはできるが、エスケープシーケンスや行末コメントは
解釈しない。

```conf
# Window size is measured in terminal cells.
window-width = 100
window-height = 30
font-family = "Fira Code"
font-size = 15
theme = "Catppuccin Mocha"
```

同じ scalar キーが複数回現れた場合は最後の行が優先される。`font-family*`、
`font-feature`、`font-variation*`、`keybind` は繰り返し可能で、記述順に蓄積される。
CLI オプションは config ファイルより優先される。

現在の解決済み設定は次のコマンドで確認できる。

```bash
noa +show-config
```

現時点の `+show-config` は `background-image*` と `resize-overlay` を出力しないため、
それらは config ファイルの値を直接確認する。

## ウィンドウとセッション

| キー | 許容値 | 既定値 | 説明 |
|---|---|---|---|
| `window-width` | `0..=65535` の整数 | `80` | 列数。config では `window-height` と同時指定が必要で、解決時に最小 `10` へ切り上げる |
| `window-height` | `0..=65535` の整数 | `24` | 行数。config では `window-width` と同時指定が必要で、解決時に最小 `4` へ切り上げる |
| `window-padding-x` | `0` 以上の有限小数 | 未指定 | 左右 padding。未指定時は左 `24`、右 `16` physical px |
| `window-padding-y` | `0` 以上の有限小数 | 未指定 | 上下 padding。未指定時は上 `0`、下 `16` physical px |
| `window-save-state` | `default`, `never`, `always` | `default` | `default` と `always` は保存・復元し、`never` は無効化する |
| `confirm-quit` | `true`, `false` | `true` | アプリ終了前の確認 |
| `resize-overlay` | `after-first`, `always`, `never` | `after-first` | リサイズ時の `cols × rows` 表示。`after-first` は初回レイアウトだけ除外 |

`window-width` と `window-height` の片方だけを config に書いた場合は両方とも無視され、診断が
表示される。CLI の `--cols` / `--rows` はそれぞれ単独指定できる。

## フォント

| キー | 許容値 | 既定値 | 説明 |
|---|---|---|---|
| `font-size` | `0` より大きい有限小数 | `14` | フォントサイズ |
| `font-family` | 空でない family 名 | 未指定 | 通常体の優先順。未指定時は macOS の `Menlo` を優先する platform fallback |
| `font-family-bold` | 空でない family 名 | 未指定 | bold 専用 family の優先順 |
| `font-family-italic` | 空でない family 名 | 未指定 | italic 専用 family の優先順 |
| `font-family-bold-italic` | 空でない family 名 | 未指定 | bold italic 専用 family の優先順 |
| `font-feature` | 4 文字 ASCII tag、または `-` + tag | なし | 例: `calt`, `liga`, `-dlig`。繰り返し可能 |
| `font-variation` | `<4文字ASCII axis>=<有限小数>` | なし | 例: `wght=650`。繰り返し可能 |
| `font-variation-bold` | 同上 | なし | bold 用 variable-font axis |
| `font-variation-italic` | 同上 | なし | italic 用 variable-font axis |
| `font-variation-bold-italic` | 同上 | なし | bold italic 用 variable-font axis |
| `font-synthetic-style` | `true`, `false`, `no-bold`, `no-italic` | `true` 相当 | synthetic bold / italic の許可 |
| `font-thicken` | `true`, `false` | `true` | グリフの stem thickening |
| `font-thicken-strength` | `0..=255` の整数 | `255` | thickening 強度。`0` は効果なし |
| `alpha-blending` | `native`, `linear`, `linear-corrected` | `native` | `linear` 系は認識するが診断を表示し、現在は `native` にフォールバック |

family、feature、variation の各キーは複数行書ける。通常体の例:

```conf
font-family = Fira Code
font-family = Menlo
font-feature = calt
font-feature = -dlig
font-variation = wght=550
```

## テーマ、色、カーソル

| キー | 許容値 | 既定値 | 説明 |
|---|---|---|---|
| `theme` | 同梱テーマ名 1 個 | 未指定 | `crates/noa-theme/vendor/themes/` の `.conf` を除いた名前。`light:...` / `dark:...` のペア指定は未対応 |
| `background` | `#RRGGBB` または `RRGGBB` | テーマ値 | 背景色 override |
| `foreground` | `#RRGGBB` または `RRGGBB` | テーマ値 | 前景色 override |
| `cursor-color` | `#RRGGBB` または `RRGGBB` | テーマ値 | カーソル色 override |
| `selection-foreground` | `#RRGGBB` または `RRGGBB` | テーマ値 | 選択文字色 override |
| `selection-background` | `#RRGGBB` または `RRGGBB` | テーマ値 | 選択背景色 override |
| `minimum-contrast` | `1.0..=21.0` の有限小数 | `1.0` | WCAG コントラスト比の下限。`1.0` は補正なし |
| `cursor-style` | `block`, `bar`, `underline` | blinking block | `block_hollow` は認識するが未対応として無視 |
| `cursor-style-blink` | `true`, `false` | `true` 相当 | カーソル点滅。shape だけ指定した場合も点滅する |
| `background-opacity` | 有限小数 | `1.0` | `0.0..=1.0` へ clamp |
| `background-blur-radius` | `true`, `false`, 非負整数 | `0` | macOS blur。`true` は `20`、`false` は `0`、整数は `0..=64` へ clamp |

テーマ一覧は `noa +list-themes` で確認できる。

## 背景画像

| キー | 許容値 | 既定値 | 説明 |
|---|---|---|---|
| `background-image` | `noa`、PNG ファイル、ディレクトリのパス | なし | `noa` はアプリ同梱壁紙、パスは `~` を展開する。ディレクトリの場合は直下の PNG を名前順にローテーション |
| `background-image-opacity` | 有限小数 | `1.0` | `0.0..=1.0` へ clamp。ウィンドウの opacity とは独立 |
| `background-image-position` | `top-left`, `top-center`, `top-right`, `center-left`, `center`, `center-right`, `bottom-left`, `bottom-center`, `bottom-right` | `center` | 配置または crop の anchor |
| `background-image-fit` | `none`, `contain`, `cover`, `stretch` | `contain` | 拡大縮小方法 |
| `background-image-repeat` | `true`, `false` | `false` | 画像を tile 表示 |
| `background-image-interval` | 正の整数秒 | `30` | ディレクトリの切替間隔。`1..=4` は `5` 秒へ切り上げる |

未指定または空の `background-image` は背景画像を表示しない。完全一致の `noa` を指定した場合
だけアプリ同梱壁紙を使用する。画像デコードは PNG のみ対応する。ファイルがない、PNG でない、
デコードできない場合は診断を表示して背景画像を無効化する。

## 端末、クリップボード、ベル

| キー | 許容値 | 既定値 | 説明 |
|---|---|---|---|
| `scrollback-limit` | `0` 以上の整数 | `10000000` | scrollback の総 byte 数。`0` は無効化 |
| `clipboard-read` | `deny` / `false`, `ask`, `allow` / `true` | `ask` | OSC 52 clipboard read の policy |
| `clipboard-paste-protection` | `true`, `false` | `true` | コマンド実行につながり得る paste の確認 |
| `title-report` | `true`, `false` | `false` | `CSI 21 t` による window title 応答を許可 |
| `visual-bell` | `true`, `false` | `false` | BEL 時にウィンドウを flash |
| `audible-bell` | `true`, `false` | `false` | BEL 時に platform sound を再生 |
| `audible-bell-when-unfocused` | `true`, `false` | `false` | audible bell を非フォーカス時だけ鳴らす |
| `audible-bell-dock-bounce` | `true`, `false` | `false` | 非フォーカス時の audible BEL で Dock attention。macOS のみ |
| `auto-approve` | `true`, `false` | `false` | 新規 tab の agent CLI auto approval 初期値 |

## Quick Terminal とサイドバー

| キー | 許容値 | 既定値 | 説明 |
|---|---|---|---|
| `quick-terminal-hotkey` | global hotkey chord、または `none` / `off` / `false` | `cmd+grave` | Quick Terminal の system-wide hotkey。空値も無効化 |
| `quick-terminal-size` | 正の有限小数、または百分率 | `0.4` | 画面高に対する比率。`0.1..=1.0` へ clamp。例: `40%` |
| `quick-terminal-autohide` | `true`, `false` | `true` | focus を失ったとき自動で隠す |
| `sidebar-enabled` | `true`, `false` | `false` | 新規 window の sidebar 初期表示 |
| `sidebar-width` | `0` 以上の有限小数 | `360` | sidebar 幅 (points) |
| `sidebar-hotkey` | global hotkey chord、または `none` / `off` / `false` | なし | Sidebar の system-wide hotkey。空値も無効化 |
| `sidebar-preview-lines` | `0..=20` の整数 | `5` | card に表示する末尾行数。`0` は preview なし |

global hotkey chord の構文と対応キーは [KEYBINDINGS.md](KEYBINDINGS.md#グローバルシステムホットキー)
を参照。

## macOS

| キー | 許容値 | 既定値 | 説明 |
|---|---|---|---|
| `macos-option-as-alt` | `false` / `none`, `true` / `both`, `left` / `only-left`, `right` / `only-right` | `false` | Option キーを terminal Alt として扱う範囲 |
| `macos-titlebar-style` | `native` / `tabs`, `transparent` | `native` | 通常 terminal window の titlebar |
| `macos-non-native-fullscreen` | `true`, `false` | `false` | native fullscreen Space の代わりに borderless fullscreen を使う |
| `macos-titlebar-proxy-icon` | `visible` / `true`, `hidden` / `false` | `visible` | titlebar に focus 中 pane の OSC 7 pwd を proxy icon として表示するか |

macOS 以外では macOS 専用の表示・window 動作は no-op になる。

## キーバインド

`keybind` は繰り返し可能で、上から順番に既定バインドへ適用される。

```conf
keybind = cmd+i=tab.set-title
keybind = cmd+t=unbind
keybind = clear
keybind = cmd+shift+n=tab.new
```

- `keybind = <chord>=<action>`: chord を追加または上書き
- `keybind = <chord>=unbind`: chord を解除
- `keybind = clear`: その時点までの既定・追加バインドをすべて削除

chord の構文、全 canonical action、既定バインドは [KEYBINDINGS.md](KEYBINDINGS.md) を参照。

## 認識するが未対応のキー

| キー | 現在の動作 |
|---|---|
| `palette` | 診断を表示して無視。palette override は未実装 |
| `config-file` | 診断を表示して無視。config include は未実装 |

未知のキーと不正な値は診断を表示し、その override を適用しない。

## Ghostty config のインポート

`noa --import-ghostty-config` は Ghostty の候補 config を読み、Noa がインポート対象として認識する
行を `$XDG_CONFIG_HOME/noa/config` へコピーする。対象ファイルが既に存在する場合は上書きしない。
未対応行は削除せず `# ` でコメントアウトする。
